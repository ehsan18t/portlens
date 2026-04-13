//! Human and JSON reporting for kill results.

use std::io::Write;

use anyhow::{Context, Result};
use serde::Serialize;

use super::platform::KillOutcome;

/// One row in the kill report.
#[derive(Debug, Clone, Serialize)]
pub struct KillReportEntry {
    /// Target PID.
    pub pid: u32,
    /// Process name at resolve time.
    pub process: String,
    /// Machine-friendly status token.
    pub status: &'static str,
    /// Optional human hint (e.g., permission advice).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl KillReportEntry {
    /// Build a report row from a target and the outcome of its kill attempt.
    #[must_use]
    pub fn from_outcome(pid: u32, process: String, outcome: KillOutcome) -> Self {
        let (status, hint) = match outcome {
            KillOutcome::Signaled => ("killed", None),
            KillOutcome::AlreadyGone => ("already-exited", None),
            KillOutcome::PermissionDenied => {
                ("permission-denied", Some(elevation_hint().to_owned()))
            }
            #[cfg(unix)]
            KillOutcome::Failed => ("failed", None),
        };
        Self {
            pid,
            process,
            status,
            hint,
        }
    }

    /// Build a report row describing a dry-run target.
    #[must_use]
    pub const fn from_dry_run(pid: u32, process: String, force: bool) -> Self {
        let status = if force {
            "would-force-kill"
        } else {
            "would-kill"
        };

        Self {
            pid,
            process,
            status,
            hint: None,
        }
    }
}

#[cfg(windows)]
const fn elevation_hint() -> &'static str {
    "retry in an elevated terminal (Run as Administrator)"
}

#[cfg(not(windows))]
const fn elevation_hint() -> &'static str {
    "retry with sudo or as the process owner"
}

/// Render a human-readable report to stdout.
pub fn print_human(entries: &[KillReportEntry]) -> Result<()> {
    let mut out = std::io::stdout().lock();
    for e in entries {
        let line = match e.status {
            "killed" => format!("killed pid {} ({})", e.pid, e.process),
            "already-exited" => format!("pid {} already exited ({})", e.pid, e.process),
            "permission-denied" => format!(
                "permission denied killing pid {} ({}); {}",
                e.pid,
                e.process,
                e.hint.as_deref().unwrap_or("")
            ),
            other => format!("pid {} ({}): {}", e.pid, e.process, other),
        };
        writeln!(out, "{line}").context("failed to write kill report")?;
    }
    Ok(())
}

/// Render the report as a JSON array.
pub fn print_json(entries: &[KillReportEntry]) -> Result<()> {
    let mut out = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut out, entries).context("failed to serialize kill report")?;
    writeln!(out).context("failed to terminate JSON output")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_run_report_uses_machine_readable_status() {
        let entry = KillReportEntry::from_dry_run(1234, "node".to_string(), false);

        assert_eq!(entry.pid, 1234);
        assert_eq!(entry.process, "node");
        assert_eq!(entry.status, "would-kill");
        assert!(
            entry.hint.is_none(),
            "dry-run entries should not add a hint"
        );
    }

    #[test]
    fn forceful_dry_run_report_marks_forceful_status() {
        let entry = KillReportEntry::from_dry_run(1234, "node".to_string(), true);

        assert_eq!(entry.status, "would-force-kill");
    }
}
