//! # Filter engine
//!
//! Applies user-specified CLI filters to the collected port entries before
//! display.

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use crate::types::{PortEntry, Protocol, State, strip_windows_exe_suffix};
use log::debug;

/// A port filter that matches either a single port or an inclusive range.
///
/// Parsed from a CLI string like `"443"` (single) or `"3000-4000"` (range).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortFilter {
    /// Match exactly one port number.
    Single(u16),
    /// Match any port in the inclusive range `[start, end]`.
    Range {
        /// First port in the range (inclusive).
        start: u16,
        /// Last port in the range (inclusive).
        end: u16,
    },
}

impl PortFilter {
    /// Returns `true` if the given port number satisfies this filter.
    #[must_use]
    pub const fn matches(self, port: u16) -> bool {
        match self {
            Self::Single(p) => port == p,
            Self::Range { start, end } => port >= start && port <= end,
        }
    }

    /// Returns `true` if this filter includes port 0 (which is invalid).
    #[must_use]
    pub const fn contains_zero(self) -> bool {
        match self {
            Self::Single(p) => p == 0,
            Self::Range { start, .. } => start == 0,
        }
    }
}

impl fmt::Display for PortFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Single(p) => write!(f, "{p}"),
            Self::Range { start, end } => write!(f, "{start}-{end}"),
        }
    }
}

impl FromStr for PortFilter {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some((start_str, end_str)) = s.split_once('-') {
            let start: u16 = start_str.parse().map_err(|_| {
                format!("invalid range start: '{start_str}' is not a valid port number (1-65535)")
            })?;
            let end: u16 = end_str.parse().map_err(|_| {
                format!("invalid range end: '{end_str}' is not a valid port number (1-65535)")
            })?;
            if start > end {
                return Err(format!(
                    "invalid port range: start ({start}) must not exceed end ({end})"
                ));
            }
            Ok(Self::Range { start, end })
        } else {
            let port: u16 = s
                .parse()
                .map_err(|_| format!("'{s}' is not a valid port number (1-65535)"))?;
            Ok(Self::Single(port))
        }
    }
}

/// Options controlling which entries pass through the filter.
#[allow(clippy::struct_excessive_bools)]
pub struct FilterOptions {
    /// Show only TCP sockets.
    pub tcp_only: bool,
    /// Show only UDP sockets.
    pub udp_only: bool,
    /// Show only sockets in LISTEN state (TCP only).
    pub listen_only: bool,
    /// Filter to a specific port number or range.
    pub port: Option<PortFilter>,
    /// Filter by exact process name (case-insensitive, `.exe` stripped).
    pub process: Option<String>,
    /// Filter by substring match in process name (case-insensitive).
    pub grep: Option<String>,
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

/// Check whether a process name matches the `--process` filter exactly.
///
/// Strips the `.exe` suffix (if present) and compares case-insensitively.
fn matches_process_name(process: &str, filter: &str) -> bool {
    strip_windows_exe_suffix(process).eq_ignore_ascii_case(filter)
}

/// Normalize a grep pattern once using ASCII case folding.
///
/// Returns a borrowed string when the pattern is already free of
/// uppercase ASCII bytes so the common CLI path avoids allocation.
fn normalize_grep_pattern(pattern: &str) -> Cow<'_, str> {
    if pattern.as_bytes().iter().any(u8::is_ascii_uppercase) {
        Cow::Owned(pattern.to_ascii_lowercase())
    } else {
        Cow::Borrowed(pattern)
    }
}

/// Check whether a process name contains the `--grep` substring.
///
/// Compares case-insensitively against the full process name (including
/// any `.exe` suffix). The caller supplies an ASCII-case-folded pattern,
/// so only the process bytes need per-character lowering.
///
/// Optimizations over a naive `to_ascii_lowercase().contains()`:
///
/// 1. **Zero heap allocation** - operates on raw byte slices.
/// 2. **Length guard** - returns immediately when the pattern is longer
///    than the process name.
/// 3. **First-byte pre-check** - skips ~96% of window positions by
///    checking if the first byte matches before comparing the rest.
///    For ASCII data the first byte matches with probability ~1/26,
///    avoiding the inner loop for most positions.
fn contains_process_pattern(process: &str, pattern: &str) -> bool {
    let pattern_bytes = pattern.as_bytes();
    if pattern_bytes.is_empty() {
        return true;
    }

    let process_bytes = process.as_bytes();
    if process_bytes.len() < pattern_bytes.len() {
        return false;
    }

    let first = pattern_bytes[0];

    process_bytes.windows(pattern_bytes.len()).any(|window| {
        window[0].to_ascii_lowercase() == first
            && window[1..]
                .iter()
                .zip(&pattern_bytes[1..])
                .all(|(a, b)| a.to_ascii_lowercase() == *b)
    })
}

