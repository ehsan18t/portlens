//! # Shared types
//!
//! Contains the [`PortEntry`] struct used across all modules.

use serde::Serialize;

/// Protocol type for a socket entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum Protocol {
    /// Transmission Control Protocol.
    Tcp,
    /// User Datagram Protocol.
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
    Listen,
    /// TCP socket in ESTABLISHED state.
    Established,
    /// TCP socket in SYN-SENT state.
    SynSent,
    /// TCP socket in SYN-RECV / SYN-RECEIVED state.
    SynReceived,
    /// TCP socket in FIN-WAIT-1 state.
    FinWait1,
    /// TCP socket in FIN-WAIT-2 state.
    FinWait2,
    /// TCP socket in TIME-WAIT state.
    TimeWait,
    /// TCP socket in CLOSE state.
    Close,
    /// TCP socket in CLOSE-WAIT state.
    CloseWait,
    /// TCP socket in LAST-ACK state.
    LastAck,
    /// TCP socket in CLOSING state.
    Closing,
    /// TCP socket in NEW-SYN-RECV state.
    NewSynReceived,
    /// Windows-specific DELETE-TCB TCP state.
    DeleteTcb,
    /// TCP state could not be determined reliably.
    Unknown,
    /// State not applicable (e.g. UDP sockets).
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
    /// Protocol (TCP or UDP).
    pub proto: Protocol,
    /// Connection state: `Listen` for TCP, `NotApplicable` for UDP.
    pub state: State,
    /// Process identifier owning this socket.
    pub pid: u32,
    /// Process executable name.
    pub process: String,
    /// Owning user or account name, or `"-"` if unavailable.
    pub user: String,
    /// Project folder name or Docker container name.
    pub project: Option<String>,
    /// Detected app/framework label, for example "Next.js" or `PostgreSQL`.
    pub app: Option<&'static str>,
    /// Process uptime in seconds.
    pub uptime_secs: Option<u64>,
}

/// Format an uptime duration in seconds into a human-readable string.
///
/// Returns `"-"` if the input is `None`.
#[must_use]
pub fn format_uptime(secs: Option<u64>) -> String {
    let Some(s) = secs else {
        return "-".to_string();
    };
    let minutes = s / 60;
    let hours = minutes / 60;
    let days = hours / 24;

    if days > 0 {
        let day_hours = hours % 24;
        let remaining_minutes = minutes % 60;

        if remaining_minutes > 0 {
            format!("{days}d {day_hours}h {remaining_minutes}m")
        } else {
            format!("{days}d {day_hours}h")
        }
    } else if hours > 0 {
        format!("{}h {}m", hours, minutes % 60)
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        "< 1m".to_string()
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
            (State::NotApplicable, "-"),
        ] {
            assert_eq!(state.to_string(), expected, "state display should match");
        }
    }

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
    fn format_uptime_days_hours_minutes() {
        assert_eq!(format_uptime(Some(86400 + 32400 + 900)), "1d 9h 15m");
    }
}
