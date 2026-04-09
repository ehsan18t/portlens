//! # Socket collector
//!
//! Calls the `listeners` crate to enumerate open sockets and `sysinfo` to
//! resolve process metadata (name, owning user). All OS differences are
//! encapsulated here.

use anyhow::Result;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind, Users};

use crate::types::{PortEntry, Protocol, State};

/// Collect all open TCP and UDP sockets on the system.
///
/// Returns a `Vec<PortEntry>` sorted by port number in ascending order.
/// Entries where the PID or username cannot be resolved are still included
/// with placeholder values.
pub fn collect() -> Result<Vec<PortEntry>> {
    let raw_listeners = listeners::get_all()
        .map_err(|e| anyhow::anyhow!("failed to enumerate open sockets from the OS: {e}"))?;

    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, false, process_refresh_kind());

    let users = Users::new_with_refreshed_list();

    let mut entries: Vec<PortEntry> = raw_listeners
        .into_iter()
        .map(|l| build_entry(&l, &sys, &users))
        .collect();

    entries.sort_by_key(|e| (e.port, e.proto));
    Ok(entries)
}

/// Build a single [`PortEntry`] from a [`listeners::Listener`], enriching it
/// with user information from sysinfo.
fn build_entry(l: &listeners::Listener, sys: &System, users: &Users) -> PortEntry {
    let proto = match l.protocol {
        listeners::Protocol::TCP => Protocol::Tcp,
        listeners::Protocol::UDP => Protocol::Udp,
    };

    let state = match proto {
        Protocol::Tcp => State::Listen,
        Protocol::Udp => State::NotApplicable,
    };

    let user = resolve_user(l.process.pid, sys, users);

    PortEntry {
        port: l.socket.port(),
        proto,
        state,
        pid: l.process.pid,
        process: l.process.name.clone(),
        user,
        project: None,
        app: None,
        uptime_secs: None,
    }
}

/// Resolve the owning username for a given PID via sysinfo.
///
/// Returns `"-"` if the user cannot be determined.
fn resolve_user(pid: u32, sys: &System, users: &Users) -> String {
    let sysinfo_pid = sysinfo::Pid::from_u32(pid);

    let Some(process) = sys.process(sysinfo_pid) else {
        return "-".to_string();
    };

    let Some(uid) = process.user_id() else {
        return "-".to_string();
    };

    users
        .get_user_by_id(uid)
        .map_or_else(|| "-".to_string(), |u| u.name().to_string())
}

/// Minimal refresh kind: we only need user information from each process.
fn process_refresh_kind() -> ProcessRefreshKind {
    ProcessRefreshKind::nothing().with_user(UpdateKind::OnlyIfNotSet)
}
