//! # Framework/app detection
//!
//! Identifies the technology behind a port using three strategies:
//! Docker image name, config file detection, and process name matching.

use std::collections::HashSet;
use std::path::Path;

use crate::docker::ContainerInfo;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ConfigMatchKind {
    Exact,
    Prefix,
}

/// Detect app label from a Docker container's image name.
///
/// Matches the base image name (before the colon tag) against known patterns.
#[must_use]
pub fn detect_from_image(info: &ContainerInfo) -> Option<&'static str> {
    let image = info.image.to_ascii_lowercase();
    let last_segment = image.split('/').next_back().unwrap_or(image.as_str());
    let base = last_segment
        .split([':', '@'])
        .next()
        .unwrap_or(last_segment);

    match base {
        s if s.starts_with("postgres") => Some("PostgreSQL"),
        s if s.starts_with("mysql") => Some("MySQL"),
        s if s.starts_with("mariadb") => Some("MariaDB"),
        s if s.starts_with("mongo") => Some("MongoDB"),
        s if s.starts_with("redis") => Some("Redis"),
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
        s if s.starts_with("python") => Some("Python"),
        s if s.starts_with("ruby") => Some("Ruby"),
        // Exact match to avoid false positives (e.g. "golang-migrate").
        "golang" | "go" => Some("Go"),
        // Exact match to avoid false positives (e.g. "rust-analyzer").
        "rust" => Some("Rust"),
        s if s.starts_with("openjdk") || s.starts_with("eclipse-temurin") => Some("Java"),
        s if s.starts_with("dotnet") || image.split('/').any(|segment| segment == "dotnet") => {
            Some(".NET")
        }
        _ => None,
    }
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
    ("manage.py", "Django", ConfigMatchKind::Exact),
    ("app.py", "Flask", ConfigMatchKind::Exact),
    ("wsgi.py", "Flask", ConfigMatchKind::Exact),
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
    ("pyproject.toml", "Python", ConfigMatchKind::Exact),
];

/// Extension-based config patterns.
const CONFIG_EXTENSIONS: &[(&str, &str)] = &[("csproj", ".NET"), ("fsproj", ".NET (F#)")];

/// Detect app label by scanning config files in a project root.
///
/// Iterates directory entries lazily and returns the highest-priority
/// match without reading the entire directory when an early match exists.
pub fn detect_from_config(project_root: &Path) -> Option<&'static str> {
    let Ok(entries) = std::fs::read_dir(project_root) else {
        return None;
    };

    let file_names: Vec<_> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();

    let exact_names: HashSet<_> = file_names.iter().map(String::as_str).collect();
    let extensions: HashSet<_> = file_names
        .iter()
        .filter_map(|name| std::path::Path::new(name).extension())
        .filter_map(std::ffi::OsStr::to_str)
        .collect();

    for (pattern, label, match_kind) in CONFIG_PATTERNS {
        let matches = match match_kind {
            ConfigMatchKind::Exact => exact_names.contains(pattern),
            ConfigMatchKind::Prefix => file_names.iter().any(|name| name.starts_with(pattern)),
        };

        if matches {
            return Some(label);
        }
    }

    if exact_names.contains("Gemfile") && exact_names.contains("config.ru") {
        return Some("Ruby (Rack)");
    }

    for (target_ext, label) in CONFIG_EXTENSIONS {
        if extensions.contains(target_ext) {
            return Some(label);
        }
    }

    None
}

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
pub fn detect_from_process(process_name: &str) -> Option<&'static str> {
    let name = process_name.strip_suffix(".exe").unwrap_or(process_name);
    PROCESS_MAP
        .iter()
        .find(|(key, _)| name.eq_ignore_ascii_case(key))
        .map(|(_, label)| *label)
}

/// Detect the app label for a port entry using all available information.
///
/// Priority: Docker image > config file > process name.
#[must_use]
pub fn detect(
    container: Option<&ContainerInfo>,
    project_root: Option<&Path>,
    process_name: &str,
) -> Option<&'static str> {
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
    fn image_valkey() {
        let info = ContainerInfo {
            name: "cache".to_string(),
            image: "valkey/valkey:8-alpine".to_string(),
        };
        assert_eq!(detect_from_image(&info), Some("Valkey"));
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
    fn image_node_exporter_not_nodejs() {
        let info = ContainerInfo {
            name: "metrics".to_string(),
            image: "prom/node-exporter:latest".to_string(),
        };
        assert_eq!(
            detect_from_image(&info),
            None,
            "node-exporter should not match Node.js"
        );
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
    fn image_dotnet_microsoft_registry() {
        let info = ContainerInfo {
            name: "api".to_string(),
            image: "mcr.microsoft.com/dotnet/aspnet:8.0".to_string(),
        };
        assert_eq!(detect_from_image(&info), Some(".NET"));
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
    fn config_flask() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("app.py"), "").unwrap();
        assert_eq!(detect_from_config(dir.path()), Some("Flask"));
    }

    #[test]
    fn config_rack_requires_gemfile() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("config.ru"), "").unwrap();
        assert_eq!(detect_from_config(dir.path()), None);
    }

    #[test]
    fn config_rack_when_gemfile_and_config_ru_exist() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("Gemfile"), "").unwrap();
        fs::write(dir.path().join("config.ru"), "").unwrap();
        assert_eq!(detect_from_config(dir.path()), Some("Ruby (Rack)"));
    }

    #[test]
    fn config_exact_match_does_not_overmatch_backup_file() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("Cargo.toml.bak"), "").unwrap();
        assert_eq!(detect_from_config(dir.path()), None);
    }

    #[test]
    fn config_exact_match_does_not_overmatch_renamed_script() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("manage.py.old"), "").unwrap();
        assert_eq!(detect_from_config(dir.path()), None);
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
    fn process_case_insensitive() {
        assert_eq!(detect_from_process("Nginx"), Some("Nginx"));
        assert_eq!(detect_from_process("POSTGRES"), Some("PostgreSQL"));
        assert_eq!(detect_from_process("Node"), Some("Node.js"));
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
        assert_eq!(result, Some("PostgreSQL"));
    }

    #[test]
    fn combined_config_wins_over_process() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("next.config.js"), "").unwrap();
        let result = detect(None, Some(dir.path()), "node");
        assert_eq!(result, Some("Next.js"));
    }

    #[test]
    fn combined_falls_through_to_process() {
        let result = detect(None, None, "postgres");
        assert_eq!(result, Some("PostgreSQL"));
    }

    #[test]
    fn combined_all_none() {
        let result = detect(None, None, "svchost");
        assert!(result.is_none());
    }
}
