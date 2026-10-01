//! # Platform-specific process termination
//!
//! Wraps `sysinfo` (and, on Windows, a few `kernel32` calls) so the rest of
//! the crate can stay platform-free.
//!
//! - Unix: default sends `SIGTERM`; `force = true` sends `SIGKILL`.
//! - Windows: always calls `TerminateProcess` (equivalent to `taskkill /F`).
//!   There is no reliable graceful equivalent for arbitrary processes, so
//!   `force` is accepted but has no behavioral effect.
//!
//! Every kill re-verifies the process identity (name and start time) that was
//! captured at resolve time, so a PID that was released and reused while the
//! user sat at the confirmation prompt is never signaled. On Windows the final
//! start-time check and `TerminateProcess` both go through one process handle,
//! which pins the process object and closes the reuse window entirely. On Unix
//! a window of microseconds remains between the re-check and `kill(2)`.

use std::collections::HashMap;

use log::debug;
use sysinfo::{Pid, Process, ProcessRefreshKind, ProcessesToUpdate, System};

/// Outcome of a single kill attempt.
///
/// The `Failed` variant is Unix-only: on Windows, `TerminateProcess` failures
/// that are not "process already exited" map to `PermissionDenied` (the
/// overwhelmingly common cause: access denied, protected processes), so no
/// generic-failure variant is needed. On Unix, `kill(2)` can return errors
/// beyond `ESRCH` / `EPERM` (for example `EINVAL`), hence the extra case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillOutcome {
    /// Signal/terminate request succeeded.
    Signaled,
    /// Process was already gone at signal time (idempotent success).
    AlreadyGone,
    /// Operating system refused the request (permissions, protected process).
    PermissionDenied,
    /// The PID now belongs to a different process than the one resolved
    /// (PID reuse, or the identity could not be verified). Nothing was signaled.
    ProcessChanged,
    /// `kill(2)` returned an error that is neither `ESRCH` nor `EPERM`.
    #[cfg(unix)]
    Failed,
}

/// Where a process comes from, used to tell a genuine operating system
/// process from a same-named impostor (a developer's `services.exe` build or
/// a Unix binary called `init`). Every field is `None` when it is unknown.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessOrigin {
    /// Full path of the executable image. Usually unreadable for protected
    /// processes when portlens is not elevated.
    #[cfg(windows)]
    pub exe: Option<std::path::PathBuf>,
    /// Whether the real user id is `0`.
    #[cfg(unix)]
    pub root_owned: Option<bool>,
    /// Parent PID. `sysinfo` reports a parent of `0` as `None` on Linux.
    #[cfg(unix)]
    pub parent_pid: Option<u32>,
}

impl ProcessOrigin {
    fn of(process: &Process) -> Self {
        #[cfg(windows)]
        {
            Self {
                exe: process.exe().map(std::path::Path::to_path_buf),
            }
        }

        #[cfg(unix)]
        {
            Self {
                root_owned: process.user_id().map(|uid| **uid == 0),
                parent_pid: process.parent().map(Pid::as_u32),
            }
        }

        #[cfg(not(any(unix, windows)))]
        {
            let _ = process;
            Self::default()
        }
    }
}

/// Identity of a process captured at resolve time, used to detect PID reuse
/// before signaling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessIdentity {
    /// Process name as reported by `sysinfo`.
    pub name: String,
    /// Process start time in seconds since the Unix epoch, as reported by
    /// `sysinfo` (`0` when the OS did not let us read it).
    pub start_time: u64,
    /// Executable path or ownership, used only for the critical-process
    /// check. Not part of identity matching.
    pub origin: ProcessOrigin,
}

impl ProcessIdentity {
    fn of(process: &Process) -> Self {
        Self {
            name: process.name().to_string_lossy().into_owned(),
            start_time: process.start_time(),
            origin: ProcessOrigin::of(process),
        }
    }
}

