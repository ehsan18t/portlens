# Enriched Port Display Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add project detection, app/framework identification, Docker/Podman container awareness, uptime tracking, smart default filtering, and display mode options to PortLens.

**Architecture:** Three new modules (docker, project, framework) feed enrichment data into the existing collector. The collector builds enriched `PortEntry` structs, the filter applies a new relevance filter (default on), and the display renders bordered/compact tables with column selection. Docker detection uses raw HTTP over Unix socket / named pipe with zero new dependencies.

**Tech Stack:** Rust, sysinfo 0.38 (cwd/cmd/start_time), listeners 0.5, comfy-table 7, serde/serde_json, clap 4. No new crate dependencies.

**Spec:** `docs/superpowers/specs/2026-04-09-enriched-port-display-design.md`

---

## File Structure

| Action | Path                    | Responsibility                                              |
| ------ | ----------------------- | ----------------------------------------------------------- |
| Modify | `src/types.rs`          | Add `project`, `app`, `uptime_secs` fields to `PortEntry`   |
| Create | `src/docker.rs`         | Docker/Podman socket API, container-to-port mapping         |
| Create | `src/project.rs`        | Project root detection via marker file walk + cmd fallback  |
| Create | `src/framework.rs`      | App detection: Docker image, config files, process name     |
| Modify | `src/collector.rs`      | Integrate enrichment, expand ProcessRefreshKind             |
| Modify | `src/filter.rs`         | Add relevance filter with `show_all` bypass                 |
| Modify | `src/display.rs`        | Bordered/compact table, column selection, uptime formatting |
| Modify | `src/main.rs`           | Add `--all`, `--full`, `--compact` CLI flags                |
| Modify | `src/lib.rs`            | Export new modules                                          |
| Modify | `benches/benchmarks.rs` | Update PortEntry construction with new fields               |

---

### Task 1: Extend PortEntry with new fields

**Files:**
- Modify: `src/types.rs`

- [ ] **Step 1: Add new fields to PortEntry**

Add three new `Option` fields after the existing ones. Using `Option` because enrichment is best-effort.

```rust
// In src/types.rs, add to PortEntry struct after `user`:

    /// Project folder name or Docker container name, or `None` if unknown.
    pub project: Option<String>,
    /// Detected app/framework label (e.g. "Next.js", "PostgreSQL"), or `None`.
    pub app: Option<String>,
    /// Process uptime in seconds, or `None` if unavailable.
    pub uptime_secs: Option<u64>,
```

- [ ] **Step 2: Add uptime format helper to types.rs**

```rust
/// Format an uptime duration in seconds into a human-readable string.
///
/// Returns `None` if the input is `None`.
#[must_use]
pub fn format_uptime(secs: Option<u64>) -> String {
    let Some(s) = secs else {
        return "-".to_string();
    };
    let minutes = s / 60;
    let hours = minutes / 60;
    let days = hours / 24;

    if days > 0 {
        format!("{}d {}h", days, hours % 24)
    } else if hours > 0 {
        format!("{}h {}m", hours, minutes % 60)
    } else if minutes > 0 {
        format!("{}m", minutes)
    } else {
        "< 1m".to_string()
    }
}
```

- [ ] **Step 3: Add tests for format_uptime and new field serialization**

```rust
#[test]
fn format_uptime_none() {
    assert_eq!(format_uptime(None), "-");
}

#[test]
fn format_uptime_seconds() {
    assert_eq!(format_uptime(Some(30)), "< 1m");
}

#[test]
fn format_uptime_minutes() {
    assert_eq!(format_uptime(Some(300)), "5m");
}

#[test]
fn format_uptime_hours_minutes() {
    assert_eq!(format_uptime(Some(7200 + 2400)), "2h 40m");
}

#[test]
fn format_uptime_days_hours() {
    assert_eq!(format_uptime(Some(86400 + 32400)), "1d 9h");
}
```

- [ ] **Step 4: Fix all existing code that constructs PortEntry**

Update every place that creates a `PortEntry` to include the new fields with `None` defaults. This includes:

- `src/collector.rs` `build_entry()` - add `project: None, app: None, uptime_secs: None`
- `src/filter.rs` `make_entry()` test helper - add `project: None, app: None, uptime_secs: None`
- `benches/benchmarks.rs` - add `project: None, app: None, uptime_secs: None`

- [ ] **Step 5: Run tests, verify they pass**

Run: `cargo test`
Expected: All 17 existing tests pass + 5 new uptime tests = 22 tests

- [ ] **Step 6: Commit**

```
git add src/types.rs src/collector.rs src/filter.rs benches/benchmarks.rs
git commit -m "feat(types): add project, app, and uptime fields to PortEntry"
```

---

### Task 2: Create docker.rs - Docker/Podman container detection

**Files:**
- Create: `src/docker.rs`

- [ ] **Step 1: Define ContainerInfo struct and parse function**

