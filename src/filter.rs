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

/// Check whether a port entry is considered developer-relevant.
///
/// An entry is relevant if collection already attached a project or app label.
///
/// Because [`crate::collector::build_entry`] calls
/// `framework::detect` -- which includes a process-name fallback --
/// the `app` field is already populated when the process is known.
/// Checking `project` or `app` alone is sufficient; a redundant
/// re-scan of the process map is not needed.
const fn is_relevant(entry: &PortEntry) -> bool {
    entry.project.is_some() || entry.app.is_some()
}

/// Apply the given filter options to a collection of entries.
///
/// Filters the input vector in place and returns the surviving entries.
/// An explicit port query bypasses the developer-relevance filter so
/// `--port` never hides a matching socket just because the process is not
/// recognized.
#[must_use]
pub fn apply(mut entries: Vec<PortEntry>, opts: &FilterOptions) -> Vec<PortEntry> {
    let bypass_relevance = opts.show_all || opts.port.is_some();

    entries.retain(|e| {
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
        if !bypass_relevance && !is_relevant(e) {
            return false;
        }
        true
    });

    entries
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use super::*;

    fn make_entry(port: u16, proto: Protocol, state: State) -> PortEntry {
        PortEntry {
            port,
            local_addr: IpAddr::V4(Ipv4Addr::LOCALHOST),
            proto,
            state,
            pid: 1234,
            process: "test".into(),
            user: "user".into(),
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
        let result = apply(entries, &opts);
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
        let result = apply(entries, &opts);
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
        let result = apply(entries, &opts);
        assert_eq!(
            result.len(),
            1,
            "port filter should match exactly one entry"
        );
        assert_eq!(result[0].port, 443);
    }

    #[test]
    fn port_filter_bypasses_relevance_filter() {
        let entries = vec![make_entry(8080, Protocol::Tcp, State::Listen)];
        let opts = FilterOptions {
            tcp_only: false,
            udp_only: false,
            listen_only: false,
            port: Some(8080),
            show_all: false,
        };

        let result = apply(entries, &opts);
        assert_eq!(
            result.len(),
            1,
            "explicit port queries should bypass relevance filtering"
        );
        assert_eq!(result[0].port, 8080, "matching port should remain visible");
    }

    #[test]
    fn listen_only_excludes_udp() {
        let entries = vec![
            make_entry(80, Protocol::Tcp, State::Listen),
            make_entry(443, Protocol::Tcp, State::Established),
            make_entry(53, Protocol::Udp, State::NotApplicable),
        ];
        let opts = FilterOptions {
            tcp_only: false,
            udp_only: false,
            listen_only: true,
            port: None,
            show_all: true,
        };
        let result = apply(entries, &opts);
        assert_eq!(
            result.len(),
            1,
            "listen_only should keep only LISTEN TCP sockets"
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
        let result = apply(entries, &opts);
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
        let result = apply(entries, &opts);
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
        let result = apply(entries, &opts);
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
        let result = apply(entries, &opts);
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
        let result = apply(entries, &default_filter());
        assert!(
            result.is_empty(),
            "unknown process 'test' should be filtered out"
        );
    }

    #[test]
    fn relevance_filter_keeps_entry_with_app_from_known_process() {
        let mut entry = make_entry(3000, Protocol::Tcp, State::Listen);
        entry.process = "node".into();
        // The collector populates `app` via framework::detect for known processes.
        entry.app = Some("Node.js".into());
        let result = apply(vec![entry], &default_filter());
        assert_eq!(
            result.len(),
            1,
            "entry with app label from node should pass"
        );
    }

    #[test]
    fn relevance_filter_keeps_entry_with_project() {
        let mut entry = make_entry(8080, Protocol::Tcp, State::Listen);
        entry.project = Some("my-app".to_string());
        let result = apply(vec![entry], &default_filter());
        assert_eq!(result.len(), 1, "entry with project should pass");
    }

    #[test]
    fn relevance_filter_keeps_entry_with_app() {
        let mut entry = make_entry(5432, Protocol::Tcp, State::Listen);
        entry.app = Some("PostgreSQL".into());
        let result = apply(vec![entry], &default_filter());
        assert_eq!(result.len(), 1, "entry with app label should pass");
    }

    #[test]
    fn show_all_bypasses_relevance() {
        let entries = vec![make_entry(12345, Protocol::Tcp, State::Listen)];
        let opts = FilterOptions {
            show_all: true,
            ..default_filter()
        };
        let result = apply(entries, &opts);
        assert_eq!(result.len(), 1, "show_all should bypass relevance filter");
    }

    #[test]
    fn relevance_filter_recognizes_app_from_exe_suffix() {
        let mut entry = make_entry(80, Protocol::Tcp, State::Listen);
        entry.process = "nginx.exe".into();
        // The collector populates `app` via framework::detect for known processes.
        entry.app = Some("Nginx".into());
        let result = apply(vec![entry], &default_filter());
        assert_eq!(result.len(), 1, "entry with app from nginx.exe should pass");
    }

    #[test]
    fn relevance_filter_recognizes_app_from_capitalized_name() {
        let mut entry = make_entry(3000, Protocol::Tcp, State::Listen);
        entry.process = "Python".into();
        // The collector populates `app` via framework::detect for known processes.
        entry.app = Some("Python".into());
        let result = apply(vec![entry], &default_filter());
        assert_eq!(result.len(), 1, "entry with app from Python should pass");
    }
}
