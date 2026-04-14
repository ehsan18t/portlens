//! Resolve a local port number to the set of unique PIDs using it.
//!
//! Reuses the socket collector so every platform-specific detail (IPv4/IPv6
//! duplication, `SO_REUSEPORT` workers, Docker userland-proxy collapsing) is
//! handled in one place.

use anyhow::Result;

use crate::collector::{self, CollectOptions};

/// A PID/process-name pair for one target of a kill request.
#[derive(Debug, Clone)]
pub struct Target {
    /// OS process identifier.
    pub pid: u32,
    /// Best-effort process name, "-" if unknown.
    pub process: String,
}

/// Enumerate unique PIDs owning local sockets on `port`.
///
/// Deduplicates on PID: a single process bound to both IPv4 and IPv6, or to
/// multiple sockets on the same port, produces exactly one `Target`.
///
/// This intentionally matches any process using the local port, not only TCP
/// listeners, so "what is blocking this port?" works for UDP and other bound
/// local sockets too.
pub fn targets_for_port(port: u16) -> Result<Vec<Target>> {
    let entries = collector::collect_with_options(&CollectOptions {
        deep_enrichment: false,
    })?;

    let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut targets = Vec::new();
    for entry in entries {
        if entry.port == port && seen.insert(entry.pid) {
            targets.push(Target {
                pid: entry.pid,
                process: entry.process.as_ref().to_owned(),
            });
        }
    }
    Ok(targets)
}

/// Resolve a PID by itself: look up its process name if possible.
///
/// Returns a synthetic target with "-" process name when the PID is not
/// currently enumerable (the kill path still treats that as `AlreadyGone` later).
pub fn target_for_pid(pid: u32) -> Target {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

    let mut sys = System::new();
    let sys_pid = Pid::from_u32(pid);
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[sys_pid]),
        false,
        ProcessRefreshKind::nothing(),
    );
    let process = sys.process(sys_pid).map_or_else(
        || "-".to_owned(),
        |p| p.name().to_string_lossy().into_owned(),
    );
    Target { pid, process }
}
