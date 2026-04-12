//! # Docker/Podman container detection
//!
//! Connects to the Docker or Podman socket and queries running containers
//! to map published ports to container names and images.
//!
//! Uses raw HTTP/1.0 over Unix socket (Linux) or named pipe (Windows)
//! with zero additional dependencies.

use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::collections::HashSet;
#[cfg(target_os = "linux")]
use std::ffi::OsStr;
use std::io::{BufRead, BufReader};
use std::net::IpAddr;
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::{ffi::OsStr, ffi::c_void, os::windows::ffi::OsStrExt, os::windows::io::AsRawHandle};

#[cfg(target_os = "linux")]
use std::fs;

use log::debug;
use serde::Deserialize;

use crate::types::Protocol;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TransferEncoding {
    Identity,
    Chunked,
    Unsupported,
}

#[derive(Deserialize)]
struct DockerPort<'a> {
    #[serde(
        rename = "IP",
        alias = "host_ip",
        default,
        deserialize_with = "deserialize_host_ip"
    )]
    host_ip: Option<IpAddr>,
    #[serde(rename = "PublicPort", alias = "host_port")]
    public_port: Option<u16>,
    #[serde(rename = "Type", alias = "protocol")]
    proto: Option<&'a str>,
    #[serde(alias = "range")]
    port_range: Option<u16>,
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

/// Cache for rootless Podman container lookups keyed by process and network namespace.
#[cfg(target_os = "linux")]
#[derive(Default)]
pub struct RootlessPodmanResolver {
    containers_by_netns: Option<HashMap<PathBuf, ContainerInfo>>,
    containers_by_pid: HashMap<u32, Option<ContainerInfo>>,
}

#[cfg(target_os = "linux")]
#[derive(Deserialize)]
struct PodmanStorageContainer {
    id: String,
    #[serde(default)]
    names: Vec<String>,
    metadata: Option<String>,
}

#[cfg(target_os = "linux")]
#[derive(Deserialize)]
struct PodmanStorageMetadata {
    #[serde(rename = "image-name")]
    image_name: Option<String>,
    name: Option<String>,
}

#[cfg(target_os = "linux")]
#[derive(Deserialize)]
struct PodmanContainerConfig {
    linux: Option<PodmanLinuxConfig>,
}

#[cfg(target_os = "linux")]
#[derive(Deserialize)]
struct PodmanLinuxConfig {
    #[serde(default)]
    namespaces: Vec<PodmanNamespace>,
}

#[cfg(target_os = "linux")]
#[derive(Deserialize)]
struct PodmanNamespace {
    #[serde(rename = "type")]
    namespace_type: String,
    path: Option<PathBuf>,
}

/// Maps `(host_port, protocol)` to container info.
pub type ContainerPortMap = HashMap<(Option<IpAddr>, u16, Protocol), ContainerInfo>;

/// Resolve a rootless Podman `rootlessport` helper process back to its container.
///
/// When the Podman API socket is unavailable to the current process, this falls
/// back to local overlay metadata and Linux network namespace paths.
#[cfg(target_os = "linux")]
pub fn lookup_rootless_podman_container(
    pid: u32,
    process_name: &str,
    resolver: &mut RootlessPodmanResolver,
    home: Option<&Path>,
) -> Option<ContainerInfo> {
    if !is_podman_rootlessport_process(process_name) {
        return None;
    }

    if let Some(container) = resolver.containers_by_pid.get(&pid) {
        return container.clone();
    }

    let containers_by_netns = resolver
        .containers_by_netns
        .get_or_insert_with(|| load_rootless_podman_containers_by_netns(home));
    let container =
        match_container_by_netns_paths(&read_process_netns_paths(pid), containers_by_netns);

    resolver.containers_by_pid.insert(pid, container.clone());
    container
}

/// Maximum time to wait for the Docker/Podman daemon to respond.
///
/// On Unix the socket itself has a per-read timeout, but on Windows the
/// named pipe has no built-in timeout support. A thread-level timeout
/// covers both platforms uniformly.
const DAEMON_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Raw HTTP/1.0 request sent to the Docker/Podman daemon to list running
/// containers. The API version prefix is intentionally omitted so the daemon
/// uses its own default, avoiding 400 errors on older engines.
const CONTAINERS_HTTP_REQUEST: &[u8] = b"GET /containers/json HTTP/1.0\r\nHost: localhost\r\n\r\n";

#[cfg(target_os = "linux")]
fn load_rootless_podman_containers_by_netns(
    home: Option<&Path>,
) -> HashMap<PathBuf, ContainerInfo> {
    let mut containers = HashMap::new();

    for overlay_root in podman_overlay_container_roots(home) {
        containers.extend(load_podman_rootless_containers_from_overlay_root(
            &overlay_root,
        ));
    }

    containers
}

