//! # Display module
//!
//! Renders `Vec<PortEntry>` as either an aligned terminal table or a JSON
//! array to stdout.

use std::io::{self, IsTerminal, Write};

use anyhow::{Context, Result};

use crate::types::PortEntry;

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

const QUICK_ACTIONS: &[ActionItem] = &[
    ActionItem {
        name: "Filter one port",
        flag: "-p 3000",
        detail: "focus a single service",
    },
    ActionItem {
        name: "Show everything",
        flag: "-a",
        detail: "include every open port",
    },
    ActionItem {
        name: "More detail",
        flag: "--full",
        detail: "add state and user",
    },
    ActionItem {
        name: "Export JSON",
        flag: "--json",
        detail: "script-friendly output",
    },
    ActionItem {
        name: "Help",
        flag: "-h",
        detail: "list every flag",
    },
];

const WIDE_TIPS_THRESHOLD: usize = 72;

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

struct ActionItem {
    name: &'static str,
    flag: &'static str,
    detail: &'static str,
}

#[derive(Clone, Copy)]
enum TerminalStream {
    Stdout,
    Stderr,
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
    write_table_with_width(writer, entries, opts, stdout_terminal_width())
}

fn write_table_with_width(
    writer: &mut impl Write,
    entries: &[PortEntry],
    opts: &DisplayOptions,
    terminal_width: Option<usize>,
) -> Result<()> {
    let columns = table_columns(opts.full);
    let rows = build_rows(entries, columns);
    let widths = fit_table_widths(
        columns,
        &measure_column_widths(columns, &rows, opts.show_header),
        opts.compact,
        terminal_width,
    );

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
    write_tips_with_width(writer, stderr_terminal_width())
}

fn write_tips_with_width(writer: &mut impl Write, terminal_width: Option<usize>) -> Result<()> {
    let version = env!("CARGO_PKG_VERSION");
    let title = format!("Quick Actions  portview v{version}");
    let style = if terminal_supports_utf8_borders() {
        utf8_border_style()
    } else {
        ascii_border_style()
    };
    let tip_box = render_tips_panel(&title, terminal_width, style);

    write!(writer, "\n{tip_box}\n").context("failed to write tips to stderr")?;
    Ok(())
}

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

    shrink_widths_to_budget(
        &mut widths,
        columns,
        &preferred_min_widths,
        compact,
        available_width,
    );

    if rendered_table_width(&widths, compact) > available_width {
        let hard_min_widths = vec![1; columns.len()];
        shrink_widths_to_budget(
            &mut widths,
            columns,
            &hard_min_widths,
            compact,
            available_width,
        );
    }

    widths
}

fn shrink_widths_to_budget(
    widths: &mut [usize],
    columns: &[Column],
    min_widths: &[usize],
    compact: bool,
    available_width: usize,
) {
    let mut overage = rendered_table_width(widths, compact).saturating_sub(available_width);
    if overage == 0 {
        return;
    }

    let mut column_indexes = (0..columns.len()).collect::<Vec<_>>();
    column_indexes.sort_by_key(|index| columns[*index].shrink_priority());

    for index in column_indexes {
        if overage == 0 {
            break;
        }

        let reducible = widths[index].saturating_sub(min_widths[index]);
        let reduction = reducible.min(overage);
        widths[index] -= reduction;
        overage -= reduction;
    }
}

fn rendered_table_width(widths: &[usize], compact: bool) -> usize {
    let content_width = widths.iter().sum::<usize>();
    if compact {
        content_width + widths.len().saturating_sub(1) * 2
    } else {
        content_width + widths.len() * 3 + 1
    }
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
        .map(|(column, width)| pad_value(column.heading(), *width, Alignment::Left))
        .collect::<Vec<_>>();

    render_compact_cells(&cells)
}

fn render_compact_row(row: &[String], columns: &[Column], widths: &[usize]) -> String {
    let cells = row
        .iter()
        .zip(columns)
        .zip(widths)
        .map(|((cell, column), width)| pad_value(cell, *width, column.alignment()))
        .collect::<Vec<_>>();

    render_compact_cells(&cells)
}

fn render_bordered_cells(cells: &[String], vertical: char) -> String {
    let separator = vertical.to_string();
    let joined = cells.join(&separator);
    format!("{vertical}{joined}{vertical}")
}

