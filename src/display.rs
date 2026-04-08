//! # Display module
//!
//! Renders `Vec<PortEntry>` as either an aligned terminal table or a JSON
//! array to stdout.

use anyhow::Result;
use comfy_table::{ContentArrangement, Table};

use crate::types::PortEntry;

/// Maximum display width for the process name column before truncation.
const MAX_PROCESS_NAME_LEN: usize = 20;

/// Print the entries as an aligned table to stdout.
///
/// When `show_header` is `true`, a header row is printed above the data.
pub fn print_table(entries: &[PortEntry], show_header: bool) {
    let mut table = Table::new();
    table.set_content_arrangement(ContentArrangement::Dynamic);

    // Disable borders for a clean netstat-like appearance
    table.load_preset(comfy_table::presets::NOTHING);

    if show_header {
        table.set_header(vec!["PORT", "PROTO", "STATE", "PID", "PROCESS", "USER"]);
    }

    for entry in entries {
        let process_display = truncate_process_name(&entry.process);

        table.add_row(vec![
            entry.port.to_string(),
            entry.proto.to_string(),
            entry.state.clone(),
            entry.pid.to_string(),
            process_display,
            entry.user.clone(),
        ]);
    }

    println!("{table}");
}

/// Print the entries as a JSON array to stdout.
pub fn print_json(entries: &[PortEntry]) -> Result<()> {
    let json = serde_json::to_string_pretty(entries)?;
    println!("{json}");
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