#[cfg(target_os = "linux")]
fn podman_overlay_container_roots(home: Option<&Path>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut roots = Vec::new();

    let mut push_unique = |path: PathBuf| {
        if seen.insert(path.clone()) {
            roots.push(path);
        }
    };

    if let Some(xdg_data_home) = std::env::var_os("XDG_DATA_HOME") {
        push_unique(PathBuf::from(xdg_data_home).join("containers/storage/overlay-containers"));
    }

    if let Some(home) = home {
        push_unique(home.join(".local/share/containers/storage/overlay-containers"));
    }

    push_unique(PathBuf::from(
        "/var/lib/containers/storage/overlay-containers",
    ));

    roots
}

#[cfg(target_os = "linux")]
fn load_podman_rootless_containers_from_overlay_root(
    overlay_root: &Path,
) -> HashMap<PathBuf, ContainerInfo> {
    let catalog_path = overlay_root.join("containers.json");
    let Ok(catalog_json) = fs::read_to_string(catalog_path) else {
        return HashMap::new();
    };
    let Ok(containers) = serde_json::from_str::<Vec<PodmanStorageContainer>>(&catalog_json) else {
        return HashMap::new();
    };

    let mut containers_by_netns = HashMap::new();
    for container in containers {
        let info = podman_storage_container_info(&container);
        let config_path = overlay_root
            .join(&container.id)
            .join("userdata/config.json");
        let Some(netns_path) = read_podman_network_namespace_path(&config_path) else {
            continue;
        };
        containers_by_netns.insert(netns_path, info);
    }

    containers_by_netns
}

#[cfg(target_os = "linux")]
fn podman_storage_container_info(container: &PodmanStorageContainer) -> ContainerInfo {
    let metadata: Option<PodmanStorageMetadata> = container
        .metadata
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok());
    let name = container
        .names
        .first()
        .cloned()
        .or_else(|| metadata.as_ref().and_then(|value| value.name.clone()))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| short_container_id(&container.id));
    let image = metadata
        .and_then(|value| value.image_name)
        .unwrap_or_default();

    ContainerInfo { name, image }
}

#[cfg(target_os = "linux")]
fn read_podman_network_namespace_path(config_path: &Path) -> Option<PathBuf> {
    let config_json = fs::read_to_string(config_path).ok()?;
    let config = serde_json::from_str::<PodmanContainerConfig>(&config_json).ok()?;

    config.linux?.namespaces.into_iter().find_map(|namespace| {
        (namespace.namespace_type == "network")
            .then_some(namespace.path)
            .flatten()
    })
}

#[cfg(target_os = "linux")]
fn read_process_netns_paths(pid: u32) -> Vec<PathBuf> {
    let fd_dir = PathBuf::from("/proc").join(pid.to_string()).join("fd");
    let entries = match fs::read_dir(&fd_dir) {
        Ok(entries) => entries,
        Err(error) => {
            debug!(
                "failed to read process fd directory for rootless Podman lookup: pid={pid} fd_dir={} error={error}",
                fd_dir.display()
            );
            return Vec::new();
        }
    };

    let mut netns_paths = HashSet::new();
    for entry in entries.flatten() {
        let entry_path = entry.path();
        let target = match fs::read_link(&entry_path) {
            Ok(target) => target,
            Err(error) => {
                debug!(
                    "failed to read process fd symlink for rootless Podman lookup: pid={pid} fd_entry={} error={error}",
                    entry_path.display()
                );
                continue;
            }
        };
        if is_podman_network_namespace_path(&target) {
            netns_paths.insert(target);
        }
    }

    let mut netns_paths: Vec<_> = netns_paths.into_iter().collect();
    netns_paths.sort();
    netns_paths
}

#[cfg(target_os = "linux")]
fn is_podman_network_namespace_path(path: &Path) -> bool {
    path.parent().and_then(Path::file_name) == Some(OsStr::new("netns"))
        && path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("netns-"))
}

#[cfg(target_os = "linux")]
fn match_container_by_netns_paths(
    netns_paths: &[PathBuf],
    containers_by_netns: &HashMap<PathBuf, ContainerInfo>,
) -> Option<ContainerInfo> {
    let mut candidate = None;

    for netns_path in netns_paths {
        let Some(info) = containers_by_netns.get(netns_path) else {
            continue;
        };

        match &candidate {
            None => candidate = Some(info.clone()),
            Some(existing) if existing == info => {}
            Some(_) => return None,
        }
    }

    candidate
}

