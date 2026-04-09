//! # Docker/Podman container detection
//!
//! Connects to the Docker or Podman socket and queries running containers
//! to map published ports to container names and images.
//!
//! Uses raw HTTP/1.0 over Unix socket (Linux) or named pipe (Windows)
//! with zero additional dependencies.

use std::collections::HashMap;
#[cfg(unix)]
use std::io::{BufRead, BufReader};
use std::net::IpAddr;
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(windows)]
use std::{ffi::OsStr, ffi::c_void, os::windows::ffi::OsStrExt, os::windows::io::AsRawHandle};

use serde::Deserialize;

use crate::types::Protocol;

#[derive(Deserialize)]
struct DockerPort<'a> {
    #[serde(rename = "IP")]
    host_ip: Option<IpAddr>,
    #[serde(rename = "PublicPort")]
    public_port: Option<u16>,
    #[serde(rename = "Type")]
    proto: Option<&'a str>,
}

#[derive(Deserialize)]
struct DockerContainer<'a> {
    #[serde(rename = "Id")]
    id: Option<&'a str>,
    #[serde(rename = "Names")]
    names: Option<Vec<&'a str>>,
    #[serde(rename = "Image")]
    image: Option<&'a str>,
    #[serde(rename = "Ports")]
    ports: Option<Vec<DockerPort<'a>>>,
}

/// Metadata about a running container that has published ports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerInfo {
    /// Container name (e.g. "backend-postgres-1").
    pub name: String,
    /// Container image (e.g. "postgres:16").
    pub image: String,
}

/// Maps `(host_port, protocol)` to container info.
pub type ContainerPortMap = HashMap<(Option<IpAddr>, u16, Protocol), ContainerInfo>;

/// Maximum time to wait for the Docker/Podman daemon to respond.
///
/// On Unix the socket itself has a per-read timeout, but on Windows the
/// named pipe has no built-in timeout support. A thread-level timeout
/// covers both platforms uniformly.
const DAEMON_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

#[cfg(windows)]
type RawHandle = *mut c_void;

#[cfg(windows)]
const ERROR_BROKEN_PIPE: i32 = 109;

#[cfg(windows)]
const ERROR_PIPE_BUSY: i32 = 231;

#[cfg(windows)]
const PIPE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn WaitNamedPipeW(name: *const u16, timeout: u32) -> i32;
    fn PeekNamedPipe(
        named_pipe: RawHandle,
        buffer: *mut c_void,
        buffer_size: u32,
        bytes_read: *mut u32,
        total_bytes_avail: *mut u32,
        bytes_left_this_message: *mut u32,
    ) -> i32;
}

/// Handle for an in-progress Docker/Podman container detection.
///
/// Created by [`start_detection`] and consumed by [`await_detection`].
pub type DetectionHandle = std::sync::mpsc::Receiver<Option<ContainerPortMap>>;

/// Start asynchronous detection of Docker/Podman containers.
///
/// Spawns a background thread to query the Docker/Podman daemon.
/// The returned handle should be passed to [`await_detection`] to
/// retrieve the results. This allows other work (socket enumeration,
/// process metadata refresh) to proceed concurrently.
#[must_use]
pub fn start_detection() -> DetectionHandle {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // Ignore send error: receiver may have timed out and been dropped.
        drop(tx.send(query_daemon()));
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
    handle
        .recv_timeout(DAEMON_TIMEOUT)
        .ok()
        .flatten()
        .unwrap_or_default()
}

fn query_daemon() -> Option<ContainerPortMap> {
    let body = fetch_containers_json()?;
    Some(parse_containers_json(&body))
}

/// Parse the JSON response from `GET /containers/json` into a port map.
///
/// Each container may publish multiple ports. The map keys are
/// `(public_ip, public_port, protocol)` tuples.
#[must_use]
pub fn parse_containers_json(json_body: &str) -> ContainerPortMap {
    let mut map = ContainerPortMap::new();

    let Ok(containers) = serde_json::from_str::<Vec<DockerContainer<'_>>>(json_body) else {
        return map;
    };

    for container in containers {
        let name = container_display_name(&container);
        let image = container.image.unwrap_or("").to_string();
        let info = ContainerInfo { name, image };

        let Some(ports) = container.ports else {
            continue;
        };

        for port in ports {
            let Some(public_port) = port.public_port else {
                continue;
            };
            let proto = match port.proto.unwrap_or("tcp") {
                "udp" => Protocol::Udp,
                _ => Protocol::Tcp,
            };

            map.insert((port.host_ip, public_port, proto), info.clone());
        }
    }

    map
}