/// What a resolve-time snapshot must load beyond name and start time: the
/// executable path on Windows, the owning user on Unix.
fn snapshot_refresh_kind() -> ProcessRefreshKind {
    #[cfg(windows)]
    {
        ProcessRefreshKind::nothing().with_exe(sysinfo::UpdateKind::OnlyIfNotSet)
    }

    #[cfg(unix)]
    {
        ProcessRefreshKind::nothing().with_user(sysinfo::UpdateKind::OnlyIfNotSet)
    }

    #[cfg(not(any(unix, windows)))]
    {
        ProcessRefreshKind::nothing()
    }
}

/// Return `true` when `current` is the same process that was captured as
/// `expected`. Both the name and the start time must match exactly.
#[must_use]
pub fn identity_matches(expected: &ProcessIdentity, current: &ProcessIdentity) -> bool {
    expected.start_time == current.start_time && expected.name == current.name
}

/// Capture the identity of every live PID in `pids` with one process refresh.
///
/// PIDs that are not currently visible are simply absent from the map.
#[must_use]
pub fn snapshot_identities(pids: &[u32]) -> HashMap<u32, ProcessIdentity> {
    let sys_pids: Vec<Pid> = pids.iter().copied().map(Pid::from_u32).collect();
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&sys_pids),
        false,
        snapshot_refresh_kind(),
    );

    pids.iter()
        .filter_map(|&pid| {
            sys.process(Pid::from_u32(pid))
                .map(|process| (pid, ProcessIdentity::of(process)))
        })
        .collect()
}

/// Return whether `pid` currently refers to a live process.
#[must_use]
pub(super) fn pid_exists(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Some(pid) = unix_pid(pid) else {
            return false;
        };

        // SAFETY: `kill(pid, 0)` never delivers a signal; it only probes
        // whether the process exists and whether we have permission to signal it.
        let rc = unsafe { libc::kill(pid, 0) };
        rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    #[cfg(windows)]
    {
        windows_pid_exists(pid)
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

/// Attempt to terminate `pid`, but only if it is still the process described
/// by `expected`. See module docs for platform behavior.
///
/// `expected` is `None` when no identity could be captured at resolve time; a
/// live process under that PID then cannot be verified and is not signaled.
#[must_use]
pub fn kill_pid(pid: u32, expected: Option<&ProcessIdentity>, force: bool) -> KillOutcome {
    let mut sys = System::new();
    let target = Pid::from_u32(pid);
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[target]),
        false,
        ProcessRefreshKind::nothing(),
    );

    let Some(process) = sys.process(target) else {
        return KillOutcome::AlreadyGone;
    };

    let current = ProcessIdentity::of(process);
    let Some(expected) = expected.filter(|e| identity_matches(e, &current)) else {
        debug!(
            "pid {pid} identity changed since resolve: expected={expected:?} current={current:?}"
        );
        return KillOutcome::ProcessChanged;
    };

    #[cfg(unix)]
    {
        let _ = expected;
        let signal = if force {
            sysinfo::Signal::Kill
        } else {
            sysinfo::Signal::Term
        };
        match process.kill_with(signal) {
            Some(true) => KillOutcome::Signaled,
            Some(false) => classify_unix_failure(pid),
            None => KillOutcome::Failed,
        }
    }

    #[cfg(windows)]
    {
        // sysinfo's Windows kill spawns `taskkill /PID`, which re-resolves the
        // PID after an arbitrary delay. Terminate through a verified handle instead.
        let _ = (process, force);
        terminate_verified(pid, expected.start_time)
    }

    #[cfg(not(any(unix, windows)))]
    {
        // Unsupported target: no platform kill primitive is wired up.
        // Treat as permission-denied so the caller surfaces a clear status
        // without requiring a Unix-only `Failed` variant on this target.
        let _ = (process, expected, force);
        KillOutcome::PermissionDenied
    }
}

#[cfg(unix)]
fn unix_pid(pid: u32) -> Option<libc::pid_t> {
    libc::pid_t::try_from(pid).ok()
}