#[cfg(target_os = "linux")]
const fn is_podman_rootlessport_process(process_name: &str) -> bool {
    process_name.eq_ignore_ascii_case("rootlessport")
}

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
    match handle.recv_timeout(DAEMON_TIMEOUT) {
        Ok(Some(container_map)) => container_map,
        Ok(None) => {
            debug!("container runtime detection returned no data");
            ContainerPortMap::default()
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            debug!(
                "container runtime detection timed out: timeout_secs={}",
                DAEMON_TIMEOUT.as_secs()
            );
            ContainerPortMap::default()
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            debug!("container runtime detection channel disconnected");
            ContainerPortMap::default()
        }
    }
}

#[cfg(unix)]
fn query_daemon() -> Option<ContainerPortMap> {
    // Honour DOCKER_HOST when it specifies a TCP address.
    if let Some(addr) = docker_host_tcp_addr() {
        return fetch_tcp_json(&addr).map(|body| parse_containers_json(&body));
    }

    // Honour DOCKER_HOST when it points at a Unix socket (unix://).
    if let Some(path) = docker_host_unix_path() {
        return fetch_unix_socket_json(Path::new(&path)).map(|body| parse_containers_json(&body));
    }

    // Safety: getuid() is a simple syscall with no preconditions.
    let uid = unsafe { libc::getuid() };
    let responses =
        fetch_all_successes(unix_socket_paths(uid, crate::project::home_dir()), |path| {
            fetch_unix_socket_json(Path::new(&path))
        });

    merge_daemon_responses(responses)
}

#[cfg(windows)]
fn query_daemon() -> Option<ContainerPortMap> {
    let deadline = std::time::Instant::now() + DAEMON_TIMEOUT;

    // Honour DOCKER_HOST when it specifies a TCP address.
    if let Some(addr) = docker_host_tcp_addr() {
        return fetch_tcp_json(&addr).map(|body| parse_containers_json(&body));
    }

    // Honour DOCKER_HOST when it points at a named pipe (npipe://).
    if let Some(path) = docker_host_npipe_path()
        && let Some(body) = fetch_named_pipe_json(&path, deadline)
    {
        return Some(parse_containers_json(&body));
    }

    let pipe_paths = [
        r"\\.\pipe\docker_engine",
        r"\\.\pipe\podman-machine-default",
    ];

    for path in pipe_paths {
        if let Some(body) = fetch_named_pipe_json(path, deadline) {
            return Some(parse_containers_json(&body));
        }
    }

    None
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
        merged.extend(parse_containers_json(response.as_ref()));
    }

    saw_response.then_some(merged)
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

            let port_count = port.port_range.unwrap_or(1);
            for offset in 0..port_count {
                let Some(mapped_port) = public_port.checked_add(offset) else {
                    break;
                };

                map.insert((port.host_ip, mapped_port, proto), info.clone());
            }
        }
    }

    map
}

fn deserialize_host_ip<'de, D>(deserializer: D) -> Result<Option<IpAddr>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    value
        .as_deref()
        .map(str::trim)
        .filter(|ip| !ip.is_empty())
        .map(str::parse)
        .transpose()
        .map_err(serde::de::Error::custom)
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

/// Truncate a full container ID to its 12-character short form.
///
/// Docker/Podman container IDs are hex-encoded (ASCII-only), so byte
/// length equals character count and a byte slice is safe.
#[must_use]
pub fn short_container_id(id: &str) -> String {
    id.get(..12).unwrap_or(id).to_string()
}

#[cfg(unix)]
fn fetch_all_successes<P, I, F>(candidates: I, fetch: F) -> Vec<String>
where
    P: Send + 'static,
    I: IntoIterator<Item = P>,
    F: Fn(P) -> Option<String> + Send + Sync + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    let fetch = std::sync::Arc::new(fetch);

    for candidate in candidates {
        let tx = tx.clone();
        let fetch = std::sync::Arc::clone(&fetch);
        std::thread::spawn(move || {
            if let Some(body) = fetch(candidate) {
                drop(tx.send(body));
            }
        });
    }

    drop(tx);
    let mut responses = Vec::new();
    let deadline = std::time::Instant::now() + DAEMON_TIMEOUT;

    while let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) {
        match rx.recv_timeout(remaining) {
            Ok(body) => responses.push(body),
            Err(
                std::sync::mpsc::RecvTimeoutError::Timeout
                | std::sync::mpsc::RecvTimeoutError::Disconnected,
            ) => {
                break;
            }
        }
    }

    responses
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
            Err(error) => {
                debug!("failed to open container runtime named pipe: pipe={path} error={error}");
                return None;
            }
        };

        return send_http_request_windows(&mut stream, deadline);
    }
}