fn container_display_name(container: &DockerContainer<'_>) -> String {
    container
        .names
        .as_ref()
        .and_then(|names| names.iter().copied().find_map(normalize_container_name))
        .or_else(|| {
            container
                .image
                .map(str::trim)
                .filter(|image| !image.is_empty())
                .map(ToOwned::to_owned)
        })
        .or_else(|| {
            container
                .id
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(short_container_id)
        })
        .unwrap_or_else(|| "container".to_string())
}

fn normalize_container_name(name: &str) -> Option<String> {
    let normalized = name.trim().trim_start_matches('/');
    (!normalized.is_empty()).then(|| normalized.to_string())
}

fn short_container_id(id: &str) -> String {
    id.chars().take(12).collect()
}

#[cfg(unix)]
fn fetch_first_success<P, I, F>(candidates: I, fetch: F) -> Option<String>
where
    P: Send + 'static,
    I: IntoIterator<Item = P>,
    F: Fn(P) -> Option<String> + Send + Sync + 'static,
{
    let mut has_candidates = false;
    let (tx, rx) = std::sync::mpsc::channel();
    let fetch = std::sync::Arc::new(fetch);

    for candidate in candidates {
        has_candidates = true;
        let tx = tx.clone();
        let fetch = std::sync::Arc::clone(&fetch);
        std::thread::spawn(move || {
            if let Some(body) = fetch(candidate) {
                drop(tx.send(body));
            }
        });
    }

    drop(tx);
    has_candidates.then_some(())?;
    rx.recv_timeout(DAEMON_TIMEOUT).ok()
}

#[cfg(unix)]
fn fetch_containers_json() -> Option<String> {
    use std::os::unix::net::UnixStream;

    // Safety: getuid() is a simple syscall with no preconditions.
    let uid = unsafe { libc::getuid() };

    fetch_first_success(unix_socket_paths(uid, home_dir()), |path| {
        let mut stream = UnixStream::connect(path).ok()?;
        // Best-effort timeout; proceed even if it cannot be set.
        drop(stream.set_read_timeout(Some(DAEMON_TIMEOUT)));
        send_http_request(&mut stream)
    })
}

#[cfg(windows)]
fn fetch_containers_json() -> Option<String> {
    let deadline = std::time::Instant::now() + DAEMON_TIMEOUT;

    let pipe_paths = [
        r"\\.\pipe\docker_engine",
        r"\\.\pipe\podman-machine-default",
    ];

    for path in pipe_paths {
        if let Some(body) = fetch_named_pipe_json(path, deadline) {
            return Some(body);
        }
    }

    None
}

#[cfg(windows)]
fn fetch_named_pipe_json(path: &str, deadline: std::time::Instant) -> Option<String> {
    use std::fs::OpenOptions;

    loop {
        let mut stream = match OpenOptions::new().read(true).write(true).open(path) {
            Ok(stream) => stream,
            Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                wait_named_pipe(path, deadline)?;
                continue;
            }
            Err(_) => return None,
        };

        return send_http_request_windows(&mut stream, deadline);
    }
}

#[cfg(unix)]
fn unix_socket_paths(uid: u32, home: Option<PathBuf>) -> Vec<String> {
    let mut socket_paths = vec![
        "/var/run/docker.sock".to_string(),
        format!("/run/user/{uid}/docker.sock"),
        format!("/run/user/{uid}/podman/podman.sock"),
        "/run/podman/podman.sock".to_string(),
    ];

    if let Some(home) = home {
        socket_paths.push(home.join(".docker/run/docker.sock").display().to_string());
    }

    socket_paths
}

#[cfg(unix)]
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(unix)]
fn send_http_request(stream: &mut (impl std::io::Read + std::io::Write)) -> Option<String> {
    use std::io::Read as _;

    // Omit the API version prefix so the daemon uses its own default version.
    // Hardcoding e.g. `/v1.45/` would cause a 400 on older daemons whose max
    // API version is lower than the requested one.
    stream
        .write_all(b"GET /containers/json HTTP/1.0\r\nHost: localhost\r\n\r\n")
        .ok()?;

    let mut reader = BufReader::new(stream);

    // Read and validate the HTTP status line (e.g. "HTTP/1.0 200 OK").
    // Bail out early on non-2xx responses to avoid parsing error bodies.
    let mut status_line = String::new();
    reader.read_line(&mut status_line).ok()?;
    let status_code: u16 = status_line.split_whitespace().nth(1)?.parse().ok()?;
    if !(200..300).contains(&status_code) {
        return None;
    }

    // Skip remaining response headers (read until empty line)
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        if line.trim().is_empty() {
            break;
        }
    }

    // Read the response body
    let mut body = String::new();
    reader.read_to_string(&mut body).ok()?;
    Some(body)
}