```rust
//! # Docker/Podman container detection
//!
//! Connects to the Docker or Podman socket and queries running containers
//! to map published ports to container names and images.

use std::collections::HashMap;

/// Metadata about a running container that has published ports.
#[derive(Debug, Clone)]
pub struct ContainerInfo {
    /// Container name (e.g. "backend-postgres-1").
    pub name: String,
    /// Container image (e.g. "postgres:16").
    pub image: String,
}

/// A port published by a container, keyed by the host port number.
/// Maps `(host_port, protocol)` to container info.
pub type ContainerPortMap = HashMap<(u16, String), ContainerInfo>;

/// Parse the JSON response from `GET /containers/json` into a port map.
///
/// Each container may publish multiple ports. The map keys are
/// `(public_port, protocol)` tuples.
pub fn parse_containers_json(json_body: &str) -> ContainerPortMap {
    // Implementation: parse with serde_json::Value, extract Names/Image/Ports
}
```

- [ ] **Step 2: Write tests for parse_containers_json**

```rust
#[cfg(test)]
mod tests {
    use super::*;

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

        let pg = map.get(&(5432, "tcp".to_string())).unwrap();
        assert_eq!(pg.name, "backend-postgres-1");
        assert_eq!(pg.image, "postgres:16");

        let redis = map.get(&(6379, "tcp".to_string())).unwrap();
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
        assert!(map.is_empty(), "entries without PublicPort should be skipped");
    }

    #[test]
    fn container_name_strips_leading_slash() {
        let json = r#"[{
            "Names": ["/my-container"],
            "Image": "nginx:latest",
            "Ports": [{"PrivatePort": 80, "PublicPort": 80, "Type": "tcp"}]
        }]"#;
        let map = parse_containers_json(json);
        let info = map.get(&(80, "tcp".to_string())).unwrap();
        assert_eq!(info.name, "my-container");
    }
}
```

- [ ] **Step 3: Implement parse_containers_json**

```rust
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

        let image = container["Image"]
            .as_str()
            .unwrap_or("")
            .to_string();

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
            let proto = port["Type"]
                .as_str()
                .unwrap_or("tcp")
                .to_string();

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
```

- [ ] **Step 4: Run tests**

Run: `cargo test docker`
Expected: All 5 docker tests pass

- [ ] **Step 5: Add socket connection and HTTP query function**

This is the platform-specific part. Uses raw HTTP/1.0 over Unix socket (Linux) or named pipe (Windows).

```rust
use std::io::{BufRead, BufReader, Read as _, Write as _};
use std::time::Duration;

/// Socket paths to try, in priority order.
const DOCKER_SOCKET_LINUX: &str = "/var/run/docker.sock";
const DOCKER_PIPE_WINDOWS: &str = r"\\.\pipe\docker_engine";

/// Detect running Docker/Podman containers and their published ports.
///
/// Returns an empty map if the Docker/Podman daemon is unavailable.
/// Never returns an error - this is best-effort enrichment.
pub fn detect_containers() -> ContainerPortMap {
    query_daemon().unwrap_or_default()
}

fn query_daemon() -> Option<ContainerPortMap> {
    let body = fetch_containers_json()?;
    Some(parse_containers_json(&body))
}
```

Then add platform-specific `fetch_containers_json()`:

```rust
#[cfg(unix)]
fn fetch_containers_json() -> Option<String> {
    use std::os::unix::net::UnixStream;

    let uid = unsafe { libc::getuid() };

    let socket_paths = [
        DOCKER_SOCKET_LINUX.to_string(),
        format!("/run/user/{uid}/podman/podman.sock"),
        "/run/podman/podman.sock".to_string(),
    ];

    for path in &socket_paths {
        if let Ok(mut stream) = UnixStream::connect(path) {
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .ok()?;
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
        .open(DOCKER_PIPE_WINDOWS)
        .ok()?;
    send_http_request(&mut stream)
}

fn send_http_request(stream: &mut impl (Read + Write)) -> Option<String>
where
    // Note: uses std::io::{Read, Write} imported at top
{
    let request = "GET /v1.45/containers/json HTTP/1.0\r\nHost: localhost\r\n\r\n";
    stream.write_all(request.as_bytes()).ok()?;

    let mut reader = BufReader::new(stream);

    // Skip HTTP headers (read until empty line)
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

    // Read the body
    let mut body = String::new();
    reader.read_to_string(&mut body).ok()?;
    Some(body)
}
```

Note: On Unix we need the `libc` crate for `getuid()`. Add to Cargo.toml:

```toml
[target.'cfg(unix)'.dependencies]
libc = "0.2"
```

- [ ] **Step 6: Run full test suite + clippy**

Run: `cargo test && cargo clippy --all-targets`
Expected: All tests pass, no clippy warnings

- [ ] **Step 7: Commit**

```
git add src/docker.rs Cargo.toml Cargo.lock
git commit -m "feat(docker): add Docker/Podman container detection via socket API"
```

---

### Task 3: Create project.rs - Project root detection

**Files:**
- Create: `src/project.rs`

- [ ] **Step 1: Define the module and marker list**

```rust
//! # Project detection
//!
//! Walks upward from a process working directory looking for project
//! marker files to determine the project root and name.

use std::path::{Path, PathBuf};

/// Files whose presence indicates a project root directory.
const PROJECT_MARKERS: &[&str] = &[
    "package.json",
    "Cargo.toml",
    "go.mod",
    "pyproject.toml",
    "requirements.txt",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "composer.json",
    "Gemfile",
    "mix.exs",
    "deno.json",
    "bun.lockb",
];

/// File extensions whose presence indicates a project root directory.
const PROJECT_MARKER_EXTENSIONS: &[&str] = &["csproj", "fsproj"];
```

