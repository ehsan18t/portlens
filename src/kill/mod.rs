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

use self::platform::kill_pid;
use self::report::KillReportEntry;
use self::resolve::{Target, target_for_pid, targets_for_port};

/// Target selector for a kill invocation.
#[derive(Debug, Clone, Copy)]
pub enum KillTarget {
    /// Kill every process listening on this port.
    Port(u16),
    /// Kill a single PID directly.
    Pid(u32),
}

/// Options controlling a kill invocation.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy)]
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
pub fn run(opts: KillOptions) -> Result<u8> {
    let targets = resolve_targets(&opts)?;

    if targets.is_empty() {
        let msg = match opts.target {
            KillTarget::Port(p) => format!("no process listening on port {p}"),
            KillTarget::Pid(pid) => format!("no process with pid {pid}"),
        };
        eprintln!("{msg}");
        return Ok(3);
    }

    reject_protected_pids(&targets)?;

    if !opts.yes && !opts.dry_run && std::io::stdin().is_terminal() && !confirm(&targets, &opts)? {
        eprintln!("aborted");
        return Ok(0);
    }

    if opts.dry_run {
        announce_dry_run(&targets, &opts)?;
        return Ok(0);
    }

    let mut report = Vec::with_capacity(targets.len());
    let mut any_failure = false;
    for t in targets {
        let outcome = kill_pid(t.pid, opts.force);
        if !outcome.is_success() {
            any_failure = true;
        }
        report.push(KillReportEntry::from_outcome(t.pid, t.process, outcome));
    }

    if opts.json {
        report::print_json(&report)?;
    } else {
        report::print_human(&report)?;
    }

    Ok(u8::from(any_failure))
}

fn resolve_targets(opts: &KillOptions) -> Result<Vec<Target>> {
    // Note: `--port 0` is rejected at CLI-parse time so it produces a usage
    // exit code (2); callers here can rely on `port >= 1`.
    match opts.target {
        KillTarget::Port(port) => targets_for_port(port),
        KillTarget::Pid(pid) => Ok(vec![target_for_pid(pid)]),
    }
}

fn reject_protected_pids(targets: &[Target]) -> Result<()> {
    let self_pid = std::process::id();
    for t in targets {
        if t.pid == 0 {
            bail!("refusing to kill pid 0 (kernel/system idle process)");
        }
        if t.pid == self_pid {
            bail!("refusing to kill self (pid {})", t.pid);
        }
        #[cfg(unix)]
        if t.pid == 1 {
            bail!("refusing to kill pid 1 (init)");
        }
        #[cfg(windows)]
        if t.pid == 4 {
            bail!("refusing to kill pid 4 (Windows System process)");
        }
    }
    Ok(())
}

fn announce_dry_run(targets: &[Target], opts: &KillOptions) -> Result<()> {
    if opts.json {
        let report = dry_run_report(targets, opts.force);
        return report::print_json(&report);
    }

    let mut out = std::io::stdout().lock();
    let kind = if opts.force {
        "SIGKILL/terminate"
    } else {
        "graceful"
    };
    writeln!(out, "dry-run: would {kind} {} process(es):", targets.len())?;
    for t in targets {
        writeln!(out, "  pid {} ({})", t.pid, t.process)?;
    }
    Ok(())
}

fn dry_run_report(targets: &[Target], force: bool) -> Vec<KillReportEntry> {
    targets
        .iter()
        .map(|target| KillReportEntry::from_dry_run(target.pid, target.process.clone(), force))
        .collect()
}

fn confirm(targets: &[Target], opts: &KillOptions) -> Result<bool> {
    let mut err = std::io::stderr().lock();
    let verb = if opts.force {
        "forcefully kill"
    } else {
        "kill"
    };
    writeln!(err, "about to {verb} {} process(es):", targets.len())?;
    for t in targets {
        writeln!(err, "  pid {} ({})", t.pid, t.process)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_run_report_uses_json_status_tokens() {
        let targets = vec![Target {
            pid: 1234,
            process: "node".to_string(),
        }];

        let report = dry_run_report(&targets, false);

        assert_eq!(report.len(), 1, "dry-run reports should keep every target");
        assert_eq!(report[0].status, "would-kill");
    }

    #[test]
    fn dry_run_report_marks_forceful_targets() {
        let targets = vec![Target {
            pid: 1234,
            process: "node".to_string(),
        }];

        let report = dry_run_report(&targets, true);

        assert_eq!(report[0].status, "would-force-kill");
    }
}
