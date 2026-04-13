//! # Framework/app detection
//!
//! Identifies the technology behind a port using three strategies:
//! Docker image name, config file detection, and process name matching.

use std::borrow::Cow;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::Path;

use crate::docker::ContainerInfo;
use crate::types::AppLabel;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ConfigMatchKind {
    Exact,
    Prefix,
}

/// Cached listing of file names in a project root directory.
///
/// Uses a `HashSet` so that exact-match lookups (the hot path for
/// config-file detection) are O(1) instead of O(n).
struct ProjectFiles {
    names: HashSet<String>,
}

impl ProjectFiles {
    fn read(project_root: &Path) -> Option<Self> {
        let entries = std::fs::read_dir(project_root).ok()?;
        let names = entries
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect();

        Some(Self { names })
    }

    fn contains_exact(&self, target: &str) -> bool {
        self.names.contains(target)
    }

    fn contains_prefix(&self, prefix: &str) -> bool {
        self.names
            .iter()
            .any(|name| matches_config_name(name, prefix, ConfigMatchKind::Prefix))
    }

    fn any_exact(&self, targets: &[&str]) -> bool {
        targets.iter().any(|target| self.contains_exact(target))
    }

    fn contains_extension(&self, target_ext: &str) -> bool {
        self.names.iter().any(|name| {
            std::path::Path::new(name)
                .extension()
                .and_then(OsStr::to_str)
                .is_some_and(|ext| ext == target_ext)
        })
    }

    /// Read the first portion of a project file as UTF-8 text.
    ///
    /// Caps reads at 64 KB because framework detection only needs
    /// import statements and dependency declarations, which appear
    /// near the top of every file. This avoids reading multi-megabyte
    /// source files into memory for a substring scan.
    fn read_text(&self, project_root: &Path, file_name: &str) -> Option<String> {
        use std::io::Read;

        const MAX_SCAN_BYTES: u64 = 64 * 1024;

        if !self.contains_exact(file_name) {
            return None;
        }

        let file = std::fs::File::open(project_root.join(file_name)).ok()?;
        let mut buffer = String::new();
        file.take(MAX_SCAN_BYTES).read_to_string(&mut buffer).ok()?;
        Some(buffer)
    }
}

/// Detect app label from a Docker container's image name.
///
/// Matches the base image name (before the colon tag) against known patterns.
#[must_use]
pub fn detect_from_image(info: &ContainerInfo) -> Option<AppLabel> {
    let image = info.image.to_ascii_lowercase();
    let last_segment = image.split('/').next_back().unwrap_or(image.as_str());
    let base = last_segment
        .split([':', '@'])
        .next()
        .unwrap_or(last_segment);

    let label = match base {
        s if s.starts_with("postgres") => Some("PostgreSQL"),
        s if s.starts_with("mysql") => Some("MySQL"),
        s if s.starts_with("mariadb") => Some("MariaDB"),
        // Exact "mongo" or "mongodb*" to avoid false positives (e.g. "mongo-express").
        s if s == "mongo" || s.starts_with("mongodb") => Some("MongoDB"),
        // Exact "redis" or "redis-stack*" to avoid false positives (e.g. "redis-commander").
        s if s == "redis" || s.starts_with("redis-stack") => Some("Redis"),
        s if s.starts_with("valkey") => Some("Valkey"),
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
        // Exact match to avoid false positives (e.g. "node-exporter").
        "node" => Some("Node.js"),
        // Exact match to avoid false positives (e.g. "python-linter").
        "python" | "python3" => Some("Python"),
        // Exact match to avoid false positives (e.g. "rubygems-mirror").
        "ruby" => Some("Ruby"),
        // Exact match to avoid false positives (e.g. "golang-migrate").
        "golang" | "go" => Some("Go"),
        // Exact match to avoid false positives (e.g. "rust-analyzer").
        "rust" => Some("Rust"),
        s if s.starts_with("openjdk") || s.starts_with("eclipse-temurin") => Some("Java"),
        s if s.starts_with("dotnet") || image.split('/').any(|segment| segment == "dotnet") => {
            Some(".NET")
        }
        _ => None,
    }?;

    Some(Cow::Borrowed(label))
}

