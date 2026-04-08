//! # Shared types
//!
//! Contains the [`PortEntry`] struct used across all modules.

use serde::Serialize;

/// Protocol type for a socket entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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

/// A single row in the port listing output.
///
/// Each entry represents one open socket on the local machine, enriched with
/// process metadata where available.
#[derive(Debug, Clone, Serialize)]
pub struct PortEntry {
    /// Local port number.
    pub port: u16,
    /// Protocol (TCP or UDP).
    pub proto: Protocol,
    /// Connection state (e.g. `LISTEN`, `ESTABLISHED`). UDP entries use `"-"`.
    pub state: String,
    /// Process identifier owning this socket.
    pub pid: u32,
    /// Process executable name.
    pub process: String,
    /// Owning user or account name, or `"-"` if unavailable.
    pub user: String,
}