#[cfg(unix)]
fn classify_unix_failure(pid: u32) -> KillOutcome {
    let Some(pid) = unix_pid(pid) else {
        return KillOutcome::AlreadyGone;
    };

    // SAFETY: `kill(pid, 0)` never delivers a signal; it only probes whether
    // the process exists and whether we have permission to signal it.
    let rc = unsafe { libc::kill(pid, 0) };
    if rc == 0 {
        KillOutcome::Failed
    } else {
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::ESRCH) => KillOutcome::AlreadyGone,
            Some(libc::EPERM) => KillOutcome::PermissionDenied,
            _ => KillOutcome::Failed,
        }
    }
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;

    /// Win32 `FILETIME`: 100-nanosecond intervals since 1601-01-01 UTC.
    #[repr(C)]
    #[derive(Default)]
    pub struct FileTime {
        pub low: u32,
        pub high: u32,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn OpenProcess(
            desired_access: u32,
            inherit_handle: i32,
            process_id: u32,
        ) -> *mut c_void;
        pub fn GetExitCodeProcess(process: *mut c_void, exit_code: *mut u32) -> i32;
        pub fn GetProcessTimes(
            process: *mut c_void,
            creation: *mut FileTime,
            exit: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
        pub fn TerminateProcess(process: *mut c_void, exit_code: u32) -> i32;
    }

    pub const PROCESS_TERMINATE: u32 = 0x0001;
    pub const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    pub const ERROR_ACCESS_DENIED: i32 = 5;
    pub const STILL_ACTIVE: u32 = 259;
}

/// Open `pid` with `access` and wrap the handle so it is closed on drop.
#[cfg(windows)]
fn open_process(access: u32, pid: u32) -> Option<std::os::windows::io::OwnedHandle> {
    use std::os::windows::io::FromRawHandle;

    // Safety: `OpenProcess` only reads the PID and returns either a process
    // handle or a null pointer. No borrowed Rust references cross the FFI boundary.
    let raw = unsafe { win::OpenProcess(access, 0, pid) };
    if raw.is_null() {
        return None;
    }
    // Safety: `raw` is a valid process handle we exclusively own; `OwnedHandle`
    // closes it exactly once when dropped.
    Some(unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(raw) })
}

/// Return whether the process behind `handle` has not exited yet.
#[cfg(windows)]
fn handle_is_running(handle: &std::os::windows::io::OwnedHandle) -> bool {
    use std::os::windows::io::AsRawHandle;

    let mut exit_code = 0_u32;
    // Safety: the handle is open for the lifetime of `handle`, and `exit_code`
    // points to valid writable memory for the duration of the call.
    let ok = unsafe { win::GetExitCodeProcess(handle.as_raw_handle(), &raw mut exit_code) };
    ok != 0 && exit_code == win::STILL_ACTIVE
}

/// Creation time of the process behind `handle`, in seconds since the Unix
/// epoch, using the same conversion as `sysinfo` so the values are comparable.
#[cfg(windows)]
fn handle_start_time(handle: &std::os::windows::io::OwnedHandle) -> Option<u64> {
    use std::os::windows::io::AsRawHandle;

    // Seconds between the Windows epoch (1601-01-01) and the Unix epoch.
    const WINDOWS_TO_UNIX_EPOCH_SECS: u64 = 11_644_473_600;

    let mut creation = win::FileTime::default();
    let mut exit = win::FileTime::default();
    let mut kernel = win::FileTime::default();
    let mut user = win::FileTime::default();
    // Safety: the handle is open for the lifetime of `handle`, and every
    // out-pointer refers to a distinct, writable `FileTime` on this stack frame.
    let ok = unsafe {
        win::GetProcessTimes(
            handle.as_raw_handle(),
            &raw mut creation,
            &raw mut exit,
            &raw mut kernel,
            &raw mut user,
        )
    };
    if ok == 0 {
        return None;
    }
    let ticks = (u64::from(creation.high) << 32) | u64::from(creation.low);
    (ticks / 10_000_000).checked_sub(WINDOWS_TO_UNIX_EPOCH_SECS)
}

