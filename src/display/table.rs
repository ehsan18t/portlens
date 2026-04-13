//! Table rendering engine.
//!
//! Converts `Vec<PortEntry>` data into aligned, bordered or compact ASCII/UTF-8
//! table output. Owns the `Column` enum and all data-to-string formatting.

use std::io::Write;

use anyhow::{Context, Result};

use crate::types::PortEntry;

use super::DisplayOptions;
use super::render::{
    Alignment, BorderStyle, ascii_border_style, display_width, format_cell, pad_value,
    reduce_widths_to_fit, render_border_line, render_bordered_cells, rendered_table_width,
    truncate_to_width, utf8_border_style,
};
use super::terminal::{stdout_terminal_width, terminal_supports_utf8_borders};

/// Maximum display width for the process name column before truncation.
const MAX_PROCESS_NAME_LEN: usize = 20;

const DEFAULT_COLUMNS: &[Column] = &[
    Column::Port,
    Column::Proto,
    Column::Address,
    Column::Process,
    Column::Pid,
    Column::Project,
    Column::App,
    Column::Uptime,
];

const FULL_COLUMNS: &[Column] = &[
    Column::Port,
    Column::Proto,
    Column::Address,
    Column::State,
    Column::Process,
    Column::Pid,
    Column::User,
    Column::Project,
    Column::App,
    Column::Uptime,
];

#[derive(Clone, Copy)]
enum Column {
    Port,
    Proto,
    Address,
    State,
    Process,
    Pid,
    User,
    Project,
    App,
    Uptime,
}

/// Render entries as a table to the given writer.
pub(super) fn write_table(
    writer: &mut impl Write,
    entries: &[PortEntry],
    opts: &DisplayOptions,
) -> Result<()> {
    write_table_with_width(writer, entries, opts, stdout_terminal_width())
}

pub(super) fn write_table_with_width(
    writer: &mut impl Write,
    entries: &[PortEntry],
    opts: &DisplayOptions,
    terminal_width: Option<usize>,
) -> Result<()> {
    let columns = table_columns(opts.full);
    let use_compact = should_use_compact_layout(columns, opts.compact, terminal_width);
    let rows = build_rows(entries, columns);
    let widths = fit_table_widths(
        columns,
        &measure_column_widths(columns, &rows, opts.show_header),
        use_compact,
        terminal_width,
    );

    if use_compact {
        write_compact_table(writer, columns, &rows, &widths, opts.show_header)?;
    } else {
        let style = if terminal_supports_utf8_borders() {
            utf8_border_style()
        } else {
            ascii_border_style()
        };
        write_bordered_table(writer, columns, &rows, &widths, opts.show_header, style)?;
    }

    Ok(())
}

const fn table_columns(full: bool) -> &'static [Column] {
    if full { FULL_COLUMNS } else { DEFAULT_COLUMNS }
}

fn build_rows(entries: &[PortEntry], columns: &[Column]) -> Vec<Vec<String>> {
    entries
        .iter()
        .map(|entry| columns.iter().map(|column| column.value(entry)).collect())
        .collect()
}

fn should_use_compact_layout(
    columns: &[Column],
    compact_requested: bool,
    terminal_width: Option<usize>,
) -> bool {
    compact_requested
        || terminal_width.is_some_and(|available_width| {
            minimum_table_width(columns.len(), false) > available_width
        })
}

fn measure_column_widths(
    columns: &[Column],
    rows: &[Vec<String>],
    show_header: bool,
) -> Vec<usize> {
    columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            let header_width = if show_header {
                display_width(column.heading())
            } else {
                0
            };
            let row_width = rows
                .iter()
                .filter_map(|row| row.get(index))
                .map(String::as_str)
                .map(display_width)
                .max()
                .unwrap_or_default();

            header_width.max(row_width)
        })
        .collect()
}

fn fit_table_widths(
    columns: &[Column],
    natural_widths: &[usize],
    compact: bool,
    terminal_width: Option<usize>,
) -> Vec<usize> {
    let Some(available_width) = terminal_width else {
        return natural_widths.to_vec();
    };

    let mut widths = natural_widths.to_vec();
    let preferred_min_widths = columns
        .iter()
        .map(|column| column.preferred_min_width())
        .collect::<Vec<_>>();
    let shrink_order = sorted_shrink_order(columns);

    reduce_widths_to_fit(
        &mut widths,
        &preferred_min_widths,
        &shrink_order,
        compact,
        available_width,
    );

    if rendered_table_width(&widths, compact) > available_width {
        let hard_min_widths = vec![1; columns.len()];
        reduce_widths_to_fit(
            &mut widths,
            &hard_min_widths,
            &shrink_order,
            compact,
            available_width,
        );
    }

    widths
}

