//! Build a single [`PortEntry`] from a raw [`listeners::Listener`].
//!
//! [`build_entry`] is the per-listener hot path: it picks up metadata from
//! sysinfo, queries the caches in [`CollectContext`], and assembles the
//! final enrichment fields (container, project, framework, uptime).

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use sysinfo::{ProcessRefreshKind, UpdateKind};

use crate::framework;
use crate::types::{PortEntry, Protocol, State};

use super::CollectContext;
use super::resolve;
use super::tcp_state::TcpStateIndex;
use super::user;

/// Intern a process name into the shared `Arc<str>` set.
///
/// Worker-pool processes (php-fpm, node clusters, puma workers) fan out
/// across dozens of PIDs that share the same name; interning collapses
/// those to one heap allocation shared via refcount bumps.
pub(super) fn intern_process_name(cache: &mut HashSet<Arc<str>>, name: &str) -> Arc<str> {
    if let Some(existing) = cache.get(name) {
        return Arc::clone(existing);
    }
    let arc: Arc<str> = Arc::from(name);
    cache.insert(Arc::clone(&arc));
    arc
}

/// Build a single [`PortEntry`] from a [`listeners::Listener`], enriching it
/// with Docker, project, framework, and uptime information.
pub(super) fn build_entry(l: &listeners::Listener, context: &mut CollectContext<'_>) -> PortEntry {
    let proto = match l.protocol {
        listeners::Protocol::TCP => Protocol::Tcp,
        listeners::Protocol::UDP => Protocol::Udp,
    };

    let state = resolve_state(l, context.tcp_states);

    let sysinfo_pid = sysinfo::Pid::from_u32(l.process.pid);
    let sysinfo_process = context.sys.process(sysinfo_pid);
    let exe_path = sysinfo_process.and_then(sysinfo::Process::exe);
    let exe_name = process_executable_name(exe_path);
    let user = user::resolve_user(sysinfo_process, l.process.pid, context.user_resolver);

    let (container, project_name, project_root) = if context.deep_enrichment {
        let container = resolve::resolve_container(
            context,
            l.socket,
            proto,
            l.process.pid,
            &l.process.name,
            exe_name,
        );

        // Project detection: use container name for Docker ports, otherwise walk cwd.
        // The cache avoids redundant directory walks for processes sharing a cwd.
        let (project_name, project_root) = container.as_ref().map_or_else(
            || {
                let cwd = sysinfo_process.and_then(sysinfo::Process::cwd);
                let cmd = sysinfo_process.map_or(&[][..], sysinfo::Process::cmd);
                let root = resolve::lookup_project_root(
                    cwd,
                    exe_path,
                    cmd,
                    context.project_cache,
                    context.home,
                );
                let name = root
                    .as_ref()
                    .and_then(|r| r.file_name())
                    .map(|n| n.to_string_lossy().into_owned());
                (name, root)
            },
            |c| (Some(c.name.clone()), None),
        );

        (container, project_name, project_root)
    } else {
        (None, None, None)
    };

    let app = if context.deep_enrichment {
        framework::detect(container.as_ref(), project_root.as_deref(), &l.process.name)
            .or_else(|| exe_name.and_then(framework::detect_from_process))
    } else {
        framework::detect_from_process(&l.process.name)
            .or_else(|| exe_name.and_then(framework::detect_from_process))
    };

    let uptime_secs = sysinfo_process.and_then(|p| {
        let start = p.start_time();
        if start > 0 && context.now_epoch > start {
            Some(context.now_epoch - start)
        } else {
            None
        }
    });

    let process = intern_process_name(context.process_names, &l.process.name);

    PortEntry {
        port: l.socket.port(),
        local_addr: l.socket.ip(),
        proto,
        state,
        pid: l.process.pid,
        process,
        user,
        project: project_name,
        app,
        uptime_secs,
    }
}

fn process_executable_name(exe_path: Option<&Path>) -> Option<&str> {
    exe_path
        .and_then(Path::file_name)
        .and_then(std::ffi::OsStr::to_str)
        .filter(|name| !name.is_empty())
}

fn resolve_state(l: &listeners::Listener, tcp_states: &TcpStateIndex) -> State {
    match l.protocol {
        listeners::Protocol::TCP => tcp_states.get(&l.socket).copied().unwrap_or(State::Listen),
        listeners::Protocol::UDP => State::Unknown,
    }
}

/// Refresh kind for process metadata needed by enrichment.
///
/// Always collects user and executable-path metadata. Deep enrichment also
/// collects working-directory and command-line data for project detection.
pub(super) fn process_refresh_kind(deep_enrichment: bool) -> ProcessRefreshKind {
    let refresh_kind = ProcessRefreshKind::nothing()
        .with_user(UpdateKind::OnlyIfNotSet)
        .with_exe(UpdateKind::OnlyIfNotSet);

    if deep_enrichment {
        refresh_kind
            .with_cwd(UpdateKind::OnlyIfNotSet)
            .with_cmd(UpdateKind::OnlyIfNotSet)
    } else {
        refresh_kind
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_executable_name_uses_file_name() {
        let exe_path = Path::new("/usr/bin/google-chrome-stable");
        assert_eq!(
            process_executable_name(Some(exe_path)),
            Some("google-chrome-stable"),
            "the executable file name should be used when available"
        );
    }

    #[test]
    fn process_executable_name_ignores_empty_file_name() {
        let exe_path = Path::new("/");
        assert!(
            process_executable_name(Some(exe_path)).is_none(),
            "paths without a usable file name should be ignored"
        );
    }
}