- [ ] **Step 2: Implement detect function**

```rust
/// Detect the project name for a process.
///
/// Tries the working directory first, then falls back to parsing
/// command-line arguments for file paths.
///
/// Returns `None` if no project root can be determined.
pub fn detect(cwd: Option<&Path>, cmd: &[impl AsRef<str>]) -> Option<String> {
    if let Some(cwd) = cwd {
        if let Some(name) = find_project_root(cwd) {
            return Some(name);
        }
    }

    // Fallback: look for paths in command-line arguments
    for arg in cmd {
        let path = Path::new(arg.as_ref());
        if path.is_absolute() {
            if let Some(parent) = path.parent() {
                if let Some(name) = find_project_root(parent) {
                    return Some(name);
                }
            }
        }
    }

    None
}

/// Walk upward from `start` looking for project marker files.
///
/// Returns the folder name of the directory containing the first
/// marker found, or `None` if no marker is found before reaching
/// the filesystem root.
fn find_project_root(start: &Path) -> Option<String> {
    let mut current = start.to_path_buf();

    loop {
        if has_marker(&current) {
            return current
                .file_name()
                .map(|n| n.to_string_lossy().into_owned());
        }
        if !current.pop() {
            return None;
        }
    }
}

fn has_marker(dir: &Path) -> bool {
    for marker in PROJECT_MARKERS {
        if dir.join(marker).exists() {
            return true;
        }
    }

    // Check extension-based markers (*.csproj, *.fsproj)
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.filter_map(Result::ok) {
            if let Some(ext) = entry.path().extension() {
                let ext_str = ext.to_string_lossy();
                if PROJECT_MARKER_EXTENSIONS.iter().any(|m| *m == ext_str.as_ref()) {
                    return true;
                }
            }
        }
    }

    false
}
```