const fn minimum_table_width(column_count: usize, compact: bool) -> usize {
    if compact {
        column_count.saturating_mul(3).saturating_sub(2)
    } else {
        column_count.saturating_mul(4).saturating_add(1)
    }
}

fn sorted_shrink_order(columns: &[Column]) -> Vec<usize> {
    let mut column_indexes = (0..columns.len()).collect::<Vec<_>>();
    column_indexes.sort_by_key(|index| columns[*index].shrink_priority());
    column_indexes
}

fn write_bordered_table(
    writer: &mut impl Write,
    columns: &[Column],
    rows: &[Vec<String>],
    widths: &[usize],
    show_header: bool,
    style: BorderStyle,
) -> Result<()> {
    let mut lines = vec![render_border_line(
        widths,
        style.top_left,
        style.top_join,
        style.top_right,
        style.horizontal,
    )];

    if show_header {
        lines.push(render_header_row(columns, widths, style.vertical));
        lines.push(render_border_line(
            widths,
            style.middle_left,
            style.middle_join,
            style.middle_right,
            style.horizontal,
        ));
    }

    lines.extend(
        rows.iter()
            .map(|row| render_data_row(row, columns, widths, style.vertical)),
    );

    lines.push(render_border_line(
        widths,
        style.bottom_left,
        style.bottom_join,
        style.bottom_right,
        style.horizontal,
    ));

    writeln!(writer, "{}", lines.join("\n")).context("failed to write table to stdout")?;
    Ok(())
}

fn write_compact_table(
    writer: &mut impl Write,
    columns: &[Column],
    rows: &[Vec<String>],
    widths: &[usize],
    show_header: bool,
) -> Result<()> {
    let mut lines = Vec::new();

    if show_header {
        lines.push(render_compact_header(columns, widths));
    }

    lines.extend(
        rows.iter()
            .map(|row| render_compact_row(row, columns, widths)),
    );

    if lines.is_empty() {
        return Ok(());
    }

    writeln!(writer, "{}", lines.join("\n")).context("failed to write compact table to stdout")?;

    Ok(())
}

fn render_header_row(columns: &[Column], widths: &[usize], vertical: char) -> String {
    let cells = columns
        .iter()
        .zip(widths)
        .map(|(column, width)| {
            format_cell(column.heading_for_width(*width), *width, Alignment::Left)
        })
        .collect::<Vec<_>>();

    render_bordered_cells(&cells, vertical)
}

fn render_data_row(row: &[String], columns: &[Column], widths: &[usize], vertical: char) -> String {
    let cells = row
        .iter()
        .zip(columns)
        .zip(widths)
        .map(|((cell, column), width)| format_cell(cell, *width, column.alignment()))
        .collect::<Vec<_>>();

    render_bordered_cells(&cells, vertical)
}

fn render_compact_header(columns: &[Column], widths: &[usize]) -> String {
    let cells = columns
        .iter()
        .zip(widths)
        .map(|(column, width)| {
            format_compact_cell(column.heading_for_width(*width), *width, Alignment::Left)
        })
        .collect::<Vec<_>>();

    render_compact_cells(&cells)
}

fn render_compact_row(row: &[String], columns: &[Column], widths: &[usize]) -> String {
    let cells = row
        .iter()
        .zip(columns)
        .zip(widths)
        .map(|((cell, column), width)| format_compact_cell(cell, *width, column.alignment()))
        .collect::<Vec<_>>();

    render_compact_cells(&cells)
}

fn render_compact_cells(cells: &[String]) -> String {
    cells.join("  ")
}

fn format_compact_cell(value: &str, width: usize, alignment: Alignment) -> String {
    let clipped = truncate_to_width(value, width);
    pad_value(&clipped, width, alignment)
}

// ── Data formatting ─────────────────────────────────────────────────

fn format_uptime(secs: Option<u64>) -> String {
    let Some(seconds) = secs else {
        return "-".to_string();
    };
    let minutes = seconds / 60;
    let hours = minutes / 60;
    let days = hours / 24;

    if days > 0 {
        let day_hours = hours % 24;
        let remaining_minutes = minutes % 60;

        return match (day_hours > 0, remaining_minutes > 0) {
            (true, true) => format!("{days}d {day_hours}h {remaining_minutes}m"),
            (true, false) => format!("{days}d {day_hours}h"),
            (false, true) => format!("{days}d {remaining_minutes}m"),
            (false, false) => format!("{days}d"),
        };
    }

    if hours > 0 {
        let remaining_minutes = minutes % 60;
        return if remaining_minutes > 0 {
            format!("{hours}h {remaining_minutes}m")
        } else {
            format!("{hours}h")
        };
    }

    if minutes > 0 {
        return format!("{minutes}m");
    }

    "< 1m".to_string()
}