/// Return `true` when the creation time read from the handle proves it is the
/// process captured at resolve time. An unknown expected start time (`0`)
/// cannot prove anything, so it never matches.
#[cfg(any(windows, test))]
const fn handle_start_time_matches(expected: u64, actual: Option<u64>) -> bool {
    match actual {
        Some(actual) => expected != 0 && expected == actual,
        None => false,
    }
}

/// Terminate `pid` through a handle whose creation time matches
/// `expected_start`. The handle pins the process object, so the check and the
/// termination refer to the same process even if the PID is reused meanwhile.
#[cfg(windows)]
fn terminate_verified(pid: u32, expected_start: u64) -> KillOutcome {
    use std::os::windows::io::AsRawHandle;

    let access = win::PROCESS_TERMINATE | win::PROCESS_QUERY_LIMITED_INFORMATION;
    let Some(handle) = open_process(access, pid) else {
        return classify_windows_failure(pid);
    };

    if !handle_start_time_matches(expected_start, handle_start_time(&handle)) {
        debug!("pid {pid} creation time does not match the resolved process");
        return KillOutcome::ProcessChanged;
    }
    if !handle_is_running(&handle) {
        return KillOutcome::AlreadyGone;
    }

    // Safety: the handle is open with PROCESS_TERMINATE access for the
    // lifetime of `handle`.
    let ok = unsafe { win::TerminateProcess(handle.as_raw_handle(), 1) };
    if ok != 0 {
        KillOutcome::Signaled
    } else if handle_is_running(&handle) {
        // Most common remaining cause on Windows is ERROR_ACCESS_DENIED.
        KillOutcome::PermissionDenied
    } else {
        KillOutcome::AlreadyGone
    }
}

#[cfg(windows)]
fn windows_pid_exists(pid: u32) -> bool {
    // The error closure runs immediately after the failed `OpenProcess`, so
    // `last_os_error` still holds its error code.
    open_process(win::PROCESS_QUERY_LIMITED_INFORMATION, pid).map_or_else(
        || std::io::Error::last_os_error().raw_os_error() == Some(win::ERROR_ACCESS_DENIED),
        |handle| handle_is_running(&handle),
    )
}