- [ ] **Step 3: Write tests using temp directories**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup_project(marker: &str) -> TempDir {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(marker), "").unwrap();
        dir
    }

    #[test]
    fn detect_node_project() {
        let dir = setup_project("package.json");
        let result = detect(Some(dir.path()), &Vec::<String>::new());
        assert!(result.is_some());
    }

    #[test]
    fn detect_rust_project() {
        let dir = setup_project("Cargo.toml");
        let result = detect(Some(dir.path()), &Vec::<String>::new());
        assert!(result.is_some());
    }

    #[test]
    fn detect_go_project() {
        let dir = setup_project("go.mod");
        let result = detect(Some(dir.path()), &Vec::<String>::new());
        assert!(result.is_some());
    }

    #[test]
    fn detect_python_project() {
        let dir = setup_project("pyproject.toml");
        let result = detect(Some(dir.path()), &Vec::<String>::new());
        assert!(result.is_some());
    }

    #[test]
    fn detect_walks_upward() {
        let dir = setup_project("package.json");
        let sub = dir.path().join("src").join("deep");
        fs::create_dir_all(&sub).unwrap();
        let result = detect(Some(&sub), &Vec::<String>::new());
        assert!(result.is_some());
        // Should find the parent with package.json
        assert_eq!(
            result.unwrap(),
            dir.path().file_name().unwrap().to_string_lossy()
        );
    }

    #[test]
    fn detect_no_marker_returns_none() {
        let dir = TempDir::new().unwrap();
        let result = detect(Some(dir.path()), &Vec::<String>::new());
        assert!(result.is_none());
    }

    #[test]
    fn detect_none_cwd_returns_none() {
        let result = detect(None, &Vec::<String>::new());
        assert!(result.is_none());
    }

    #[test]
    fn detect_csproj_extension_marker() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("MyApp.csproj"), "").unwrap();
        let result = detect(Some(dir.path()), &Vec::<String>::new());
        assert!(result.is_some());
    }

    #[test]
    fn detect_fallback_to_cmd_args() {
        let dir = setup_project("Cargo.toml");
        let fake_path = dir.path().join("src").join("main.rs");
        fs::create_dir_all(fake_path.parent().unwrap()).unwrap();
        fs::write(&fake_path, "").unwrap();
        let cmd = vec![fake_path.to_string_lossy().into_owned()];
        let result = detect(None, &cmd);
        assert!(result.is_some());
    }
}
```

Note: Add `tempfile` as a dev-dependency in Cargo.toml:

```toml
[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 4: Run tests**

Run: `cargo test project`
Expected: All 9 project tests pass

- [ ] **Step 5: Commit**

```
git add src/project.rs Cargo.toml Cargo.lock
git commit -m "feat(project): add project root detection via marker file walk"
```

---

### Task 4: Create framework.rs - App/framework detection

**Files:**
- Create: `src/framework.rs`

- [ ] **Step 1: Define the module with all detection tables**

```rust
//! # Framework/app detection
//!
//! Identifies the technology behind a port using three strategies:
//! Docker image name, config file detection, and process name matching.

use std::path::Path;

use crate::docker::ContainerInfo;
```

- [ ] **Step 2: Implement Docker image detection**

```rust
/// Detect app label from a Docker container's image name.
///
/// Matches the base image name (before the colon tag) against known patterns.
pub fn detect_from_image(info: &ContainerInfo) -> Option<&'static str> {
    let image = info.image.split('/').last().unwrap_or(&info.image);
    let base = image.split(':').next().unwrap_or(image);

    match base {
        s if s.starts_with("postgres") => Some("PostgreSQL"),
        s if s.starts_with("mysql") => Some("MySQL"),
        s if s.starts_with("mariadb") => Some("MariaDB"),
        s if s.starts_with("mongo") => Some("MongoDB"),
        s if s.starts_with("redis") => Some("Redis"),
        s if s.starts_with("memcached") => Some("Memcached"),
        s if s.starts_with("nginx") => Some("Nginx"),
        s if s == "httpd" || s.starts_with("apache") => Some("Apache"),
        s if s.starts_with("rabbitmq") => Some("RabbitMQ"),
        s if s.starts_with("localstack") => Some("LocalStack"),
        s if s.starts_with("elasticsearch") => Some("Elasticsearch"),
        s if s.starts_with("opensearch") => Some("OpenSearch"),
        s if s.starts_with("clickhouse") => Some("ClickHouse"),
        s if s.starts_with("caddy") => Some("Caddy"),
        s if s.starts_with("traefik") => Some("Traefik"),
        s if s.starts_with("node") => Some("Node.js"),
        s if s.starts_with("python") => Some("Python"),
        s if s.starts_with("ruby") => Some("Ruby"),
        s if s.starts_with("golang") || s == "go" => Some("Go"),
        s if s.starts_with("rust") => Some("Rust"),
        s if s.starts_with("openjdk") || s.starts_with("eclipse-temurin") => Some("Java"),
        s if s.starts_with("mcr.microsoft.com/dotnet") // handled by split('/') above
            || s.starts_with("dotnet") => Some(".NET"),
        _ => None,
    }
}
```

- [ ] **Step 3: Implement config file detection**

```rust
/// Config file patterns checked inside a project root directory.
/// More specific patterns (e.g. next.config) come before generic ones.
const CONFIG_PATTERNS: &[(&str, &str)] = &[
    ("next.config", "Next.js"),
    ("nuxt.config", "Nuxt"),
    ("angular.json", "Angular"),
    ("svelte.config", "SvelteKit"),
    ("astro.config", "Astro"),
    ("vite.config", "Vite"),
    ("remix.config", "Remix"),
    ("gatsby-config", "Gatsby"),
    ("vue.config", "Vue CLI"),
    ("webpack.config", "Webpack"),
    ("manage.py", "Django"),
    ("wsgi.py", "Flask"),
    ("app.py", "Flask"),
    ("Cargo.toml", "Rust"),
    ("go.mod", "Go"),
    ("pom.xml", "Java (Maven)"),
    ("build.gradle.kts", "Kotlin (Gradle)"),
    ("build.gradle", "Java (Gradle)"),
    ("composer.json", "PHP"),
    ("config.ru", "Ruby (Rack)"),
    ("mix.exs", "Elixir"),
    ("deno.json", "Deno"),
    ("pyproject.toml", "Python"),
];

/// Extension-based config patterns.
const CONFIG_EXTENSIONS: &[(&str, &str)] = &[
    ("csproj", ".NET"),
    ("fsproj", ".NET (F#)"),
];

/// Detect app label by scanning config files in a project root.
pub fn detect_from_config(project_root: &Path) -> Option<&'static str> {
    // Check prefix-based patterns (handles next.config.js, next.config.mjs, etc.)
    if let Ok(entries) = std::fs::read_dir(project_root) {
        let filenames: Vec<String> = entries
            .filter_map(Result::ok)
            .filter_map(|e| e.file_name().to_str().map(String::from))
            .collect();

        for (pattern, label) in CONFIG_PATTERNS {
            if filenames.iter().any(|f| f.starts_with(pattern)) {
                return Some(label);
            }
        }

        for (ext, label) in CONFIG_EXTENSIONS {
            if filenames.iter().any(|f| {
                f.rsplit('.').next().is_some_and(|e| e == *ext)
            }) {
                return Some(label);
            }
        }
    }

    None
}
```

- [ ] **Step 4: Implement process name detection**

```rust
/// Detect app label from a process executable name.
pub fn detect_from_process(process_name: &str) -> Option<&'static str> {
    // Normalize: strip .exe suffix on Windows
    let name = process_name.strip_suffix(".exe").unwrap_or(process_name);

    match name {
        "postgres" | "postgresql" => Some("PostgreSQL"),
        "mysqld" | "mysql" => Some("MySQL"),
        "mariadbd" | "mariadb" => Some("MariaDB"),
        "mongod" | "mongos" => Some("MongoDB"),
        "redis-server" | "redis" => Some("Redis"),
        "memcached" => Some("Memcached"),
        "nginx" => Some("Nginx"),
        "apache2" | "httpd" => Some("Apache"),
        "caddy" => Some("Caddy"),
        "traefik" => Some("Traefik"),
        "envoy" => Some("Envoy"),
        "haproxy" => Some("HAProxy"),
        "rabbitmq-server" => Some("RabbitMQ"),
        "elasticsearch" => Some("Elasticsearch"),
        "clickhouse-server" => Some("ClickHouse"),
        "hugo" => Some("Hugo"),
        "jekyll" => Some("Jekyll"),
        "node" | "nodejs" => Some("Node.js"),
        _ => None,
    }
}
```

- [ ] **Step 5: Add the combined detect function**

```rust
/// Detect the app label for a port entry using all available information.
///
/// Priority: Docker image > config file > process name.
pub fn detect(
    container: Option<&ContainerInfo>,
    project_root: Option<&Path>,
    process_name: &str,
) -> Option<String> {
    if let Some(info) = container {
        if let Some(label) = detect_from_image(info) {
            return Some(label.to_string());
        }
    }

    if let Some(root) = project_root {
        if let Some(label) = detect_from_config(root) {
            return Some(label.to_string());
        }
    }

    detect_from_process(process_name).map(|l| l.to_string())
}
```

- [ ] **Step 6: Write comprehensive tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::docker::ContainerInfo;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn image_postgres() {
        let info = ContainerInfo {
            name: "db".to_string(),
            image: "postgres:16".to_string(),
        };
        assert_eq!(detect_from_image(&info), Some("PostgreSQL"));
    }

    #[test]
    fn image_redis_alpine() {
        let info = ContainerInfo {
            name: "cache".to_string(),
            image: "redis:7-alpine".to_string(),
        };
        assert_eq!(detect_from_image(&info), Some("Redis"));
    }

    #[test]
    fn image_unknown() {
        let info = ContainerInfo {
            name: "custom".to_string(),
            image: "my-custom-app:latest".to_string(),
        };
        assert_eq!(detect_from_image(&info), None);
    }

    #[test]
    fn image_with_registry_prefix() {
        let info = ContainerInfo {
            name: "app".to_string(),
            image: "ghcr.io/org/nginx:latest".to_string(),
        };
        assert_eq!(detect_from_image(&info), Some("Nginx"));
    }

    #[test]
    fn config_nextjs() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("next.config.mjs"), "").unwrap();
        assert_eq!(detect_from_config(dir.path()), Some("Next.js"));
    }

    #[test]
    fn config_rust() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        assert_eq!(detect_from_config(dir.path()), Some("Rust"));
    }

    #[test]
    fn config_django() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("manage.py"), "").unwrap();
        assert_eq!(detect_from_config(dir.path()), Some("Django"));
    }

    #[test]
    fn config_dotnet_csproj() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("MyApp.csproj"), "").unwrap();
        assert_eq!(detect_from_config(dir.path()), Some(".NET"));
    }

    #[test]
    fn config_no_match() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("random.txt"), "").unwrap();
        assert_eq!(detect_from_config(dir.path()), None);
    }

    #[test]
    fn process_postgres() {
        assert_eq!(detect_from_process("postgres"), Some("PostgreSQL"));
    }

    #[test]
    fn process_node() {
        assert_eq!(detect_from_process("node"), Some("Node.js"));
    }

    #[test]
    fn process_unknown() {
        assert_eq!(detect_from_process("svchost"), None);
    }

    #[test]
    fn process_windows_exe_suffix() {
        assert_eq!(detect_from_process("nginx.exe"), Some("Nginx"));
    }

    #[test]
    fn combined_docker_wins_over_config() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        let info = ContainerInfo {
            name: "db".to_string(),
            image: "postgres:16".to_string(),
        };
        let result = detect(Some(&info), Some(dir.path()), "node");
        assert_eq!(result.as_deref(), Some("PostgreSQL"));
    }

    #[test]
    fn combined_config_wins_over_process() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("next.config.js"), "").unwrap();
        let result = detect(None, Some(dir.path()), "node");
        assert_eq!(result.as_deref(), Some("Next.js"));
    }

    #[test]
    fn combined_falls_through_to_process() {
        let result = detect(None, None, "postgres");
        assert_eq!(result.as_deref(), Some("PostgreSQL"));
    }

    #[test]
    fn combined_all_none() {
        let result = detect(None, None, "svchost");
        assert!(result.is_none());
    }
}
```

- [ ] **Step 7: Run tests**

Run: `cargo test framework`
Expected: All 18 framework tests pass

- [ ] **Step 8: Commit**

```
git add src/framework.rs
git commit -m "feat(framework): add app detection from Docker images, config files, and process names"
```

---

### Task 5: Update lib.rs - Export new modules

**Files:**
- Modify: `src/lib.rs`

- [ ] **Step 1: Add new module exports**

```rust
//! # PortLens
//!
//! A cross-platform CLI tool that lists open network ports and their associated
//! processes. Provides a fast, readable alternative to `netstat` and `ss`.
//!
//! ## Module structure
//!
//! - [`types`] - `PortEntry` struct and shared enums
//! - [`collector`] - socket enumeration + process/project/app enrichment
//! - [`filter`] - CLI filters and developer-relevance filter
//! - [`display`] - renders results as bordered/compact table or JSON
//! - [`docker`] - Docker/Podman container detection via socket API
//! - [`project`] - project root detection via marker file walk
//! - [`framework`] - app/framework detection from images, configs, process names

