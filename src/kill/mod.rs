//! # Kill: terminate processes by port or PID
//!
//! Cross-platform process termination. Targets are resolved to a unique set
//! of PIDs (multiple sockets per process are collapsed; multiple processes
//! on one port are all targeted) and then signaled via the `sysinfo` wrapper.
//!
//! See the `platform` submodule for exact per-OS signal semantics and
//! [`run`] for the end-to-end orchestration including confirmation, reporting,
//! and exit-code classification.

mod platform;
mod report;
mod resolve;

use std::io::{BufRead, IsTerminal, Write};

use anyhow::{Result, bail};
use log::debug;

use self::platform::{ProcessOrigin, kill_pid, pid_exists};
use self::report::KillReportEntry;
use self::resolve::{ResolvedTarget, Target, target_for_pid, targets_for_port};
use crate::display::{is_broken_pipe, sanitize_for_terminal};
use crate::filter::PortFilter;

/// Target selector for a kill invocation.
#[derive(Debug, Clone)]
pub enum KillTarget {
    /// Kill TCP listeners or UDP binders on one or more local ports.
    Port(PortFilter),
    /// Kill a single PID directly.
    Pid(u32),
}

/// Options controlling a kill invocation.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone)]
pub struct KillOptions {
    /// What to kill.
    pub target: KillTarget,
    /// Escalate to SIGKILL (Unix); no-op on Windows (always forceful).
    pub force: bool,
    /// Skip the interactive confirmation prompt.
    pub yes: bool,
    /// Resolve targets and report them, but do not signal anything.
    pub dry_run: bool,
    /// Emit JSON instead of human-readable lines.
    pub json: bool,
}

/// Exit code when the user declined the confirmation prompt.
const EXIT_ABORTED: u8 = 1;
/// Exit code for usage errors, matching the CLI's own usage-error code.
const EXIT_USAGE: u8 = 2;
/// Exit code when the selector matched nothing.
const EXIT_NOTHING_TO_KILL: u8 = 3;
/// Exit code when a `--port` selector matched only protected processes, so
/// nothing would be (or was) signaled. Not `3`: something did match.
const EXIT_ALL_PROTECTED: u8 = 1;

/// How the confirmation step should behave for one invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfirmMode {
    /// No prompt: `--yes` or `--dry-run` was given.
    Skip,
    /// Ask on stderr and read the answer from the interactive stdin.
    Prompt,
    /// Confirmation is required but stdin is not a terminal, so nobody can
    /// answer. Refuse instead of killing unconfirmed.
    RefuseNonInteractive,
}

/// Decide how to confirm a kill. Pure so the non-TTY refusal is testable.
const fn confirm_mode(yes: bool, dry_run: bool, stdin_is_terminal: bool) -> ConfirmMode {
    if yes || dry_run {
        ConfirmMode::Skip
    } else if stdin_is_terminal {
        ConfirmMode::Prompt
    } else {
        ConfirmMode::RefuseNonInteractive
    }
}

/// Run the confirmation step for `mode`.
///
/// Returns `Some(exit_code)` when the run must stop (the user declined, or
/// nobody can answer), or `None` to proceed. `ask` is only invoked in
/// [`ConfirmMode::Prompt`].
fn confirmation_exit(mode: ConfirmMode, ask: impl FnOnce() -> Result<bool>) -> Result<Option<u8>> {
    match mode {
        ConfirmMode::Prompt if !ask()? => Ok(Some(EXIT_ABORTED)),
        ConfirmMode::RefuseNonInteractive => Ok(Some(EXIT_USAGE)),
        ConfirmMode::Skip | ConfirmMode::Prompt => Ok(None),
    }
}

