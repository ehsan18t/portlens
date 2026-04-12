//! Container and project-root resolution for socket listeners.
//!
//! These helpers turn a `(socket, pid, process_name)` triple into an
//! optional Docker/Podman `ContainerInfo` and an optional project
//! root `PathBuf`, using the caches carried by [`CollectContext`].

use std::collections::HashMap;
use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use crate::docker::{self, ContainerPortMap};
use crate::project;
use crate::types::Protocol;

use super::CollectContext;
use super::dedup;

// ── Container resolution ─────────────────────────────────────────────

#[cfg(target_os = "linux")]
pub(super) fn resolve_container(
    context: &mut CollectContext<'_>,
    socket: SocketAddr,
    proto: Protocol,
    pid: u32,
    process_name: &str,
    exe_name: Option<&str>,
) -> Option<docker::ContainerInfo> {
    if let Some(container) =
        lookup_container(context.container_map, socket, proto, process_name, exe_name)
    {
        return Some(container.clone());
    }

    let rootless_name =
        rootless_podman_process_name(process_name, exe_name).unwrap_or(process_name);
    docker::lookup_rootless_podman_container(
        pid,
        rootless_name,
        context.podman_rootless_resolver,
        context.home,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::needless_pass_by_ref_mut)]
pub(super) fn resolve_container(
    context: &mut CollectContext<'_>,
    socket: SocketAddr,
    proto: Protocol,
    _pid: u32,
    process_name: &str,
    exe_name: Option<&str>,
) -> Option<docker::ContainerInfo> {
    lookup_container(context.container_map, socket, proto, process_name, exe_name).cloned()
}

fn lookup_container<'a>(
    container_map: &'a ContainerPortMap,
    socket: SocketAddr,
    proto: Protocol,
    process_name: &str,
    exe_name: Option<&str>,
) -> Option<&'a docker::ContainerInfo> {
    // Exact match: the listener's local address matches the container's published IP.
    if let Some(container) = container_map.get(&(Some(socket.ip()), socket.port(), proto)) {
        return Some(container);
    }

    // Wildcard match: the container is mapped to a specific host IP, but the
    // OS-level listener reports 0.0.0.0 (or [::]) because the process itself
    // binds on all interfaces.
    if let Some(container) = container_map.get(&(None, socket.port(), proto)) {
        return Some(container);
    }

    // Proxy fallback: Docker Desktop Windows publishes ports via proxy
    // processes (wslrelay.exe, com.docker.backend.exe) that bind on a
    // different address than the container. When the port matches AND the
    // process is a known Docker proxy, try every container mapped to this
    // port+proto regardless of the published IP.
    if dedup::is_docker_proxy_process(process_name)
        || exe_name.is_some_and(dedup::is_docker_proxy_process)
    {
        return container_map
            .iter()
            .find(|((_, port, p), _)| *port == socket.port() && *p == proto)
            .map(|(_, container)| container);
    }

    None
}

#[cfg(target_os = "linux")]
fn rootless_podman_process_name<'a>(
    process_name: &'a str,
    exe_name: Option<&'a str>,
) -> Option<&'a str> {
    if docker::is_podman_rootlessport_process(process_name) {
        return Some(process_name);
    }
    exe_name.filter(|name| docker::is_podman_rootlessport_process(name))
}

// ── Project cache ────────────────────────────────────────────────────

/// Look up the project root for a process, using a cache to skip repeated
/// directory walks for processes that share the same working directory.
///
/// Falls back to the executable path and then the first absolute path
/// found in the command-line arguments when cwd-based detection fails.
///
/// `home` is the user's home directory ceiling resolved once by the
/// collector and passed down to avoid repeated env-var reads.
pub(super) fn lookup_project_root(
    cwd: Option<&Path>,
    exe: Option<&Path>,
    cmd: &[OsString],
    cache: &mut HashMap<PathBuf, Option<PathBuf>>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(cwd_path) = cwd
        && let Some(root) = lookup_cached_project_root(cwd_path, cache, home)
    {
        return Some(root);
    }

    if let Some(exe_path) = exe.and_then(Path::parent)
        && let Some(root) = lookup_cached_project_root(exe_path, cache, home)
    {
        return Some(root);
    }

    for cmd_path in project::absolute_cmd_parents(cmd) {
        if let Some(root) = lookup_cached_project_root(cmd_path, cache, home) {
            return Some(root);
        }
    }

    None
}

