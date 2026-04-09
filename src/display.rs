//! # Display module
//!
//! Renders `Vec<PortEntry>` as either an aligned terminal table or a JSON
//! array to stdout.

use std::io::{self, Write};

use anyhow::Result;
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
                "PORT", "PROTO", "STATE", "PROCESS", "PID", "USER", "PROJECT", "APP", "UPTIME",
            ]);
        } else {
            table.set_header(vec![
                "PORT", "PROTO", "PROCESS", "PID", "PROJECT", "APP", "UPTIME",
            ]);
        }
    }

    for entry in entries {
        let process_display = truncate_process_name(&entry.process);
        let project = entry.project.as_deref().unwrap_or("-");
        let app = entry.app.as_deref().unwrap_or("-");
        let uptime = format_uptime(entry.uptime_secs);

        if opts.full {
            table.add_row(vec![
                entry.port.to_string(),
                entry.proto.to_string(),
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
                process_display,
                entry.pid.to_string(),
                project.to_string(),
                app.to_string(),
                uptime,
            ]);
        }
    }

    writeln!(io::stdout().lock(), "{table}")?;
    Ok(())
}

/// Print the entries as a JSON array to stdout.
///
/// Returns an error if serialization or writing to stdout fails.
pub fn print_json(entries: &[PortEntry]) -> Result<()> {
    let json = serde_json::to_string_pretty(entries)?;
    writeln!(io::stdout().lock(), "{json}")?;
    Ok(())
}

/// Truncate a process name to [`MAX_PROCESS_NAME_LEN`] characters with an
/// ellipsis if it exceeds the limit.
///
/// Uses character (not byte) counting so multi-byte UTF-8 names are handled
/// safely without panicking on char boundaries.
fn truncate_process_name(name: &str) -> String {
    let char_count = name.chars().count();
    if char_count > MAX_PROCESS_NAME_LEN {
        let truncated: String = name.chars().take(MAX_PROCESS_NAME_LEN - 1).collect();
        format!("{truncated}\u{2026}")
    } else {
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