fn render_compact_cells(cells: &[String]) -> String {
    cells.join("  ")
}

fn format_cell(value: &str, width: usize, alignment: Alignment) -> String {
    let clipped = truncate_to_width(value, width);
    format!(" {} ", pad_value(&clipped, width, alignment))
}

fn pad_value(value: &str, width: usize, alignment: Alignment) -> String {
    let padding = width.saturating_sub(display_width(value));

    match alignment {
        Alignment::Left => format!("{value}{}", " ".repeat(padding)),
        Alignment::Right => format!("{}{value}", " ".repeat(padding)),
    }
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

fn render_tips_panel(title: &str, terminal_width: Option<usize>, style: BorderStyle) -> String {
    if terminal_width.unwrap_or(usize::MAX) >= WIDE_TIPS_THRESHOLD {
        render_wide_tips_panel(title, terminal_width, style)
    } else {
        render_narrow_tips_panel(title, terminal_width, style)
    }
}

fn render_wide_tips_panel(
    title: &str,
    terminal_width: Option<usize>,
    style: BorderStyle,
) -> String {
    let action_width = QUICK_ACTIONS
        .iter()
        .map(|action| display_width(action.name))
        .max()
        .unwrap_or(6)
        .max(display_width("ACTION"));
    let flag_width = QUICK_ACTIONS
        .iter()
        .map(|action| display_width(action.flag))
        .max()
        .unwrap_or(4)
        .max(display_width("FLAG"));
    let detail_width = QUICK_ACTIONS
        .iter()
        .map(|action| display_width(action.detail))
        .max()
        .unwrap_or(12)
        .max(display_width("WHAT IT DOES"));
    let natural_widths = [action_width, flag_width, detail_width];
    let widths = fit_action_widths(&natural_widths, terminal_width);
    let total_width = rendered_table_width(&widths, false);
    let mut lines = vec![render_titled_top_border(title, total_width, style)];

    lines.push(render_action_header(&widths, style.vertical));
    lines.push(render_border_line(
        &widths,
        style.middle_left,
        style.middle_join,
        style.middle_right,
        style.horizontal,
    ));
    lines.extend(
        QUICK_ACTIONS
            .iter()
            .map(|action| render_action_row(action, &widths, style.vertical)),
    );
    lines.push(render_border_line(
        &widths,
        style.bottom_left,
        style.bottom_join,
        style.bottom_right,
        style.horizontal,
    ));

    lines.join("\n")
}

fn render_narrow_tips_panel(
    title: &str,
    terminal_width: Option<usize>,
    style: BorderStyle,
) -> String {
    let inner_width = tip_inner_width(terminal_width)
        .max(display_width(title))
        .max(24);
    let full_width = inner_width + 2;
    let mut lines = vec![render_titled_top_border(title, full_width, style)];

    for (index, action) in QUICK_ACTIONS.iter().enumerate() {
        lines.push(render_panel_line(
            &format!("{}  {}", action.name, action.flag),
            inner_width,
            style.vertical,
        ));
        lines.push(render_panel_line(
            action.detail,
            inner_width,
            style.vertical,
        ));

        if index + 1 != QUICK_ACTIONS.len() {
            lines.push(render_full_width_rule(inner_width, style));
        }
    }

    lines.push(render_panel_bottom_border(inner_width, style));
    lines.join("\n")
}

fn fit_action_widths(natural_widths: &[usize; 3], terminal_width: Option<usize>) -> [usize; 3] {
    let Some(available_width) = terminal_width else {
        return *natural_widths;
    };

    let mut widths = *natural_widths;
    let min_widths = [10, 4, 14];
    let hard_min_widths = [6, 2, 8];

    shrink_action_widths_to_budget(&mut widths, &min_widths, available_width);
    if rendered_table_width(&widths, false) > available_width {
        shrink_action_widths_to_budget(&mut widths, &hard_min_widths, available_width);
    }

    widths
}

fn shrink_action_widths_to_budget(
    widths: &mut [usize; 3],
    min_widths: &[usize; 3],
    available_width: usize,
) {
    let mut overage = rendered_table_width(widths, false).saturating_sub(available_width);
    if overage == 0 {
        return;
    }

    for index in [2, 0, 1] {
        if overage == 0 {
            break;
        }

        let reducible = widths[index].saturating_sub(min_widths[index]);
        let reduction = reducible.min(overage);
        widths[index] -= reduction;
        overage -= reduction;
    }
}

fn render_titled_top_border(title: &str, total_width: usize, style: BorderStyle) -> String {
    let inner_width = total_width.saturating_sub(2);
    let title = format!(" {} ", truncate_to_width(title, inner_width));
    let fill = style
        .horizontal
        .to_string()
        .repeat(inner_width.saturating_sub(display_width(&title)));

    format!("{}{}{fill}{}", style.top_left, title, style.top_right)
}

fn render_action_header(widths: &[usize; 3], vertical: char) -> String {
    let cells = [
        format_cell("ACTION", widths[0], Alignment::Left),
        format_cell("FLAG", widths[1], Alignment::Left),
        format_cell("WHAT IT DOES", widths[2], Alignment::Left),
    ]
    .into_iter()
    .collect::<Vec<_>>();

    render_bordered_cells(&cells, vertical)
}

fn render_action_row(action: &ActionItem, widths: &[usize; 3], vertical: char) -> String {
    let cells = [
        format_cell(action.name, widths[0], Alignment::Left),
        format_cell(action.flag, widths[1], Alignment::Left),
        format_cell(action.detail, widths[2], Alignment::Left),
    ]
    .into_iter()
    .collect::<Vec<_>>();

    render_bordered_cells(&cells, vertical)
}

fn tip_inner_width(terminal_width: Option<usize>) -> usize {
    terminal_width.map_or_else(
        || {
            QUICK_ACTIONS
                .iter()
                .flat_map(|action| [action.name, action.detail])
                .map(display_width)
                .max()
                .unwrap_or(24)
                .max(24)
        },
        |width| width.saturating_sub(2),
    )
}

fn render_panel_line(value: &str, inner_width: usize, vertical: char) -> String {
    let clipped = truncate_to_width(value, inner_width);
    let padding = " ".repeat(inner_width.saturating_sub(display_width(&clipped)));
    format!("{vertical}{clipped}{padding}{vertical}")
}

fn render_full_width_rule(inner_width: usize, style: BorderStyle) -> String {
    format!(
        "{}{}{}",
        style.middle_left,
        style.horizontal.to_string().repeat(inner_width),
        style.middle_right
    )
}

fn render_panel_bottom_border(inner_width: usize, style: BorderStyle) -> String {
    format!(
        "{}{}{}",
        style.bottom_left,
        style.horizontal.to_string().repeat(inner_width),
        style.bottom_right
    )
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
            Self::Process => truncate_process_name(&entry.process),
            Self::Pid => entry.pid.to_string(),
            Self::User => entry.user.clone(),
            Self::Project => entry.project.as_deref().unwrap_or("-").to_string(),
            Self::App => entry.app.as_deref().unwrap_or("-").to_string(),
            Self::Uptime => format_uptime(entry.uptime_secs),
        }
    }
}

