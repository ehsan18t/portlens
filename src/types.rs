//! # Shared types
//!
//! Contains the [`PortEntry`] struct used across all modules.

use std::borrow::Cow;
use std::net::IpAddr;
use std::sync::Arc;

use serde::Serialize;

/// Human-readable app or framework label for an entry.
///
/// Most detections use borrowed string literals, but `Cow` leaves room for
/// future runtime-generated labels without changing the data model.
pub type AppLabel = Cow<'static, str>;

/// Protocol type for a socket entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum Protocol {
    /// Transmission Control Protocol.
    #[serde(rename = "TCP")]
    Tcp,
    /// User Datagram Protocol.
    #[serde(rename = "UDP")]
    Udp,
}

impl std::fmt::Display for Protocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tcp => write!(f, "TCP"),
            Self::Udp => write!(f, "UDP"),
        }
    }
}

/// Connection state for a socket entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum State {
    /// TCP socket in LISTEN state.
    #[serde(rename = "LISTEN")]
    Listen,
    /// TCP socket in ESTABLISHED state.
    #[serde(rename = "ESTABLISHED")]
    Established,
    /// TCP socket in SYN-SENT state.
    #[serde(rename = "SYN_SENT")]
    SynSent,
    /// TCP socket in SYN-RECV / SYN-RECEIVED state.
    #[serde(rename = "SYN_RECV")]
    SynReceived,
    /// TCP socket in FIN-WAIT-1 state.
    #[serde(rename = "FIN_WAIT1")]
    FinWait1,
    /// TCP socket in FIN-WAIT-2 state.
    #[serde(rename = "FIN_WAIT2")]
    FinWait2,
    /// TCP socket in TIME-WAIT state.
    #[serde(rename = "TIME_WAIT")]
    TimeWait,
    /// TCP socket in CLOSE state.
    #[serde(rename = "CLOSE")]
    Close,
    /// TCP socket in CLOSE-WAIT state.
    #[serde(rename = "CLOSE_WAIT")]
    CloseWait,
    /// TCP socket in LAST-ACK state.
    #[serde(rename = "LAST_ACK")]
    LastAck,
    /// TCP socket in CLOSING state.
    #[serde(rename = "CLOSING")]
    Closing,
    /// TCP socket in NEW-SYN-RECV state.
    #[serde(rename = "NEW_SYN_RECV")]
    NewSynReceived,
    /// Windows-specific DELETE-TCB TCP state.
    #[serde(rename = "DELETE_TCB")]
    DeleteTcb,
    /// TCP state could not be determined reliably.
    #[serde(rename = "UNKNOWN")]
    Unknown,
    /// State not applicable (e.g. UDP sockets).
    #[serde(rename = "-")]
    NotApplicable,
}

impl std::fmt::Display for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Listen => write!(f, "LISTEN"),
            Self::Established => write!(f, "ESTABLISHED"),
            Self::SynSent => write!(f, "SYN_SENT"),
            Self::SynReceived => write!(f, "SYN_RECV"),
            Self::FinWait1 => write!(f, "FIN_WAIT1"),
            Self::FinWait2 => write!(f, "FIN_WAIT2"),
            Self::TimeWait => write!(f, "TIME_WAIT"),
            Self::Close => write!(f, "CLOSE"),
            Self::CloseWait => write!(f, "CLOSE_WAIT"),
            Self::LastAck => write!(f, "LAST_ACK"),
            Self::Closing => write!(f, "CLOSING"),
            Self::NewSynReceived => write!(f, "NEW_SYN_RECV"),
            Self::DeleteTcb => write!(f, "DELETE_TCB"),
            Self::Unknown => write!(f, "UNKNOWN"),
            Self::NotApplicable => write!(f, "-"),
        }
    }
}

