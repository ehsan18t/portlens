//! # Filter engine
//!
//! Applies user-specified CLI filters to the collected port entries before
//! display.

use crate::types::{PortEntry, Protocol};

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
            if opts.listen_only && e.state != "LISTEN" {
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
    use crate::types::PortEntry;

    fn make_entry(port: u16, proto: Protocol, state: &str) -> PortEntry {
        PortEntry {
            port,
            proto,
            state: state.to_string(),
            pid: 1234,
            process: "test".to_string(),
            user: "user".to_string(),
        }
    }

    #[test]
    fn no_filters_passes_all() {
        let entries = vec![
            make_entry(80, Protocol::Tcp, "LISTEN"),
            make_entry(53, Protocol::Udp, "-"),
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
            make_entry(80, Protocol::Tcp, "LISTEN"),
            make_entry(53, Protocol::Udp, "-"),
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
            make_entry(80, Protocol::Tcp, "LISTEN"),
            make_entry(443, Protocol::Tcp, "LISTEN"),
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
            make_entry(80, Protocol::Tcp, "LISTEN"),
            make_entry(8080, Protocol::Tcp, "ESTABLISHED"),
            make_entry(53, Protocol::Udp, "-"),
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
        assert_eq!(result[0].state, "LISTEN");
    }
}