#[cfg(unix)]
fn unix_socket_paths(uid: u32, home: Option<PathBuf>) -> Vec<String> {
    let mut socket_paths = Vec::new();

    socket_paths.extend([
        "/var/run/docker.sock".to_string(),
        format!("/run/user/{uid}/docker.sock"),
        format!("/run/user/{uid}/podman/podman.sock"),
        "/run/podman/podman.sock".to_string(),
    ]);

    if let Some(home) = home {
        socket_paths.push(home.join(".docker/run/docker.sock").display().to_string());
    }

    socket_paths
}

#[cfg(unix)]
fn fetch_unix_socket_json(path: &Path) -> Option<String> {
    use std::os::unix::net::UnixStream;

    let mut stream = match UnixStream::connect(path) {
        Ok(stream) => stream,
        Err(error) => {
            debug!(
                "failed to connect to container runtime socket: socket={} error={error}",
                path.display()
            );
            return None;
        }
    };
    // Best-effort timeout; proceed even if it cannot be set.
    drop(stream.set_read_timeout(Some(DAEMON_TIMEOUT)));
    drop(stream.set_write_timeout(Some(DAEMON_TIMEOUT)));
    let response = send_http_request(&mut stream);
    if response.is_none() {
        debug!(
            "container runtime socket returned no usable response: socket={}",
            path.display()
        );
    }
    response
}

fn send_http_request(stream: &mut (impl std::io::Read + std::io::Write)) -> Option<String> {
    stream.write_all(CONTAINERS_HTTP_REQUEST).ok()?;

    let mut reader = BufReader::new(stream);

    let headers = read_response_headers(&mut reader)?;
    if !headers.status_ok {
        return None;
    }

    read_response_body(&mut reader, &headers)
}

fn read_response_headers(reader: &mut impl BufRead) -> Option<ParsedHeaders> {
    let mut status_line = String::new();
    reader.read_line(&mut status_line).ok()?;

    let mut header_lines = Vec::new();
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        if line.trim().is_empty() {
            break;
        }
        header_lines.push(line.trim_end_matches(['\r', '\n']).to_string());
    }

    parse_header_metadata(&status_line, header_lines.iter().map(String::as_str), 0)
}

fn read_response_body(reader: &mut impl BufRead, headers: &ParsedHeaders) -> Option<String> {
    match headers.transfer_encoding {
        TransferEncoding::Identity => {
            let mut body = Vec::new();
            if let Some(content_length) = headers.content_length {
                body.resize(content_length, 0);
                reader.read_exact(&mut body).ok()?;
            } else {
                reader.read_to_end(&mut body).ok()?;
            }
            String::from_utf8(body).ok()
        }
        TransferEncoding::Chunked => {
            let decoded = read_chunked_body(reader)?;
            String::from_utf8(decoded).ok()
        }
        TransferEncoding::Unsupported => None,
    }
}

fn read_chunked_body(reader: &mut impl BufRead) -> Option<Vec<u8>> {
    let mut body = Vec::new();

    loop {
        let mut size_line = String::new();
        if reader.read_line(&mut size_line).ok()? == 0 {
            return None;
        }

        let chunk_size = parse_chunk_size_line(&size_line)?;
        if chunk_size == 0 {
            consume_chunked_trailers(reader)?;
            return Some(body);
        }

        let start = body.len();
        body.resize(start + chunk_size, 0);
        reader.read_exact(&mut body[start..]).ok()?;

        let mut chunk_terminator = [0_u8; 2];
        reader.read_exact(&mut chunk_terminator).ok()?;
        if chunk_terminator != *b"\r\n" {
            return None;
        }
    }
}

fn parse_chunk_size_line(line: &str) -> Option<usize> {
    let trimmed = line.trim_end_matches(['\r', '\n']);
    let size = trimmed.split_once(';').map_or(trimmed, |(value, _)| value);
    usize::from_str_radix(size.trim(), 16).ok()
}

fn consume_chunked_trailers(reader: &mut impl BufRead) -> Option<()> {
    loop {
        let mut trailer_line = String::new();
        if reader.read_line(&mut trailer_line).ok()? == 0 {
            return None;
        }
        if trailer_line.trim().is_empty() {
            return Some(());
        }
    }
}