/// Apply the given filter options to a collection of entries.
///
/// Filters the input vector in place and returns the surviving entries.
/// An explicit port, process, or grep query bypasses the developer-relevance
/// filter so targeted searches never hide a matching socket just because the
/// process is not recognized.
///
/// Process-name and grep filters run as separate `retain` passes so
/// the hot-path closure (protocol/state/port/relevance) stays small
/// and does not pay code-generation overhead for the string-matching
/// functions when those filters are inactive.
#[must_use]
pub fn apply(mut entries: Vec<PortEntry>, opts: &FilterOptions) -> Vec<PortEntry> {
    let original_len = entries.len();
    let process_filter = opts.process.as_deref().map(strip_windows_exe_suffix);
    let grep_pattern = opts.grep.as_deref().map(normalize_grep_pattern);
    let bypass_relevance =
        opts.show_all || opts.port.is_some() || process_filter.is_some() || grep_pattern.is_some();

    debug!(
        "applying filters: input_entries={} tcp_only={} udp_only={} listen_only={} port={:?} process={:?} grep={:?} show_all={} bypass_relevance={}",
        original_len,
        opts.tcp_only,
        opts.udp_only,
        opts.listen_only,
        opts.port,
        process_filter,
        grep_pattern.as_deref(),
        opts.show_all,
        bypass_relevance
    );

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
        if let Some(ref port_filter) = opts.port
            && !port_filter.matches(e.port)
        {
            return false;
        }
        if !bypass_relevance && !is_relevant(e) {
            return false;
        }
        true
    });

    if let Some(name) = process_filter {
        entries.retain(|e| matches_process_name(&e.process, name));
    }

    if let Some(pattern) = grep_pattern.as_deref() {
        entries.retain(|e| contains_process_pattern(&e.process, pattern));
    }

    debug!(
        "filtering complete: input_entries={} output_entries={}",
        original_len,
        entries.len()
    );
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

    fn default_filter() -> FilterOptions {
        FilterOptions {
            tcp_only: false,
            udp_only: false,
            listen_only: false,
            port: None,
            process: None,
            grep: None,
            show_all: false,
        }
    }

    fn show_all_filter() -> FilterOptions {
        FilterOptions {
            show_all: true,
            ..default_filter()
        }
    }

    fn port_filter_option(port: u16) -> FilterOptions {
        FilterOptions {
            port: Some(PortFilter::Single(port)),
            ..show_all_filter()
        }
    }

    fn make_entries(entries: &[(u16, Protocol, State)]) -> Vec<PortEntry> {
        entries
            .iter()
            .copied()
            .map(|(port, proto, state)| make_entry(port, proto, state))
            .collect()
    }

    fn tcp_and_udp_entries() -> Vec<PortEntry> {
        make_entries(&[
            (80, Protocol::Tcp, State::Listen),
            (53, Protocol::Udp, State::NotApplicable),
        ])
    }

    fn assert_single_entry(
        result: &[PortEntry],
        expected_port: u16,
        expected_proto: Protocol,
        message: &str,
    ) {
        assert_eq!(result.len(), 1, "{message}");
        assert_eq!(result[0].port, expected_port, "{message}");
        assert_eq!(result[0].proto, expected_proto, "{message}");
    }

    fn assert_relevance_passes(entry: PortEntry, message: &str) {
        let result = apply(vec![entry], &default_filter());
        assert_eq!(result.len(), 1, "{message}");
    }

    #[test]
    fn no_filters_passes_all() {
        let result = apply(tcp_and_udp_entries(), &show_all_filter());
        assert_eq!(result.len(), 2, "no filters should pass all entries");
    }

    #[test]
    fn tcp_only_filter() {
        let opts = FilterOptions {
            tcp_only: true,
            ..show_all_filter()
        };
        let result = apply(tcp_and_udp_entries(), &opts);
        assert_single_entry(
            &result,
            80,
            Protocol::Tcp,
            "tcp_only should exclude UDP entries",
        );
    }

    #[test]
    fn port_filter() {
        let result = apply(
            make_entries(&[
                (80, Protocol::Tcp, State::Listen),
                (443, Protocol::Tcp, State::Listen),
            ]),
            &port_filter_option(443),
        );
        assert_single_entry(
            &result,
            443,
            Protocol::Tcp,
            "port filter should match exactly one entry",
        );
    }

    #[test]
    fn port_filter_bypasses_relevance_filter() {
        let entries = vec![make_entry(8080, Protocol::Tcp, State::Listen)];
        let opts = FilterOptions {
            tcp_only: false,
            udp_only: false,
            listen_only: false,
            port: Some(PortFilter::Single(8080)),
            process: None,
            grep: None,
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
            listen_only: true,
            ..show_all_filter()
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
            udp_only: true,
            ..show_all_filter()
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
            port: Some(PortFilter::Single(80)),
            ..show_all_filter()
        };
        let result = apply(entries, &opts);
        assert_single_entry(
            &result,
            80,
            Protocol::Tcp,
            "combined tcp+port filter should match exactly one entry",
        );
    }

    #[test]
    fn no_matches_returns_empty() {
        let result = apply(tcp_and_udp_entries(), &port_filter_option(9999));
        assert!(result.is_empty(), "non-matching port should return empty");
    }

    #[test]
    fn empty_input_returns_empty() {
        let entries: Vec<PortEntry> = vec![];
        let opts = FilterOptions {
            tcp_only: true,
            ..show_all_filter()
        };
        let result = apply(entries, &opts);
        assert!(result.is_empty(), "empty input should return empty output");
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
        assert_relevance_passes(entry, "entry with app label from node should pass");
    }

    #[test]
    fn relevance_filter_keeps_entry_with_project() {
        let mut entry = make_entry(8080, Protocol::Tcp, State::Listen);
        entry.project = Some("my-app".to_string());
        assert_relevance_passes(entry, "entry with project should pass");
    }

    #[test]
    fn relevance_filter_keeps_entry_with_app() {
        let mut entry = make_entry(5432, Protocol::Tcp, State::Listen);
        entry.app = Some("PostgreSQL".into());
        assert_relevance_passes(entry, "entry with app label should pass");
    }

    #[test]
    fn show_all_bypasses_relevance() {
        let entries = vec![make_entry(12345, Protocol::Tcp, State::Listen)];
        let opts = show_all_filter();
        let result = apply(entries, &opts);
        assert_eq!(result.len(), 1, "show_all should bypass relevance filter");
    }

    #[test]
    fn relevance_filter_recognizes_app_from_exe_suffix() {
        let mut entry = make_entry(80, Protocol::Tcp, State::Listen);
        entry.process = "nginx.exe".into();
        // The collector populates `app` via framework::detect for known processes.
        entry.app = Some("Nginx".into());
        assert_relevance_passes(entry, "entry with app from nginx.exe should pass");
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

    // ── PortFilter parsing tests ────────────────────────────────────

    #[test]
    fn parse_single_port() {
        let filter: PortFilter = "443".parse().expect("valid single port");
        assert_eq!(
            filter,
            PortFilter::Single(443),
            "single port should parse correctly"
        );
    }

    #[test]
    fn parse_port_range() {
        let filter: PortFilter = "3000-4000".parse().expect("valid port range");
        assert_eq!(
            filter,
            PortFilter::Range {
                start: 3000,
                end: 4000
            },
            "port range should parse correctly"
        );
    }

    #[test]
    fn parse_single_port_boundary() {
        let filter: PortFilter = "65535".parse().expect("max port value");
        assert_eq!(
            filter,
            PortFilter::Single(65535),
            "max u16 should parse as single port"
        );
    }

    #[test]
    fn parse_same_start_end_range() {
        let filter: PortFilter = "3000-3000".parse().expect("same start and end");
        assert_eq!(
            filter,
            PortFilter::Range {
                start: 3000,
                end: 3000
            },
            "same start and end should be a valid range"
        );
    }

    #[test]
    fn parse_full_range() {
        let filter: PortFilter = "1-65535".parse().expect("full port range");
        assert_eq!(
            filter,
            PortFilter::Range {
                start: 1,
                end: 65535
            },
            "full u16 range should parse"
        );
    }

    #[test]
    fn parse_rejects_reversed_range() {
        let err = "5000-3000"
            .parse::<PortFilter>()
            .expect_err("reversed range");
        assert!(
            err.contains("must not exceed"),
            "reversed range should report start > end: {err}"
        );
    }

    #[test]
    fn parse_rejects_non_numeric() {
        let err = "abc".parse::<PortFilter>().expect_err("non-numeric value");
        assert!(
            err.contains("not a valid port number"),
            "non-numeric should report parsing failure: {err}"
        );
    }

    #[test]
    fn parse_rejects_empty_range_start() {
        let err = "-4000"
            .parse::<PortFilter>()
            .expect_err("empty range start");
        assert!(
            err.contains("not a valid port number"),
            "empty range start should report parsing failure: {err}"
        );
    }

    #[test]
    fn parse_rejects_empty_range_end() {
        let err = "3000-".parse::<PortFilter>().expect_err("empty range end");
        assert!(
            err.contains("not a valid port number"),
            "empty range end should report parsing failure: {err}"
        );
    }

    #[test]
    fn parse_rejects_overflow() {
        let err = "70000".parse::<PortFilter>().expect_err("port overflow");
        assert!(
            err.contains("not a valid port number"),
            "overflow should report parsing failure: {err}"
        );
    }

    // ── PortFilter::matches tests ───────────────────────────────────

    #[test]
    fn single_matches_exact_port() {
        let filter = PortFilter::Single(8080);
        assert!(
            filter.matches(8080),
            "single filter should match exact port"
        );
        assert!(
            !filter.matches(8081),
            "single filter should not match other ports"
        );
    }

    #[test]
    fn range_matches_inclusive_boundaries() {
        let filter = PortFilter::Range {
            start: 3000,
            end: 3005,
        };
        assert!(filter.matches(3000), "range should match start boundary");
        assert!(filter.matches(3005), "range should match end boundary");
        assert!(filter.matches(3003), "range should match interior port");
        assert!(!filter.matches(2999), "range should not match below start");
        assert!(!filter.matches(3006), "range should not match above end");
    }

    #[test]
    fn same_port_range_matches_only_that_port() {
        let filter = PortFilter::Range {
            start: 443,
            end: 443,
        };
        assert!(
            filter.matches(443),
            "degenerate range should match the port"
        );
        assert!(
            !filter.matches(444),
            "degenerate range should not match adjacent"
        );
    }

    // ── PortFilter::contains_zero tests ─────────────────────────────

    #[test]
    fn contains_zero_single() {
        assert!(
            PortFilter::Single(0).contains_zero(),
            "single 0 should contain zero"
        );
        assert!(
            !PortFilter::Single(1).contains_zero(),
            "single 1 should not contain zero"
        );
    }

    #[test]
    fn contains_zero_range() {
        let filter = PortFilter::Range { start: 0, end: 100 };
        assert!(
            filter.contains_zero(),
            "range starting at 0 should contain zero"
        );
        let filter = PortFilter::Range { start: 1, end: 100 };
        assert!(
            !filter.contains_zero(),
            "range starting at 1 should not contain zero"
        );
    }

    // ── PortFilter::Display tests ───────────────────────────────────

    #[test]
    fn display_single_port() {
        assert_eq!(
            PortFilter::Single(443).to_string(),
            "443",
            "single port display"
        );
    }

    #[test]
    fn display_range() {
        let filter = PortFilter::Range {
            start: 3000,
            end: 4000,
        };
        assert_eq!(filter.to_string(), "3000-4000", "range display");
    }

    // ── Filter integration with ranges ──────────────────────────────

    #[test]
    fn range_filter_matches_entries_in_range() {
        let entries = make_entries(&[
            (2999, Protocol::Tcp, State::Listen),
            (3000, Protocol::Tcp, State::Listen),
            (3003, Protocol::Tcp, State::Listen),
            (3005, Protocol::Tcp, State::Listen),
            (3006, Protocol::Tcp, State::Listen),
        ]);
        let opts = FilterOptions {
            port: Some(PortFilter::Range {
                start: 3000,
                end: 3005,
            }),
            ..show_all_filter()
        };
        let result = apply(entries, &opts);
        assert_eq!(result.len(), 3, "only ports 3000, 3003, 3005 should match");
        assert_eq!(result[0].port, 3000, "first match should be 3000");
        assert_eq!(result[1].port, 3003, "second match should be 3003");
        assert_eq!(result[2].port, 3005, "third match should be 3005");
    }

    #[test]
    fn range_filter_bypasses_relevance() {
        let entries = vec![make_entry(3001, Protocol::Tcp, State::Listen)];
        let opts = FilterOptions {
            tcp_only: false,
            udp_only: false,
            listen_only: false,
            port: Some(PortFilter::Range {
                start: 3000,
                end: 3005,
            }),
            process: None,
            grep: None,
            show_all: false,
        };
        let result = apply(entries, &opts);
        assert_eq!(result.len(), 1, "port range should bypass relevance filter");
    }

    #[test]
    fn range_filter_combined_with_tcp_only() {
        let entries = vec![
            make_entry(3000, Protocol::Tcp, State::Listen),
            make_entry(3001, Protocol::Udp, State::NotApplicable),
            make_entry(3002, Protocol::Tcp, State::Listen),
        ];
        let opts = FilterOptions {
            tcp_only: true,
            port: Some(PortFilter::Range {
                start: 3000,
                end: 3005,
            }),
            ..show_all_filter()
        };
        let result = apply(entries, &opts);
        assert_eq!(result.len(), 2, "range + tcp_only should exclude UDP");
        assert!(
            result.iter().all(|e| e.proto == Protocol::Tcp),
            "all results should be TCP"
        );
    }

    // ── Process name filter tests ───────────────────────────────────

    fn make_process_entry(port: u16, process: &str) -> PortEntry {
        PortEntry {
            port,
            local_addr: IpAddr::V4(Ipv4Addr::LOCALHOST),
            proto: Protocol::Tcp,
            state: State::Listen,
            pid: 1234,
            process: process.into(),
            user: "user".into(),
            project: None,
            app: None,
            uptime_secs: None,
        }
    }

    #[test]
    fn process_filter_exact_match() {
        let entries = vec![
            make_process_entry(80, "nginx"),
            make_process_entry(3000, "node"),
            make_process_entry(5432, "postgres"),
        ];
        let opts = FilterOptions {
            process: Some("node".to_string()),
            ..default_filter()
        };
        let result = apply(entries, &opts);
        assert_eq!(result.len(), 1, "process filter should match exactly one");
        assert_eq!(result[0].port, 3000, "matched entry should be node on 3000");
    }

    #[test]
    fn process_filter_strips_exe_suffix() {
        let entries = vec![
            make_process_entry(80, "node.exe"),
            make_process_entry(443, "nginx"),
        ];
        let opts = FilterOptions {
            process: Some("node".to_string()),
            ..default_filter()
        };
        let result = apply(entries, &opts);
        assert_eq!(
            result.len(),
            1,
            "process filter should match node.exe when searching for node"
        );
        assert_eq!(result[0].port, 80, "matched entry should be node.exe on 80");
    }

    #[test]
    fn process_filter_accepts_exe_suffix_in_filter() {
        let entries = vec![make_process_entry(80, "node.exe")];
        let opts = FilterOptions {
            process: Some("node.EXE".to_string()),
            ..default_filter()
        };
        let result = apply(entries, &opts);
        assert_eq!(
            result.len(),
            1,
            "process filter should also accept a .exe suffix in the filter value"
        );
    }

    #[test]
    fn process_filter_case_insensitive() {
        let entries = vec![
            make_process_entry(80, "Code.exe"),
            make_process_entry(443, "nginx"),
        ];
        let opts = FilterOptions {
            process: Some("code".to_string()),
            ..default_filter()
        };
        let result = apply(entries, &opts);
        assert_eq!(
            result.len(),
            1,
            "process filter should match Code.exe case-insensitively"
        );
    }

    #[test]
    fn process_filter_no_match() {
        let entries = vec![make_process_entry(80, "nginx")];
        let opts = FilterOptions {
            process: Some("node".to_string()),
            ..default_filter()
        };
        let result = apply(entries, &opts);
        assert!(
            result.is_empty(),
            "process filter should return empty on no match"
        );
    }

    #[test]
    fn process_filter_bypasses_relevance() {
        let entries = vec![make_process_entry(12345, "unknown-daemon")];
        let opts = FilterOptions {
            process: Some("unknown-daemon".to_string()),
            ..default_filter()
        };
        let result = apply(entries, &opts);
        assert_eq!(
            result.len(),
            1,
            "process filter should bypass relevance filter"
        );
    }

    #[test]
    fn process_filter_does_not_match_substring() {
        let entries = vec![make_process_entry(80, "com.docker.backend")];
        let opts = FilterOptions {
            process: Some("docker".to_string()),
            ..default_filter()
        };
        let result = apply(entries, &opts);
        assert!(
            result.is_empty(),
            "process filter should not match substring of process name"
        );
    }

    // ── Grep filter tests ───────────────────────────────────────────

    #[test]
    fn grep_filter_matches_substring() {
        let entries = vec![
            make_process_entry(80, "com.docker.backend"),
            make_process_entry(443, "nginx"),
        ];
        let opts = FilterOptions {
            grep: Some("docker".to_string()),
            ..default_filter()
        };
        let result = apply(entries, &opts);
        assert_eq!(result.len(), 1, "grep filter should match substring");
        assert_eq!(
            result[0].port, 80,
            "matched entry should be com.docker.backend"
        );
    }

    #[test]
    fn grep_filter_case_insensitive() {
        let entries = vec![make_process_entry(80, "Code.exe")];
        let opts = FilterOptions {
            grep: Some("code".to_string()),
            ..default_filter()
        };
        let result = apply(entries, &opts);
        assert_eq!(
            result.len(),
            1,
            "grep filter should match case-insensitively"
        );
    }

    #[test]
    fn grep_filter_uppercase_pattern_is_normalized() {
        let entries = vec![make_process_entry(80, "Code.exe")];
        let opts = FilterOptions {
            grep: Some("CODE".to_string()),
            ..default_filter()
        };
        let result = apply(entries, &opts);
        assert_eq!(
            result.len(),
            1,
            "grep filter should not depend on CLI-side lowercase normalization"
        );
    }

    #[test]
    fn grep_filter_matches_full_name() {
        let entries = vec![make_process_entry(80, "nginx")];
        let opts = FilterOptions {
            grep: Some("nginx".to_string()),
            ..default_filter()
        };
        let result = apply(entries, &opts);
        assert_eq!(
            result.len(),
            1,
            "grep filter should match when pattern equals full name"
        );
    }

    #[test]
    fn grep_filter_no_match() {
        let entries = vec![make_process_entry(80, "nginx")];
        let opts = FilterOptions {
            grep: Some("apache".to_string()),
            ..default_filter()
        };
        let result = apply(entries, &opts);
        assert!(
            result.is_empty(),
            "grep filter should return empty on no match"
        );
    }

    #[test]
    fn grep_filter_bypasses_relevance() {
        let entries = vec![make_process_entry(12345, "some-random-daemon")];
        let opts = FilterOptions {
            grep: Some("random".to_string()),
            ..default_filter()
        };
        let result = apply(entries, &opts);
        assert_eq!(
            result.len(),
            1,
            "grep filter should bypass relevance filter"
        );
    }

    #[test]
    fn grep_filter_matches_exe_suffix() {
        let entries = vec![make_process_entry(80, "myapp.exe")];
        let opts = FilterOptions {
            grep: Some("exe".to_string()),
            ..default_filter()
        };
        let result = apply(entries, &opts);
        assert_eq!(
            result.len(),
            1,
            "grep filter should match against the full process name including .exe"
        );
    }

    #[test]
    fn process_filter_combined_with_port() {
        let entries = vec![
            make_process_entry(80, "node"),
            make_process_entry(3000, "node"),
            make_process_entry(5432, "postgres"),
        ];
        let opts = FilterOptions {
            port: Some(PortFilter::Single(3000)),
            process: Some("node".to_string()),
            ..default_filter()
        };
        let result = apply(entries, &opts);
        assert_eq!(
            result.len(),
            1,
            "combined port + process should narrow to one entry"
        );
        assert_eq!(result[0].port, 3000, "matched entry should be node on 3000");
    }

    #[test]
    fn grep_filter_combined_with_tcp_only() {
        let mut tcp_entry = make_process_entry(80, "com.docker.backend");
        tcp_entry.proto = Protocol::Tcp;
        let mut udp_entry = make_process_entry(53, "com.docker.backend");
        udp_entry.proto = Protocol::Udp;
        let opts = FilterOptions {
            tcp_only: true,
            grep: Some("docker".to_string()),
            ..default_filter()
        };
        let result = apply(vec![tcp_entry, udp_entry], &opts);
        assert_eq!(result.len(), 1, "grep + tcp_only should exclude UDP");
        assert_eq!(result[0].proto, Protocol::Tcp, "result should be TCP");
    }
}
