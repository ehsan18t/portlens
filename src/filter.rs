//! # Filter engine
//!
//! Applies user-specified CLI filters to the collected port entries before
//! display.

use crate::types::{PortEntry, Protocol, State};

/// Options controlling which entries pass through the filter.
#[allow(clippy::struct_excessive_bools)]
pub struct FilterOptions {
    /// Show only TCP sockets.
    pub tcp_only: bool,
    /// Show only UDP sockets.
    pub udp_only: bool,
    /// Show only sockets in LISTEN state (TCP only).
    pub listen_only: bool,
    /// Filter to a specific port number.
    pub port: Option<u16>,
    /// When true, bypass the developer-relevance filter and show all ports.
    pub show_all: bool,
}

/// Process names considered developer-relevant for the default filter.
const RELEVANT_PROCESSES: &[&str] = &[
    // Runtimes
    "node",
    "nodejs",
    "python",
    "python3",
    "ruby",
    "java",
    "go",
    "deno",
    "bun",
    "dotnet",
    "php",
    "perl",
    "cargo",
    "rustc",
    "erlang",
    "elixir",
    "beam.smp",
    "dart",
    "swift",
    // Databases
    "postgres",
    "postgresql",
    "mysqld",
    "mysql",
    "mariadbd",
    "mariadb",
    "mongod",
    "mongos",
    "redis-server",
    "redis",
    "valkey-server",
    "valkey",
    "memcached",
    "clickhouse-server",
    "cockroach",
    // Web servers / app servers
    "nginx",
    "apache2",
    "httpd",
    "caddy",
    "traefik",
    "envoy",
    "haproxy",
    "gunicorn",
    "uvicorn",
    // Search/messaging
    "elasticsearch",
    "opensearch",
    "rabbitmq-server",
    "kafka",
    // Dev tools / frameworks
    "webpack",
    "vite",
    "next-server",
    "nuxt",
    "hugo",
    "jekyll",
    "flask",
    "rails",
    "gradle",
    "mvn",
];

/// Check whether a port entry is considered developer-relevant.
///
/// An entry is relevant if it has a detected project, app label, Docker
/// container, or a recognized process name.
fn is_relevant(entry: &PortEntry) -> bool {
    if entry.project.is_some() || entry.app.is_some() {
        return true;
    }

    let name = entry.process.strip_suffix(".exe").unwrap_or(&entry.process);
    let lower = name.to_ascii_lowercase();
    RELEVANT_PROCESSES.contains(&lower.as_str())
}

