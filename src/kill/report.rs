//! Human and JSON reporting for kill results.

use std::io::Write;

use anyhow::{Context, Result};
use serde::Serialize;

use super::platform::KillOutcome;
use super::resolve::ContainerTarget;
use crate::display::sanitize_for_terminal;
use crate::docker::StopOutcome;

/// Machine-friendly status token for a kill report entry.
///
/// Each variant serializes to a stable kebab-case string for JSON output
/// compatibility. Using an enum instead of raw `&'static str` prevents
/// silent typo bugs and makes failure classification exhaustive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum KillStatus {
    /// Process was successfully signaled.
    #[serde(rename = "killed")]
    Killed,
    /// Process had already exited before the signal was sent.
    #[serde(rename = "already-exited")]
    AlreadyExited,
    /// OS refused the signal (insufficient privileges).
    #[serde(rename = "permission-denied")]
    PermissionDenied,
    /// The PID now belongs to a different process than the one resolved
    /// (PID reuse or unverifiable identity); nothing was signaled.
    #[serde(rename = "process-changed")]
    ProcessChanged,
    /// Signal delivery failed for an OS-specific reason.
    #[cfg(unix)]
    #[serde(rename = "failed")]
    Failed,
    /// A protected process matched a `--port` selector and was skipped
    /// without being signaled. Not a failure on its own.
    #[serde(rename = "protected")]
    Protected,
    /// A container runtime or VM/WSL port forwarder matched a `--port`
    /// selector but no single container could be matched to the port, so it
    /// was skipped without being signaled. Not a failure on its own.
    #[serde(rename = "forwarder")]
    Forwarder,
    /// Dry-run: process would be killed (graceful).
    #[serde(rename = "would-kill")]
    WouldKill,
    /// Dry-run: process would be force-killed.
    #[serde(rename = "would-force-kill")]
    WouldForceKill,
    /// Container was successfully stopped via the daemon API.
    #[serde(rename = "container-stopped")]
    ContainerStopped,
    /// Container was already stopped.
    #[serde(rename = "container-already-stopped")]
    ContainerAlreadyStopped,
    /// Container was not found by the daemon.
    #[serde(rename = "container-not-found")]
    ContainerNotFound,
    /// No container runtime daemon could be reached, so the stop request was
    /// never delivered and the container was not touched.
    #[serde(rename = "container-unreachable")]
    ContainerUnreachable,
    /// A daemon received the stop request but gave no usable reply; the
    /// container may still be stopping.
    #[serde(rename = "container-no-response")]
    ContainerNoResponse,
    /// The daemon answered the stop request with an unexpected HTTP status.
    #[serde(rename = "container-rejected")]
    ContainerRejected,
    /// The stop failed for a reason this version of portlens does not
    /// classify (a stop outcome added by a newer nanodock release).
    #[serde(rename = "container-stop-failed")]
    ContainerStopFailed,
    /// Dry-run: container would be stopped (graceful).
    #[serde(rename = "would-stop-container")]
    WouldStopContainer,
    /// Dry-run: container would be force-stopped.
    #[serde(rename = "would-force-stop-container")]
    WouldForceStopContainer,
}

