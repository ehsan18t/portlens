//! # Docker/Podman container detection
//!
//! Connects to the Docker or Podman socket and queries running containers
//! to map published ports to container names and images.
//!
//! Uses raw HTTP/1.0 over Unix socket (Linux) or named pipe (Windows)
//! with zero additional dependencies.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};

use crate::types::Protocol;

/// Metadata about a running container that has published ports.
#[derive(Debug, Clone)]
pub struct ContainerInfo {
    /// Container name (e.g. "backend-postgres-1").
    pub name: String,
    /// Container image (e.g. "postgres:16").
    pub image: String,
}

/// Maps `(host_port, protocol)` to container info.
pub type ContainerPortMap = HashMap<(u16, Protocol), ContainerInfo>;

/// Maximum time to wait for the Docker/Podman daemon to respond.
///
/// On Unix the socket itself has a per-read timeout, but on Windows the
/// named pipe has no built-in timeout support. A thread-level timeout
/// covers both platforms uniformly.
const DAEMON_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Detect running Docker/Podman containers and their published ports.
///
/// Returns an empty map if the Docker/Podman daemon is unavailable or
/// does not respond within [`DAEMON_TIMEOUT`].
/// Never returns an error - this is best-effort enrichment.
#[must_use]
pub fn detect_containers() -> ContainerPortMap {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // Ignore send error: receiver may have timed out and been dropped.
        drop(tx.send(query_daemon()));
    });
    rx.recv_timeout(DAEMON_TIMEOUT)
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
/// `(public_port, protocol)` tuples.
pub fn parse_containers_json(json_body: &str) -> ContainerPortMap {
    let mut map = ContainerPortMap::new();

    let Ok(containers) = serde_json::from_str::<serde_json::Value>(json_body) else {
        return map;
    };

    let Some(containers) = containers.as_array() else {
        return map;
    };

    for container in containers {
        let name = container["Names"]
            .as_array()
            .and_then(|names| names.first())
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim_start_matches('/')
            .to_string();

        let image = container["Image"].as_str().unwrap_or("").to_string();

        if name.is_empty() {
            continue;
        }

        let Some(ports) = container["Ports"].as_array() else {
            continue;
        };

        for port in ports {
            let Some(public_port) = port["PublicPort"].as_u64() else {
                continue;
            };
            let Ok(public_port) = u16::try_from(public_port) else {
                continue;
            };
            let proto = match port["Type"].as_str().unwrap_or("tcp") {
                "udp" => Protocol::Udp,
                _ => Protocol::Tcp,
            };

            map.insert(
                (public_port, proto),
                ContainerInfo {
                    name: name.clone(),
                    image: image.clone(),
                },
            );
        }
    }

    map
}

#[cfg(unix)]
fn fetch_containers_json() -> Option<String> {
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    // Safety: getuid() is a simple syscall with no preconditions.
    let uid = unsafe { libc::getuid() };

    let socket_paths = [
        "/var/run/docker.sock".to_string(),
        format!("/run/user/{uid}/podman/podman.sock"),
        "/run/podman/podman.sock".to_string(),
    ];

    for path in &socket_paths {
        if let Ok(mut stream) = UnixStream::connect(path) {
            // Best-effort timeout; proceed even if it cannot be set.
            drop(stream.set_read_timeout(Some(Duration::from_secs(3))));
            return send_http_request(&mut stream);
        }
    }
    None
}

#[cfg(windows)]
fn fetch_containers_json() -> Option<String> {
    use std::fs::OpenOptions;

    let mut stream = OpenOptions::new()
        .read(true)
        .write(true)
        .open(r"\\.\pipe\docker_engine")
        .ok()?;
    send_http_request(&mut stream)
}

fn send_http_request(stream: &mut (impl std::io::Read + std::io::Write)) -> Option<String> {
    use std::io::Read as _;

    stream
        .write_all(b"GET /v1.45/containers/json HTTP/1.0\r\nHost: localhost\r\n\r\n")
        .ok()?;

    let mut reader = BufReader::new(stream);

    // Skip HTTP response headers (read until empty line)
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

#[cfg(test)]
mod tests {
    use super::*;

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

        let pg = map.get(&(5432, Protocol::Tcp)).unwrap();
        assert_eq!(pg.name, "backend-postgres-1");
        assert_eq!(pg.image, "postgres:16");

        let redis = map.get(&(6379, Protocol::Tcp)).unwrap();
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
        let info = map.get(&(80, Protocol::Tcp)).unwrap();
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
        assert!(map.contains_key(&(8080, Protocol::Tcp)));
        assert!(map.contains_key(&(8443, Protocol::Tcp)));
    }

    #[test]
    fn parse_container_with_empty_name() {
        let json = r#"[{
            "Names": [],
            "Image": "app:latest",
            "Ports": [{"PrivatePort": 80, "PublicPort": 80, "Type": "tcp"}]
        }]"#;
        let map = parse_containers_json(json);
        assert!(
            map.is_empty(),
            "container with empty Names should be skipped"
        );
    }
}
