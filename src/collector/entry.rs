//! Build a single [`PortEntry`] from a raw [`listeners::Listener`].
//!
//! [`build_entry`] is the per-listener hot path: it picks up metadata from
//! sysinfo, queries the caches in [`CollectContext`], and assembles the
//! final enrichment fields (container, project, framework, uptime).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use log::{debug, trace};
use sysinfo::{ProcessRefreshKind, UpdateKind};

use crate::framework;
use crate::types::{AppLabel, PortEntry, Protocol, State};

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

    let state = resolve_state(l.protocol, l.socket, context.tcp_states);

    let sysinfo_pid = sysinfo::Pid::from_u32(l.process.pid);
    let sysinfo_process = context.sys.process(sysinfo_pid);
    let exe_path = listener_executable_path(&l.process)
        .or_else(|| sysinfo_process.and_then(sysinfo::Process::exe));
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
        detect_enriched_app(
            container.as_ref(),
            project_root.as_deref(),
            &l.process.name,
            exe_name,
            exe_path,
            context.framework_cache,
        )
    } else {
        detect_process_app(&l.process.name, exe_name)
    };

    let uptime_secs = sysinfo_process
        .and_then(|process| process_uptime_secs(context.now_epoch, process.start_time()));

    let process = intern_process_name(context.process_names, &l.process.name);

    trace!(
        "built entry: pid={} process={} proto={} socket={} state={} user={} project={:?} app={:?} uptime_secs={:?}",
        l.process.pid,
        l.process.name,
        proto,
        l.socket,
        state,
        user,
        project_name,
        app.as_deref(),
        uptime_secs
    );

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

fn listener_executable_path(process: &listeners::Process) -> Option<&Path> {
    (!process.path.is_empty()).then(|| Path::new(process.path.as_str()))
}

fn process_executable_name(exe_path: Option<&Path>) -> Option<&str> {
    exe_path
        .and_then(Path::file_name)
        .and_then(std::ffi::OsStr::to_str)
        .filter(|name| !name.is_empty())
}

fn detect_enriched_app(
    container: Option<&crate::docker::ContainerInfo>,
    project_root: Option<&Path>,
    process_name: &str,
    exe_name: Option<&str>,
    exe_path: Option<&Path>,
    framework_cache: &mut HashMap<PathBuf, Option<AppLabel>>,
) -> Option<AppLabel> {
    if let Some(info) = container
        && let Some(label) = framework::detect_from_image(info)
    {
        return Some(label);
    }

    let process_app = detect_process_app(process_name, exe_name);

    if let Some(root) = project_root
        && config_detection_allowed(process_app.as_deref(), exe_path, root)
        && let Some(label) = cached_detect_from_config(root, framework_cache)
    {
        return Some(label);
    }

    process_app
}

/// Look up the framework config detection result in the cache, or compute
/// and store it on a cache miss. Avoids redundant `read_dir` and file I/O
/// when multiple entries share the same project root.
fn cached_detect_from_config(
    project_root: &Path,
    cache: &mut HashMap<PathBuf, Option<AppLabel>>,
) -> Option<AppLabel> {
    if let Some(cached) = cache.get(project_root) {
        trace!(
            "framework cache hit: project_root={} app={:?}",
            project_root.display(),
            cached.as_deref()
        );
        return cached.clone();
    }
    let result = framework::detect_from_config(project_root);
    debug!(
        "framework cache miss: project_root={} detected_app={:?}",
        project_root.display(),
        result.as_deref()
    );
    cache.insert(project_root.to_path_buf(), result.clone());
    result
}

fn detect_process_app(process_name: &str, exe_name: Option<&str>) -> Option<AppLabel> {
    framework::detect_from_process(process_name)
        .or_else(|| exe_name.and_then(framework::detect_from_process))
}

fn config_detection_allowed(
    process_app: Option<&str>,
    exe_path: Option<&Path>,
    project_root: &Path,
) -> bool {
    process_app.is_some() || executable_belongs_to_project(exe_path, project_root)
}

fn executable_belongs_to_project(exe_path: Option<&Path>, project_root: &Path) -> bool {
    exe_path.is_some_and(|path| path.starts_with(project_root))
}

fn resolve_state(
    protocol: listeners::Protocol,
    socket: std::net::SocketAddr,
    tcp_states: &TcpStateIndex,
) -> State {
    match protocol {
        // `listeners::get_all()` includes non-listening TCP sockets too, so a
        // missing OS state lookup must stay `UNKNOWN` instead of guessing LISTEN.
        listeners::Protocol::TCP => tcp_states.get(&socket).copied().unwrap_or(State::Unknown),
        listeners::Protocol::UDP => State::NotApplicable,
    }
}

fn process_uptime_secs(now_epoch: u64, start_time: u64) -> Option<u64> {
    (start_time > 0)
        .then_some(start_time)
        .and_then(|start_time| now_epoch.checked_sub(start_time))
}