// ── Column metadata ─────────────────────────────────────────────────

impl Column {
    const fn preferred_min_width(self) -> usize {
        match self {
            Self::Port | Self::User => 4,
            Self::Proto | Self::State | Self::Pid => 5,
            Self::Address | Self::Process | Self::Project => 7,
            Self::App => 3,
            Self::Uptime => 6,
        }
    }

    const fn shrink_priority(self) -> u8 {
        match self {
            Self::Project => 0,
            Self::Process => 1,
            Self::Address => 2,
            Self::User => 3,
            Self::App => 4,
            Self::State => 5,
            Self::Uptime => 6,
            Self::Proto => 7,
            Self::Pid => 8,
            Self::Port => 9,
        }
    }

    const fn heading(self) -> &'static str {
        match self {
            Self::Port => "PORT",
            Self::Proto => "PROTO",
            Self::Address => "ADDRESS",
            Self::State => "STATE",
            Self::Process => "PROCESS",
            Self::Pid => "PID",
            Self::User => "USER",
            Self::Project => "PROJECT",
            Self::App => "APP",
            Self::Uptime => "UPTIME",
        }
    }

    const fn heading_for_width(self, width: usize) -> &'static str {
        match self {
            Self::Address if width < 7 => "ADDR",
            Self::Process if width < 7 => "PROC",
            Self::Project if width < 7 => "PROJ",
            Self::Uptime if width < 6 => "UP",
            Self::State if width < 5 => "ST",
            Self::User if width < 4 => "USR",
            _ => self.heading(),
        }
    }

    const fn alignment(self) -> Alignment {
        match self {
            Self::Port | Self::Pid => Alignment::Right,
            Self::Proto
            | Self::Address
            | Self::State
            | Self::Process
            | Self::User
            | Self::Project
            | Self::App
            | Self::Uptime => Alignment::Left,
        }
    }

    fn value(self, entry: &PortEntry) -> String {
        match self {
            Self::Port => entry.port.to_string(),
            Self::Proto => entry.proto.to_string(),
            Self::Address => entry.local_addr.to_string(),
            Self::State => entry.state.to_string(),
            Self::Process => truncate_to_width(&entry.process, MAX_PROCESS_NAME_LEN),
            Self::Pid => entry.pid.to_string(),
            Self::User => entry.user.to_string(),
            Self::Project => entry.project.as_deref().unwrap_or("-").to_string(),
            Self::App => entry.app.as_deref().unwrap_or("-").to_string(),
            Self::Uptime => format_uptime(entry.uptime_secs),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::sample_entry_for_tests;
    use super::*;
    use crate::types::PortEntry;

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
    fn format_uptime_exact_hours_no_minutes() {
        assert_eq!(
            format_uptime(Some(7200)),
            "2h",
            "exact hours should not show 0m"
        );
    }

    #[test]
    fn format_uptime_days_hours_minutes() {
        assert_eq!(format_uptime(Some(86400 + 32400 + 900)), "1d 9h 15m");
    }

    #[test]
    fn format_uptime_exact_days_no_hours_no_minutes() {
        assert_eq!(
            format_uptime(Some(86400)),
            "1d",
            "exact day should not show 0h"
        );
    }

    #[test]
    fn format_uptime_days_with_zero_hours_and_minutes() {
        assert_eq!(
            format_uptime(Some(86400 + 900)),
            "1d 15m",
            "days with only minutes should skip the 0h component"
        );
    }

    #[test]
    fn format_uptime_days_with_hours_no_minutes() {
        assert_eq!(
            format_uptime(Some(86400 + 3600)),
            "1d 1h",
            "days with only hours should not show 0m"
        );
    }

    #[test]
    fn format_uptime_zero_seconds() {
        assert_eq!(
            format_uptime(Some(0)),
            "< 1m",
            "zero seconds should show sub-minute label"
        );
    }

    #[test]
    fn format_uptime_just_under_one_minute() {
        assert_eq!(
            format_uptime(Some(59)),
            "< 1m",
            "59 seconds should still be sub-minute"
        );
    }

    #[test]
    fn format_uptime_exact_one_minute() {
        assert_eq!(
            format_uptime(Some(60)),
            "1m",
            "exactly 60 seconds should show 1m"
        );
    }

    #[test]
    fn format_uptime_just_under_one_hour() {
        assert_eq!(
            format_uptime(Some(3599)),
            "59m",
            "3599 seconds should show 59m, not 1h"
        );
    }

    #[test]
    fn format_uptime_exact_one_hour() {
        assert_eq!(
            format_uptime(Some(3600)),
            "1h",
            "exactly one hour should not show 0m"
        );
    }

    fn display_options(show_header: bool, full: bool, compact: bool) -> DisplayOptions {
        DisplayOptions {
            show_header,
            full,
            compact,
        }
    }

    fn render_table_output(
        entries: &[PortEntry],
        opts: &DisplayOptions,
        width: Option<usize>,
    ) -> String {
        let mut buffer = Vec::new();
        write_table_with_width(&mut buffer, entries, opts, width)
            .expect("write_table_with_width should succeed");
        String::from_utf8(buffer).expect("output should be valid UTF-8")
    }

    #[test]
    fn short_name_unchanged() {
        assert_eq!(truncate_to_width("sshd", MAX_PROCESS_NAME_LEN), "sshd");
    }

    #[test]
    fn exact_length_unchanged() {
        let name = "a".repeat(MAX_PROCESS_NAME_LEN);
        assert_eq!(truncate_to_width(&name, MAX_PROCESS_NAME_LEN), name);
    }

    #[test]
    fn long_name_truncated() {
        let name = "a".repeat(MAX_PROCESS_NAME_LEN + 5);
        let result = truncate_to_width(&name, MAX_PROCESS_NAME_LEN);
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
        let result = truncate_to_width(&name, MAX_PROCESS_NAME_LEN);
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
    fn write_table_default_columns_include_expected_headers() {
        let entries = vec![sample_entry_for_tests()];
        let opts = display_options(true, false, false);
        let output = render_table_output(&entries, &opts, None);

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
        let entries = vec![sample_entry_for_tests()];
        let opts = display_options(true, true, false);
        let output = render_table_output(&entries, &opts, None);

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
        let entries = vec![sample_entry_for_tests()];
        let opts = display_options(false, false, false);
        let output = render_table_output(&entries, &opts, None);

        assert!(
            !output.contains("PROTO"),
            "no-header should omit column names"
        );
    }

    #[test]
    fn write_table_renders_entry_values() {
        let entries = vec![sample_entry_for_tests()];
        let opts = display_options(false, false, true);
        let output = render_table_output(&entries, &opts, None);

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

    #[test]
    fn write_table_fits_within_requested_terminal_width() {
        let mut entry = sample_entry_for_tests();
        entry.project = Some("ms-python.vscode-pylance-2026.2.1".to_string());
        let entries = vec![entry];
        let opts = display_options(true, false, false);
        let output = render_table_output(&entries, &opts, Some(60));
        for line in output.lines() {
            assert!(
                display_width(line) <= 60,
                "rendered table line should fit the requested width: {line}"
            );
        }
        assert!(
            output.contains("…"),
            "narrow table output should truncate oversized cells"
        );
    }

    #[test]
    fn write_compact_table_fits_within_requested_terminal_width() {
        let mut entry = sample_entry_for_tests();
        entry.project = Some("ms-python.vscode-pylance-2026.2.1".to_string());
        entry.app = Some("Extremely Verbose Framework Name".into());
        let entries = vec![entry];
        let opts = display_options(true, false, true);
        let output = render_table_output(&entries, &opts, Some(40));
        for line in output.lines() {
            assert!(
                display_width(line) <= 40,
                "compact table line should fit the requested width: {line}"
            );
        }
        assert!(
            output.contains("…"),
            "compact output should truncate oversized cells"
        );
    }

    #[test]
    fn write_table_falls_back_to_compact_when_borders_cannot_fit() {
        let mut entry = sample_entry_for_tests();
        entry.project = Some("ms-python.vscode-pylance-2026.2.1".to_string());
        let entries = vec![entry];
        let opts = display_options(false, false, false);
        let output = render_table_output(&entries, &opts, Some(24));
        for line in output.lines() {
            assert!(
                display_width(line) <= 24,
                "fallback compact line should fit the requested width: {line}"
            );
        }
        assert!(
            !output.contains('│')
                && !output.contains('|')
                && !output.contains('╭')
                && !output.contains('+'),
            "very narrow tables should fall back to compact rendering instead of borders"
        );
    }

    #[test]
    fn empty_compact_table_without_header_writes_nothing() {
        let opts = display_options(false, false, true);
        let mut buffer = Vec::new();

        write_table_with_width(&mut buffer, &[], &opts, Some(40))
            .expect("write_table_with_width should succeed");

        assert!(buffer.is_empty(), "empty compact output should stay silent");
    }
}