#[cfg(windows)]
fn classify_windows_failure(pid: u32) -> KillOutcome {
    if windows_pid_exists(pid) {
        // Most common remaining cause on Windows is ERROR_ACCESS_DENIED.
        KillOutcome::PermissionDenied
    } else {
        KillOutcome::AlreadyGone
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(name: &str, start_time: u64) -> ProcessIdentity {
        ProcessIdentity {
            name: name.to_owned(),
            start_time,
            origin: ProcessOrigin::default(),
        }
    }

    #[test]
    fn identity_matches_same_name_and_start_time() {
        assert!(
            identity_matches(
                &identity("node", 1_700_000_000),
                &identity("node", 1_700_000_000)
            ),
            "an unchanged process must be accepted"
        );
    }

    #[test]
    fn identity_mismatch_on_start_time_means_pid_reuse() {
        assert!(
            !identity_matches(
                &identity("node", 1_700_000_000),
                &identity("node", 1_700_000_042)
            ),
            "a different start time under the same pid is a reused pid"
        );
    }

    #[test]
    fn identity_mismatch_on_name() {
        assert!(
            !identity_matches(
                &identity("node", 1_700_000_000),
                &identity("lsass.exe", 1_700_000_000)
            ),
            "a different process name under the same pid must not be signaled"
        );
    }

    #[test]
    fn handle_start_time_requires_known_matching_value() {
        assert!(handle_start_time_matches(
            1_700_000_000,
            Some(1_700_000_000)
        ));
        assert!(
            !handle_start_time_matches(1_700_000_000, Some(1_700_000_001)),
            "a different creation time on the handle means a different process"
        );
        assert!(
            !handle_start_time_matches(0, Some(1_700_000_000)),
            "an unknown resolve-time start time cannot verify the handle"
        );
        assert!(
            !handle_start_time_matches(1_700_000_000, None),
            "an unreadable creation time cannot verify the handle"
        );
    }

    #[test]
    fn snapshot_identities_captures_own_process() {
        let me = std::process::id();
        let identities = snapshot_identities(&[me, u32::MAX]);
        let own = identities
            .get(&me)
            .expect("the test process should be visible to sysinfo");
        assert!(!own.name.is_empty(), "own process name should be captured");
        assert!(
            own.start_time > 0,
            "own process start time should be captured"
        );
        assert!(
            !identities.contains_key(&u32::MAX),
            "impossible pids must not produce an identity"
        );
    }

    /// A harmless child process used as a kill target, so a regression in the
    /// identity check can only ever terminate this child, never the test
    /// runner. The child is killed and reaped on drop.
    struct SacrificialChild(std::process::Child);

    impl SacrificialChild {
        fn spawn() -> Self {
            #[cfg(windows)]
            let mut command = {
                let mut c = std::process::Command::new("ping");
                c.args(["-n", "30", "127.0.0.1"]);
                c
            };
            #[cfg(not(windows))]
            let mut command = {
                let mut c = std::process::Command::new("sleep");
                c.arg("30");
                c
            };
            let child = command
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("the sacrificial child process should spawn");
            Self(child)
        }

        fn pid(&self) -> u32 {
            self.0.id()
        }

        /// Return `true` while the child has not exited.
        fn is_running(&mut self) -> bool {
            matches!(self.0.try_wait(), Ok(None))
        }
    }

    impl Drop for SacrificialChild {
        fn drop(&mut self) {
            drop(self.0.kill());
            drop(self.0.wait());
        }
    }

    #[test]
    fn kill_pid_refuses_unverifiable_identity() {
        let mut child = SacrificialChild::spawn();
        assert_eq!(
            kill_pid(child.pid(), None, false),
            KillOutcome::ProcessChanged,
            "a live pid without a captured identity must not be signaled"
        );
        assert!(child.is_running(), "the child must not have been signaled");
    }

    #[test]
    fn kill_pid_refuses_mismatched_identity() {
        let mut child = SacrificialChild::spawn();
        let pid = child.pid();
        let mut expected = snapshot_identities(&[pid])
            .remove(&pid)
            .expect("the child process should be visible to sysinfo");
        expected.start_time += 1;
        assert_eq!(
            kill_pid(pid, Some(&expected), false),
            KillOutcome::ProcessChanged,
            "a reused pid must be reported, not signaled"
        );
        assert!(child.is_running(), "the child must not have been signaled");
    }

    #[test]
    fn kill_pid_refuses_mismatched_name() {
        let mut child = SacrificialChild::spawn();
        let pid = child.pid();
        let mut expected = snapshot_identities(&[pid])
            .remove(&pid)
            .expect("the child process should be visible to sysinfo");
        expected.name.push_str("-other");
        assert_eq!(
            kill_pid(pid, Some(&expected), false),
            KillOutcome::ProcessChanged,
            "a different process name under the pid must not be signaled"
        );
        assert!(child.is_running(), "the child must not have been signaled");
    }

    #[cfg(windows)]
    #[test]
    fn snapshot_captures_executable_path() {
        let child = SacrificialChild::spawn();
        let pid = child.pid();
        let identity = snapshot_identities(&[pid])
            .remove(&pid)
            .expect("the child process should be visible to sysinfo");
        let exe = identity
            .origin
            .exe
            .expect("an unprivileged child's executable path should be readable");
        assert!(
            exe.to_string_lossy()
                .to_ascii_lowercase()
                .ends_with("ping.exe"),
            "unexpected exe path: {}",
            exe.display()
        );
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_captures_owner_and_parent() {
        let child = SacrificialChild::spawn();
        let pid = child.pid();
        let identity = snapshot_identities(&[pid])
            .remove(&pid)
            .expect("the child process should be visible to sysinfo");
        assert_eq!(
            identity.origin.parent_pid,
            Some(std::process::id()),
            "the child's parent should be the test process"
        );
        assert!(
            identity.origin.root_owned.is_some(),
            "the child's owner should be readable"
        );
    }
}