#[cfg(windows)]
fn send_http_request_windows(
    stream: &mut std::fs::File,
    deadline: std::time::Instant,
) -> Option<String> {
    use std::io::{Read as _, Write as _};

    stream.write_all(CONTAINERS_HTTP_REQUEST).ok()?;

    let mut response = Vec::new();
    let mut chunk = [0_u8; 8192];
    let mut headers: Option<ParsedHeaders> = None;

    loop {
        // Once headers are parsed, continue extracting against the
        // buffered body instead of reparsing the header boundary.
        if let Some(ref hdr) = headers {
            match extract_http_body_from_buffer(&response, hdr, false) {
                Ok(Some(body)) => return Some(body),
                Ok(None) => {}
                Err(()) => return None,
            }
        } else if let Some(hdr) = parse_response_headers(&response) {
            if !hdr.status_ok {
                return None;
            }
            match extract_http_body_from_buffer(&response, &hdr, false) {
                Ok(Some(body)) => return Some(body),
                Ok(None) => {}
                Err(()) => return None,
            }
            headers = Some(hdr);
        }

        let available = match peek_available_bytes(stream) {
            Some(available) => available,
            None if last_os_error_is(ERROR_BROKEN_PIPE) => {
                return extract_body_at_eof(&response, headers.as_ref());
            }
            None => return None,
        };

        if available == 0 {
            if std::time::Instant::now() >= deadline {
                return extract_body_at_eof(&response, headers.as_ref());
            }
            std::thread::sleep(PIPE_POLL_INTERVAL);
            continue;
        }

        let max_chunk = u32::try_from(chunk.len()).ok()?;
        let read_len = usize::try_from(available.min(max_chunk)).ok()?;
        match stream.read(&mut chunk[..read_len]) {
            Ok(0) => return extract_body_at_eof(&response, headers.as_ref()),
            Ok(read) => response.extend_from_slice(&chunk[..read]),
            Err(error) if error.raw_os_error() == Some(ERROR_BROKEN_PIPE) => {
                return extract_body_at_eof(&response, headers.as_ref());
            }
            Err(_) => return None,
        }
    }
}

/// Extract a Unix socket path from the `DOCKER_HOST` environment variable.
///
/// Returns the path suffix when `DOCKER_HOST` starts with `unix://`,
/// or `None` if the variable is unset or uses a different scheme.
#[cfg(unix)]
fn docker_host_unix_path() -> Option<String> {
    let docker_host = std::env::var("DOCKER_HOST").ok()?;
    let path = docker_host.strip_prefix("unix://")?;
    (!path.is_empty()).then(|| path.to_string())
}

/// Extract a named pipe path from the `DOCKER_HOST` environment variable.
///
/// Returns the pipe path (with forward slashes replaced by backslashes)
/// when `DOCKER_HOST` starts with `npipe://`, or `None` if the variable
/// is unset or uses a different scheme.
#[cfg(windows)]
fn docker_host_npipe_path() -> Option<String> {
    let docker_host = std::env::var("DOCKER_HOST").ok()?;
    let raw = docker_host.strip_prefix("npipe://")?;
    (!raw.is_empty()).then(|| raw.replace('/', "\\"))
}

/// Extract a TCP address from the `DOCKER_HOST` environment variable.
///
/// Returns the `host:port` string when `DOCKER_HOST` starts with `tcp://`,
/// or `None` if the variable is unset or uses a different scheme.
fn docker_host_tcp_addr() -> Option<String> {
    let docker_host = std::env::var("DOCKER_HOST").ok()?;
    let addr = docker_host.strip_prefix("tcp://")?;
    (!addr.is_empty()).then(|| addr.to_string())
}

/// Connect to a Docker/Podman daemon over plain TCP and fetch container JSON.
///
/// Used when `DOCKER_HOST` is set to `tcp://host:port`.
fn fetch_tcp_json(addr: &str) -> Option<String> {
    let mut stream = connect_tcp_stream(addr)?;
    drop(stream.set_read_timeout(Some(DAEMON_TIMEOUT)));
    drop(stream.set_write_timeout(Some(DAEMON_TIMEOUT)));
    let response = send_http_request(&mut stream);
    if response.is_none() {
        debug!("container runtime TCP endpoint returned no usable response: tcp={addr}");
    }
    response
}

fn connect_tcp_stream(addr: &str) -> Option<std::net::TcpStream> {
    use std::net::ToSocketAddrs;

    let socket_addrs = match addr.to_socket_addrs() {
        Ok(socket_addrs) => socket_addrs,
        Err(error) => {
            debug!("failed to resolve container runtime TCP address: tcp={addr} error={error}");
            return None;
        }
    };

    for socket_addr in socket_addrs {
        match std::net::TcpStream::connect_timeout(&socket_addr, DAEMON_TIMEOUT) {
            Ok(stream) => return Some(stream),
            Err(error) => {
                debug!(
                    "failed to connect to container runtime TCP address: socket_addr={socket_addr} error={error}"
                );
            }
        }
    }

    None
}