/// A single row in the port listing output.
///
/// Each entry represents one open socket on the local machine, enriched with
/// process metadata where available.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PortEntry {
    /// Local port number.
    pub port: u16,
    /// Local bind address for the socket.
    pub local_addr: IpAddr,
    /// Protocol (TCP or UDP).
    pub proto: Protocol,
    /// Connection state: `Listen` for TCP, `NotApplicable` for UDP.
    pub state: State,
    /// Process identifier owning this socket.
    pub pid: u32,
    /// Process executable name.
    ///
    /// Stored as `Arc<str>` so that many `PortEntry` rows sharing the same
    /// process name (for example worker-pool processes) share a single
    /// heap allocation via cheap refcount bumps.
    pub process: Arc<str>,
    /// Owning user or account name, or `"-"` if unavailable.
    ///
    /// Stored as `Arc<str>` so that all entries owned by the same user
    /// share one allocation produced by the user-resolver cache.
    pub user: Arc<str>,
    /// Project folder name or Docker container name.
    pub project: Option<String>,
    /// Detected app/framework label, for example "Next.js" or `PostgreSQL`.
    pub app: Option<AppLabel>,
    /// Process uptime in seconds.
    pub uptime_secs: Option<u64>,
}

/// Strip a trailing `.exe` suffix from a process name (case-insensitive).
///
/// Returns the original string unchanged when the suffix is absent.
/// Used by both the collector and framework modules to normalize
/// Windows process names before matching known process patterns.
#[must_use]
pub fn strip_windows_exe_suffix(process_name: &str) -> &str {
    let Some(prefix_len) = process_name.len().checked_sub(4) else {
        return process_name;
    };

    match process_name.get(prefix_len..) {
        Some(suffix) if suffix.eq_ignore_ascii_case(".exe") => {
            process_name.get(..prefix_len).unwrap_or(process_name)
        }
        _ => process_name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_display_matches_variants() {
        for (protocol, expected) in [(Protocol::Tcp, "TCP"), (Protocol::Udp, "UDP")] {
            assert_eq!(
                protocol.to_string(),
                expected,
                "protocol display should match"
            );
        }
    }

    #[test]
    fn state_display_matches_variants() {
        for (state, expected) in [
            (State::Listen, "LISTEN"),
            (State::Established, "ESTABLISHED"),
            (State::SynSent, "SYN_SENT"),
            (State::SynReceived, "SYN_RECV"),
            (State::FinWait1, "FIN_WAIT1"),
            (State::FinWait2, "FIN_WAIT2"),
            (State::TimeWait, "TIME_WAIT"),
            (State::Close, "CLOSE"),
            (State::CloseWait, "CLOSE_WAIT"),
            (State::LastAck, "LAST_ACK"),
            (State::Closing, "CLOSING"),
            (State::NewSynReceived, "NEW_SYN_RECV"),
            (State::DeleteTcb, "DELETE_TCB"),
            (State::Unknown, "UNKNOWN"),
            (State::NotApplicable, "-"),
        ] {
            assert_eq!(state.to_string(), expected, "state display should match");
        }
    }

    #[test]
    fn strip_exe_empty_string() {
        assert_eq!(
            strip_windows_exe_suffix(""),
            "",
            "empty string should remain empty"
        );
    }

    #[test]
    fn strip_exe_shorter_than_four_chars() {
        assert_eq!(
            strip_windows_exe_suffix("abc"),
            "abc",
            "strings shorter than .exe should be unchanged"
        );
    }

    #[test]
    fn strip_exe_exactly_dot_exe() {
        assert_eq!(
            strip_windows_exe_suffix(".exe"),
            "",
            "bare .exe should strip to empty"
        );
    }

    #[test]
    fn strip_exe_lowercase_suffix() {
        assert_eq!(strip_windows_exe_suffix("nginx.exe"), "nginx");
    }

    #[test]
    fn strip_exe_uppercase_suffix() {
        assert_eq!(strip_windows_exe_suffix("NGINX.EXE"), "NGINX");
    }

    #[test]
    fn strip_exe_mixed_case_suffix() {
        assert_eq!(strip_windows_exe_suffix("node.Exe"), "node");
    }

    #[test]
    fn strip_exe_no_suffix() {
        assert_eq!(
            strip_windows_exe_suffix("postgres"),
            "postgres",
            "names without .exe should be unchanged"
        );
    }

    #[test]
    fn strip_exe_dot_exe_in_middle() {
        assert_eq!(
            strip_windows_exe_suffix("my.executable"),
            "my.executable",
            "only trailing .exe should be stripped"
        );
    }
}