/// Run a kill operation end-to-end.
///
/// Returns `Ok(exit_code)` where:
/// - `0`: every non-protected target succeeded (or was already gone), or a
///   dry run with at least one target. Protected processes skipped by a
///   `--port` selector do not count as failures.
/// - `1`: at least one target failed (permission denied, pid reused, other
///   errors), every match of a `--port` selector was protected (also in a
///   dry run), or the user declined the confirmation prompt.
/// - `2`: confirmation is required but stdin is not a terminal; pass `--yes`.
///   Checked before targets are resolved.
/// - `3`: nothing to kill (no PID matched the selector).
///
/// Errors propagate only for unexpected conditions such as socket enumeration
/// failure, a protected `--pid` target, or stdout/stderr write failure.
pub fn run(opts: &KillOptions) -> Result<u8> {
    debug!(
        "kill run: target={:?} force={} yes={} dry_run={} json={}",
        opts.target, opts.force, opts.yes, opts.dry_run, opts.json
    );

    // Decide before resolving anything: a piped or scripted run without
    // `--yes` must never kill unconfirmed.
    let mode = confirm_mode(opts.yes, opts.dry_run, std::io::stdin().is_terminal());
    if mode == ConfirmMode::RefuseNonInteractive {
        eprintln!(
            "error: refusing to kill without confirmation because stdin is not a terminal; pass --yes to proceed or --dry-run to preview"
        );
        eprintln!();
        eprintln!("Try 'portlens --help' for more information.");
        return Ok(EXIT_USAGE);
    }

    let targets = resolve_targets(opts)?;

    if targets.is_empty() {
        debug!("no kill targets resolved for selector");
        let msg = match &opts.target {
            KillTarget::Port(f) => {
                format!("no TCP listener or UDP binder is using local port {f}")
            }
            KillTarget::Pid(pid) => format!("no process with pid {pid}"),
        };
        eprintln!("{msg}");
        return Ok(EXIT_NOTHING_TO_KILL);
    }

    let pid_mode = matches!(opts.target, KillTarget::Pid(_));
    let (targets, skipped) = partition_protected(targets, pid_mode, std::process::id())?;

    debug!(
        "resolved {} kill target(s), skipping {} protected",
        targets.len(),
        skipped.len()
    );

    if targets.is_empty() {
        // Only reachable in `--port` mode: `--pid` refuses a protected target.
        eprintln!("nothing to kill: every matching process is protected");
        return report_then_exit(&skipped, opts.json, EXIT_ALL_PROTECTED);
    }

    // A prompt that cannot be shown (stderr closed) is treated as declined,
    // so nothing is killed and the run exits 1 rather than looking successful.
    let prompt = || {
        confirm(&targets, &skipped, opts).or_else(|e| {
            if is_broken_pipe(&e) {
                Ok(false)
            } else {
                Err(e)
            }
        })
    };
    if let Some(code) = confirmation_exit(mode, prompt)? {
        writeln!(std::io::stderr(), "aborted").ok();
        return Ok(code);
    }

    if opts.dry_run {
        announce_dry_run(&targets, &skipped, opts)?;
        return Ok(0);
    }

    let mut report = Vec::with_capacity(targets.len() + skipped.len());
    let mut any_failure = false;
    for t in targets {
        let entry = execute_target(t, opts.force);
        if entry.is_failure() {
            any_failure = true;
        }
        report.push(entry);
    }
    report.extend(skipped);

    report_then_exit(&report, opts.json, u8::from(any_failure))
}

/// Print the report and return `code`. The kills have already happened by
/// now, so a reader that closed the pipe early (e.g. `| head -0`) must not
/// turn a failure code into success; only other write errors propagate.
fn report_then_exit(report: &[KillReportEntry], json: bool, code: u8) -> Result<u8> {
    keep_code_on_closed_pipe(print_report(report, json), code)
}

fn keep_code_on_closed_pipe(printed: Result<()>, code: u8) -> Result<u8> {
    match printed {
        Err(e) if !is_broken_pipe(&e) => Err(e),
        _ => Ok(code),
    }
}

fn print_report(report: &[KillReportEntry], json: bool) -> Result<()> {
    if json {
        report::print_json(report)
    } else {
        report::print_human(report)
    }
}

/// Execute a single resolved target (process kill or container stop).
fn execute_target(target: ResolvedTarget, force: bool) -> KillReportEntry {
    match target {
        ResolvedTarget::Process(t) => {
            debug!(
                "killing process: pid={} process={} force={force}",
                t.pid, t.process
            );
            let outcome = kill_pid(t.pid, t.identity.as_ref(), force);
            KillReportEntry::from_outcome(t.pid, t.process, outcome)
        }
        ResolvedTarget::Container(ct) => {
            debug!(
                "stopping container: id={} name={} force={force}",
                ct.container_id, ct.container_name
            );
            let outcome =
                crate::docker::stop_container(&ct.container_id, force, what_stack::home_dir());
            KillReportEntry::from_container_outcome(ct, outcome)
        }
    }
}

fn resolve_targets(opts: &KillOptions) -> Result<Vec<ResolvedTarget>> {
    // Note: `--port 0` is rejected at CLI-parse time so it produces a usage
    // exit code (2); callers here can rely on `port >= 1`.
    match &opts.target {
        KillTarget::Port(filter) => targets_for_port(*filter),
        KillTarget::Pid(pid) => Ok(resolve_pid_target(*pid).into_iter().collect()),
    }
}

fn resolve_pid_target(pid: u32) -> Option<ResolvedTarget> {
    // Protected PIDs are kept even when they are not enumerable (pid 0 on
    // Windows, for example) so the user gets a refusal instead of "no process".
    if protected_reason(pid, std::process::id(), &[], None).is_some() || pid_exists(pid) {
        let target = target_for_pid(pid).unwrap_or_else(|| Target {
            pid,
            process: "-".to_owned(),
            identity: None,
        });
        return Some(ResolvedTarget::Process(target));
    }

    None
}