/// Apply the given filter options to a slice of entries.
///
/// Returns a new `Vec` containing only the entries that match all active
/// filters. Filters are combined with AND semantics.
#[must_use]
pub fn apply(entries: &[PortEntry], opts: &FilterOptions) -> Vec<PortEntry> {
    entries
        .iter()
        .filter(|e| {
            if opts.tcp_only && e.proto != Protocol::Tcp {
                return false;
            }
            if opts.udp_only && e.proto != Protocol::Udp {
                return false;
            }
            if opts.listen_only && e.state != State::Listen {
                return false;
            }
            if let Some(port) = opts.port
                && e.port != port
            {
                return false;
            }
            if !opts.show_all && !is_relevant(e) {
                return false;
            }
            true
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{PortEntry, State};

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

    #[test]
    fn no_filters_passes_all() {
        let entries = vec![
            make_entry(80, Protocol::Tcp, State::Listen),
            make_entry(53, Protocol::Udp, State::NotApplicable),
        ];
        let opts = FilterOptions {
            tcp_only: false,
            udp_only: false,
            listen_only: false,
            port: None,
            show_all: true,
        };
        let result = apply(&entries, &opts);
        assert_eq!(result.len(), 2, "no filters should pass all entries");
    }

    #[test]
    fn tcp_only_filter() {
        let entries = vec![
            make_entry(80, Protocol::Tcp, State::Listen),
            make_entry(53, Protocol::Udp, State::NotApplicable),
        ];
        let opts = FilterOptions {
            tcp_only: true,
            udp_only: false,
            listen_only: false,
            port: None,
            show_all: true,
        };
        let result = apply(&entries, &opts);
        assert_eq!(result.len(), 1, "tcp_only should exclude UDP entries");
        assert_eq!(result[0].proto, Protocol::Tcp);
    }

    #[test]
    fn port_filter() {
        let entries = vec![
            make_entry(80, Protocol::Tcp, State::Listen),
            make_entry(443, Protocol::Tcp, State::Listen),
        ];
        let opts = FilterOptions {
            tcp_only: false,
            udp_only: false,
            listen_only: false,
            port: Some(443),
            show_all: true,
        };
        let result = apply(&entries, &opts);
        assert_eq!(
            result.len(),
            1,
            "port filter should match exactly one entry"
        );
        assert_eq!(result[0].port, 443);
    }

    #[test]
    fn listen_only_excludes_udp() {
        // In practice, the collector assigns Listen to all TCP entries and
        // NotApplicable to all UDP entries (the listeners crate only returns
        // listening sockets). So --listen effectively filters out UDP.
        let entries = vec![
            make_entry(80, Protocol::Tcp, State::Listen),
            make_entry(443, Protocol::Tcp, State::Listen),
            make_entry(53, Protocol::Udp, State::NotApplicable),
        ];
        let opts = FilterOptions {
            tcp_only: false,
            udp_only: false,
            listen_only: true,
            port: None,
            show_all: true,
        };
        let result = apply(&entries, &opts);
        assert_eq!(
            result.len(),
            2,
            "listen_only should keep TCP (LISTEN) and exclude UDP"
        );
        assert!(
            result.iter().all(|e| e.state == State::Listen),
            "all remaining entries should be LISTEN"
        );
    }

    #[test]
    fn udp_only_filter() {
        let entries = vec![
            make_entry(80, Protocol::Tcp, State::Listen),
            make_entry(53, Protocol::Udp, State::NotApplicable),
            make_entry(5353, Protocol::Udp, State::NotApplicable),
        ];
        let opts = FilterOptions {
            tcp_only: false,
            udp_only: true,
            listen_only: false,
            port: None,
            show_all: true,
        };
        let result = apply(&entries, &opts);
        assert_eq!(result.len(), 2, "udp_only should exclude TCP entries");
        assert!(
            result.iter().all(|e| e.proto == Protocol::Udp),
            "all results should be UDP"
        );
    }

    #[test]
    fn combined_tcp_and_port_filter() {
        let entries = vec![
            make_entry(80, Protocol::Tcp, State::Listen),
            make_entry(80, Protocol::Udp, State::NotApplicable),
            make_entry(443, Protocol::Tcp, State::Listen),
        ];
        let opts = FilterOptions {
            tcp_only: true,
            udp_only: false,
            listen_only: false,
            port: Some(80),
            show_all: true,
        };
        let result = apply(&entries, &opts);
        assert_eq!(
            result.len(),
            1,
            "combined tcp+port filter should match exactly one entry"
        );
        assert_eq!(result[0].port, 80);
        assert_eq!(result[0].proto, Protocol::Tcp);
    }

    #[test]
    fn no_matches_returns_empty() {
        let entries = vec![
            make_entry(80, Protocol::Tcp, State::Listen),
            make_entry(53, Protocol::Udp, State::NotApplicable),
        ];
        let opts = FilterOptions {
            tcp_only: false,
            udp_only: false,
            listen_only: false,
            port: Some(9999),
            show_all: true,
        };
        let result = apply(&entries, &opts);
        assert!(result.is_empty(), "non-matching port should return empty");
    }

    #[test]
    fn empty_input_returns_empty() {
        let entries: Vec<PortEntry> = vec![];
        let opts = FilterOptions {
            tcp_only: true,
            udp_only: false,
            listen_only: false,
            port: None,
            show_all: true,
        };
        let result = apply(&entries, &opts);
        assert!(result.is_empty(), "empty input should return empty output");
    }

    fn default_filter() -> FilterOptions {
        FilterOptions {
            tcp_only: false,
            udp_only: false,
            listen_only: false,
            port: None,
            show_all: false,
        }
    }

    #[test]
    fn relevance_filter_hides_unknown_process() {
        let entries = vec![make_entry(12345, Protocol::Tcp, State::Listen)];
        let result = apply(&entries, &default_filter());
        assert!(
            result.is_empty(),
            "unknown process 'test' should be filtered out"
        );
    }

    #[test]
    fn relevance_filter_keeps_known_process() {
        let mut entry = make_entry(3000, Protocol::Tcp, State::Listen);
        entry.process = "node".to_string();
        let result = apply(&[entry], &default_filter());
        assert_eq!(result.len(), 1, "known process 'node' should pass");
    }

    #[test]
    fn relevance_filter_keeps_entry_with_project() {
        let mut entry = make_entry(8080, Protocol::Tcp, State::Listen);
        entry.project = Some("my-app".to_string());
        let result = apply(&[entry], &default_filter());
        assert_eq!(result.len(), 1, "entry with project should pass");
    }

    #[test]
    fn relevance_filter_keeps_entry_with_app() {
        let mut entry = make_entry(5432, Protocol::Tcp, State::Listen);
        entry.app = Some("PostgreSQL".to_string());
        let result = apply(&[entry], &default_filter());
        assert_eq!(result.len(), 1, "entry with app label should pass");
    }

    #[test]
    fn show_all_bypasses_relevance() {
        let entries = vec![make_entry(12345, Protocol::Tcp, State::Listen)];
        let opts = FilterOptions {
            show_all: true,
            ..default_filter()
        };
        let result = apply(&entries, &opts);
        assert_eq!(result.len(), 1, "show_all should bypass relevance filter");
    }

    #[test]
    fn relevance_filter_windows_exe_suffix() {
        let mut entry = make_entry(80, Protocol::Tcp, State::Listen);
        entry.process = "nginx.exe".to_string();
        let result = apply(&[entry], &default_filter());
        assert_eq!(result.len(), 1, "nginx.exe should be recognized");
    }

    #[test]
    fn relevance_filter_case_insensitive() {
        let mut entry = make_entry(3000, Protocol::Tcp, State::Listen);
        entry.process = "Python".to_string();
        let result = apply(&[entry], &default_filter());
        assert_eq!(result.len(), 1, "capitalized 'Python' should match");
    }
}