/// One row in the kill report.
#[derive(Debug, Clone, Serialize)]
pub struct KillReportEntry {
    /// Target PID (proxy PID for containers, 0 when irrelevant).
    pub pid: u32,
    /// Process name at resolve time.
    pub process: String,
    /// Machine-friendly status token.
    pub status: KillStatus,
    /// Optional human hint (e.g., permission advice).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// Container ID (short), present only for container targets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container_id: Option<String>,
    /// Container name, present only for container targets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container_name: Option<String>,
    /// Port being freed, present only for container targets and skipped
    /// port forwarders.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

impl KillReportEntry {
    /// Build a report row from a target and the outcome of its kill attempt.
    #[must_use]
    pub fn from_outcome(pid: u32, process: String, outcome: KillOutcome) -> Self {
        let (status, hint) = match outcome {
            KillOutcome::Signaled => (KillStatus::Killed, None),
            KillOutcome::AlreadyGone => (KillStatus::AlreadyExited, None),
            KillOutcome::PermissionDenied => (
                KillStatus::PermissionDenied,
                Some(elevation_hint().to_owned()),
            ),
            KillOutcome::ProcessChanged => (
                KillStatus::ProcessChanged,
                Some(PROCESS_CHANGED_HINT.to_owned()),
            ),
            #[cfg(unix)]
            KillOutcome::Failed => (KillStatus::Failed, None),
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
            KillStatus::WouldForceKill
        } else {
            KillStatus::WouldKill
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

    /// Build a report row for a protected process that was skipped; the
    /// reason is carried in `hint`.
    #[must_use]
    pub const fn from_protected(pid: u32, process: String, reason: String) -> Self {
        Self {
            pid,
            process,
            status: KillStatus::Protected,
            hint: Some(reason),
            container_id: None,
            container_name: None,
            port: None,
        }
    }

    /// Build a report row for a port forwarder on `port` that was skipped
    /// because no single container could be matched to it; the reason is
    /// carried in `hint`.
    #[must_use]
    pub const fn from_forwarder(pid: u32, process: String, port: u16, reason: String) -> Self {
        Self {
            pid,
            process,
            status: KillStatus::Forwarder,
            hint: Some(reason),
            container_id: None,
            container_name: None,
            port: Some(port),
        }
    }

    /// Build a report row from a container stop/kill attempt.
    #[must_use]
    pub fn from_container_outcome(ct: ContainerTarget, outcome: &StopOutcome) -> Self {
        let (status, hint) = match outcome {
            StopOutcome::Stopped => (KillStatus::ContainerStopped, None),
            StopOutcome::AlreadyStopped => (KillStatus::ContainerAlreadyStopped, None),
            StopOutcome::NotFound => (
                KillStatus::ContainerNotFound,
                Some("the container may have been removed".to_owned()),
            ),
            StopOutcome::Unreachable => (
                KillStatus::ContainerUnreachable,
                Some(CONTAINER_UNREACHABLE_HINT.to_owned()),
            ),
            StopOutcome::NoResponse => (
                KillStatus::ContainerNoResponse,
                Some(CONTAINER_NO_RESPONSE_HINT.to_owned()),
            ),
            StopOutcome::Rejected { status } => (
                KillStatus::ContainerRejected,
                Some(format!("the daemon refused the stop (HTTP {status})")),
            ),
            // `StopOutcome` is non-exhaustive: an outcome added later gets a
            // generic status and a hint that claims no specific cause.
            _ => (
                KillStatus::ContainerStopFailed,
                Some(CONTAINER_STOP_FAILED_HINT.to_owned()),
            ),
        };
        Self {
            pid: ct.proxy_pid,
            process: ct.proxy_process,
            status,
            hint,
            container_id: Some(crate::docker::short_container_id(&ct.container_id).to_owned()),
            container_name: Some(ct.container_name),
            port: Some(ct.port),
        }
    }

    /// Build a dry-run report row for a container target.
    #[must_use]
    pub fn from_container_dry_run(ct: &ContainerTarget, force: bool) -> Self {
        let status = if force {
            KillStatus::WouldForceStopContainer
        } else {
            KillStatus::WouldStopContainer
        };
        Self {
            pid: ct.proxy_pid,
            process: ct.proxy_process.clone(),
            status,
            hint: None,
            container_id: Some(crate::docker::short_container_id(&ct.container_id).to_owned()),
            container_name: Some(ct.container_name.clone()),
            port: Some(ct.port),
        }
    }

    /// Returns `true` when this entry represents a failure.
    #[cfg(unix)]
    #[must_use]
    pub const fn is_failure(&self) -> bool {
        matches!(
            self.status,
            KillStatus::PermissionDenied
                | KillStatus::ProcessChanged
                | KillStatus::Failed
                | KillStatus::ContainerUnreachable
                | KillStatus::ContainerNoResponse
                | KillStatus::ContainerRejected
                | KillStatus::ContainerStopFailed
                | KillStatus::ContainerNotFound
        )
    }

    /// Returns `true` when this entry represents a failure.
    #[cfg(not(unix))]
    #[must_use]
    pub const fn is_failure(&self) -> bool {
        matches!(
            self.status,
            KillStatus::PermissionDenied
                | KillStatus::ProcessChanged
                | KillStatus::ContainerUnreachable
                | KillStatus::ContainerNoResponse
                | KillStatus::ContainerRejected
                | KillStatus::ContainerStopFailed
                | KillStatus::ContainerNotFound
        )
    }
}

/// Hint attached to `process-changed` entries.
const PROCESS_CHANGED_HINT: &str = "the pid now belongs to a different process (or its identity could not be verified); nothing was killed, re-run to resolve targets again";

/// Hint attached to `container-unreachable` entries.
const CONTAINER_UNREACHABLE_HINT: &str =
    "could not reach the container runtime daemon; the container was not touched";

/// Hint attached to `container-no-response` entries.
const CONTAINER_NO_RESPONSE_HINT: &str = "the daemon received the stop request but did not confirm it; the container may still be stopping";

/// Hint attached to `container-stop-failed` entries.
const CONTAINER_STOP_FAILED_HINT: &str = "the container runtime did not confirm the stop";

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
        writeln!(out, "{}", sanitize_for_terminal(&line)).context("failed to write kill report")?;
    }
    Ok(())
}

/// Format one process row as a human-readable line (not yet sanitized).
pub fn format_process_line(e: &KillReportEntry) -> String {
    match e.status {
        KillStatus::Killed => format!("killed pid {} ({})", e.pid, e.process),
        KillStatus::AlreadyExited => format!("pid {} already exited ({})", e.pid, e.process),
        KillStatus::PermissionDenied => format!(
            "permission denied killing pid {} ({}); {}",
            e.pid,
            e.process,
            e.hint.as_deref().unwrap_or("")
        ),
        KillStatus::ProcessChanged => format!(
            "not killing pid {} ({}): {}",
            e.pid,
            e.process,
            e.hint.as_deref().unwrap_or("")
        ),
        KillStatus::Protected => format!(
            "skipped pid {} ({}): {}",
            e.pid,
            e.process,
            e.hint.as_deref().unwrap_or("protected process")
        ),
        KillStatus::Forwarder => format!(
            "skipped pid {} ({}) on port {}: {}",
            e.pid,
            e.process,
            e.port
                .map_or_else(|| "?".to_owned(), |port| port.to_string()),
            e.hint
                .as_deref()
                .unwrap_or("port forwarder with no matching container")
        ),
        _ => format!("pid {} ({}): {:?}", e.pid, e.process, e.status),
    }
}

fn format_container_line(e: &KillReportEntry, name: &str) -> String {
    let id = e.container_id.as_deref().unwrap_or("?");
    match e.status {
        KillStatus::ContainerStopped => format!("stopped container '{name}' ({id})"),
        KillStatus::ContainerAlreadyStopped => {
            format!("container '{name}' ({id}) was already stopped")
        }
        KillStatus::ContainerNotFound => format!("container '{name}' ({id}) not found"),
        KillStatus::ContainerUnreachable
        | KillStatus::ContainerRejected
        | KillStatus::ContainerStopFailed => format!(
            "failed to stop container '{name}' ({id}); {}",
            e.hint.as_deref().unwrap_or("")
        ),
        KillStatus::ContainerNoResponse => format!(
            "stop of container '{name}' ({id}) was not confirmed; {}",
            e.hint.as_deref().unwrap_or("")
        ),
        _ => format!("container '{name}' ({id}): {:?}", e.status),
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
        assert_eq!(entry.status, KillStatus::WouldKill);
        assert!(
            entry.hint.is_none(),
            "dry-run entries should not add a hint"
        );
    }

    #[test]
    fn forceful_dry_run_report_marks_forceful_status() {
        let entry = KillReportEntry::from_dry_run(1234, "node".to_string(), true);

        assert_eq!(entry.status, KillStatus::WouldForceKill);
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
        let entry = KillReportEntry::from_container_outcome(ct, &StopOutcome::Stopped);
        assert_eq!(entry.status, KillStatus::ContainerStopped);
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
        let entry = KillReportEntry::from_container_outcome(ct, &StopOutcome::AlreadyStopped);
        assert_eq!(entry.status, KillStatus::ContainerAlreadyStopped);
        assert!(!entry.is_failure());
    }

    fn web_target() -> ContainerTarget {
        ContainerTarget {
            container_id: "abc123".to_string(),
            container_name: "web".to_string(),
            port: 3000,
            proxy_pid: 300,
            proxy_process: "docker-proxy".to_string(),
        }
    }

    #[test]
    fn unsuccessful_container_outcomes_have_distinct_statuses() {
        let cases = [
            (
                StopOutcome::Unreachable,
                KillStatus::ContainerUnreachable,
                "container-unreachable",
                "could not reach the container runtime daemon",
            ),
            (
                StopOutcome::NoResponse,
                KillStatus::ContainerNoResponse,
                "container-no-response",
                "may still be stopping",
            ),
            (
                StopOutcome::Rejected { status: 500 },
                KillStatus::ContainerRejected,
                "container-rejected",
                "the daemon refused the stop (HTTP 500)",
            ),
        ];

        for (outcome, status, token, hint) in cases {
            let entry = KillReportEntry::from_container_outcome(web_target(), &outcome);
            assert_eq!(entry.status, status, "{outcome:?} should map to {status:?}");
            assert!(entry.is_failure(), "{outcome:?} must count as a failure");
            assert_eq!(
                serde_json::to_value(entry.status).expect("status should serialize"),
                serde_json::json!(token),
                "JSON status token should be stable"
            );
            let actual_hint = entry.hint.as_deref().unwrap_or("");
            assert!(
                actual_hint.contains(hint),
                "{outcome:?} hint should say {hint:?}: {actual_hint}"
            );
            assert!(
                format_container_line(&entry, "web").contains(actual_hint),
                "human output should carry the hint"
            );
        }
    }

    #[test]
    fn no_response_is_not_reported_as_a_failed_stop() {
        let entry = KillReportEntry::from_container_outcome(web_target(), &StopOutcome::NoResponse);
        assert_eq!(
            format_container_line(&entry, "web"),
            "stop of container 'web' (abc123) was not confirmed; the daemon received the stop request but did not confirm it; the container may still be stopping"
        );
    }

    #[test]
    fn process_changed_outcome_is_distinct_failure() {
        let entry =
            KillReportEntry::from_outcome(1234, "node".to_string(), KillOutcome::ProcessChanged);
        assert_eq!(entry.status, KillStatus::ProcessChanged);
        assert!(
            entry.is_failure(),
            "an unverified target was not killed, so it must count as a failure"
        );
        assert_eq!(
            serde_json::to_value(entry.status).expect("status should serialize"),
            serde_json::json!("process-changed"),
            "JSON status token should be stable"
        );
        assert!(
            format_process_line(&entry).starts_with("not killing pid 1234 (node)"),
            "human output should say the pid was not killed"
        );
    }

    #[test]
    fn protected_entry_is_skipped_not_failed() {
        let entry = KillReportEntry::from_protected(
            812,
            "lsass.exe".to_string(),
            "critical operating system process 'lsass.exe'".to_string(),
        );
        assert_eq!(entry.status, KillStatus::Protected);
        assert!(
            !entry.is_failure(),
            "skipping a protected process is not a kill failure"
        );
        assert_eq!(
            serde_json::to_value(entry.status).expect("status should serialize"),
            serde_json::json!("protected"),
            "JSON status token should be stable"
        );
        assert_eq!(
            format_process_line(&entry),
            "skipped pid 812 (lsass.exe): critical operating system process 'lsass.exe'"
        );
    }

    #[test]
    fn forwarder_entry_is_skipped_not_failed() {
        let entry = KillReportEntry::from_forwarder(
            913,
            "limactl".to_string(),
            8080,
            "port forwarder with no matching container".to_string(),
        );
        assert_eq!(entry.status, KillStatus::Forwarder);
        assert!(
            !entry.is_failure(),
            "skipping an unresolved forwarder is not a kill failure"
        );
        assert_eq!(
            serde_json::to_value(&entry).expect("entry should serialize")["status"],
            serde_json::json!("forwarder"),
            "JSON status token should be stable"
        );
        assert_eq!(
            format_process_line(&entry),
            "skipped pid 913 (limactl) on port 8080: port forwarder with no matching container"
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
        assert_eq!(entry.status, KillStatus::WouldStopContainer);
        assert_eq!(entry.container_name.as_deref(), Some("nginx"));

        let entry_force = KillReportEntry::from_container_dry_run(&ct, true);
        assert_eq!(entry_force.status, KillStatus::WouldForceStopContainer);
    }
}