/// Critical OS processes that a port-killing tool must never terminate,
/// compared case-insensitively with any trailing `.exe` removed. Killing
/// `csrss` or `wininit` bugchecks Windows; the others break logon,
/// authentication, or the service control manager.
#[cfg(windows)]
const CRITICAL_PROCESS_NAMES: &[&str] =
    &["csrss", "wininit", "winlogon", "lsass", "services", "smss"];

/// Critical OS processes that a port-killing tool must never terminate,
/// compared case-insensitively. Covers init systems running outside PID 1
/// (for example a `systemd --user` session manager holding activated sockets).
#[cfg(unix)]
const CRITICAL_PROCESS_NAMES: &[&str] = &["init", "systemd", "launchd"];

/// No critical-process denylist is defined for other targets.
#[cfg(not(any(unix, windows)))]
const CRITICAL_PROCESS_NAMES: &[&str] = &[];

/// Return `true` when `name` is on the critical OS process denylist.
fn is_critical_process_name(name: &str) -> bool {
    let lower = name.trim().to_ascii_lowercase();
    let stem = lower.strip_suffix(".exe").unwrap_or(&lower);
    CRITICAL_PROCESS_NAMES.contains(&stem)
}

/// Return `true` when `exe` lies under `dir`, compared case-insensitively
/// with `/` treated as `\` and a `\\?\` prefix ignored. Paths containing a
/// `..` segment never match.
#[cfg(windows)]
fn is_path_under_dir(exe: &str, dir: &str) -> bool {
    fn normalize(path: &str) -> String {
        let path = path.replace('/', "\\").to_ascii_lowercase();
        path.strip_prefix(r"\\?\")
            .map_or_else(|| path.clone(), str::to_owned)
    }

    let exe = normalize(exe);
    let mut dir = normalize(dir);
    if !dir.ends_with('\\') {
        dir.push('\\');
    }
    exe.starts_with(&dir) && !exe.split('\\').any(|segment| segment == "..")
}

/// The real system directory (normally `C:\Windows\System32`), asked from
/// the OS rather than read from `%SystemRoot%`, which the caller controls.
/// `None` if the call fails.
#[cfg(windows)]
pub(crate) fn system32_dir() -> Option<String> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetSystemDirectoryW(buffer: *mut u16, size: u32) -> u32;
    }

    let mut buffer = [0u16; 512];
    let capacity = u32::try_from(buffer.len()).ok()?;
    // Safety: the buffer is valid for `capacity` UTF-16 units, and the call
    // writes at most that many (returning the required size if larger).
    let len = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), capacity) };
    let len = usize::try_from(len).ok()?;
    if len == 0 || len >= buffer.len() {
        return None;
    }
    String::from_utf16(&buffer[..len]).ok()
}

/// Return `true` when `exe` is a plain drive-letter path (`X:\...`, with an
/// optional `\\?\` prefix) and contains no 8.3 short-name segment. Only such
/// paths can be compared reliably against the system directory.
#[cfg(windows)]
fn is_plain_drive_path(exe: &str) -> bool {
    let path = exe.replace('/', "\\");
    let path = path.strip_prefix(r"\\?\").unwrap_or(&path);
    let bytes = path.as_bytes();
    bytes.len() > 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && bytes[2] == b'\\'
        && !path.contains('~')
}

/// Return `true` unless `origin` proves the process is not the operating
/// system's own. This fails closed: the process counts as non-system only
/// when its executable is a plain drive-letter path that lies outside the
/// system directory reported by the OS. An unknown or empty path (typical
/// for protected processes when portlens is not elevated), device or NT
/// namespace paths, 8.3 short names, and a failed system directory lookup
/// are all treated as a system process.
#[cfg(windows)]
fn is_system_origin(origin: &ProcessOrigin) -> bool {
    is_system_exe(origin.exe.as_deref(), system32_dir().as_deref())
}

#[cfg(windows)]
fn is_system_exe(exe: Option<&std::path::Path>, system32: Option<&str>) -> bool {
    let (Some(exe), Some(system32)) = (exe, system32) else {
        return true;
    };
    let exe = exe.to_string_lossy();
    if !is_plain_drive_path(&exe) || exe.split(['\\', '/']).any(|segment| segment == "..") {
        return true;
    }
    is_path_under_dir(&exe, system32)
}

/// Return `true` unless `origin` proves the process is not the operating
/// system's own: it must be owned by a non-root user and have a parent other
/// than PID 0 or 1. Missing information is treated as a system process.
#[cfg(unix)]
const fn is_system_origin(origin: &ProcessOrigin) -> bool {
    let non_root = matches!(origin.root_owned, Some(false));
    let ordinary_parent = matches!(origin.parent_pid, Some(ppid) if ppid > 1);
    !(non_root && ordinary_parent)
}

/// No origin information exists for other targets; fail safe.
#[cfg(not(any(unix, windows)))]
const fn is_system_origin(_origin: &ProcessOrigin) -> bool {
    true
}

/// Return the first of `names` that marks a critical OS process, or `None`.
///
/// A denylisted name alone is not enough: the process must also look like
/// the operating system's own (see `is_system_origin`), so a developer's
/// binary that happens to share the name stays killable. `origin` is `None`
/// when nothing is known about the process, which fails safe.
fn critical_process_name<'a>(names: &[&'a str], origin: Option<&ProcessOrigin>) -> Option<&'a str> {
    let name = names
        .iter()
        .copied()
        .find(|name| is_critical_process_name(name))?;
    origin.is_none_or(is_system_origin).then_some(name)
}

