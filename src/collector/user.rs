//! User identity resolution and OS privilege detection.
//!
//! Resolves process owners to human-readable usernames and checks
//! whether the current process has sufficient privileges for full
//! socket visibility.

use std::collections::HashMap;
#[cfg(unix)]
use std::ffi::CStr;
#[cfg(unix)]
use std::mem::MaybeUninit;
use std::sync::Arc;
#[cfg(windows)]
use sysinfo::Users;

/// Cache for resolved usernames, keyed by the OS-specific identity.
///
/// Values are `Arc<str>` so every `PortEntry` owned by the same user
/// shares one heap allocation; cache clones are refcount bumps.
#[derive(Default)]
pub(super) struct UserResolver {
    #[cfg(unix)]
    pub names_by_uid: HashMap<libc::uid_t, Arc<str>>,
    #[cfg(windows)]
    pub names_by_sid: HashMap<String, Arc<str>>,
    #[cfg(windows)]
    users: Users,
}

#[inline]
fn unknown_user() -> Arc<str> {
    Arc::from("-")
}

// ---------------------------------------------------------------------------
// Unix: getpwuid_r
// ---------------------------------------------------------------------------

/// Resolve the owning username for an already-looked-up process.
///
/// Returns `"-"` if the process or user cannot be determined.
#[cfg(unix)]
pub(super) fn resolve_user(
    process: Option<&sysinfo::Process>,
    _pid: u32,
    resolver: &mut UserResolver,
) -> Arc<str> {
    let Some(proc_ref) = process else {
        return unknown_user();
    };

    let Some(uid) = proc_ref.user_id() else {
        return unknown_user();
    };

    let uid = **uid;
    if let Some(cached) = resolver.names_by_uid.get(&uid) {
        return Arc::clone(cached);
    }

    let name: Arc<str> =
        lookup_unix_username(uid).map_or_else(unknown_user, |s| Arc::from(s.as_str()));
    resolver.names_by_uid.insert(uid, Arc::clone(&name));
    name
}

#[cfg(unix)]
fn lookup_unix_username(uid: libc::uid_t) -> Option<String> {
    let mut buffer_len = match unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) } {
        suggested if suggested > 0 => usize::try_from(suggested).ok()?,
        _ => 1024,
    };

    loop {
        let mut password = MaybeUninit::<libc::passwd>::uninit();
        let mut buffer = vec![0_u8; buffer_len];
        let mut result = std::ptr::null_mut();

        let status = unsafe {
            libc::getpwuid_r(
                uid,
                password.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &raw mut result,
            )
        };

        if status == 0 {
            if result.is_null() {
                return None;
            }

            let password = unsafe { password.assume_init() };
            if password.pw_name.is_null() {
                return None;
            }

            let name = unsafe { CStr::from_ptr(password.pw_name) }
                .to_str()
                .ok()?
                .to_string();
            return Some(name);
        }

        if status != libc::ERANGE {
            return None;
        }

        buffer_len = buffer_len.saturating_mul(2);
        if buffer_len > 1024 * 1024 {
            return None;
        }
    }
}

// ---------------------------------------------------------------------------
// Windows: SID-based user resolution
// ---------------------------------------------------------------------------

#[cfg(windows)]
pub(super) fn resolve_user(
    process: Option<&sysinfo::Process>,
    _pid: u32,
    resolver: &mut UserResolver,
) -> Arc<str> {
    let Some(uid) = process.and_then(sysinfo::Process::user_id) else {
        return unknown_user();
    };

    let sid = format_windows_user_id(uid);
    if let Some(cached) = resolver.names_by_sid.get(&sid) {
        return Arc::clone(cached);
    }

    let resolved_name = resolve_windows_user_name(
        resolver.users.get_user_by_id(uid).map(sysinfo::User::name),
        uid,
    );
    resolver
        .names_by_sid
        .insert(sid, Arc::clone(&resolved_name));
    resolved_name
}

#[cfg(windows)]
fn format_windows_user_id(uid: &sysinfo::Uid) -> String {
    (**uid).to_string()
}

