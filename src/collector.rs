//! # Socket collector
//!
//! Calls the `listeners` crate to enumerate open sockets and `sysinfo` to
//! resolve process metadata (name, owning user). Enriches each entry with
//! Docker container info, project root detection, and app/framework labels.

use std::path::Path;

use anyhow::Result;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind, Users};

use crate::docker::{self, ContainerPortMap};
use crate::types::{PortEntry, Protocol, State};
use crate::{framework, project};

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
    let container_map = docker::detect_containers();

    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut entries: Vec<PortEntry> = raw_listeners
        .into_iter()
        .map(|l| build_entry(&l, &sys, &users, &container_map, now_epoch))
        .collect();

    entries.sort_by_key(|e| (e.port, e.proto));
    Ok(entries)
}

/// Build a single [`PortEntry`] from a [`listeners::Listener`], enriching it
/// with Docker, project, framework, and uptime information.
fn build_entry(
    l: &listeners::Listener,
    sys: &System,
    users: &Users,
    container_map: &ContainerPortMap,
    now_epoch: u64,
) -> PortEntry {
    let proto = match l.protocol {
        listeners::Protocol::TCP => Protocol::Tcp,
        listeners::Protocol::UDP => Protocol::Udp,
    };

    let state = match proto {
        Protocol::Tcp => State::Listen,
        Protocol::Udp => State::NotApplicable,
    };

    let proto_str = match proto {
        Protocol::Tcp => "tcp",
        Protocol::Udp => "udp",
    };

    let sysinfo_pid = sysinfo::Pid::from_u32(l.process.pid);
    let sysinfo_process = sys.process(sysinfo_pid);
    let user = resolve_user(sysinfo_process, users);

    // Docker container lookup
    let container = container_map.get(&(l.socket.port(), proto_str.to_string()));

    // Project detection: use container name for Docker ports, otherwise walk cwd
    let (project_name, project_root) = container.map_or_else(
        || {
            let cwd = sysinfo_process.and_then(|p| p.cwd().map(Path::to_path_buf));
            let cmd: Vec<String> = sysinfo_process
                .map(|p| {
                    p.cmd()
                        .iter()
                        .map(|s| s.to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default();
            let root = project::detect_project_root(cwd.as_deref(), &cmd);
            let name = root
                .as_ref()
                .and_then(|r| r.file_name())
                .map(|n| n.to_string_lossy().into_owned());
            (name, root)
        },
        |c| (Some(c.name.clone()), None),
    );

    // App/framework detection
    let app = framework::detect(container, project_root.as_deref(), &l.process.name);

    // Uptime from process start time
    let uptime_secs = sysinfo_process.and_then(|p| {
        let start = p.start_time();
        if start > 0 && now_epoch > start {
            Some(now_epoch - start)
        } else {
            None
        }
    });

    PortEntry {
        port: l.socket.port(),
        proto,
        state,
        pid: l.process.pid,
        process: l.process.name.clone(),
        user,
        project: project_name,
        app,
        uptime_secs,
    }
}

/// Resolve the owning username for an already-looked-up process.
///
/// Returns `"-"` if the process or user cannot be determined.
fn resolve_user(process: Option<&sysinfo::Process>, users: &Users) -> String {
    let Some(proc_ref) = process else {
        return "-".to_string();
    };

    let Some(uid) = proc_ref.user_id() else {
        return "-".to_string();
    };

    users
        .get_user_by_id(uid)
        .map_or_else(|| "-".to_string(), |u| u.name().to_string())
}

/// Refresh kind for process metadata needed by enrichment.
///
/// Collects: user, working directory, command-line args.
fn process_refresh_kind() -> ProcessRefreshKind {
    ProcessRefreshKind::nothing()
        .with_user(UpdateKind::OnlyIfNotSet)
        .with_cwd(UpdateKind::OnlyIfNotSet)
        .with_cmd(UpdateKind::OnlyIfNotSet)
}