/// Pre-parsed HTTP response header metadata from a Docker daemon reply.
struct ParsedHeaders {
    /// Whether the HTTP status code is 2xx.
    status_ok: bool,
    /// Byte offset where the response body begins (after `\r\n\r\n`).
    #[cfg(any(windows, test))]
    body_offset: usize,
    /// Value of the `Content-Length` header, if present.
    content_length: Option<usize>,
    /// Transfer framing used for the response body.
    transfer_encoding: TransferEncoding,
}

fn parse_header_metadata<'a>(
    status_line: &str,
    header_lines: impl Iterator<Item = &'a str>,
    body_offset: usize,
) -> Option<ParsedHeaders> {
    #[cfg(not(any(windows, test)))]
    let _ = body_offset;

    let status_code: u16 = status_line.split_whitespace().nth(1)?.parse().ok()?;
    let status_ok = (200..300).contains(&status_code);

    let mut content_length = None;
    let mut transfer_encoding = TransferEncoding::Identity;

    for line in header_lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };

        if name.eq_ignore_ascii_case("Content-Length") {
            content_length = value.trim().parse().ok();
            continue;
        }

        if name.eq_ignore_ascii_case("Transfer-Encoding") {
            transfer_encoding = parse_transfer_encoding(value);
        }
    }

    Some(ParsedHeaders {
        status_ok,
        #[cfg(any(windows, test))]
        body_offset,
        content_length,
        transfer_encoding,
    })
}

fn parse_transfer_encoding(value: &str) -> TransferEncoding {
    let mut saw_chunked = false;
    let mut saw_unsupported = false;

    for coding in value
        .split(',')
        .map(str::trim)
        .filter(|coding| !coding.is_empty())
    {
        if coding.eq_ignore_ascii_case("chunked") {
            saw_chunked = true;
        } else if !coding.eq_ignore_ascii_case("identity") {
            saw_unsupported = true;
        }
    }

    if saw_unsupported {
        TransferEncoding::Unsupported
    } else if saw_chunked {
        TransferEncoding::Chunked
    } else {
        TransferEncoding::Identity
    }
}

/// Try to locate and parse the HTTP response headers in `response`.
///
/// Returns `None` if the header/body boundary (`\r\n\r\n`) has not yet
/// been received. Performs a single scan over the accumulated buffer.
#[cfg(any(windows, test))]
fn parse_response_headers(response: &[u8]) -> Option<ParsedHeaders> {
    let text = std::str::from_utf8(response).ok()?;
    let headers_end = text.find("\r\n\r\n")?;
    let body_offset = headers_end + 4;

    let mut header_lines = text[..headers_end].split("\r\n");
    let status_line = header_lines.next()?;
    parse_header_metadata(status_line, header_lines, body_offset)
}

/// Extract the body from a fully received (EOF) response, using
/// pre-parsed headers if available, or falling back to a full parse.
#[cfg(any(windows, test))]
fn extract_body_at_eof(response: &[u8], headers: Option<&ParsedHeaders>) -> Option<String> {
    if let Some(hdr) = headers {
        return extract_http_body_from_buffer(response, hdr, true)
            .ok()
            .flatten();
    }
    // Headers not yet parsed at EOF: fall back to a full single-pass parse.
    try_extract_http_body(response, true)
}

#[cfg(any(windows, test))]
fn try_extract_http_body(response: &[u8], eof: bool) -> Option<String> {
    let hdr = parse_response_headers(response)?;
    extract_http_body_from_buffer(response, &hdr, eof)
        .ok()
        .flatten()
}

#[cfg(any(windows, test))]
fn extract_http_body_from_buffer(
    response: &[u8],
    headers: &ParsedHeaders,
    eof: bool,
) -> Result<Option<String>, ()> {
    if !headers.status_ok {
        return Err(());
    }

    let body = response.get(headers.body_offset..).ok_or(())?;
    match headers.transfer_encoding {
        TransferEncoding::Identity => {
            if let Some(content_length) = headers.content_length {
                if body.len() < content_length {
                    return Ok(None);
                }
                return String::from_utf8(body[..content_length].to_vec())
                    .map(Some)
                    .map_err(|_| ());
            }

            if eof {
                return String::from_utf8(body.to_vec()).map(Some).map_err(|_| ());
            }

            Ok(None)
        }
        TransferEncoding::Chunked => match decode_chunked_body(body) {
            Ok(Some(decoded)) => String::from_utf8(decoded).map(Some).map_err(|_| ()),
            Ok(None) => Ok(None),
            Err(()) => Err(()),
        },
        TransferEncoding::Unsupported => Err(()),
    }
}