#[cfg(windows)]
fn send_http_request_windows(
    stream: &mut std::fs::File,
    deadline: std::time::Instant,
) -> Option<String> {
    use std::io::{Read as _, Write as _};

    stream
        .write_all(b"GET /containers/json HTTP/1.0\r\nHost: localhost\r\n\r\n")
        .ok()?;

    let mut response = Vec::new();
    let mut chunk = [0_u8; 8192];

    loop {
        if let Some(body) = try_extract_http_body(&response, false) {
            return Some(body);
        }

        let available = match peek_available_bytes(stream) {
            Some(available) => available,
            None if last_os_error_is(ERROR_BROKEN_PIPE) => {
                return try_extract_http_body(&response, true);
            }
            None => return None,
        };

        if available == 0 {
            if std::time::Instant::now() >= deadline {
                return try_extract_http_body(&response, true);
            }
            std::thread::sleep(PIPE_POLL_INTERVAL);
            continue;
        }

        let max_chunk = u32::try_from(chunk.len()).ok()?;
        let read_len = usize::try_from(available.min(max_chunk)).ok()?;
        match stream.read(&mut chunk[..read_len]) {
            Ok(0) => return try_extract_http_body(&response, true),
            Ok(read) => response.extend_from_slice(&chunk[..read]),
            Err(error) if error.raw_os_error() == Some(ERROR_BROKEN_PIPE) => {
                return try_extract_http_body(&response, true);
            }
            Err(_) => return None,
        }
    }
}

#[cfg(any(windows, test))]
fn try_extract_http_body(response: &[u8], eof: bool) -> Option<String> {
    let text = std::str::from_utf8(response).ok()?;
    let headers_end = text.find("\r\n\r\n")?;

    let mut header_lines = text[..headers_end].split("\r\n");
    let status_line = header_lines.next()?;
    let status_code: u16 = status_line.split_whitespace().nth(1)?.parse().ok()?;
    if !(200..300).contains(&status_code) {
        return None;
    }

    let body = &text[headers_end + 4..];
    if let Some(content_length) = parse_content_length(header_lines) {
        return (body.len() >= content_length).then(|| body[..content_length].to_string());
    }

    eof.then(|| body.to_string())
}

#[cfg(any(windows, test))]
fn parse_content_length<'a>(mut header_lines: impl Iterator<Item = &'a str>) -> Option<usize> {
    header_lines.find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("Content-Length")
            .then(|| value.trim().parse().ok())
            .flatten()
    })
}

#[cfg(windows)]
fn wait_named_pipe(path: &str, deadline: std::time::Instant) -> Option<()> {
    let timeout_ms = remaining_timeout_ms(deadline)?;
    let wide_path = wide_string(path);
    let success = unsafe { WaitNamedPipeW(wide_path.as_ptr(), timeout_ms) };
    (success != 0).then_some(())
}

