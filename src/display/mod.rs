//! # Display module
//!
//! Renders `Vec<PortEntry>` as either an aligned terminal table or a JSON
//! array to stdout.
//!
//! ## Module structure
//!
//! - `render` — Shared cell/border rendering primitives and style presets.
//! - `table` — Column definitions and table rendering engine.
//! - `tips` — "Quick Actions" footer panel with adaptive layout.
//! - `terminal` — Terminal width detection and UTF-8 support probing.

mod render;
mod table;
mod terminal;
mod tips;

use std::io::{self, Write};

use anyhow::{Context, Result};

use crate::types::PortEntry;

/// Options controlling how entries are rendered.
pub struct DisplayOptions {
    /// Show the header row.
    pub show_header: bool,
    /// Show all columns (adds STATE and USER).
    pub full: bool,
    /// Use compact (borderless) table style.
    pub compact: bool,
}

// ── Public API ──────────────────────────────────────────────────────

/// Print the entries as a table to stdout.
///
/// Table style and column selection are controlled by `opts`.
/// Returns an error if writing to stdout fails (e.g. broken pipe).
pub fn print_table(entries: &[PortEntry], opts: &DisplayOptions) -> Result<()> {
    table::write_table(&mut io::stdout().lock(), entries, opts)
}

/// Print the entries as a JSON array to stdout.
///
/// Returns an error if serialization or writing to stdout fails.
pub fn print_json(entries: &[PortEntry]) -> Result<()> {
    write_json(&mut io::stdout().lock(), entries)
}

/// Print the interactive tips footer to stderr.
pub fn print_tips() -> Result<()> {
    tips::write_tips(&mut io::stderr().lock())
}

// ── JSON output ─────────────────────────────────────────────────────

/// Render entries as a JSON array to the given writer.
fn write_json(writer: &mut impl Write, entries: &[PortEntry]) -> Result<()> {
    let json =
        serde_json::to_string_pretty(entries).context("failed to serialize entries to JSON")?;
    writeln!(writer, "{json}").context("failed to write JSON to stdout")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use super::*;
    use crate::types::{Protocol, State};

    fn sample_entry() -> PortEntry {
        PortEntry {
            port: 8080,
            local_addr: IpAddr::V4(Ipv4Addr::LOCALHOST),
            proto: Protocol::Tcp,
            state: State::Listen,
            pid: 1234,
            process: "node".into(),
            user: "user".into(),
            project: Some("my-app".to_string()),
            app: Some("Next.js".into()),
            uptime_secs: Some(3600),
        }
    }

    #[test]
    fn write_json_contains_expected_fields() {
        let entries = vec![sample_entry()];
        let mut buffer = Vec::new();
        write_json(&mut buffer, &entries).expect("write_json should succeed");
        let output = String::from_utf8(buffer).expect("output should be valid UTF-8");

        assert!(
            output.contains("\"port\": 8080"),
            "JSON should contain port"
        );
        assert!(
            output.contains("\"proto\": \"TCP\""),
            "JSON should contain protocol"
        );
        assert!(
            output.contains("\"process\": \"node\""),
            "JSON should contain process name"
        );
        assert!(
            output.contains("\"project\": \"my-app\""),
            "JSON should contain project name"
        );
        assert!(
            output.contains("\"app\": \"Next.js\""),
            "JSON should contain app label"
        );
    }

    #[test]
    fn write_json_empty_entries_produces_empty_array() {
        let mut buffer = Vec::new();
        write_json(&mut buffer, &[]).expect("write_json should succeed");
        let output = String::from_utf8(buffer).expect("output should be valid UTF-8");
        assert_eq!(output.trim(), "[]", "empty entries should produce []");
    }
}
