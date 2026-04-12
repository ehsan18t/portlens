//! # Display module
//!
//! Renders `Vec<PortEntry>` as either an aligned terminal table or a JSON
//! array to stdout.
//!
//! ## Module structure
//!
//! - `table` — Column definitions and table rendering engine.
//! - `tips` — "Quick Actions" footer panel with adaptive layout.
//! - `terminal` — Terminal width detection and UTF-8 support probing.

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

// ── Shared types ────────────────────────────────────────────────────

#[derive(Clone, Copy)]
enum Alignment {
    Left,
    Right,
}

#[derive(Clone, Copy)]
struct BorderStyle {
    vertical: char,
    horizontal: char,
    top_left: char,
    top_join: char,
    top_right: char,
    middle_left: char,
    middle_join: char,
    middle_right: char,
    bottom_left: char,
    bottom_join: char,
    bottom_right: char,
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

// ── Shared rendering primitives ─────────────────────────────────────

fn display_width(value: &str) -> usize {
    value.chars().count()
}

fn truncate_to_width(value: &str, width: usize) -> String {
    if display_width(value) <= width {
        return value.to_string();
    }

    if width == 0 {
        return String::new();
    }

    if width == 1 {
        return "…".to_string();
    }

    let mut truncated = value.chars().take(width - 1).collect::<String>();
    truncated.push('…');
    truncated
}

fn pad_value(value: &str, width: usize, alignment: Alignment) -> String {
    let padding = width.saturating_sub(display_width(value));

    match alignment {
        Alignment::Left => format!("{value}{}", " ".repeat(padding)),
        Alignment::Right => format!("{}{value}", " ".repeat(padding)),
    }
}

fn format_cell(value: &str, width: usize, alignment: Alignment) -> String {
    let clipped = truncate_to_width(value, width);
    format!(" {} ", pad_value(&clipped, width, alignment))
}

fn render_border_line(
    widths: &[usize],
    left: char,
    join: char,
    right: char,
    horizontal: char,
) -> String {
    let segment = horizontal.to_string();
    let join = join.to_string();
    let body = widths
        .iter()
        .map(|width| segment.repeat(width + 2))
        .collect::<Vec<_>>()
        .join(&join);

    format!("{left}{body}{right}")
}

fn render_bordered_cells(cells: &[String], vertical: char) -> String {
    let separator = vertical.to_string();
    let joined = cells.join(&separator);
    format!("{vertical}{joined}{vertical}")
}

fn rendered_table_width(widths: &[usize], compact: bool) -> usize {
    let content_width = widths.iter().sum::<usize>();
    if compact {
        content_width + widths.len().saturating_sub(1) * 2
    } else {
        content_width + widths.len() * 3 + 1
    }
}

// ── Border style presets ────────────────────────────────────────────

const fn utf8_border_style() -> BorderStyle {
    BorderStyle {
        vertical: '│',
        horizontal: '─',
        top_left: '╭',
        top_join: '┬',
        top_right: '╮',
        middle_left: '├',
        middle_join: '┼',
        middle_right: '┤',
        bottom_left: '╰',
        bottom_join: '┴',
        bottom_right: '╯',
    }
}

const fn ascii_border_style() -> BorderStyle {
    BorderStyle {
        vertical: '|',
        horizontal: '-',
        top_left: '+',
        top_join: '+',
        top_right: '+',
        middle_left: '+',
        middle_join: '+',
        middle_right: '+',
        bottom_left: '+',
        bottom_join: '+',
        bottom_right: '+',
    }
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
            process: "node".to_string(),
            user: "user".to_string(),
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