/// Config file patterns checked inside a project root directory.
/// More specific patterns (e.g. `next.config`) come before generic ones.
const CONFIG_PATTERNS: &[(&str, &str, ConfigMatchKind)] = &[
    ("next.config", "Next.js", ConfigMatchKind::Prefix),
    ("nuxt.config", "Nuxt", ConfigMatchKind::Prefix),
    ("angular.json", "Angular", ConfigMatchKind::Exact),
    ("svelte.config", "SvelteKit", ConfigMatchKind::Prefix),
    ("astro.config", "Astro", ConfigMatchKind::Prefix),
    ("vite.config", "Vite", ConfigMatchKind::Prefix),
    ("remix.config", "Remix", ConfigMatchKind::Prefix),
    ("gatsby-config", "Gatsby", ConfigMatchKind::Prefix),
    ("vue.config", "Vue CLI", ConfigMatchKind::Prefix),
    ("webpack.config", "Webpack", ConfigMatchKind::Prefix),
    ("Cargo.toml", "Rust", ConfigMatchKind::Exact),
    ("go.mod", "Go", ConfigMatchKind::Exact),
    ("pom.xml", "Java (Maven)", ConfigMatchKind::Exact),
    (
        "build.gradle.kts",
        "Kotlin (Gradle)",
        ConfigMatchKind::Exact,
    ),
    ("build.gradle", "Java (Gradle)", ConfigMatchKind::Exact),
    ("composer.json", "PHP", ConfigMatchKind::Exact),
    ("mix.exs", "Elixir", ConfigMatchKind::Exact),
    ("deno.json", "Deno", ConfigMatchKind::Exact),
];

/// Extension-based config patterns.
const CONFIG_EXTENSIONS: &[(&str, &str)] = &[("csproj", ".NET"), ("fsproj", ".NET (F#)")];

const PYTHON_APP_FILE: &str = "app.py";
const PYTHON_MAIN_FILE: &str = "main.py";
const PYTHON_SERVER_FILE: &str = "server.py";
const PYTHON_WSGI_FILE: &str = "wsgi.py";
const PYTHON_ASGI_FILE: &str = "asgi.py";

const PYTHON_ENTRY_FILES: &[&str] = &[
    PYTHON_APP_FILE,
    PYTHON_MAIN_FILE,
    PYTHON_SERVER_FILE,
    PYTHON_WSGI_FILE,
    PYTHON_ASGI_FILE,
];
const PYTHON_DEPENDENCY_FILES: &[&str] = &[
    "pyproject.toml",
    "requirements.txt",
    "requirements-dev.txt",
    "Pipfile",
    "poetry.lock",
    "uv.lock",
    "setup.py",
];

/// Check whether the project contains any Python marker file.
///
/// Combines `manage.py`, [`PYTHON_ENTRY_FILES`], and
/// [`PYTHON_DEPENDENCY_FILES`] in a single pass so the constituent
/// lists remain the single source of truth.
fn is_python_project(files: &ProjectFiles) -> bool {
    files.contains_exact("manage.py")
        || files.any_exact(PYTHON_ENTRY_FILES)
        || files.any_exact(PYTHON_DEPENDENCY_FILES)
}
const DJANGO_SOURCE_PATTERNS: &[&str] = &[
    "django.core.wsgi",
    "django.core.asgi",
    "django_settings_module",
    "get_wsgi_application",
    "get_asgi_application",
];
const PYTHON_DEPENDENCY_PATTERNS: &[(&str, &str)] = &[
    ("django", "Django"),
    ("flask", "Flask"),
    ("fastapi", "FastAPI"),
    ("starlette", "Starlette"),
    ("litestar", "Litestar"),
];

/// Detect app label by scanning config files in a project root.
///
/// Scans the directory once and preserves the configured pattern priority.
#[must_use]
pub fn detect_from_config(project_root: &Path) -> Option<AppLabel> {
    let files = ProjectFiles::read(project_root)?;

    detect_from_config_patterns(&files)
        .or_else(|| detect_python_project(project_root, &files))
        .or_else(|| detect_rack_project(&files))
        .or_else(|| detect_from_config_extensions(&files))
}