fn stdout_terminal_width() -> Option<usize> {
    terminal_width(TerminalStream::Stdout)
}

fn stderr_terminal_width() -> Option<usize> {
    terminal_width(TerminalStream::Stderr)
}

fn terminal_width(stream: TerminalStream) -> Option<usize> {
    env_terminal_width().or_else(|| platform_terminal_width(stream))
}

fn env_terminal_width() -> Option<usize> {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|width| *width > 0)
}

#[cfg(unix)]
fn platform_terminal_width(stream: TerminalStream) -> Option<usize> {
    let fd = match stream {
        TerminalStream::Stdout if io::stdout().is_terminal() => libc::STDOUT_FILENO,
        TerminalStream::Stderr if io::stderr().is_terminal() => libc::STDERR_FILENO,
        TerminalStream::Stdout | TerminalStream::Stderr => return None,
    };

    let mut size = std::mem::MaybeUninit::<libc::winsize>::zeroed();
    let result = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, size.as_mut_ptr()) };
    if result != 0 {
        return None;
    }

    let size = unsafe { size.assume_init() };
    let width = usize::from(size.ws_col);
    (width > 0).then_some(width)
}

#[cfg(windows)]
fn platform_terminal_width(stream: TerminalStream) -> Option<usize> {
    #[repr(C)]
    struct Coord {
        x: i16,
        y: i16,
    }

    #[repr(C)]
    struct SmallRect {
        left: i16,
        top: i16,
        right: i16,
        bottom: i16,
    }

    #[repr(C)]
    struct ConsoleScreenBufferInfo {
        size: Coord,
        cursor_position: Coord,
        attributes: u16,
        window: SmallRect,
        maximum_window_size: Coord,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(handle: i32) -> *mut std::ffi::c_void;
        fn GetConsoleScreenBufferInfo(
            console_output: *mut std::ffi::c_void,
            console_screen_buffer_info: *mut ConsoleScreenBufferInfo,
        ) -> i32;
    }

    const INVALID_HANDLE_VALUE: isize = -1;
    const STD_OUTPUT_HANDLE: i32 = -11;
    const STD_ERROR_HANDLE: i32 = -12;

    let handle_id = match stream {
        TerminalStream::Stdout if io::stdout().is_terminal() => STD_OUTPUT_HANDLE,
        TerminalStream::Stderr if io::stderr().is_terminal() => STD_ERROR_HANDLE,
        TerminalStream::Stdout | TerminalStream::Stderr => return None,
    };

    let handle = unsafe { GetStdHandle(handle_id) };
    if handle.is_null() || handle as isize == INVALID_HANDLE_VALUE {
        return None;
    }

    let mut info = std::mem::MaybeUninit::<ConsoleScreenBufferInfo>::zeroed();
    let ok = unsafe { GetConsoleScreenBufferInfo(handle, info.as_mut_ptr()) };
    if ok == 0 {
        return None;
    }

    let info = unsafe { info.assume_init() };
    let width = i32::from(info.window.right) - i32::from(info.window.left) + 1;
    usize::try_from(width).ok().filter(|value| *value > 0)
}

