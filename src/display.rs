//! # Display module
//!
//! Renders `Vec<PortEntry>` as either an aligned terminal table or a JSON
//! array to stdout.

use std::io::{self, Write};

use anyhow::{Context, Result};

use crate::types::{PortEntry, format_uptime};

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

/// Options controlling how entries are rendered.
pub struct DisplayOptions {
    /// Show the header row.
    pub show_header: bool,
    /// Show all columns (adds STATE and USER).
    pub full: bool,
    /// Use compact (borderless) table style.
    pub compact: bool,
}

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

/// Print the interactive tips footer to stderr.
pub fn print_tips() -> Result<()> {
    write_tips(&mut io::stderr().lock())
}

/// Render entries as a table to the given writer.
fn write_table(
    writer: &mut impl Write,
    entries: &[PortEntry],
    opts: &DisplayOptions,
) -> Result<()> {
    let columns = table_columns(opts.full);
    let rows = build_rows(entries, columns);
    let widths = measure_column_widths(columns, &rows, opts.show_header);

    if opts.compact {
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

/// Render entries as a JSON array to the given writer.
fn write_json(writer: &mut impl Write, entries: &[PortEntry]) -> Result<()> {
    let json =
        serde_json::to_string_pretty(entries).context("failed to serialize entries to JSON")?;
    writeln!(writer, "{json}").context("failed to write JSON to stdout")?;
    Ok(())
}

fn write_tips(writer: &mut impl Write) -> Result<()> {
    let version = env!("CARGO_PKG_VERSION");
    let title = format!("portview v{version}");
    let lines = [
        render_tip_row(&[("filter", "-p PORT"), ("all", "-a"), ("details", "--full")]),
        render_tip_row(&[("json", "--json"), ("help", "-h")]),
    ];
    let style = if terminal_supports_utf8_borders() {
        utf8_border_style()
    } else {
        ascii_border_style()
    };
    let tip_box = render_tip_box(&title, &lines, style);

    write!(writer, "\n{tip_box}\n").context("failed to write tips to stderr")?;
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
        writeln!(writer).context("failed to write compact table to stdout")?;
    } else {
        writeln!(writer, "{}", lines.join("\n"))
            .context("failed to write compact table to stdout")?;
    }

    Ok(())
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

fn render_header_row(columns: &[Column], widths: &[usize], vertical: char) -> String {
    let separator = vertical.to_string();
    let cells = columns
        .iter()
        .zip(widths)
        .map(|(column, width)| format_cell(column.heading(), *width, Alignment::Left))
        .collect::<Vec<_>>()
        .join(&separator);

    format!("{vertical}{cells}{vertical}")
}

fn render_data_row(row: &[String], columns: &[Column], widths: &[usize], vertical: char) -> String {
    let separator = vertical.to_string();
    let cells = row
        .iter()
        .zip(columns)
        .zip(widths)
        .map(|((cell, column), width)| format_cell(cell, *width, column.alignment()))
        .collect::<Vec<_>>()
        .join(&separator);

    format!("{vertical}{cells}{vertical}")
}

fn render_compact_header(columns: &[Column], widths: &[usize]) -> String {
    columns
        .iter()
        .zip(widths)
        .map(|(column, width)| pad_value(column.heading(), *width, Alignment::Left))
        .collect::<Vec<_>>()
        .join("  ")
}

fn render_compact_row(row: &[String], columns: &[Column], widths: &[usize]) -> String {
    row.iter()
        .zip(columns)
        .zip(widths)
        .map(|((cell, column), width)| pad_value(cell, *width, column.alignment()))
        .collect::<Vec<_>>()
        .join("  ")
}

fn format_cell(value: &str, width: usize, alignment: Alignment) -> String {
    format!(" {} ", pad_value(value, width, alignment))
}

fn pad_value(value: &str, width: usize, alignment: Alignment) -> String {
    let padding = width.saturating_sub(display_width(value));

    match alignment {
        Alignment::Left => format!("{value}{}", " ".repeat(padding)),
        Alignment::Right => format!("{}{value}", " ".repeat(padding)),
    }
}

fn render_tip_row(items: &[(&str, &str)]) -> String {
    items
        .iter()
        .map(|(label, value)| format!("{label:<7} {value}"))
        .collect::<Vec<_>>()
        .join("   ")
}

fn render_tip_box(title: &str, lines: &[String], style: BorderStyle) -> String {
    let title = format!(" {title} ");
    let content_width = lines
        .iter()
        .map(String::as_str)
        .map(display_width)
        .max()
        .unwrap_or_default()
        .max(display_width(&title).saturating_sub(2));
    let top_fill = style
        .horizontal
        .to_string()
        .repeat(content_width + 2 - display_width(&title));
    let bottom_fill = style.horizontal.to_string().repeat(content_width + 2);
    let mut rendered = Vec::with_capacity(lines.len() + 2);

    rendered.push(format!(
        "{}{}{top_fill}{}",
        style.top_left, title, style.top_right
    ));

    rendered.extend(lines.iter().map(|line| {
        let padding = " ".repeat(content_width.saturating_sub(display_width(line)));
        format!("{} {line}{padding} {}", style.vertical, style.vertical)
    }));

    rendered.push(format!(
        "{}{}{}",
        style.bottom_left, bottom_fill, style.bottom_right
    ));

    rendered.join("\n")
}

fn display_width(value: &str) -> usize {
    value.chars().count()
}

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

impl Column {
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
            Self::Process => truncate_process_name(&entry.process),
            Self::Pid => entry.pid.to_string(),
            Self::User => entry.user.clone(),
            Self::Project => entry.project.as_deref().unwrap_or("-").to_string(),
            Self::App => entry.app.unwrap_or("-").to_string(),
            Self::Uptime => format_uptime(entry.uptime_secs),
        }
    }
}