/// Return why a process must never be signaled, or `None` when it is an
/// acceptable target. `names` lists every known name for the process and
/// `origin` its executable path or ownership, when known.
///
/// This is the single source of truth for protected targets. It is not
/// overridable by `--force`: none of these is a legitimate target for a
/// port-killing tool.
fn protected_reason(
    pid: u32,
    self_pid: u32,
    names: &[&str],
    origin: Option<&ProcessOrigin>,
) -> Option<String> {
    if pid == 0 {
        return Some("kernel/system idle process".to_owned());
    }
    if pid == self_pid {
        return Some("this portlens process".to_owned());
    }
    #[cfg(unix)]
    if pid == 1 {
        return Some("init process".to_owned());
    }
    #[cfg(windows)]
    if pid == 4 {
        return Some("Windows System process".to_owned());
    }
    critical_process_name(names, origin)
        .map(|name| format!("critical operating system process '{name}'"))
}

/// Return why the process target `p` is protected, or `None`.
fn target_protected_reason(p: &Target, self_pid: u32) -> Option<String> {
    let identity_name = p.identity.as_ref().map(|i| i.name.as_str());
    let names: Vec<&str> = std::iter::once(p.process.as_str())
        .chain(identity_name)
        .collect();
    let origin = p.identity.as_ref().map(|i| &i.origin);
    protected_reason(p.pid, self_pid, &names, origin)
}

/// Split resolved targets into those to act on and skipped protected ones.
///
/// In `--pid` mode (`pid_mode`) a protected target is an error: the user
/// named exactly that process. In `--port` mode protected processes are
/// skipped with a `protected` report entry so that a range spanning OS-owned
/// ports (for example the Windows dynamic RPC range, where `lsass`,
/// `wininit`, and `services` listen) still frees every other port.
fn partition_protected(
    targets: Vec<ResolvedTarget>,
    pid_mode: bool,
    self_pid: u32,
) -> Result<(Vec<ResolvedTarget>, Vec<KillReportEntry>)> {
    let mut kept = Vec::with_capacity(targets.len());
    let mut skipped = Vec::new();
    for t in targets {
        // Container targets are stopped via the daemon API. The proxy PID is
        // informational; we never signal it directly.
        let ResolvedTarget::Process(p) = t else {
            kept.push(t);
            continue;
        };
        let Some(reason) = target_protected_reason(&p, self_pid) else {
            kept.push(ResolvedTarget::Process(p));
            continue;
        };
        if pid_mode {
            bail!(
                "refusing to kill pid {} ({}): {}",
                p.pid,
                sanitize_for_terminal(&p.process),
                sanitize_for_terminal(&reason)
            );
        }
        debug!("skipping protected pid {}: {reason}", p.pid);
        skipped.push(KillReportEntry::from_protected(p.pid, p.process, reason));
    }
    Ok((kept, skipped))
}

fn announce_dry_run(
    targets: &[ResolvedTarget],
    skipped: &[KillReportEntry],
    opts: &KillOptions,
) -> Result<()> {
    if opts.json {
        let mut report = dry_run_report(targets, opts.force);
        report.extend_from_slice(skipped);
        return report::print_json(&report);
    }

    let mut out = std::io::stdout().lock();
    let (n_proc, n_ctr) = count_target_kinds(targets);
    let kind = dry_run_kind(opts.force);

    if n_proc > 0 {
        writeln!(out, "dry-run: would {kind} {n_proc} process(es):")?;
    }
    if n_ctr > 0 {
        let verb = if opts.force { "force-stop" } else { "stop" };
        writeln!(out, "dry-run: would {verb} {n_ctr} container(s):")?;
    }

    for t in targets {
        write_target_line(&mut out, t)?;
    }
    if !skipped.is_empty() {
        writeln!(
            out,
            "dry-run: skipping {} protected process(es):",
            skipped.len()
        )?;
        write_skipped_lines(&mut out, skipped)?;
    }
    Ok(())
}