#[cfg(not(any(unix, windows)))]
fn platform_terminal_width(_stream: TerminalStream) -> Option<usize> {
    None
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
    fn write_table_fits_within_requested_terminal_width() {
        let mut entry = sample_entry();
        entry.project = Some("ms-python.vscode-pylance-2026.2.1".to_string());
        let entries = vec![entry];
        let opts = DisplayOptions {
            show_header: true,
            full: false,
            compact: false,
        };
        let mut buffer = Vec::new();

        write_table_with_width(&mut buffer, &entries, &opts, Some(60))
            .expect("write_table_with_width should succeed");

        let output = String::from_utf8(buffer).expect("output should be valid UTF-8");
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
    fn write_tips_renders_shortcut_box() {
        let mut buffer = Vec::new();
        write_tips(&mut buffer).expect("write_tips should succeed");
        let output = String::from_utf8(buffer).expect("output should be valid UTF-8");

        assert!(
            output.contains("Quick Actions"),
            "tips should include the quick actions title"
        );
        assert!(
            output.contains("portview v"),
            "tips should include the version title"
        );
        assert!(
            output.contains("Filter one port"),
            "tips should include the filter action"
        );
        assert!(
            output.contains("-p 3000"),
            "tips should include the filter example flag"
        );
        assert!(
            output.contains("focus a single service"),
            "tips should include a human-readable description"
        );
        assert!(
            output.contains("script-friendly output"),
            "tips should include the JSON description"
        );
        assert!(
            output.contains("list every flag"),
            "tips should include the help shortcut"
        );
    }

    #[test]
    fn write_tips_narrow_layout_stays_within_width() {
        let mut buffer = Vec::new();

        write_tips_with_width(&mut buffer, Some(48)).expect("write_tips_with_width should succeed");

        let output = String::from_utf8(buffer).expect("output should be valid UTF-8");
        for line in output.lines().filter(|line| !line.is_empty()) {
            assert!(
                display_width(line) <= 48,
                "tip panel line should fit the requested width: {line}"
            );
        }
        assert!(
            output.contains("Filter one port"),
            "narrow tip layout should still include the action label"
        );
        assert!(
            output.contains("focus a single service"),
            "narrow tip layout should still include the action description"
        );
    }
}