pub mod collector;
pub mod display;
pub mod docker;
pub mod filter;
pub mod framework;
pub mod project;
pub mod types;
```

- [ ] **Step 2: Run tests + clippy**

Run: `cargo test && cargo clippy --all-targets`
Expected: All tests pass, no warnings

- [ ] **Step 3: Commit**

```
git add src/lib.rs
git commit -m "feat(lib): export docker, project, and framework modules"
```

---

### Task 6: Update collector.rs - Integrate enrichment

**Files:**
- Modify: `src/collector.rs`

- [ ] **Step 1: Expand ProcessRefreshKind to include cwd, cmd, and start_time**

Replace the `process_refresh_kind()` function:

```rust
/// Refresh kind for process metadata needed by enrichment.
///
/// Collects: user, working directory, command-line args.
/// Note: start_time is always available without explicit refresh.
fn process_refresh_kind() -> ProcessRefreshKind {
    ProcessRefreshKind::nothing()
        .with_user(UpdateKind::OnlyIfNotSet)
        .with_cwd(UpdateKind::OnlyIfNotSet)
        .with_cmd(UpdateKind::OnlyIfNotSet)
}
```

- [ ] **Step 2: Update collect() to query Docker and enrich entries**

```rust
use std::path::PathBuf;

use crate::docker;
use crate::framework;
use crate::project;

