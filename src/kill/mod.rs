//! # Kill — terminate processes by port or PID
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

use self::platform::{kill_pid, pid_exists};
use self::report::KillReportEntry;
use self::resolve::{ResolvedTarget, Target, target_for_pid, targets_for_port};
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

/// Run a kill operation end-to-end.
///
/// Returns `Ok(exit_code)` where:
/// - `0` — every resolved target succeeded (or was already gone).
/// - `1` — at least one target failed (permission denied, other errors).
/// - `3` — nothing to kill (no PID matched the selector).
///
/// Errors propagate only for unexpected conditions such as socket enumeration
/// failure or stdout/stderr write failure.
pub fn run(opts: &KillOptions) -> Result<u8> {
    debug!(
        "kill run: target={:?} force={} yes={} dry_run={} json={}",
        opts.target, opts.force, opts.yes, opts.dry_run, opts.json
    );
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
        return Ok(3);
    }

    reject_protected_pids(&targets)?;

    debug!("resolved {} kill target(s)", targets.len());

    if !opts.yes && !opts.dry_run && std::io::stdin().is_terminal() && !confirm(&targets, opts)? {
        eprintln!("aborted");
        return Ok(0);
    }

    if opts.dry_run {
        announce_dry_run(&targets, opts)?;
        return Ok(0);
    }

    let mut report = Vec::with_capacity(targets.len());
    let mut any_failure = false;
    for t in targets {
        let entry = execute_target(t, opts.force);
        if entry.is_failure() {
            any_failure = true;
        }
        report.push(entry);
    }

    if opts.json {
        report::print_json(&report)?;
    } else {
        report::print_human(&report)?;
    }

    Ok(u8::from(any_failure))
}

/// Execute a single resolved target (process kill or container stop).
fn execute_target(target: ResolvedTarget, force: bool) -> KillReportEntry {
    match target {
        ResolvedTarget::Process(t) => {
            debug!(
                "killing process: pid={} process={} force={force}",
                t.pid, t.process
            );
            let outcome = kill_pid(t.pid, force);
            KillReportEntry::from_outcome(t.pid, t.process, outcome)
        }
        ResolvedTarget::Container(ct) => {
            debug!(
                "stopping container: id={} name={} force={force}",
                ct.container_id, ct.container_name
            );
            let outcome =
                crate::docker::stop_container(&ct.container_id, force, crate::project::home_dir());
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
    if preserve_pid_target(pid) || pid_exists(pid) {
        let target = target_for_pid(pid).unwrap_or_else(|| Target {
            pid,
            process: "-".to_owned(),
        });
        return Some(ResolvedTarget::Process(target));
    }

    None
}

fn preserve_pid_target(pid: u32) -> bool {
    if pid == 0 || pid == std::process::id() {
        return true;
    }

    #[cfg(unix)]
    if pid == 1 {
        return true;
    }

    #[cfg(windows)]
    if pid == 4 {
        return true;
    }

    false
}

fn reject_protected_pids(targets: &[ResolvedTarget]) -> Result<()> {
    let self_pid = std::process::id();
    for t in targets {
        let pid = match t {
            ResolvedTarget::Process(p) => p.pid,
            // Container targets are stopped via the daemon API. The proxy
            // PID is informational; we never signal it directly.
            ResolvedTarget::Container(_) => continue,
        };
        if pid == 0 {
            bail!("refusing to kill pid 0 (kernel/system idle process)");
        }
        if pid == self_pid {
            bail!("refusing to kill self (pid {pid})");
        }
        #[cfg(unix)]
        if pid == 1 {
            bail!("refusing to kill pid 1 (init)");
        }
        #[cfg(windows)]
        if pid == 4 {
            bail!("refusing to kill pid 4 (Windows System process)");
        }
    }
    Ok(())
}

fn announce_dry_run(targets: &[ResolvedTarget], opts: &KillOptions) -> Result<()> {
    if opts.json {
        let report = dry_run_report(targets, opts.force);
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

fn confirm(targets: &[ResolvedTarget], opts: &KillOptions) -> Result<bool> {
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
            writeln!(writer, "  pid {} ({})", p.pid, p.process)
        }
        ResolvedTarget::Container(ct) => {
            writeln!(
                writer,
                "  container '{}' [proxy pid {} ({})]",
                ct.container_name, ct.proxy_pid, ct.proxy_process
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
    fn count_target_kinds_classifies_correctly() {
        let targets = vec![
            ResolvedTarget::Process(Target {
                pid: 1,
                process: "node".to_string(),
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
            }),
        ];
        let (n_proc, n_ctr) = count_target_kinds(&targets);
        assert_eq!(n_proc, 2, "should count 2 process targets");
        assert_eq!(n_ctr, 1, "should count 1 container target");
    }
}