/// Write one indented `skipped pid N (name): reason` line per entry.
fn write_skipped_lines(writer: &mut impl Write, skipped: &[KillReportEntry]) -> Result<()> {
    for entry in skipped {
        let line = report::format_process_line(entry);
        writeln!(writer, "  {}", sanitize_for_terminal(&line))?;
    }
    Ok(())
}

const fn dry_run_kind(force: bool) -> &'static str {
    #[cfg(windows)]
    {
        let _ = force;
        "terminate"
    }

    #[cfg(not(windows))]
    {
        if force {
            "SIGKILL/terminate"
        } else {
            "graceful"
        }
    }
}

fn dry_run_report(targets: &[ResolvedTarget], force: bool) -> Vec<KillReportEntry> {
    targets
        .iter()
        .map(|target| match target {
            ResolvedTarget::Process(t) => {
                KillReportEntry::from_dry_run(t.pid, t.process.clone(), force)
            }
            ResolvedTarget::Container(ct) => KillReportEntry::from_container_dry_run(ct, force),
        })
        .collect()
}

fn confirm(
    targets: &[ResolvedTarget],
    skipped: &[KillReportEntry],
    opts: &KillOptions,
) -> Result<bool> {
    let mut err = std::io::stderr().lock();
    let (n_proc, n_ctr) = count_target_kinds(targets);
    let verb = confirmation_verb(opts.force);

    if n_proc > 0 {
        writeln!(err, "about to {verb} {n_proc} process(es):")?;
    }
    if n_ctr > 0 {
        let ctr_verb = if opts.force { "force-stop" } else { "stop" };
        writeln!(err, "about to {ctr_verb} {n_ctr} container(s):")?;
    }

    for t in targets {
        write_target_line(&mut err, t)?;
    }
    if !skipped.is_empty() {
        writeln!(err, "skipping {} protected process(es):", skipped.len())?;
        write_skipped_lines(&mut err, skipped)?;
    }
    write!(err, "proceed? [y/N] ")?;
    err.flush()?;
    drop(err);

    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

/// Count how many process and container targets are in the list.
fn count_target_kinds(targets: &[ResolvedTarget]) -> (usize, usize) {
    let mut n_proc = 0;
    let mut n_ctr = 0;
    for t in targets {
        match t {
            ResolvedTarget::Process(_) => n_proc += 1,
            ResolvedTarget::Container(_) => n_ctr += 1,
        }
    }
    (n_proc, n_ctr)
}

/// Write a single indented target line to the given writer.
fn write_target_line(writer: &mut impl Write, target: &ResolvedTarget) -> std::io::Result<()> {
    match target {
        ResolvedTarget::Process(p) => {
            writeln!(
                writer,
                "  pid {} ({})",
                p.pid,
                sanitize_for_terminal(&p.process)
            )
        }
        ResolvedTarget::Container(ct) => {
            writeln!(
                writer,
                "  container '{}' [proxy pid {} ({})]",
                sanitize_for_terminal(&ct.container_name),
                ct.proxy_pid,
                sanitize_for_terminal(&ct.proxy_process)
            )
        }
    }
}

const fn confirmation_verb(force: bool) -> &'static str {
    #[cfg(windows)]
    {
        let _ = force;
        "terminate"
    }

    #[cfg(not(windows))]
    {
        if force { "forcefully kill" } else { "kill" }
    }
}

#[cfg(test)]
mod tests {
    use self::resolve::{ContainerTarget, Target};
    use super::report::KillStatus;
    use super::*;

    #[test]
    fn run_returns_three_for_missing_pid() {
        let exit_code = run(&KillOptions {
            target: KillTarget::Pid(u32::MAX),
            force: false,
            yes: true,
            dry_run: true,
            json: true,
        })
        .expect("missing pid should not produce a runtime error");

        assert_eq!(
            exit_code, 3,
            "nonexistent pid selectors should report nothing to kill"
        );
    }

    #[test]
    fn dry_run_report_uses_json_status_tokens() {
        let targets = vec![ResolvedTarget::Process(Target {
            pid: 1234,
            process: "node".to_string(),
            identity: None,
        })];

        let report = dry_run_report(&targets, false);

        assert_eq!(report.len(), 1, "dry-run reports should keep every target");
        assert_eq!(report[0].status, KillStatus::WouldKill);
    }

    #[test]
    fn dry_run_report_marks_forceful_targets() {
        let targets = vec![ResolvedTarget::Process(Target {
            pid: 1234,
            process: "node".to_string(),
            identity: None,
        })];

        let report = dry_run_report(&targets, true);

        assert_eq!(report[0].status, KillStatus::WouldForceKill);
    }

