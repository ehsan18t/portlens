//! # Display module
//!
//! Renders `Vec<PortEntry>` as either an aligned terminal table or a JSON
//! array to stdout.

use std::io::{self, Write};

use anyhow::{Context, Result};
use comfy_table::{ContentArrangement, Table};

use crate::types::{PortEntry, format_uptime};

/// Maximum display width for the process name column before truncation.
const MAX_PROCESS_NAME_LEN: usize = 20;

/// Options controlling how entries are rendered.
pub struct DisplayOptions {
    /// Show the header row.
    pub show_header: bool,
    /// Show all columns (adds STATE and USER).
    pub full: bool,
    /// Use compact (borderless) table style.
    pub compact: bool,
}

/// Print the entries as a table to stdout.
///
/// Table style and column selection are controlled by `opts`.
/// Returns an error if writing to stdout fails (e.g. broken pipe).
pub fn print_table(entries: &[PortEntry], opts: &DisplayOptions) -> Result<()> {
    write_table(&mut io::stdout().lock(), entries, opts)
}

/// Print the entries as a JSON array to stdout.
///
/// Returns an error if serialization or writing to stdout fails.
pub fn print_json(entries: &[PortEntry]) -> Result<()> {
    write_json(&mut io::stdout().lock(), entries)
}

/// Render entries as a table to the given writer.
fn write_table(
    writer: &mut impl Write,
    entries: &[PortEntry],
    opts: &DisplayOptions,
) -> Result<()> {
    let mut table = Table::new();
    table.set_content_arrangement(ContentArrangement::Dynamic);

    if opts.compact {
        table.load_preset(comfy_table::presets::NOTHING);
    } else {
        table.load_preset(comfy_table::presets::UTF8_FULL);
        table.apply_modifier(comfy_table::modifiers::UTF8_ROUND_CORNERS);
    }

    if opts.show_header {
        if opts.full {
            table.set_header(vec![
                "PORT", "PROTO", "ADDRESS", "STATE", "PROCESS", "PID", "USER", "PROJECT", "APP",
                "UPTIME",
            ]);
        } else {
            table.set_header(vec![
                "PORT", "PROTO", "ADDRESS", "PROCESS", "PID", "PROJECT", "APP", "UPTIME",
            ]);
        }
    }

    for entry in entries {
        let local_addr = entry.local_addr.to_string();
        let process_display = truncate_process_name(&entry.process);
        let project = entry.project.as_deref().unwrap_or("-");
        let app = entry.app.unwrap_or("-");
        let uptime = format_uptime(entry.uptime_secs);

        if opts.full {
            table.add_row(vec![
                entry.port.to_string(),
                entry.proto.to_string(),
                local_addr,
                entry.state.to_string(),
                process_display,
                entry.pid.to_string(),
                entry.user.clone(),
                project.to_string(),
                app.to_string(),
                uptime,
            ]);
        } else {
            table.add_row(vec![
                entry.port.to_string(),
                entry.proto.to_string(),
                local_addr,
                process_display,
                entry.pid.to_string(),
                project.to_string(),
                app.to_string(),
                uptime,
            ]);
        }
    }

    writeln!(writer, "{table}").context("failed to write table to stdout")?;
    Ok(())
}

/// Render entries as a JSON array to the given writer.
fn write_json(writer: &mut impl Write, entries: &[PortEntry]) -> Result<()> {
    let json =
        serde_json::to_string_pretty(entries).context("failed to serialize entries to JSON")?;
    writeln!(writer, "{json}").context("failed to write JSON to stdout")?;
    Ok(())
}