fn lookup_cached_project_root(
    start: &Path,
    cache: &mut HashMap<PathBuf, Option<PathBuf>>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    let mut visited = Vec::new();

    for dir in project::walk_ancestors(start, home) {
        if let Some(cached) = cache.get(&dir).cloned() {
            for path in visited {
                cache.insert(path, cached.clone());
            }
            return cached;
        }

        visited.push(dir.clone());

        if project::has_marker(&dir) {
            let result = Some(dir);
            for path in visited {
                cache.insert(path, result.clone());
            }
            return result;
        }
    }

    for path in visited {
        cache.insert(path, None);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    use tempfile::TempDir;

    #[test]
    fn container_lookup_prefers_exact_address_matches() {
        let mut map = HashMap::new();
        map.insert(
            (Some(IpAddr::V4(Ipv4Addr::LOCALHOST)), 8080, Protocol::Tcp),
            docker::ContainerInfo {
                name: "loopback-app".to_string(),
                image: "node:22".to_string(),
            },
        );

        let exact = lookup_container(
            &map,
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8080),
            Protocol::Tcp,
            "node",
            None,
        );
        assert_eq!(exact.map(|info| info.name.as_str()), Some("loopback-app"));

        let mismatch = lookup_container(
            &map,
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10)), 8080),
            Protocol::Tcp,
            "node",
            None,
        );
        assert!(
            mismatch.is_none(),
            "non-matching local addresses must not inherit container enrichment"
        );
    }

    #[test]
    fn container_lookup_uses_proxy_fallback_for_unique_port_mapping() {
        let mut map = HashMap::new();
        map.insert(
            (Some(IpAddr::V4(Ipv4Addr::UNSPECIFIED)), 5432, Protocol::Tcp),
            docker::ContainerInfo {
                name: "postgres".to_string(),
                image: "postgres:16".to_string(),
            },
        );

        let container = lookup_container(
            &map,
            SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 5432),
            Protocol::Tcp,
            "wslrelay.exe",
            None,
        );
        assert_eq!(container.map(|info| info.name.as_str()), Some("postgres"));
    }

    #[test]
    fn container_lookup_uses_proxy_fallback_for_rootlessport() {
        let mut map = HashMap::new();
        map.insert(
            (Some(IpAddr::V4(Ipv4Addr::UNSPECIFIED)), 6379, Protocol::Tcp),
            docker::ContainerInfo {
                name: "redis".to_string(),
                image: "redis:7-alpine".to_string(),
            },
        );

        let container = lookup_container(
            &map,
            SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 6379),
            Protocol::Tcp,
            "rootlessport",
            None,
        );
        assert_eq!(container.map(|info| info.name.as_str()), Some("redis"));
    }

    #[test]
    fn container_lookup_uses_exe_name_for_proxy_fallback() {
        let mut map = HashMap::new();
        map.insert(
            (Some(IpAddr::V4(Ipv4Addr::UNSPECIFIED)), 3000, Protocol::Tcp),
            docker::ContainerInfo {
                name: "web".to_string(),
                image: "node:22".to_string(),
            },
        );

        let container = lookup_container(
            &map,
            SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 3000),
            Protocol::Tcp,
            "truncated-helper",
            Some("wslrelay.exe"),
        );
        assert_eq!(container.map(|info| info.name.as_str()), Some("web"));
    }

    #[test]
    fn project_root_cache_learns_visited_ancestors() {
        let root = TempDir::new().unwrap();
        fs::write(root.path().join("Cargo.toml"), "").unwrap();

        let first = root.path().join("src").join("db");
        let second = root.path().join("src").join("utils");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();

        let mut cache = HashMap::new();

        let first_result = lookup_cached_project_root(&first, &mut cache, None);
        assert_eq!(first_result.as_deref(), Some(root.path()));
        assert_eq!(
            cache.get(first.as_path()).and_then(Option::as_deref),
            Some(root.path()),
            "the original cwd should be cached"
        );
        assert_eq!(
            cache
                .get(first.parent().unwrap())
                .and_then(Option::as_deref),
            Some(root.path()),
            "visited ancestors should also be cached"
        );

        let second_result = lookup_cached_project_root(&second, &mut cache, None);
        assert_eq!(second_result.as_deref(), Some(root.path()));
        assert_eq!(
            cache.get(second.as_path()).and_then(Option::as_deref),
            Some(root.path()),
            "sibling directories should learn from the cached ancestor"
        );
    }

    #[test]
    fn project_root_cache_does_not_poison_unrelated_ancestors() {
        let workspace = TempDir::new().unwrap();
        let outer = workspace.path().join("workspace");
        let project_root = outer.join("app");
        let inside = project_root.join("src").join("db");
        let unrelated = outer.join("services").join("worker");

        fs::create_dir_all(&inside).unwrap();
        fs::create_dir_all(&unrelated).unwrap();
        fs::write(project_root.join("Cargo.toml"), "").unwrap();

        let mut cache = HashMap::new();

        let first_result = lookup_cached_project_root(&inside, &mut cache, None);
        assert_eq!(first_result.as_deref(), Some(project_root.as_path()));
        assert!(
            !cache.contains_key(outer.as_path()),
            "ancestors above the discovered project root must not be cached as project hits"
        );

        let unrelated_result = lookup_cached_project_root(&unrelated, &mut cache, None);
        assert!(
            unrelated_result.is_none(),
            "an unrelated path under the same ancestor must not inherit another project's root"
        );
    }

    #[test]
    fn lookup_project_root_prefers_exe_parent_before_cmd_paths() {
        let workspace = TempDir::new().unwrap();
        let exe_root = workspace.path().join("service");
        let cmd_root = workspace.path().join("tooling");
        let exe_path = exe_root.join("bin").join("service.exe");
        let cmd_path = cmd_root.join("scripts").join("launcher.py");

        fs::create_dir_all(exe_path.parent().unwrap()).unwrap();
        fs::create_dir_all(cmd_path.parent().unwrap()).unwrap();
        fs::write(exe_root.join("Cargo.toml"), "").unwrap();
        fs::write(cmd_root.join("pyproject.toml"), "").unwrap();
        fs::write(&exe_path, "").unwrap();
        fs::write(&cmd_path, "").unwrap();

        let cmd = vec![OsString::from(&cmd_path)];
        let mut cache = HashMap::new();

        let result = lookup_project_root(None, Some(&exe_path), &cmd, &mut cache, None);
        assert_eq!(
            result.as_deref(),
            Some(exe_root.as_path()),
            "the executable path should win before command-line argument scanning"
        );
    }
}