#[cfg(windows)]
fn peek_available_bytes(stream: &std::fs::File) -> Option<u32> {
    let mut available = 0;
    let success = unsafe {
        PeekNamedPipe(
            stream.as_raw_handle(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            &raw mut available,
            std::ptr::null_mut(),
        )
    };
    (success != 0).then_some(available)
}

#[cfg(windows)]
fn remaining_timeout_ms(deadline: std::time::Instant) -> Option<u32> {
    let remaining = deadline.checked_duration_since(std::time::Instant::now())?;
    u32::try_from(remaining.as_millis().min(u128::from(u32::MAX))).ok()
}

#[cfg(windows)]
fn wide_string(value: &str) -> Vec<u16> {
    OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(windows)]
fn last_os_error_is(expected: i32) -> bool {
    std::io::Error::last_os_error().raw_os_error() == Some(expected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    use crate::types::Protocol;

    const SAMPLE_RESPONSE: &str = r#"[
        {
            "Names": ["/backend-postgres-1"],
            "Image": "postgres:16",
            "Ports": [
                {"PrivatePort": 5432, "PublicPort": 5432, "Type": "tcp"}
            ]
        },
        {
            "Names": ["/backend-redis-1"],
            "Image": "redis:7-alpine",
            "Ports": [
                {"PrivatePort": 6379, "PublicPort": 6379, "Type": "tcp"}
            ]
        },
        {
            "Names": ["/no-ports"],
            "Image": "busybox",
            "Ports": []
        }
    ]"#;

    #[test]
    fn parse_valid_response() {
        let map = parse_containers_json(SAMPLE_RESPONSE);
        assert_eq!(map.len(), 2);

        let pg = map.get(&(None, 5432, Protocol::Tcp)).unwrap();
        assert_eq!(pg.name, "backend-postgres-1");
        assert_eq!(pg.image, "postgres:16");

        let redis = map.get(&(None, 6379, Protocol::Tcp)).unwrap();
        assert_eq!(redis.name, "backend-redis-1");
        assert_eq!(redis.image, "redis:7-alpine");
    }

    #[test]
    fn parse_empty_array() {
        let map = parse_containers_json("[]");
        assert!(map.is_empty());
    }

    #[test]
    fn parse_invalid_json_returns_empty() {
        let map = parse_containers_json("not json");
        assert!(map.is_empty());
    }

    #[test]
    fn parse_container_without_public_port() {
        let json = r#"[{
            "Names": ["/internal"],
            "Image": "app:latest",
            "Ports": [{"PrivatePort": 8080, "Type": "tcp"}]
        }]"#;
        let map = parse_containers_json(json);
        assert!(
            map.is_empty(),
            "entries without PublicPort should be skipped"
        );
    }

    #[test]
    fn container_name_strips_leading_slash() {
        let json = r#"[{
            "Names": ["/my-container"],
            "Image": "nginx:latest",
            "Ports": [{"PrivatePort": 80, "PublicPort": 80, "Type": "tcp"}]
        }]"#;
        let map = parse_containers_json(json);
        let info = map.get(&(None, 80, Protocol::Tcp)).unwrap();
        assert_eq!(info.name, "my-container");
    }

    #[test]
    fn parse_multiple_ports_same_container() {
        let json = r#"[{
            "Names": ["/multi"],
            "Image": "app:latest",
            "Ports": [
                {"PrivatePort": 80, "PublicPort": 8080, "Type": "tcp"},
                {"PrivatePort": 443, "PublicPort": 8443, "Type": "tcp"}
            ]
        }]"#;
        let map = parse_containers_json(json);
        assert_eq!(map.len(), 2);
        assert!(map.contains_key(&(None, 8080, Protocol::Tcp)));
        assert!(map.contains_key(&(None, 8443, Protocol::Tcp)));
    }

    #[test]
    fn parse_missing_protocol_defaults_to_tcp() {
        let json = r#"[{
            "Names": ["/web"],
            "Image": "nginx:latest",
            "Ports": [{"PrivatePort": 80, "PublicPort": 8080}]
        }]"#;
        let map = parse_containers_json(json);
        assert!(
            map.contains_key(&(None, 8080, Protocol::Tcp)),
            "missing Type should default to TCP"
        );
    }

    #[test]
    fn parse_container_with_empty_name() {
        let json = r#"[{
            "Names": [],
            "Image": "app:latest",
            "Ports": [{"PrivatePort": 80, "PublicPort": 80, "Type": "tcp"}]
        }]"#;
        let map = parse_containers_json(json);
        let info = map.get(&(None, 80, Protocol::Tcp)).unwrap();
        assert_eq!(
            info.name, "app:latest",
            "containers without names should fall back to their image"
        );
    }

    #[test]
    fn parse_container_without_name_or_image_uses_short_id() {
        let json = r#"[{
            "Id": "0123456789abcdef0123456789abcdef",
            "Names": ["/"],
            "Ports": [{"PrivatePort": 80, "PublicPort": 80, "Type": "tcp"}]
        }]"#;
        let map = parse_containers_json(json);
        let info = map.get(&(None, 80, Protocol::Tcp)).unwrap();
        assert_eq!(
            info.name, "0123456789ab",
            "containers without names or images should fall back to a short id"
        );
    }

    #[test]
    fn parse_container_with_explicit_host_ip() {
        let json = r#"[{
            "Names": ["/api"],
            "Image": "node:22",
            "Ports": [{"IP": "127.0.0.1", "PrivatePort": 3000, "PublicPort": 8080, "Type": "tcp"}]
        }]"#;
        let map = parse_containers_json(json);

        assert!(map.contains_key(&(Some(IpAddr::V4(Ipv4Addr::LOCALHOST)), 8080, Protocol::Tcp,)));
    }

    #[test]
    fn http_body_parser_waits_for_complete_content_length() {
        let partial = b"HTTP/1.0 200 OK\r\nContent-Length: 5\r\n\r\n123";
        assert!(try_extract_http_body(partial, false).is_none());

        let complete = b"HTTP/1.0 200 OK\r\nContent-Length: 5\r\n\r\n12345";
        assert_eq!(
            try_extract_http_body(complete, false).as_deref(),
            Some("12345")
        );
    }

    #[test]
    fn http_body_parser_accepts_eof_without_content_length() {
        let response = b"HTTP/1.0 200 OK\r\nServer: docker\r\n\r\n[]";
        assert_eq!(try_extract_http_body(response, true).as_deref(), Some("[]"));
    }

    #[cfg(unix)]
    #[test]
    fn unix_socket_paths_include_rootless_docker_locations() {
        let home = PathBuf::from("/home/tester");
        let paths = unix_socket_paths(1000, Some(home));

        assert!(paths.contains(&"/run/user/1000/docker.sock".to_string()));
        assert!(
            paths.contains(&"/home/tester/.docker/run/docker.sock".to_string()),
            "rootless home socket should be probed"
        );
    }
}