#[cfg(any(windows, test))]
fn decode_chunked_body(body: &[u8]) -> Result<Option<Vec<u8>>, ()> {
    let mut decoded = Vec::new();
    let mut offset = 0;

    loop {
        let Some(line_end) = find_crlf(body, offset) else {
            return Ok(None);
        };

        let size_line = std::str::from_utf8(&body[offset..line_end]).map_err(|_| ())?;
        let chunk_size = parse_chunk_size_line(size_line).ok_or(())?;
        offset = line_end + 2;

        if chunk_size == 0 {
            return parse_chunked_trailers(body, offset)
                .map(|complete| complete.then_some(decoded));
        }

        let chunk_end = offset.checked_add(chunk_size).ok_or(())?;
        let terminator_end = chunk_end.checked_add(2).ok_or(())?;
        if body.len() < terminator_end {
            return Ok(None);
        }
        if &body[chunk_end..terminator_end] != b"\r\n" {
            return Err(());
        }

        decoded.extend_from_slice(&body[offset..chunk_end]);
        offset = terminator_end;
    }
}

#[cfg(any(windows, test))]
fn parse_chunked_trailers(body: &[u8], offset: usize) -> Result<bool, ()> {
    let trailers = body.get(offset..).ok_or(())?;
    if trailers.starts_with(b"\r\n") {
        return Ok(true);
    }

    if trailers.windows(4).any(|window| window == b"\r\n\r\n") {
        return Ok(true);
    }

    Ok(false)
}

