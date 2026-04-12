//! Shared rendering primitives used by [`super::table`] and [`super::tips`].
//!
//! This module holds the low-level cell/border/border-style helpers so that
//! `mod.rs` stays a thin public-API façade and each consumer submodule
//! imports only what it needs.

#[derive(Clone, Copy)]
pub(super) enum Alignment {
    Left,
    Right,
}

#[derive(Clone, Copy)]
pub(super) struct BorderStyle {
    pub(super) vertical: char,
    pub(super) horizontal: char,
    pub(super) top_left: char,
    pub(super) top_join: char,
    pub(super) top_right: char,
    pub(super) middle_left: char,
    pub(super) middle_join: char,
    pub(super) middle_right: char,
    pub(super) bottom_left: char,
    pub(super) bottom_join: char,
    pub(super) bottom_right: char,
}

pub(super) fn display_width(value: &str) -> usize {
    value.chars().count()
}

pub(super) fn truncate_to_width(value: &str, width: usize) -> String {
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

pub(super) fn pad_value(value: &str, width: usize, alignment: Alignment) -> String {
    let padding = width.saturating_sub(display_width(value));

    match alignment {
        Alignment::Left => format!("{value}{}", " ".repeat(padding)),
        Alignment::Right => format!("{}{value}", " ".repeat(padding)),
    }
}

pub(super) fn format_cell(value: &str, width: usize, alignment: Alignment) -> String {
    let clipped = truncate_to_width(value, width);
    format!(" {} ", pad_value(&clipped, width, alignment))
}

pub(super) fn render_border_line(
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

pub(super) fn render_bordered_cells(cells: &[String], vertical: char) -> String {
    let separator = vertical.to_string();
    let joined = cells.join(&separator);
    format!("{vertical}{joined}{vertical}")
}

pub(super) fn rendered_table_width(widths: &[usize], compact: bool) -> usize {
    let content_width = widths.iter().sum::<usize>();
    if compact {
        content_width + widths.len().saturating_sub(1) * 2
    } else {
        content_width + widths.len() * 3 + 1
    }
}

// ── Border style presets ────────────────────────────────────────────

pub(super) const fn utf8_border_style() -> BorderStyle {
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

pub(super) const fn ascii_border_style() -> BorderStyle {
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
