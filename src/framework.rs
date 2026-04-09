//! # Framework/app detection
//!
//! Identifies the technology behind a port using three strategies:
//! Docker image name, config file detection, and process name matching.

use std::path::Path;

use crate::docker::ContainerInfo;

/// Detect app label from a Docker container's image name.
///
/// Matches the base image name (before the colon tag) against known patterns.
#[must_use]
pub fn detect_from_image(info: &ContainerInfo) -> Option<&'static str> {
    let image = info.image.split('/').next_back().unwrap_or(&info.image);
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
        // Exact match to avoid false positives (e.g. "node-exporter").
        "node" => Some("Node.js"),
        s if s.starts_with("python") => Some("Python"),
        s if s.starts_with("ruby") => Some("Ruby"),
        // Exact match to avoid false positives (e.g. "golang-migrate").
        "golang" | "go" => Some("Go"),
        // Exact match to avoid false positives (e.g. "rust-analyzer").
        "rust" => Some("Rust"),
        s if s.starts_with("openjdk") || s.starts_with("eclipse-temurin") => Some("Java"),
        s if s.starts_with("dotnet") => Some(".NET"),
        _ => None,
    }
}

/// Config file patterns checked inside a project root directory.
/// More specific patterns (e.g. `next.config`) come before generic ones.
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
const CONFIG_EXTENSIONS: &[(&str, &str)] = &[("csproj", ".NET"), ("fsproj", ".NET (F#)")];

/// Detect app label by scanning config files in a project root.
pub fn detect_from_config(project_root: &Path) -> Option<&'static str> {
    let Ok(entries) = std::fs::read_dir(project_root) else {
        return None;
    };

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
        if filenames
            .iter()
            .any(|f| f.rsplit('.').next().is_some_and(|e| e == *ext))
        {
            return Some(label);
        }
    }

    None
}

/// Detect app label from a process executable name.
#[must_use]
pub fn detect_from_process(process_name: &str) -> Option<&'static str> {
    let name = process_name.strip_suffix(".exe").unwrap_or(process_name);
    let lower = name.to_ascii_lowercase();

    match lower.as_str() {
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

/// Detect the app label for a port entry using all available information.
///
/// Priority: Docker image > config file > process name.
#[must_use]
pub fn detect(
    container: Option<&ContainerInfo>,
    project_root: Option<&Path>,
    process_name: &str,
) -> Option<String> {
    if let Some(info) = container
        && let Some(label) = detect_from_image(info)
    {
        return Some(label.to_string());
    }

    if let Some(root) = project_root
        && let Some(label) = detect_from_config(root)
    {
        return Some(label.to_string());
    }

    detect_from_process(process_name).map(ToString::to_string)
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
