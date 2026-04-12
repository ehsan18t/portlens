//! # Docker/Podman container detection
//!
//! Connects to the Docker or Podman socket and queries running containers
//! to map published ports to container names and images.
//!
//! ## Module structure
//!
//! - `api` — JSON response parsing and container name resolution.
//! - `http` — Minimal HTTP/1.0 response parser (headers via `httparse`).
//! - `ipc` — OS-specific transport (Unix socket, Windows named pipe, TCP).
//! - `podman` — Rootless Podman resolver via overlay metadata (Linux only).

mod api;
mod http;
mod ipc;
#[cfg(target_os = "linux")]
mod podman;

use std::collections::HashMap;
use std::net::IpAddr;

use log::debug;

use crate::types::Protocol;

// ── Public API re-exports ────────────────────────────────────────────

pub use api::{parse_containers_json, short_container_id};
#[cfg(target_os = "linux")]
pub(crate) use podman::is_podman_rootlessport_process;
#[cfg(target_os = "linux")]
pub use podman::{RootlessPodmanResolver, lookup_rootless_podman_container};

/// Metadata about a running container that has published ports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerInfo {
    /// Container name (e.g. "backend-postgres-1").
    pub name: String,
    /// Container image (e.g. "postgres:16").
    pub image: String,
}

/// Maps `(host_ip, host_port, protocol)` to container info.
pub type ContainerPortMap = HashMap<(Option<IpAddr>, u16, Protocol), ContainerInfo>;

/// Handle for an in-progress Docker/Podman container detection.
///
/// Created by [`start_detection`] and consumed by [`await_detection`].
pub type DetectionHandle = std::sync::mpsc::Receiver<Option<ContainerPortMap>>;

// ── Detection orchestration ──────────────────────────────────────────

/// Start asynchronous detection of Docker/Podman containers.
///
/// Spawns a background thread to query the Docker/Podman daemon.
/// The returned handle should be passed to [`await_detection`] to
/// retrieve the results. This allows other work (socket enumeration,
/// process metadata refresh) to proceed concurrently.
#[must_use]
pub fn start_detection() -> DetectionHandle {
    let (tx, rx) = std::sync::mpsc::channel();
    debug!("starting container runtime detection");
    std::thread::spawn(move || {
        let result = query_daemon();
        debug!(
            "finished container runtime detection: port_mappings={}",
            result.as_ref().map_or(0, HashMap::len)
        );
        // Ignore send error: receiver may have timed out and been dropped.
        drop(tx.send(result));
    });
    rx
}

/// Wait for Docker/Podman detection to complete.
///
/// Blocks for at most 3 seconds before returning an empty map.
/// Never returns an error -- this is best-effort enrichment.
// The handle is a `Receiver` which must be consumed (moved) to
// read from it; passing by reference is not possible.
#[allow(clippy::needless_pass_by_value)]
#[must_use]
pub fn await_detection(handle: DetectionHandle) -> ContainerPortMap {
    match handle.recv_timeout(ipc::DAEMON_TIMEOUT) {
        Ok(Some(container_map)) => container_map,
        Ok(None) => {
            debug!("container runtime detection returned no data");
            ContainerPortMap::default()
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            debug!(
                "container runtime detection timed out: timeout_secs={}",
                ipc::DAEMON_TIMEOUT.as_secs()
            );
            ContainerPortMap::default()
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            debug!("container runtime detection channel disconnected");
            ContainerPortMap::default()
        }
    }
}

// ── Platform-specific daemon queries ─────────────────────────────────

/// If `DOCKER_HOST` is set to a `tcp://` URL, query it and return the map.
///
/// Shared across Unix and Windows since the TCP transport is platform-agnostic.
fn query_docker_host_tcp() -> Option<ContainerPortMap> {
    let addr = ipc::docker_host_tcp_addr()?;
    ipc::fetch_tcp_json(&addr).map(|body| api::parse_containers_json(&body))
}

#[cfg(unix)]
fn query_daemon() -> Option<ContainerPortMap> {
    use std::path::Path;

    if let Some(map) = query_docker_host_tcp() {
        return Some(map);
    }

    // Honour DOCKER_HOST when it points at a Unix socket (unix://).
    if let Some(path) = ipc::docker_host_unix_path() {
        return ipc::fetch_unix_socket_json(Path::new(&path))
            .map(|body| api::parse_containers_json(&body));
    }

    // Safety: getuid() is a simple syscall with no preconditions.
    let uid = unsafe { libc::getuid() };
    let responses = ipc::fetch_all_successes(
        ipc::unix_socket_paths(uid, crate::project::home_dir()),
        |path| ipc::fetch_unix_socket_json(Path::new(&path)),
    );

    merge_daemon_responses(responses)
}

#[cfg(windows)]
const DEFAULT_PIPE_PATHS: &[&str] = &[
    r"\\.\pipe\docker_engine",
    r"\\.\pipe\podman-machine-default",
];

#[cfg(windows)]
fn query_daemon() -> Option<ContainerPortMap> {
    if let Some(map) = query_docker_host_tcp() {
        return Some(map);
    }

    let deadline = std::time::Instant::now() + ipc::DAEMON_TIMEOUT;

    // Honour DOCKER_HOST when it points at a named pipe (npipe://).
    if let Some(path) = ipc::docker_host_npipe_path()
        && let Some(body) = ipc::fetch_named_pipe_json(&path, deadline)
    {
        return Some(api::parse_containers_json(&body));
    }

    DEFAULT_PIPE_PATHS
        .iter()
        .find_map(|path| ipc::fetch_named_pipe_json(path, deadline))
        .map(|body| api::parse_containers_json(&body))
}

#[cfg(unix)]
fn merge_daemon_responses<T, I>(responses: I) -> Option<ContainerPortMap>
where
    T: AsRef<str>,
    I: IntoIterator<Item = T>,
{
    let mut saw_response = false;
    let mut merged = ContainerPortMap::new();

    for response in responses {
        saw_response = true;
        merged.extend(api::parse_containers_json(response.as_ref()));
    }

    saw_response.then_some(merged)
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::*;

    #[cfg(unix)]
    #[test]
    fn merge_daemon_responses_combines_multiple_runtime_payloads() {
        let merged = merge_daemon_responses([
            "[]",
            r#"[{
                "Names": ["/backend-postgres-1"],
                "Image": "postgres:16",
                "Ports": [{"PublicPort": 5432, "Type": "tcp"}]
            }]"#,
        ])
        .expect("at least one daemon response should produce a map");

        let container = merged
            .get(&(None, 5432, Protocol::Tcp))
            .expect("podman/docker ports should survive multi-daemon merging");
        assert_eq!(container.name, "backend-postgres-1");
        assert_eq!(container.image, "postgres:16");
    }
}