fn detect_from_config_patterns(files: &ProjectFiles) -> Option<AppLabel> {
    for (pattern, label, match_kind) in CONFIG_PATTERNS {
        let matches = match match_kind {
            ConfigMatchKind::Exact => files.contains_exact(pattern),
            ConfigMatchKind::Prefix => files.contains_prefix(pattern),
        };

        if matches {
            return Some(Cow::Borrowed(label));
        }
    }

    None
}

fn detect_python_project(project_root: &Path, files: &ProjectFiles) -> Option<AppLabel> {
    if !is_python_project(files) {
        return None;
    }

    if files.contains_exact("manage.py") {
        return Some(Cow::Borrowed("Django"));
    }

    detect_python_framework_from_entry_files(project_root, files)
        .or_else(|| detect_python_framework_from_dependencies(project_root, files))
        .or(Some(Cow::Borrowed("Python")))
}

fn detect_python_framework_from_entry_files(
    project_root: &Path,
    files: &ProjectFiles,
) -> Option<AppLabel> {
    for file_name in PYTHON_ENTRY_FILES {
        let Some(source) = files.read_text(project_root, file_name) else {
            continue;
        };

        if let Some(label) = detect_python_framework_from_source(&source) {
            return Some(Cow::Borrowed(label));
        }
    }

    None
}

fn detect_python_framework_from_dependencies(
    project_root: &Path,
    files: &ProjectFiles,
) -> Option<AppLabel> {
    for file_name in PYTHON_DEPENDENCY_FILES {
        let Some(contents) = files.read_text(project_root, file_name) else {
            continue;
        };

        let normalized = contents.to_ascii_lowercase();
        for (package, label) in PYTHON_DEPENDENCY_PATTERNS {
            if contains_dependency_token(&normalized, package) {
                return Some(Cow::Borrowed(label));
            }
        }
    }

    None
}

fn detect_python_framework_from_source(source: &str) -> Option<&'static str> {
    let normalized = source.to_ascii_lowercase();

    if contains_any(&normalized, DJANGO_SOURCE_PATTERNS) {
        return Some("Django");
    }

    if source_mentions_framework(
        &normalized,
        &["from fastapi import", "import fastapi"],
        "fastapi(",
    ) {
        return Some("FastAPI");
    }
    if source_mentions_framework(
        &normalized,
        &["from starlette.applications import", "import starlette"],
        "starlette(",
    ) {
        return Some("Starlette");
    }
    if source_mentions_framework(
        &normalized,
        &["from litestar import", "import litestar"],
        "litestar(",
    ) {
        return Some("Litestar");
    }
    if source_mentions_framework(
        &normalized,
        &["from flask import", "import flask"],
        "flask(",
    ) {
        return Some("Flask");
    }

    None
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn source_mentions_framework(haystack: &str, imports: &[&str], constructor: &str) -> bool {
    haystack.contains(constructor) && contains_any(haystack, imports)
}

fn contains_dependency_token(haystack: &str, token: &str) -> bool {
    let mut offset = 0;

    while let Some(index) = haystack[offset..].find(token) {
        let start = offset + index;
        let end = start + token.len();
        let bytes = haystack.as_bytes();
        let before = start
            .checked_sub(1)
            .and_then(|position| bytes.get(position))
            .copied();
        let after = bytes.get(end).copied();

        if is_dependency_boundary(before) && is_dependency_boundary(after) {
            return true;
        }

        offset = start + 1;
    }

    false
}