#[cfg(windows)]
fn resolve_windows_user_name(resolved_name: Option<&str>, uid: &sysinfo::Uid) -> Arc<str> {
    resolved_name.filter(|name| !name.is_empty()).map_or_else(
        || Arc::from(format_windows_user_id(uid).as_str()),
        Arc::from,
    )
}

// ---------------------------------------------------------------------------
// Fallback: no user resolution
// ---------------------------------------------------------------------------

#[cfg(not(any(unix, windows)))]
pub(super) fn resolve_user(
    _process: Option<&sysinfo::Process>,
    _pid: u32,
    _resolver: &mut UserResolver,
) -> Arc<str> {
    unknown_user()
}

// ---------------------------------------------------------------------------
// Privilege detection
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
pub(super) fn has_full_visibility_privileges() -> bool {
    // Safety: `geteuid` is a simple libc call with no preconditions.
    unsafe { libc::geteuid() == 0 }
}

#[cfg(windows)]
pub(super) fn has_full_visibility_privileges() -> bool {
    use std::ffi::c_void;

    #[repr(C)]
    struct TokenElevation {
        token_is_elevated: i32,
    }

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn OpenProcessToken(
            process_handle: *mut c_void,
            desired_access: u32,
            token_handle: *mut *mut c_void,
        ) -> i32;
        fn GetTokenInformation(
            token_handle: *mut c_void,
            token_information_class: u32,
            token_information: *mut c_void,
            token_information_length: u32,
            return_length: *mut u32,
        ) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    const TOKEN_QUERY: u32 = 0x0008;
    const TOKEN_ELEVATION_CLASS: u32 = 20;

    let Ok(info_len) = u32::try_from(std::mem::size_of::<TokenElevation>()) else {
        return false;
    };

    let mut token_handle = std::ptr::null_mut();
    // Safety: the pseudo-handle from `GetCurrentProcess` is always valid for
    // querying the current token, and `token_handle` points to writable memory.
    let opened =
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token_handle) };
    if opened == 0 {
        return false;
    }

    let mut elevation = TokenElevation {
        token_is_elevated: 0,
    };
    let mut returned_len = 0;
    // Safety: `token_handle` came from `OpenProcessToken`, and both output
    // buffers point to valid writable memory of the declared size.
    let ok = unsafe {
        GetTokenInformation(
            token_handle,
            TOKEN_ELEVATION_CLASS,
            (&raw mut elevation).cast(),
            info_len,
            &raw mut returned_len,
        )
    };
    // Safety: `token_handle` was successfully opened above and must be closed
    // exactly once regardless of whether the token query succeeded.
    let _ = unsafe { CloseHandle(token_handle) };

    ok != 0 && elevation.token_is_elevated != 0
}

#[cfg(not(any(target_os = "linux", windows)))]
pub(super) fn has_full_visibility_privileges() -> bool {
    true
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use super::*;

    #[cfg(windows)]
    #[test]
    fn windows_user_id_formatting_uses_sid_string() {
        let uid = "S-1-5-18"
            .parse::<sysinfo::Uid>()
            .expect("well-known SID should parse into sysinfo::Uid");

        assert_eq!(
            format_windows_user_id(&uid),
            "S-1-5-18",
            "Windows fallback should preserve the SID string when account-name lookup is unavailable"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_user_name_prefers_resolved_account_name() {
        let uid = "S-1-5-18"
            .parse::<sysinfo::Uid>()
            .expect("well-known SID should parse into sysinfo::Uid");

        let resolved = resolve_windows_user_name(Some("SYSTEM"), &uid);

        assert_eq!(
            &*resolved, "SYSTEM",
            "resolved Windows account names should be preferred over raw SID strings"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_user_name_falls_back_to_sid_when_lookup_is_missing() {
        let uid = "S-1-5-18"
            .parse::<sysinfo::Uid>()
            .expect("well-known SID should parse into sysinfo::Uid");

        let resolved = resolve_windows_user_name(None, &uid);

        assert_eq!(
            &*resolved, "S-1-5-18",
            "missing Windows account lookups should fall back to the SID string"
        );
    }
}