    #[test]
    fn dry_run_report_container_targets() {
        let targets = vec![ResolvedTarget::Container(ContainerTarget {
            container_id: "abc123def456".to_string(),
            container_name: "postgres".to_string(),
            port: 5432,
            proxy_pid: 1234,
            proxy_process: "docker-proxy".to_string(),
        })];

        let report = dry_run_report(&targets, false);
        assert_eq!(report[0].status, KillStatus::WouldStopContainer);
        assert_eq!(
            report[0].container_name.as_deref(),
            Some("postgres"),
            "container name should be preserved in dry-run report"
        );

        let report_force = dry_run_report(&targets, true);
        assert_eq!(report_force[0].status, KillStatus::WouldForceStopContainer);
    }

    #[test]
    fn dry_run_wording_matches_platform_semantics() {
        #[cfg(windows)]
        {
            assert_eq!(dry_run_kind(false), "terminate");
            assert_eq!(dry_run_kind(true), "terminate");
        }

        #[cfg(not(windows))]
        {
            assert_eq!(dry_run_kind(false), "graceful");
            assert_eq!(dry_run_kind(true), "SIGKILL/terminate");
        }
    }

    #[test]
    fn confirmation_wording_matches_platform_semantics() {
        #[cfg(windows)]
        {
            assert_eq!(confirmation_verb(false), "terminate");
            assert_eq!(confirmation_verb(true), "terminate");
        }

        #[cfg(not(windows))]
        {
            assert_eq!(confirmation_verb(false), "kill");
            assert_eq!(confirmation_verb(true), "forcefully kill");
        }
    }

    #[test]
    fn confirm_mode_refuses_non_interactive_stdin() {
        assert_eq!(
            confirm_mode(false, false, false),
            ConfirmMode::RefuseNonInteractive,
            "piped or scripted runs without --yes must not kill unconfirmed"
        );
        assert_eq!(confirm_mode(false, false, true), ConfirmMode::Prompt);
        assert_eq!(
            confirm_mode(true, false, false),
            ConfirmMode::Skip,
            "--yes allows non-interactive kills"
        );
        assert_eq!(
            confirm_mode(false, true, false),
            ConfirmMode::Skip,
            "--dry-run never kills, so it needs no confirmation"
        );
    }

    #[test]
    fn confirmation_exit_codes() {
        let declined = confirmation_exit(ConfirmMode::Prompt, || Ok(false))
            .expect("declining should not be a runtime error");
        assert_eq!(declined, Some(1), "declining the prompt should exit 1");

        let accepted = confirmation_exit(ConfirmMode::Prompt, || Ok(true))
            .expect("accepting should not be a runtime error");
        assert_eq!(accepted, None, "accepting the prompt should proceed");

        let refused = confirmation_exit(ConfirmMode::RefuseNonInteractive, || {
            panic!("a non-interactive run must not prompt")
        })
        .expect("refusal should not be a runtime error");
        assert_eq!(refused, Some(2), "non-interactive refusal is a usage error");

        let skipped = confirmation_exit(ConfirmMode::Skip, || panic!("--yes must not prompt"))
            .expect("skipping should not be a runtime error");
        assert_eq!(skipped, None, "--yes should proceed without prompting");
    }

    #[test]
    fn closed_pipe_keeps_the_kill_exit_code() {
        let closed = || {
            Err(anyhow::Error::new(std::io::Error::from(
                std::io::ErrorKind::BrokenPipe,
            )))
        };

        assert_eq!(
            keep_code_on_closed_pipe(closed(), 1).expect("a closed pipe is not a runtime error"),
            1,
            "a failed kill must still exit 1 when the reader went away"
        );
        assert_eq!(keep_code_on_closed_pipe(Ok(()), 0).expect("printed"), 0);
        assert!(
            keep_code_on_closed_pipe(
                Err(anyhow::Error::new(std::io::Error::from(
                    std::io::ErrorKind::PermissionDenied
                ))),
                0
            )
            .is_err(),
            "other write errors still propagate"
        );
    }

    #[test]
    fn protected_reason_covers_reserved_pids() {
        assert!(
            protected_reason(0, 999, &[], None).is_some(),
            "pid 0 is protected"
        );
        assert!(
            protected_reason(999, 999, &[], None).is_some(),
            "self is protected"
        );
        assert!(protected_reason(5000, 999, &["node"], None).is_none());
        #[cfg(unix)]
        assert!(
            protected_reason(1, 999, &[], None).is_some(),
            "init is protected"
        );
        #[cfg(windows)]
        assert!(
            protected_reason(4, 999, &[], None).is_some(),
            "System is protected"
        );
    }

