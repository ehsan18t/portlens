//! Human and JSON reporting for kill results.

use std::io::Write;

use anyhow::{Context, Result};
use serde::Serialize;

use super::platform::KillOutcome;
use super::resolve::ContainerTarget;
use crate::docker::StopOutcome;

/// One row in the kill report.
#[derive(Debug, Clone, Serialize)]
pub struct KillReportEntry {
    /// Target PID (proxy PID for containers, 0 when irrelevant).
    pub pid: u32,
    /// Process name at resolve time.
    pub process: String,
    /// Machine-friendly status token.
    pub status: &'static str,
    /// Optional human hint (e.g., permission advice).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// Container ID (short), present only for container targets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container_id: Option<String>,
    /// Container name, present only for container targets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container_name: Option<String>,
    /// Port being freed, present only for container targets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
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
            container_id: None,
            container_name: None,
            port: None,
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
            container_id: None,
            container_name: None,
            port: None,
        }
    }

    /// Build a report row from a container stop/kill attempt.
    #[must_use]
    pub fn from_container_outcome(ct: ContainerTarget, outcome: StopOutcome) -> Self {
        let (status, hint) = match outcome {
            StopOutcome::Stopped => ("container-stopped", None),
            StopOutcome::AlreadyStopped => ("container-already-stopped", None),
            StopOutcome::NotFound => (
                "container-not-found",
                Some("the container may have been removed".to_owned()),
            ),
            StopOutcome::Failed => (
                "container-stop-failed",
                Some("could not reach the container runtime daemon".to_owned()),
            ),
        };
        Self {
            pid: ct.proxy_pid,
            process: ct.proxy_process,
            status,
            hint,
            container_id: Some(crate::docker::short_container_id(&ct.container_id)),
            container_name: Some(ct.container_name),
            port: Some(ct.port),
        }
    }

    /// Build a dry-run report row for a container target.
    #[must_use]
    pub fn from_container_dry_run(ct: &ContainerTarget, force: bool) -> Self {
        let status = if force {
            "would-force-stop-container"
        } else {
            "would-stop-container"
        };
        Self {
            pid: ct.proxy_pid,
            process: ct.proxy_process.clone(),
            status,
            hint: None,
            container_id: Some(crate::docker::short_container_id(&ct.container_id)),
            container_name: Some(ct.container_name.clone()),
            port: Some(ct.port),
        }
    }

    /// Returns `true` when this entry represents a failure.
    #[must_use]
    pub fn is_failure(&self) -> bool {
        matches!(
            self.status,
            "permission-denied" | "failed" | "container-stop-failed" | "container-not-found"
        )
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
        let line = e.container_name.as_ref().map_or_else(
            || format_process_line(e),
            |name| format_container_line(e, name),
        );
        writeln!(out, "{line}").context("failed to write kill report")?;
    }
    Ok(())
}

fn format_process_line(e: &KillReportEntry) -> String {
    match e.status {
        "killed" => format!("killed pid {} ({})", e.pid, e.process),
        "already-exited" => format!("pid {} already exited ({})", e.pid, e.process),
        "permission-denied" => format!(
            "permission denied killing pid {} ({}); {}",
            e.pid,
            e.process,
            e.hint.as_deref().unwrap_or("")
        ),
        other => format!("pid {} ({}): {}", e.pid, e.process, other),
    }
}

fn format_container_line(e: &KillReportEntry, name: &str) -> String {
    let id = e.container_id.as_deref().unwrap_or("?");
    match e.status {
        "container-stopped" => format!("stopped container '{name}' ({id})"),
        "container-already-stopped" => {
            format!("container '{name}' ({id}) was already stopped")
        }
        "container-not-found" => format!("container '{name}' ({id}) not found"),
        "container-stop-failed" => format!(
            "failed to stop container '{name}' ({id}); {}",
            e.hint.as_deref().unwrap_or("")
        ),
        other => format!("container '{name}' ({id}): {other}"),
    }
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

    #[test]
    fn container_outcome_stopped() {
        let ct = ContainerTarget {
            container_id: "abc123def456789000".to_string(),
            container_name: "postgres".to_string(),
            port: 5432,
            proxy_pid: 100,
            proxy_process: "docker-proxy".to_string(),
        };
        let entry = KillReportEntry::from_container_outcome(ct, StopOutcome::Stopped);
        assert_eq!(entry.status, "container-stopped");
        assert_eq!(entry.container_name.as_deref(), Some("postgres"));
        assert_eq!(
            entry.container_id.as_deref(),
            Some("abc123def456"),
            "container ID should be truncated to 12 characters"
        );
        assert!(!entry.is_failure());
    }

    #[test]
    fn container_outcome_already_stopped() {
        let ct = ContainerTarget {
            container_id: "abc123".to_string(),
            container_name: "redis".to_string(),
            port: 6379,
            proxy_pid: 200,
            proxy_process: "docker-proxy".to_string(),
        };
        let entry = KillReportEntry::from_container_outcome(ct, StopOutcome::AlreadyStopped);
        assert_eq!(entry.status, "container-already-stopped");
        assert!(!entry.is_failure());
    }

    #[test]
    fn container_outcome_failed_is_failure() {
        let ct = ContainerTarget {
            container_id: "abc123".to_string(),
            container_name: "web".to_string(),
            port: 3000,
            proxy_pid: 300,
            proxy_process: "docker-proxy".to_string(),
        };
        let entry = KillReportEntry::from_container_outcome(ct, StopOutcome::Failed);
        assert_eq!(entry.status, "container-stop-failed");
        assert!(
            entry.is_failure(),
            "failed container stop should be a failure"
        );
    }

    #[test]
    fn container_dry_run_uses_container_status() {
        let ct = ContainerTarget {
            container_id: "abc123".to_string(),
            container_name: "nginx".to_string(),
            port: 80,
            proxy_pid: 400,
            proxy_process: "docker-proxy".to_string(),
        };
        let entry = KillReportEntry::from_container_dry_run(&ct, false);
        assert_eq!(entry.status, "would-stop-container");
        assert_eq!(entry.container_name.as_deref(), Some("nginx"));

        let entry_force = KillReportEntry::from_container_dry_run(&ct, true);
        assert_eq!(entry_force.status, "would-force-stop-container");
    }
}