const fn is_dependency_boundary(byte: Option<u8>) -> bool {
    match byte {
        None => true,
        Some(value) => !matches!(value, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'),
    }
}

fn detect_rack_project(files: &ProjectFiles) -> Option<AppLabel> {
    (files.contains_exact("Gemfile") && files.contains_exact("config.ru"))
        .then_some(Cow::Borrowed("Ruby (Rack)"))
}

fn detect_from_config_extensions(files: &ProjectFiles) -> Option<AppLabel> {
    CONFIG_EXTENSIONS.iter().find_map(|(ext, label)| {
        files
            .contains_extension(ext)
            .then_some(Cow::Borrowed(*label))
    })
}

fn matches_config_name(name: &str, pattern: &str, match_kind: ConfigMatchKind) -> bool {
    match match_kind {
        ConfigMatchKind::Exact => name == pattern,
        ConfigMatchKind::Prefix => name
            .strip_prefix(pattern)
            .is_some_and(|suffix| COMMON_CONFIG_SUFFIXES.contains(&suffix)),
    }
}

const COMMON_CONFIG_SUFFIXES: &[&str] = &["", ".js", ".cjs", ".mjs", ".ts", ".cts", ".mts"];

/// Known process names mapped to their app/framework labels.
///
/// Linear scan with [`str::eq_ignore_ascii_case`] avoids allocating a
/// lowercase `String` on every call to [`detect_from_process`].
const PROCESS_MAP: &[(&str, &str)] = &[
    // Runtimes
    ("node", "Node.js"),
    ("nodejs", "Node.js"),
    ("python", "Python"),
    ("python3", "Python"),
    ("ruby", "Ruby"),
    ("java", "Java"),
    ("go", "Go"),
    ("deno", "Deno"),
    ("bun", "Bun"),
    ("dotnet", ".NET"),
    ("php", "PHP"),
    ("perl", "Perl"),
    ("cargo", "Rust"),
    ("rustc", "Rust"),
    ("erlang", "Erlang"),
    ("beam.smp", "Erlang"),
    ("elixir", "Elixir"),
    ("dart", "Dart"),
    ("swift", "Swift"),
    // Databases
    ("postgres", "PostgreSQL"),
    ("postgresql", "PostgreSQL"),
    ("mysqld", "MySQL"),
    ("mysql", "MySQL"),
    ("mariadbd", "MariaDB"),
    ("mariadb", "MariaDB"),
    ("mongod", "MongoDB"),
    ("mongos", "MongoDB"),
    ("redis-server", "Redis"),
    ("redis", "Redis"),
    ("valkey-server", "Valkey"),
    ("valkey", "Valkey"),
    ("memcached", "Memcached"),
    ("clickhouse-server", "ClickHouse"),
    ("cockroach", "CockroachDB"),
    // Web servers
    ("nginx", "Nginx"),
    ("apache2", "Apache"),
    ("httpd", "Apache"),
    ("caddy", "Caddy"),
    ("traefik", "Traefik"),
    ("envoy", "Envoy"),
    ("haproxy", "HAProxy"),
    ("gunicorn", "Gunicorn"),
    ("uvicorn", "Uvicorn"),
    // Search/messaging
    ("elasticsearch", "Elasticsearch"),
    ("opensearch", "OpenSearch"),
    ("rabbitmq-server", "RabbitMQ"),
    ("kafka", "Kafka"),
    // Dev tools
    ("webpack", "Webpack"),
    ("vite", "Vite"),
    ("next-server", "Next.js"),
    ("nuxt", "Nuxt"),
    ("hugo", "Hugo"),
    ("jekyll", "Jekyll"),
    ("flask", "Flask"),
    ("rails", "Rails"),
    ("gradle", "Java (Gradle)"),
    ("mvn", "Java (Maven)"),
];

/// Detect app label from a process executable name.
#[must_use]
pub fn detect_from_process(process_name: &str) -> Option<AppLabel> {
    let name = crate::types::strip_windows_exe_suffix(process_name);
    PROCESS_MAP
        .iter()
        .find(|(key, _)| name.eq_ignore_ascii_case(key))
        .map(|(_, label)| Cow::Borrowed(*label))
}

/// Detect the app label for a port entry using all available information.
///
/// Priority: Docker image > config file > process name.
#[must_use]
pub fn detect(
    container: Option<&ContainerInfo>,
    project_root: Option<&Path>,
    process_name: &str,
) -> Option<AppLabel> {
    if let Some(info) = container
        && let Some(label) = detect_from_image(info)
    {
        return Some(label);
    }

    if let Some(root) = project_root
        && let Some(label) = detect_from_config(root)
    {
        return Some(label);
    }

    detect_from_process(process_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docker::ContainerInfo;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn image_postgres() {
        let info = ContainerInfo {
            id: String::new(),
            name: "db".to_string(),
            image: "postgres:16".to_string(),
        };
        assert_eq!(detect_from_image(&info).as_deref(), Some("PostgreSQL"));
    }

    #[test]
    fn image_redis_alpine() {
        let info = ContainerInfo {
            id: String::new(),
            name: "cache".to_string(),
            image: "redis:7-alpine".to_string(),
        };
        assert_eq!(detect_from_image(&info).as_deref(), Some("Redis"));
    }

    #[test]
    fn image_valkey() {
        let info = ContainerInfo {
            id: String::new(),
            name: "cache".to_string(),
            image: "valkey/valkey:8-alpine".to_string(),
        };
        assert_eq!(detect_from_image(&info).as_deref(), Some("Valkey"));
    }

    #[test]
    fn image_unknown() {
        let info = ContainerInfo {
            id: String::new(),
            name: "custom".to_string(),
            image: "my-custom-app:latest".to_string(),
        };
        assert_eq!(detect_from_image(&info).as_deref(), None);
    }

    #[test]
    fn image_node_exporter_not_nodejs() {
        let info = ContainerInfo {
            id: String::new(),
            name: "metrics".to_string(),
            image: "prom/node-exporter:latest".to_string(),
        };
        assert_eq!(
            detect_from_image(&info).as_deref(),
            None,
            "node-exporter should not match Node.js"
        );
    }

    #[test]
    fn image_with_registry_prefix() {
        let info = ContainerInfo {
            id: String::new(),
            name: "app".to_string(),
            image: "ghcr.io/org/nginx:latest".to_string(),
        };
        assert_eq!(detect_from_image(&info).as_deref(), Some("Nginx"));
    }

    #[test]
    fn image_dotnet_microsoft_registry() {
        let info = ContainerInfo {
            id: String::new(),
            name: "api".to_string(),
            image: "mcr.microsoft.com/dotnet/aspnet:8.0".to_string(),
        };
        assert_eq!(detect_from_image(&info).as_deref(), Some(".NET"));
    }

    #[test]
    fn image_mongo_express_not_mongodb() {
        let info = ContainerInfo {
            id: String::new(),
            name: "admin".to_string(),
            image: "mongo-express:latest".to_string(),
        };
        assert_eq!(
            detect_from_image(&info).as_deref(),
            None,
            "mongo-express should not match MongoDB"
        );
    }

    #[test]
    fn image_mongodb_community_server_matches() {
        let info = ContainerInfo {
            id: String::new(),
            name: "db".to_string(),
            image: "mongodb/mongodb-community-server:7.0".to_string(),
        };
        assert_eq!(detect_from_image(&info).as_deref(), Some("MongoDB"));
    }

    #[test]
    fn image_redis_commander_not_redis() {
        let info = ContainerInfo {
            id: String::new(),
            name: "ui".to_string(),
            image: "redis-commander:latest".to_string(),
        };
        assert_eq!(
            detect_from_image(&info).as_deref(),
            None,
            "redis-commander should not match Redis"
        );
    }

    #[test]
    fn image_redis_stack_matches() {
        let info = ContainerInfo {
            id: String::new(),
            name: "cache".to_string(),
            image: "redis/redis-stack:latest".to_string(),
        };
        assert_eq!(detect_from_image(&info).as_deref(), Some("Redis"));
    }

    #[test]
    fn image_python_linter_not_python() {
        let info = ContainerInfo {
            id: String::new(),
            name: "lint".to_string(),
            image: "python-linter:latest".to_string(),
        };
        assert_eq!(
            detect_from_image(&info).as_deref(),
            None,
            "python-linter should not match Python"
        );
    }

    #[test]
    fn image_rubygems_mirror_not_ruby() {
        let info = ContainerInfo {
            id: String::new(),
            name: "mirror".to_string(),
            image: "rubygems-mirror:latest".to_string(),
        };
        assert_eq!(
            detect_from_image(&info).as_deref(),
            None,
            "rubygems-mirror should not match Ruby"
        );
    }

    #[test]
    fn config_nextjs() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("next.config.mjs"), "").unwrap();
        assert_eq!(detect_from_config(dir.path()).as_deref(), Some("Next.js"));
    }

    #[test]
    fn config_rust() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        assert_eq!(detect_from_config(dir.path()).as_deref(), Some("Rust"));
    }

    #[test]
    fn config_django() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("manage.py"), "").unwrap();
        assert_eq!(detect_from_config(dir.path()).as_deref(), Some("Django"));
    }

    #[test]
    fn config_flask() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("app.py"),
            "from flask import Flask\napp = Flask(__name__)\n",
        )
        .unwrap();
        assert_eq!(detect_from_config(dir.path()).as_deref(), Some("Flask"));
    }

    #[test]
    fn config_wsgi_detects_django_instead_of_flask() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("wsgi.py"),
            "from django.core.wsgi import get_wsgi_application\napplication = get_wsgi_application()\n",
        )
        .unwrap();

        assert_eq!(detect_from_config(dir.path()).as_deref(), Some("Django"));
    }

    #[test]
    fn config_app_py_fastapi_is_not_mislabeled_as_flask() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("app.py"),
            "from fastapi import FastAPI\napp = FastAPI()\n",
        )
        .unwrap();

        assert_eq!(detect_from_config(dir.path()).as_deref(), Some("FastAPI"));
    }

    #[test]
    fn config_generic_python_entry_falls_back_to_python() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("app.py"), "print('hello')\n").unwrap();

        assert_eq!(detect_from_config(dir.path()).as_deref(), Some("Python"));
    }

    #[test]
    fn config_python_dependency_file_detects_framework() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\ndependencies = [\"flask>=3.0\"]\n",
        )
        .unwrap();

        assert_eq!(detect_from_config(dir.path()).as_deref(), Some("Flask"));
    }

    #[test]
    fn config_rack_requires_gemfile() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("config.ru"), "").unwrap();
        assert_eq!(detect_from_config(dir.path()).as_deref(), None);
    }

    #[test]
    fn config_rack_when_gemfile_and_config_ru_exist() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("Gemfile"), "").unwrap();
        fs::write(dir.path().join("config.ru"), "").unwrap();
        assert_eq!(
            detect_from_config(dir.path()).as_deref(),
            Some("Ruby (Rack)")
        );
    }

    #[test]
    fn config_exact_match_does_not_overmatch_backup_file() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("Cargo.toml.bak"), "").unwrap();
        assert_eq!(detect_from_config(dir.path()).as_deref(), None);
    }

    #[test]
    fn config_exact_match_does_not_overmatch_renamed_script() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("manage.py.old"), "").unwrap();
        assert_eq!(detect_from_config(dir.path()).as_deref(), None);
    }

    #[test]
    fn config_dotnet_csproj() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("MyApp.csproj"), "").unwrap();
        assert_eq!(detect_from_config(dir.path()).as_deref(), Some(".NET"));
    }

    #[test]
    fn config_no_match() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("random.txt"), "").unwrap();
        assert_eq!(detect_from_config(dir.path()).as_deref(), None);
    }

    #[test]
    fn process_postgres() {
        assert_eq!(
            detect_from_process("postgres").as_deref(),
            Some("PostgreSQL")
        );
    }

    #[test]
    fn process_node() {
        assert_eq!(detect_from_process("node").as_deref(), Some("Node.js"));
    }

    #[test]
    fn process_unknown() {
        assert_eq!(detect_from_process("svchost").as_deref(), None);
    }

    #[test]
    fn process_windows_exe_suffix() {
        assert_eq!(detect_from_process("nginx.exe").as_deref(), Some("Nginx"));
    }

    #[test]
    fn process_windows_exe_suffix_is_case_insensitive() {
        assert_eq!(detect_from_process("NGINX.EXE").as_deref(), Some("Nginx"));
    }

    #[test]
    fn process_case_insensitive() {
        assert_eq!(detect_from_process("Nginx").as_deref(), Some("Nginx"));
        assert_eq!(
            detect_from_process("POSTGRES").as_deref(),
            Some("PostgreSQL")
        );
        assert_eq!(detect_from_process("Node").as_deref(), Some("Node.js"));
    }

    #[test]
    fn combined_docker_wins_over_config() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        let info = ContainerInfo {
            id: String::new(),
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