    #[cfg(windows)]
    #[test]
    fn critical_windows_process_names_are_denied_case_insensitively() {
        for name in [
            "csrss.exe",
            "CSRSS.EXE",
            "wininit.exe",
            "WinLogon.exe",
            "lsass.exe",
            "services.exe",
            "smss.exe",
            "lsass",
        ] {
            assert!(is_critical_process_name(name), "{name} must be denied");
        }
        for name in [
            "node.exe",
            "svchost.exe",
            "lsass2.exe",
            "my-services.exe",
            "-",
        ] {
            assert!(!is_critical_process_name(name), "{name} must be allowed");
        }
        let reason = protected_reason(5000, 999, &["node.exe", "LSASS.EXE"], None)
            .expect("any matching name should protect the target");
        assert!(reason.contains("critical operating system process"));
    }

    #[cfg(unix)]
    #[test]
    fn critical_unix_process_names_are_denied_case_insensitively() {
        for name in ["systemd", "SystemD", "init", "launchd"] {
            assert!(is_critical_process_name(name), "{name} must be denied");
        }
        for name in ["systemd-resolved", "node", "initdb", "-"] {
            assert!(!is_critical_process_name(name), "{name} must be allowed");
        }
    }

    /// A process target whose identity carries `name` and `origin`.
    fn process_target(
        pid: u32,
        process: &str,
        name: &str,
        origin: ProcessOrigin,
    ) -> ResolvedTarget {
        ResolvedTarget::Process(Target {
            pid,
            process: process.to_string(),
            identity: Some(platform::ProcessIdentity {
                name: name.to_string(),
                start_time: 1,
                origin,
                exe_name: None,
            }),
        })
    }

    /// Origin that proves nothing, so a denylisted name stays protected.
    fn unknown_origin() -> ProcessOrigin {
        ProcessOrigin::default()
    }

    /// Origin that proves the process is an ordinary user program.
    fn user_program_origin() -> ProcessOrigin {
        #[cfg(windows)]
        {
            ProcessOrigin {
                exe: Some(std::path::PathBuf::from(
                    r"C:\Users\dev\go\bin\services.exe",
                )),
            }
        }
        #[cfg(unix)]
        {
            ProcessOrigin {
                root_owned: Some(false),
                parent_pid: Some(4321),
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            ProcessOrigin::default()
        }
    }

    fn critical_name() -> &'static str {
        CRITICAL_PROCESS_NAMES.first().copied().unwrap_or("init")
    }

    #[test]
    fn partition_protected_skips_in_port_mode_and_keeps_the_rest() {
        let container = ResolvedTarget::Container(ContainerTarget {
            container_id: "abc".to_string(),
            container_name: "pg".to_string(),
            port: 5432,
            proxy_pid: 0,
            proxy_process: "docker-proxy".to_string(),
        });
        let targets = vec![
            process_target(5000, "node", critical_name(), unknown_origin()),
            process_target(5001, "node", "node", unknown_origin()),
            container,
        ];

        let (kept, skipped) = partition_protected(targets, false, 999)
            .expect("port mode must skip protected targets, not fail");

        assert_eq!(
            kept.len(),
            2,
            "the ordinary process and the container (proxy pid 0 is never signaled) stay"
        );
        assert_eq!(skipped.len(), 1, "the critical identity name is skipped");
        assert_eq!(skipped[0].pid, 5000);
        assert_eq!(skipped[0].status, KillStatus::Protected);
        assert!(
            skipped[0]
                .hint
                .as_deref()
                .is_some_and(|h| h.contains("critical operating system process")),
            "the skip reason should be reported"
        );
    }

    #[test]
    fn partition_protected_refuses_in_pid_mode_with_sanitized_name() {
        let targets = vec![process_target(
            5000,
            "evil\x1b[2Jname",
            critical_name(),
            unknown_origin(),
        )];
        let error = partition_protected(targets, true, 999)
            .expect_err("pid mode must refuse a protected target outright");
        let message = format!("{error:#}");
        assert!(message.contains("refusing to kill pid 5000"));
        assert!(
            !message.contains('\x1b'),
            "process names in errors must be sanitized: {message:?}"
        );
    }

    #[test]
    fn partition_protected_skips_self_in_port_mode() {
        let targets = vec![process_target(
            999,
            "portlens",
            "portlens",
            unknown_origin(),
        )];
        let (kept, skipped) =
            partition_protected(targets, false, 999).expect("port mode should not fail");
        assert!(kept.is_empty(), "portlens itself is never a target");
        assert_eq!(skipped.len(), 1);
    }