/// Truncate a process name to [`MAX_PROCESS_NAME_LEN`] characters with an
/// ellipsis if it exceeds the limit.
///
/// Uses character boundaries and stops after the first 21 characters, so
/// oversized names are not traversed twice.
fn truncate_process_name(name: &str) -> String {
    let mut ellipsis_index = None;
    let mut needs_truncation = false;

    for (index, (byte_index, _)) in name.char_indices().enumerate() {
        if index == MAX_PROCESS_NAME_LEN - 1 {
            ellipsis_index = Some(byte_index);
        } else if index == MAX_PROCESS_NAME_LEN {
            needs_truncation = true;
            break;
        }
    }

    if !needs_truncation {
        return name.to_string();
    }

    let mut truncated = name[..ellipsis_index.unwrap_or_default()].to_string();
    truncated.push('\u{2026}');
    truncated
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
            app: Some("Next.js"),
            uptime_secs: Some(3600),
        }
    }

    #[test]
    fn short_name_unchanged() {
        assert_eq!(truncate_process_name("sshd"), "sshd");
    }

    #[test]
    fn exact_length_unchanged() {
        let name = "a".repeat(MAX_PROCESS_NAME_LEN);
        assert_eq!(truncate_process_name(&name), name);
    }

    #[test]
    fn long_name_truncated() {
        let name = "a".repeat(MAX_PROCESS_NAME_LEN + 5);
        let result = truncate_process_name(&name);
        assert_eq!(
            result.chars().count(),
            MAX_PROCESS_NAME_LEN,
            "truncated name should be exactly MAX_PROCESS_NAME_LEN chars"
        );
        assert!(
            result.ends_with('\u{2026}'),
            "truncated name should end with ellipsis"
        );
    }

    #[test]
    fn multibyte_name_does_not_panic() {
        // CJK characters are 3 bytes each in UTF-8
        let name = "\u{4e16}\u{754c}".repeat(MAX_PROCESS_NAME_LEN);
        let result = truncate_process_name(&name);
        assert_eq!(
            result.chars().count(),
            MAX_PROCESS_NAME_LEN,
            "truncated multi-byte name should be exactly MAX_PROCESS_NAME_LEN chars"
        );
        assert!(
            result.ends_with('\u{2026}'),
            "truncated multi-byte name should end with ellipsis"
        );
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
            output.contains("\"proto\": \"Tcp\""),
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

    #[test]
    fn write_table_default_columns_include_expected_headers() {
        let entries = vec![sample_entry()];
        let opts = DisplayOptions {
            show_header: true,
            full: false,
            compact: false,
        };
        let mut buffer = Vec::new();
        write_table(&mut buffer, &entries, &opts).expect("write_table should succeed");
        let output = String::from_utf8(buffer).expect("output should be valid UTF-8");

        for header in [
            "PORT", "PROTO", "ADDRESS", "PROCESS", "PID", "PROJECT", "APP", "UPTIME",
        ] {
            assert!(
                output.contains(header),
                "default table should contain {header} header"
            );
        }
        assert!(
            !output.contains("STATE"),
            "default table should not contain STATE column"
        );
        assert!(
            !output.contains("USER"),
            "default table should not contain USER column"
        );
    }

    #[test]
    fn write_table_full_columns_include_state_and_user() {
        let entries = vec![sample_entry()];
        let opts = DisplayOptions {
            show_header: true,
            full: true,
            compact: false,
        };
        let mut buffer = Vec::new();
        write_table(&mut buffer, &entries, &opts).expect("write_table should succeed");
        let output = String::from_utf8(buffer).expect("output should be valid UTF-8");

        assert!(
            output.contains("STATE"),
            "full table should contain STATE column"
        );
        assert!(
            output.contains("USER"),
            "full table should contain USER column"
        );
    }

    #[test]
    fn write_table_no_header_omits_column_names() {
        let entries = vec![sample_entry()];
        let opts = DisplayOptions {
            show_header: false,
            full: false,
            compact: false,
        };
        let mut buffer = Vec::new();
        write_table(&mut buffer, &entries, &opts).expect("write_table should succeed");
        let output = String::from_utf8(buffer).expect("output should be valid UTF-8");

        assert!(
            !output.contains("PROTO"),
            "no-header should omit column names"
        );
    }

    #[test]
    fn write_table_renders_entry_values() {
        let entries = vec![sample_entry()];
        let opts = DisplayOptions {
            show_header: false,
            full: false,
            compact: true,
        };
        let mut buffer = Vec::new();
        write_table(&mut buffer, &entries, &opts).expect("write_table should succeed");
        let output = String::from_utf8(buffer).expect("output should be valid UTF-8");

        assert!(output.contains("8080"), "table should contain port number");
        assert!(output.contains("TCP"), "table should contain protocol");
        assert!(output.contains("node"), "table should contain process name");
        assert!(
            output.contains("my-app"),
            "table should contain project name"
        );
        assert!(output.contains("Next.js"), "table should contain app label");
        assert!(output.contains("1h"), "table should contain uptime");
    }
}