/// Refresh kind for process metadata needed by enrichment.
///
/// Always collects user and executable-path metadata. Deep enrichment also
/// collects working-directory and command-line data for project detection.
pub(super) fn process_refresh_kind(
    deep_enrichment: bool,
    refresh_exe_paths: bool,
) -> ProcessRefreshKind {
    let mut refresh_kind = ProcessRefreshKind::nothing().with_user(UpdateKind::OnlyIfNotSet);

    if refresh_exe_paths {
        refresh_kind = refresh_kind.with_exe(UpdateKind::OnlyIfNotSet);
    }

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
    use std::fs;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    use tempfile::TempDir;

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

    #[test]
    fn listener_executable_path_uses_listener_path_when_present() {
        let process = listeners::Process {
            pid: 1234,
            name: "node".to_string(),
            path: "/usr/bin/node".to_string(),
        };

        assert_eq!(
            listener_executable_path(&process),
            Some(Path::new("/usr/bin/node")),
            "listener-provided executable paths should be re-used"
        );
    }

    #[test]
    fn listener_executable_path_ignores_empty_listener_path() {
        let process = listeners::Process {
            pid: 1234,
            name: "node".to_string(),
            path: String::new(),
        };

        assert!(
            listener_executable_path(&process).is_none(),
            "empty listener paths should fall back to sysinfo"
        );
    }

    #[test]
    fn process_refresh_kind_skips_exe_refresh_when_listener_paths_exist() {
        let refresh_kind = process_refresh_kind(false, false);

        assert!(
            matches!(refresh_kind.exe(), UpdateKind::Never),
            "exe refresh should stay disabled when listener metadata already has paths"
        );
    }

    #[test]
    fn process_refresh_kind_keeps_deep_metadata_without_exe_refresh() {
        let refresh_kind = process_refresh_kind(true, false);

        assert!(
            matches!(refresh_kind.cwd(), UpdateKind::OnlyIfNotSet),
            "deep enrichment still needs cwd metadata"
        );
        assert!(
            matches!(refresh_kind.cmd(), UpdateKind::OnlyIfNotSet),
            "deep enrichment still needs command-line metadata"
        );
        assert!(
            matches!(refresh_kind.exe(), UpdateKind::Never),
            "listener paths should still avoid redundant exe refreshes during deep enrichment"
        );
    }

    #[test]
    fn missing_tcp_state_defaults_to_unknown() {
        let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 3000);
        let listener = listeners::Listener {
            process: listeners::Process {
                pid: 1234,
                name: "node".to_string(),
                path: "/usr/bin/node".to_string(),
            },
            socket,
            protocol: listeners::Protocol::TCP,
        };
        let tcp_states = TcpStateIndex::new();

        assert_eq!(
            resolve_state(listener.protocol, listener.socket, &tcp_states),
            State::Unknown,
            "missing TCP state data should stay UNKNOWN instead of guessing LISTEN"
        );
    }

    #[test]
    fn detect_enriched_app_uses_config_for_known_runtime_processes() {
        let project = TempDir::new().unwrap();
        fs::write(project.path().join("next.config.js"), "").unwrap();

        let app = detect_enriched_app(
            None,
            Some(project.path()),
            "node",
            None,
            None,
            &mut HashMap::new(),
        );

        assert_eq!(app.as_deref(), Some("Next.js"));
    }

    #[test]
    fn detect_enriched_app_uses_config_for_project_owned_binaries() {
        let project = TempDir::new().unwrap();
        let exe_path = project
            .path()
            .join("target")
            .join("debug")
            .join("service.exe");

        fs::create_dir_all(exe_path.parent().unwrap()).unwrap();
        fs::write(project.path().join("Cargo.toml"), "").unwrap();
        fs::write(&exe_path, "").unwrap();

        let app = detect_enriched_app(
            None,
            Some(project.path()),
            "service.exe",
            Some("service.exe"),
            Some(exe_path.as_path()),
            &mut HashMap::new(),
        );

        assert_eq!(app.as_deref(), Some("Rust"));
    }

    #[test]
    fn detect_enriched_app_skips_config_for_unknown_shell_processes() {
        let project = TempDir::new().unwrap();
        let external = TempDir::new().unwrap();
        let exe_path = external.path().join("pwsh.exe");

        fs::write(project.path().join("Cargo.toml"), "").unwrap();
        fs::write(&exe_path, "").unwrap();

        let app = detect_enriched_app(
            None,
            Some(project.path()),
            "pwsh.exe",
            Some("pwsh.exe"),
            Some(exe_path.as_path()),
            &mut HashMap::new(),
        );

        assert_eq!(
            app.as_deref(),
            None,
            "shell processes outside the project should not inherit the project's framework label"
        );
    }

    #[test]
    fn process_uptime_allows_same_second_start() {
        assert_eq!(
            process_uptime_secs(100, 100),
            Some(0),
            "same-second starts should produce a 0-second uptime instead of missing data"
        );
    }

    #[test]
    fn process_uptime_rejects_missing_start_time() {
        assert_eq!(
            process_uptime_secs(100, 0),
            None,
            "missing process start times should stay unavailable"
        );
    }

    #[test]
    fn process_uptime_rejects_future_start_time() {
        assert_eq!(
            process_uptime_secs(100, 101),
            None,
            "future start times should not underflow into fake uptimes"
        );
    }
}