#[cfg(any(windows, test))]
fn find_crlf(body: &[u8], offset: usize) -> Option<usize> {
    body.get(offset..)?
        .windows(2)
        .position(|window| window == b"\r\n")
        .map(|position| offset + position)
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
    #[cfg(target_os = "linux")]
    use tempfile::TempDir;

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
    fn parse_podman_style_ports_with_empty_host_ip() {
        let json = r#"[{
            "Names": ["ensurily-postgres-dev"],
            "Image": "docker.io/library/postgres:14-alpine",
            "Ports": [{"host_ip": "", "container_port": 5432, "host_port": 5432, "range": 1, "protocol": "tcp"}]
        }]"#;
        let map = parse_containers_json(json);

        let info = map.get(&(None, 5432, Protocol::Tcp)).unwrap();
        assert_eq!(info.name, "ensurily-postgres-dev");
        assert_eq!(info.image, "docker.io/library/postgres:14-alpine");
    }

    #[test]
    fn parse_podman_style_ports_expand_ranges() {
        let json = r#"[{
            "Names": ["ensurily-localstack-dev"],
            "Image": "docker.io/localstack/localstack:latest",
            "Ports": [{"host_ip": "", "container_port": 4510, "host_port": 4510, "range": 3, "protocol": "tcp"}]
        }]"#;
        let map = parse_containers_json(json);

        assert!(map.contains_key(&(None, 4510, Protocol::Tcp)));
        assert!(map.contains_key(&(None, 4511, Protocol::Tcp)));
        assert!(map.contains_key(&(None, 4512, Protocol::Tcp)));
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

    #[test]
    fn http_body_parser_decodes_chunked_payloads() {
        let response = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n[]\r\n0\r\n\r\n";
        assert_eq!(
            try_extract_http_body(response, false).as_deref(),
            Some("[]")
        );
    }

    #[test]
    fn http_body_parser_waits_for_complete_chunked_payload() {
        let partial = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n[]\r\n0\r\n";
        assert!(try_extract_http_body(partial, false).is_none());
    }

    #[test]
    fn http_body_parser_rejects_unsupported_transfer_encoding() {
        let response = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip, chunked\r\n\r\n";
        assert!(try_extract_http_body(response, false).is_none());
    }

    #[test]
    fn parse_response_headers_returns_none_for_incomplete_headers() {
        let partial = b"HTTP/1.0 200 OK\r\nContent-Len";
        assert!(
            parse_response_headers(partial).is_none(),
            "incomplete headers should return None"
        );
    }

    #[test]
    fn parse_response_headers_extracts_content_length_and_offset() {
        let response = b"HTTP/1.0 200 OK\r\nContent-Length: 42\r\n\r\nbody";
        let hdr = parse_response_headers(response).expect("headers should parse");
        assert!(hdr.status_ok, "status should be ok");
        assert_eq!(hdr.content_length, Some(42));
        assert_eq!(hdr.transfer_encoding, TransferEncoding::Identity);
        assert_eq!(hdr.body_offset, 39, "body should start after CRLFCRLF");
    }

    #[test]
    fn parse_response_headers_detects_chunked_transfer_encoding() {
        let response = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n";
        let hdr = parse_response_headers(response).expect("headers should parse");
        assert_eq!(hdr.transfer_encoding, TransferEncoding::Chunked);
    }

    #[test]
    fn parse_response_headers_detects_non_2xx_status() {
        let response = b"HTTP/1.0 404 Not Found\r\n\r\n";
        let hdr = parse_response_headers(response).expect("headers should parse");
        assert!(!hdr.status_ok, "404 should not be marked as ok");
    }

    #[test]
    fn extract_body_at_eof_returns_body_without_content_length() {
        let response = b"HTTP/1.0 200 OK\r\nServer: docker\r\n\r\n[1,2]";
        let hdr = parse_response_headers(response).unwrap();
        let body = extract_body_at_eof(response, Some(&hdr));
        assert_eq!(body.as_deref(), Some("[1,2]"));
    }

    #[test]
    fn extract_body_at_eof_falls_back_when_no_headers_parsed() {
        let response = b"HTTP/1.0 200 OK\r\n\r\nhello";
        let body = extract_body_at_eof(response, None);
        assert_eq!(body.as_deref(), Some("hello"));
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

    #[cfg(unix)]
    #[test]
    fn fetch_all_successes_collects_multiple_responses() {
        let mut responses = fetch_all_successes([1_u8, 2, 3], |candidate| {
            (candidate != 2).then(|| candidate.to_string())
        });
        responses.sort();

        assert_eq!(responses, vec!["1".to_string(), "3".to_string()]);
    }

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

    #[cfg(target_os = "linux")]
    #[test]
    fn load_podman_rootless_containers_from_overlay_root_reads_metadata() {
        let overlay_root = TempDir::new().unwrap();
        let container_id = "e603f8ebd438b8405b9b835b9d38cb913ea2479f5b29f8e4308b88e9a92e8c4b";
        let netns_path = "/run/user/1000/netns/netns-demo";

        fs::create_dir_all(overlay_root.path().join(container_id).join("userdata")).unwrap();
        fs::write(
            overlay_root.path().join("containers.json"),
            format!(
                r#"[{{
                    "id": "{container_id}",
                    "names": ["ensurily-postgres-dev"],
                    "metadata": "{{\"image-name\":\"docker.io/library/postgres:14-alpine\",\"name\":\"ensurily-postgres-dev\"}}"
                }}]"#
            ),
        )
        .unwrap();
        fs::write(
            overlay_root
                .path()
                .join(container_id)
                .join("userdata/config.json"),
            format!(r#"{{"linux":{{"namespaces":[{{"type":"network","path":"{netns_path}"}}]}}}}"#),
        )
        .unwrap();

        let containers = load_podman_rootless_containers_from_overlay_root(overlay_root.path());
        let container = containers.get(Path::new(netns_path)).unwrap();

        assert_eq!(container.name, "ensurily-postgres-dev");
        assert_eq!(container.image, "docker.io/library/postgres:14-alpine");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn match_container_by_netns_paths_returns_unique_match() {
        let netns_path = PathBuf::from("/run/user/1000/netns/netns-demo");
        let mut containers = HashMap::new();
        containers.insert(
            netns_path.clone(),
            ContainerInfo {
                name: "ensurily-redis-dev".to_string(),
                image: "docker.io/library/redis:7.2-alpine".to_string(),
            },
        );

        let container = match_container_by_netns_paths(&[netns_path], &containers).unwrap();
        assert_eq!(container.name, "ensurily-redis-dev");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn match_container_by_netns_paths_rejects_conflicting_matches() {
        let first_path = PathBuf::from("/run/user/1000/netns/netns-a");
        let second_path = PathBuf::from("/run/user/1000/netns/netns-b");
        let mut containers = HashMap::new();
        containers.insert(
            first_path.clone(),
            ContainerInfo {
                name: "postgres".to_string(),
                image: "postgres:16".to_string(),
            },
        );
        containers.insert(
            second_path.clone(),
            ContainerInfo {
                name: "redis".to_string(),
                image: "redis:7-alpine".to_string(),
            },
        );

        let container = match_container_by_netns_paths(&[first_path, second_path], &containers);
        assert!(
            container.is_none(),
            "multiple distinct netns matches should not guess a container"
        );
    }
}