/// Check whether the terminal can display UTF-8 box-drawing characters.
///
/// On Windows the check uses several heuristics (cheapest first):
///
/// 1. **Windows Terminal** -- the `WT_SESSION` environment variable is
///    set by Windows Terminal, which always supports UTF-8.
/// 2. **Console code page** -- a code page of 65001 means the console is
///    in explicit UTF-8 mode.
/// 3. **Windows version** -- Windows 10 and newer (major >= 10) render
///    UTF-8 box-drawing correctly in virtually all terminal emulators.
///    Older releases (Windows 7/8) fall back to ASCII.
#[cfg(windows)]
fn terminal_supports_utf8_borders() -> bool {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetConsoleOutputCP() -> u32;
    }

    const UTF8_CODE_PAGE: u32 = 65001;

    // Windows Terminal always supports UTF-8 box-drawing.
    if std::env::var_os("WT_SESSION").is_some() {
        return true;
    }

    // Safety: `GetConsoleOutputCP` is a simple syscall with no preconditions.
    if (unsafe { GetConsoleOutputCP() }) == UTF8_CODE_PAGE {
        return true;
    }

    // Windows 10+ (major version >= 10) renders UTF-8 correctly in most
    // terminal emulators. Only truly ancient releases need the ASCII fallback.
    is_windows_10_or_newer()
}

/// Query the Windows NT kernel for the OS major version.
///
/// Uses `RtlGetVersion` from `ntdll.dll` because the older
/// `GetVersionExW` is subject to manifest-based compatibility shims
/// that can report stale version numbers.
#[cfg(windows)]
fn is_windows_10_or_newer() -> bool {
    // The struct layout matches OSVERSIONINFOW from the Windows SDK.
    // The field name must match the Windows API naming convention.
    #[allow(clippy::struct_field_names)]
    #[repr(C)]
    struct OsVersionInfo {
        os_version_info_size: u32,
        major_version: u32,
        _minor_version: u32,
        _build_number: u32,
        _platform_id: u32,
        _sz_csd_version: [u16; 128],
    }

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn RtlGetVersion(info: *mut OsVersionInfo) -> i32;
    }

    let mut info = std::mem::MaybeUninit::<OsVersionInfo>::zeroed();
    // Safety: `RtlGetVersion` writes into our stack-allocated struct and
    // always succeeds (returns STATUS_SUCCESS == 0).
    unsafe {
        // The struct size is well under u32::MAX; truncation cannot happen.
        #[allow(clippy::cast_possible_truncation)]
        let size = std::mem::size_of::<OsVersionInfo>() as u32;
        (*info.as_mut_ptr()).os_version_info_size = size;
        if RtlGetVersion(info.as_mut_ptr()) == 0 {
            return (*info.as_ptr()).major_version >= 10;
        }
    }
    // If RtlGetVersion fails (should never happen), fall back to ASCII.
    false
}

/// Check whether the terminal can display UTF-8 box-drawing characters.
///
/// On non-Windows platforms, returns `true` unconditionally because
/// virtually all modern Unix terminals support UTF-8.
#[cfg(not(windows))]
const fn terminal_supports_utf8_borders() -> bool {
    true
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

    #[test]
    fn write_tips_renders_shortcut_box() {
        let mut buffer = Vec::new();
        write_tips(&mut buffer).expect("write_tips should succeed");
        let output = String::from_utf8(buffer).expect("output should be valid UTF-8");

        assert!(
            output.contains("portview v"),
            "tips should include the version title"
        );
        assert!(
            output.contains("filter  -p PORT"),
            "tips should include the port filter shortcut"
        );
        assert!(
            output.contains("details --full"),
            "tips should include the details shortcut"
        );
        assert!(
            output.contains("json    --json"),
            "tips should include the JSON shortcut"
        );
        assert!(
            output.contains("help    -h"),
            "tips should include the help shortcut"
        );
    }
}