pub fn collect() -> Result<Vec<PortEntry>> {
    let raw_listeners = listeners::get_all()
        .map_err(|e| anyhow::anyhow!("failed to enumerate open sockets from the OS: {e}"))?;

    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, false, process_refresh_kind());

    let users = Users::new_with_refreshed_list();

    let container_map = docker::detect_containers();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut entries: Vec<PortEntry> = raw_listeners
        .into_iter()
        .map(|l| build_entry(&l, &sys, &users, &container_map, now))
        .collect();

    entries.sort_by_key(|e| (e.port, e.proto));
    Ok(entries)
}
```

- [ ] **Step 3: Update build_entry to enrich with project, app, and uptime**

```rust
fn build_entry(
    l: &listeners::Listener,
    sys: &System,
    users: &Users,
    container_map: &docker::ContainerPortMap,
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

    let user = resolve_user(l.process.pid, sys, users);

    let sysinfo_pid = sysinfo::Pid::from_u32(l.process.pid);
    let sysinfo_process = sys.process(sysinfo_pid);

    // Docker detection
    let container = container_map.get(&(l.socket.port(), proto_str.to_string()));

    // Project detection
    let (project, project_root) = if container.is_some() {
        (container.map(|c| c.name.clone()), None)
    } else {
        let cwd = sysinfo_process.and_then(|p| p.cwd().map(Path::to_path_buf));
        let cmd: Vec<String> = sysinfo_process
            .map(|p| p.cmd().iter().map(|s| s.to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        let root = find_project_root_path(cwd.as_deref(), &cmd);
        let name = root
            .as_ref()
            .and_then(|r| r.file_name())
            .map(|n| n.to_string_lossy().into_owned());
        (name, root)
    };

    // App/framework detection
    let app = framework::detect(
        container,
        project_root.as_deref(),
        &l.process.name,
    );

    // Uptime
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
        project,
        app,
        uptime_secs,
    }
}

/// Find the project root path (not just the name).
/// Used by the collector so it can pass the path to framework detection.
fn find_project_root_path(cwd: Option<&Path>, cmd: &[String]) -> Option<PathBuf> {
    if let Some(cwd) = cwd {
        if let Some(root) = walk_for_project_root(cwd) {
            return Some(root);
        }
    }

    for arg in cmd {
        let path = Path::new(arg);
        if path.is_absolute() {
            if let Some(parent) = path.parent() {
                if let Some(root) = walk_for_project_root(parent) {
                    return Some(root);
                }
            }
        }
    }

    None
}

fn walk_for_project_root(start: &Path) -> Option<PathBuf> {
    use crate::project;
    // Re-use the same detection logic
    if project::detect(Some(start), &Vec::<String>::new()).is_some() {
        // Walk up to find the actual root path
        let mut current = start.to_path_buf();
        loop {
            if project::has_marker_at(&current) {
                return Some(current);
            }
            if !current.pop() {
                return None;
            }
        }
    }
    None
}
```

Note: This requires making `has_marker` in project.rs public as `has_marker_at`. Update `src/project.rs`:
- Rename `has_marker` to `has_marker_at` and make it `pub`

- [ ] **Step 4: Run tests**

Run: `cargo test`
Expected: All tests pass

- [ ] **Step 5: Commit**

```
git add src/collector.rs src/project.rs
git commit -m "feat(collector): integrate Docker, project, and framework enrichment"
```

---

### Task 7: Update filter.rs - Add relevance filter

**Files:**
- Modify: `src/filter.rs`

- [ ] **Step 1: Add relevance allowlist and `show_all` to FilterOptions**

```rust
/// Process names considered developer-relevant for the default filter.
const RELEVANT_PROCESSES: &[&str] = &[
    // Runtimes
    "node", "nodejs", "python", "python3", "ruby", "java", "go", "deno", "bun",
    "dotnet", "php", "perl", "cargo", "rustc", "erlang", "elixir",
    // Databases
    "postgres", "postgresql", "mysqld", "mysql", "mariadbd", "mariadb",
    "mongod", "mongos", "redis-server", "redis", "memcached",
    "clickhouse-server", "cockroach",
    // Web servers
    "nginx", "apache2", "httpd", "caddy", "traefik", "envoy", "haproxy",
    // Search/messaging
    "elasticsearch", "opensearch", "rabbitmq-server", "kafka",
    // Dev tools
    "webpack", "vite", "next-server", "nuxt", "hugo", "jekyll",
];
```

Add `show_all` field to `FilterOptions`:

```rust
pub struct FilterOptions {
    pub tcp_only: bool,
    pub udp_only: bool,
    pub listen_only: bool,
    pub port: Option<u16>,
    /// When false (default), only show developer-relevant ports.
    pub show_all: bool,
}
```

- [ ] **Step 2: Add relevance check to the filter**

Add a relevance check at the end of the filter closure, after the existing protocol/port checks:

```rust
// After the existing checks, before `true`:
if !opts.show_all && !is_relevant(e) {
    return false;
}
true
```

And the helper:

```rust
/// Check if a port entry is developer-relevant.
fn is_relevant(entry: &PortEntry) -> bool {
    // Docker containers are always relevant
    if entry.project.is_some() {
        return true;
    }
    // Known app/framework detected
    if entry.app.is_some() {
        return true;
    }
    // Known dev process name
    let name = entry
        .process
        .strip_suffix(".exe")
        .unwrap_or(&entry.process);
    RELEVANT_PROCESSES.iter().any(|&r| r == name)
}
```

- [ ] **Step 3: Update all existing tests to include `show_all: true`**

Every existing test's `FilterOptions` needs `show_all: true` so they continue passing (they test protocol/port filters, not relevance). Add to each existing `FilterOptions { ... }`:

```rust
show_all: true,
```

- [ ] **Step 4: Update make_entry helper with new fields**

```rust
fn make_entry(port: u16, proto: Protocol, state: State) -> PortEntry {
    PortEntry {
        port,
        proto,
        state,
        pid: 1234,
        process: "test".to_string(),
        user: "user".to_string(),
        project: None,
        app: None,
        uptime_secs: None,
    }
}
```

- [ ] **Step 5: Add relevance filter tests**

```rust
#[test]
fn relevance_filter_hides_unknown_process() {
    let mut entry = make_entry(12345, Protocol::Tcp, State::Listen);
    entry.process = "svchost".to_string();
    let entries = vec![entry];
    let opts = FilterOptions {
        tcp_only: false,
        udp_only: false,
        listen_only: false,
        port: None,
        show_all: false,
    };
    let result = apply(&entries, &opts);
    assert!(result.is_empty(), "unknown process should be filtered out");
}

#[test]
fn relevance_filter_shows_node() {
    let mut entry = make_entry(3000, Protocol::Tcp, State::Listen);
    entry.process = "node".to_string();
    let entries = vec![entry];
    let opts = FilterOptions {
        tcp_only: false,
        udp_only: false,
        listen_only: false,
        port: None,
        show_all: false,
    };
    let result = apply(&entries, &opts);
    assert_eq!(result.len(), 1, "node should pass relevance filter");
}

#[test]
fn relevance_filter_shows_docker_container() {
    let mut entry = make_entry(5432, Protocol::Tcp, State::Listen);
    entry.process = "docker-proxy".to_string();
    entry.project = Some("backend-postgres-1".to_string());
    let entries = vec![entry];
    let opts = FilterOptions {
        tcp_only: false,
        udp_only: false,
        listen_only: false,
        port: None,
        show_all: false,
    };
    let result = apply(&entries, &opts);
    assert_eq!(result.len(), 1, "docker container should pass relevance filter");
}

#[test]
fn relevance_filter_shows_detected_app() {
    let mut entry = make_entry(8080, Protocol::Tcp, State::Listen);
    entry.process = "unknown-binary".to_string();
    entry.app = Some("Next.js".to_string());
    let entries = vec![entry];
    let opts = FilterOptions {
        tcp_only: false,
        udp_only: false,
        listen_only: false,
        port: None,
        show_all: false,
    };
    let result = apply(&entries, &opts);
    assert_eq!(result.len(), 1, "detected app should pass relevance filter");
}

#[test]
fn show_all_bypasses_relevance() {
    let mut entry = make_entry(12345, Protocol::Tcp, State::Listen);
    entry.process = "svchost".to_string();
    let entries = vec![entry];
    let opts = FilterOptions {
        tcp_only: false,
        udp_only: false,
        listen_only: false,
        port: None,
        show_all: true,
    };
    let result = apply(&entries, &opts);
    assert_eq!(result.len(), 1, "--all should bypass relevance filter");
}
```

- [ ] **Step 6: Run tests**

Run: `cargo test filter`
Expected: All old + new filter tests pass

- [ ] **Step 7: Commit**

```
git add src/filter.rs
git commit -m "feat(filter): add developer-relevance filter with --all bypass"
```

---

### Task 8: Update display.rs - Bordered table, column modes, uptime

**Files:**
- Modify: `src/display.rs`

- [ ] **Step 1: Add DisplayOptions struct and update function signatures**

```rust
/// Options controlling how entries are rendered.
pub struct DisplayOptions {
    /// Show the header row.
    pub show_header: bool,
    /// Show all columns (adds STATE and USER).
    pub full: bool,
    /// Use compact (borderless) table style.
    pub compact: bool,
}
```

- [ ] **Step 2: Rewrite print_table to support modes**

Replace the existing `print_table` function:

```rust
/// Print the entries as a table to stdout.
///
/// Table style and column selection are controlled by `opts`.
/// Returns an error if writing to stdout fails (e.g. broken pipe).
pub fn print_table(entries: &[PortEntry], opts: &DisplayOptions) -> Result<()> {
    let mut table = Table::new();
    table.set_content_arrangement(ContentArrangement::Dynamic);

    if opts.compact {
        table.load_preset(comfy_table::presets::NOTHING);
    } else {
        table.load_preset(comfy_table::presets::UTF8_FULL);
        table.apply_modifier(comfy_table::modifiers::UTF8_ROUND_CORNERS);
    }

    if opts.show_header {
        if opts.full {
            table.set_header(vec![
                "PORT", "PROTO", "STATE", "PROCESS", "PID", "USER",
                "PROJECT", "APP", "UPTIME",
            ]);
        } else {
            table.set_header(vec![
                "PORT", "PROTO", "PROCESS", "PID", "PROJECT", "APP", "UPTIME",
            ]);
        }
    }

    for entry in entries {
        let process_display = truncate_process_name(&entry.process);
        let project = entry.project.as_deref().unwrap_or("-");
        let app = entry.app.as_deref().unwrap_or("-");
        let uptime = crate::types::format_uptime(entry.uptime_secs);

        if opts.full {
            table.add_row(vec![
                entry.port.to_string(),
                entry.proto.to_string(),
                entry.state.to_string(),
                process_display,
                entry.pid.to_string(),
                entry.user.clone(),
                project.to_string(),
                app.to_string(),
                uptime,
            ]);
        } else {
            table.add_row(vec![
                entry.port.to_string(),
                entry.proto.to_string(),
                process_display,
                entry.pid.to_string(),
                project.to_string(),
                app.to_string(),
                uptime,
            ]);
        }
    }

    writeln!(io::stdout().lock(), "{table}")?;
    Ok(())
}
```

- [ ] **Step 3: Run tests + clippy**

Run: `cargo test && cargo clippy --all-targets`
Expected: All pass (display tests only test `truncate_process_name` which is unchanged)

- [ ] **Step 4: Commit**

```
git add src/display.rs
git commit -m "feat(display): add bordered table, column modes, and uptime formatting"
```

---

### Task 9: Update main.rs - New CLI flags and wiring

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Add new CLI flags**

Add to the `Cli` struct:

```rust
    /// Show all ports (disable developer-relevant filter).
    #[arg(short = 'a', long = "all")]
    all: bool,

    /// Show all columns (adds STATE, USER).
    #[arg(short = 'f', long = "full")]
    full: bool,

    /// Use compact borderless table style.
    #[arg(short = 'c', long = "compact")]
    compact: bool,
```

- [ ] **Step 2: Update run() to pass new options**

```rust
fn run() -> Result<()> {
    let cli = Cli::parse();

    let entries = collector::collect()?;
    let filtered = filter::apply(
        &entries,
        &filter::FilterOptions {
            tcp_only: cli.tcp,
            udp_only: cli.udp,
            listen_only: cli.listen,
            port: cli.port,
            show_all: cli.all,
        },
    );

    if cli.json {
        display::print_json(&filtered)?;
    } else {
        display::print_table(
            &filtered,
            &display::DisplayOptions {
                show_header: !cli.no_header,
                full: cli.full,
                compact: cli.compact,
            },
        )?;
    }

    Ok(())
}
```

- [ ] **Step 3: Run full test suite + clippy**

Run: `cargo test && cargo clippy --all-targets`
Expected: All pass

- [ ] **Step 4: Commit**

```
git add src/main.rs
git commit -m "feat(cli): add --all, --full, and --compact flags"
```

---

### Task 10: Update benchmarks and final validation

**Files:**
- Modify: `benches/benchmarks.rs`

- [ ] **Step 1: Update benchmark PortEntry construction**

Add the new fields to the benchmark entry builder:

```rust
            project: if i % 4 == 0 {
                Some(format!("project_{i}"))
            } else {
                None
            },
            app: if i % 5 == 0 {
                Some("Next.js".to_string())
            } else {
                None
            },
            uptime_secs: Some(u64::from(i) * 3600),
```

Also update `FilterOptions` to include `show_all: true`.

- [ ] **Step 2: Run full validation**

Run: `cargo test && cargo clippy --all-targets && cargo bench --no-run && cargo doc`
Expected: All pass

- [ ] **Step 3: Commit**

```
git add benches/benchmarks.rs
git commit -m "chore(bench): update benchmarks for enriched PortEntry fields"
```

---

### Task 11: Update README and docs

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Update CLI Reference table**

Add the new flags to the table:

```markdown
| `--all`        | `-a`  | Show all ports (default hides system noise) |
| `--full`       | `-f`  | Show all columns (adds STATE, USER)         |
| `--compact`    | `-c`  | Use compact borderless table style           |
```

- [ ] **Step 2: Update Output Columns section**

Replace the output columns table with default vs full mode descriptions.

- [ ] **Step 3: Update Example Output**

Replace with bordered table example showing PROJECT, APP, UPTIME columns.

- [ ] **Step 4: Commit**

```
git add README.md
git commit -m "docs: update README for enriched display features"
```