    #[test]
    fn critical_name_with_user_program_origin_is_killable() {
        let name = critical_name();
        assert_eq!(
            critical_process_name(&[name], Some(&user_program_origin())),
            None,
            "a same-named program that is provably not the OS process stays killable"
        );
        assert_eq!(
            critical_process_name(&[name], None),
            Some(name),
            "with no identity at all the name match fails safe"
        );
        assert_eq!(
            critical_process_name(&[name], Some(&unknown_origin())),
            Some(name),
            "an unknown exe path or owner fails safe"
        );
        assert_eq!(
            critical_process_name(&["node"], Some(&unknown_origin())),
            None,
            "names off the denylist are never critical"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_system_origin_requires_system32_path() {
        let system32 = r"C:\Windows\System32";
        for exe in [
            r"C:\Windows\System32\lsass.exe",
            r"c:\windows\system32\LSASS.EXE",
            "C:/Windows/System32/services.exe",
            r"\\?\C:\Windows\System32\wininit.exe",
        ] {
            assert!(is_path_under_dir(exe, system32), "{exe} is under System32");
        }
        for exe in [
            r"C:\Users\dev\go\bin\services.exe",
            r"C:\Windows\System32Evil\lsass.exe",
            r"C:\Windows\lsass.exe",
            r"C:\Windows\System32\..\Temp\lsass.exe",
            r"D:\Windows\System32\lsass.exe",
        ] {
            assert!(
                !is_path_under_dir(exe, system32),
                "{exe} is not under System32"
            );
        }
        assert!(
            is_path_under_dir(r"C:\Windows\System32\lsass.exe", r"C:\Windows\System32\"),
            "a trailing separator on the directory is accepted"
        );

        let system32_dir = system32_dir().expect("GetSystemDirectoryW succeeds");
        let real = ProcessOrigin {
            exe: Some(std::path::PathBuf::from(&system32_dir).join("lsass.exe")),
        };
        assert!(
            is_system_origin(&real),
            "the real lsass is a system process"
        );
        assert!(
            is_system_origin(&ProcessOrigin { exe: None }),
            "an unreadable exe path (not elevated) fails safe"
        );
        assert!(!is_system_origin(&user_program_origin()));
    }

    #[cfg(windows)]
    #[test]
    fn windows_system_origin_fails_closed() {
        use std::path::Path;

        let system32 = Some(r"C:\Windows\System32");
        for exe in [
            "",
            r"\\.\C:\Users\dev\lsass.exe",
            r"\??\C:\Users\dev\lsass.exe",
            r"\Device\HarddiskVolume3\Users\dev\lsass.exe",
            r"\SystemRoot\System32\lsass.exe",
            r"C:\WINDOW~1\SYSTEM~1\lsass.exe",
            r"\\server\share\lsass.exe",
            r"C:\Users\dev\..\..\Windows\System32\lsass.exe",
            "lsass.exe",
        ] {
            assert!(
                is_system_exe(Some(Path::new(exe)), system32),
                "{exe:?} must be treated as a system process"
            );
        }
        assert!(
            is_system_exe(Some(Path::new(r"C:\Users\dev\services.exe")), None),
            "a failed system directory lookup fails safe"
        );
        assert!(!is_system_exe(
            Some(Path::new(r"C:\Users\dev\go\bin\services.exe")),
            system32
        ));
        assert!(!is_system_exe(
            Some(Path::new(r"\\?\D:\tools\lsass.exe")),
            system32
        ));
    }

    #[cfg(windows)]
    #[test]
    fn windows_system_dir_ignores_system_root_env() {
        let dir = system32_dir().expect("GetSystemDirectoryW succeeds");
        assert!(
            dir.to_ascii_lowercase().ends_with(r"\system32"),
            "unexpected system directory {dir}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_system_origin_requires_root_or_init_parent() {
        let origin = |root_owned, parent_pid| ProcessOrigin {
            root_owned,
            parent_pid,
        };
        assert!(
            is_system_origin(&origin(Some(true), Some(4321))),
            "root-owned is system"
        );
        assert!(
            is_system_origin(&origin(Some(false), Some(1))),
            "a child of init (systemd --user) is system"
        );
        assert!(
            is_system_origin(&origin(Some(false), None)),
            "parent pid 0 (reported as None) is system"
        );
        assert!(
            is_system_origin(&origin(None, Some(4321))),
            "an unknown owner fails safe"
        );
        assert!(
            !is_system_origin(&origin(Some(false), Some(4321))),
            "a user's binary started from a shell is not system"
        );
    }

    #[test]
    fn count_target_kinds_classifies_correctly() {
        let targets = vec![
            ResolvedTarget::Process(Target {
                pid: 1,
                process: "node".to_string(),
                identity: None,
            }),
            ResolvedTarget::Container(ContainerTarget {
                container_id: "abc".to_string(),
                container_name: "pg".to_string(),
                port: 5432,
                proxy_pid: 2,
                proxy_process: "docker-proxy".to_string(),
            }),
            ResolvedTarget::Process(Target {
                pid: 3,
                process: "python".to_string(),
                identity: None,
            }),
        ];
        let (n_proc, n_ctr) = count_target_kinds(&targets);
        assert_eq!(n_proc, 2, "should count 2 process targets");
        assert_eq!(n_ctr, 1, "should count 1 container target");
    }
}
