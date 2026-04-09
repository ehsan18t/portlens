//! # Filter engine
//!
//! Applies user-specified CLI filters to the collected port entries before
//! display.

use crate::types::{PortEntry, Protocol, State};

/// Options controlling which entries pass through the filter.
pub struct FilterOptions {
    /// Show only TCP sockets.
    pub tcp_only: bool,
    /// Show only UDP sockets.
    pub udp_only: bool,
    /// Show only sockets in LISTEN state (TCP only).
    pub listen_only: bool,
    /// Filter to a specific port number.
    pub port: Option<u16>,
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
    fn listen_only_excludes_non_listen() {
        let entries = vec![
            make_entry(80, Protocol::Tcp, State::Listen),
            make_entry(8080, Protocol::Tcp, State::NotApplicable),
            make_entry(53, Protocol::Udp, State::NotApplicable),
        ];
        let opts = FilterOptions {
            tcp_only: false,
            udp_only: false,
            listen_only: true,
            port: None,
        };
        let result = apply(&entries, &opts);
        assert_eq!(
            result.len(),
            1,
            "listen_only should exclude non-LISTEN entries"
        );
        assert_eq!(result[0].state, State::Listen);
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
        };
        let result = apply(&entries, &opts);
        assert!(result.is_empty(), "empty input should return empty output");
    }
}
