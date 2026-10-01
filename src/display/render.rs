//! Shared rendering primitives used by [`super::table`] and [`super::tips`].
//!
//! This module holds the low-level cell/border/border-style helpers so that
//! `mod.rs` stays a thin public-API façade and each consumer submodule
//! imports only what it needs.

use std::borrow::Cow;

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

const ELLIPSIS: char = '…';
// Keep the width logic dependency-free while still handling the terminal cases
// this crate actually emits: combining marks, zero-width controls, CJK, and
// common emoji/full-width ranges.
const ZERO_WIDTH_RANGES: &[(char, char)] = &[
    ('\u{0300}', '\u{036F}'),
    ('\u{1AB0}', '\u{1AFF}'),
    ('\u{1DC0}', '\u{1DFF}'),
    ('\u{200B}', '\u{200F}'),
    ('\u{202A}', '\u{202E}'),
    ('\u{2060}', '\u{206F}'),
    ('\u{20D0}', '\u{20FF}'),
    ('\u{FE00}', '\u{FE0F}'),
    ('\u{FE20}', '\u{FE2F}'),
    ('\u{E0100}', '\u{E01EF}'),
];
const WIDE_RANGES: &[(char, char)] = &[
    ('\u{1100}', '\u{115F}'),
    ('\u{2329}', '\u{232A}'),
    ('\u{2E80}', '\u{A4CF}'),
    ('\u{AC00}', '\u{D7A3}'),
    ('\u{F900}', '\u{FAFF}'),
    ('\u{FE10}', '\u{FE19}'),
    ('\u{FE30}', '\u{FE6F}'),
    ('\u{FF00}', '\u{FF60}'),
    ('\u{FFE0}', '\u{FFE6}'),
    ('\u{1F300}', '\u{1F64F}'),
    ('\u{1F680}', '\u{1F6FF}'),
    ('\u{1F900}', '\u{1F9FF}'),
    ('\u{1FA70}', '\u{1FAFF}'),
    ('\u{20000}', '\u{2FFFD}'),
    ('\u{30000}', '\u{3FFFD}'),
];

/// Visible stand-in for characters that must never reach the terminal raw.
const TERMINAL_REPLACEMENT: char = '\u{FFFD}';

/// Make externally sourced text safe to write to a terminal.
///
/// Process names, user names, project names and app labels come from sources
/// an unprivileged user can control (for example `prctl(PR_SET_NAME)` or repo
/// metadata). Writing them raw would let that user inject escape sequences
/// into the terminal of whoever runs portlens. Every control character
/// (C0, DEL, and C1 including the single-byte CSI `U+009B`) and every bidi
/// override, embedding, isolate or mark is replaced with `U+FFFD` so the text
/// stays visible, cannot drive the terminal, and cannot visually reorder
/// neighbouring columns.
///
/// Returns [`Cow::Borrowed`] when the input is already clean, so the common
/// path does not allocate.
pub fn sanitize_for_terminal(value: &str) -> Cow<'_, str> {
    if !value.chars().any(is_terminal_unsafe) {
        return Cow::Borrowed(value);
    }

    Cow::Owned(
        value
            .chars()
            .map(|ch| {
                if is_terminal_unsafe(ch) {
                    TERMINAL_REPLACEMENT
                } else {
                    ch
                }
            })
            .collect(),
    )
}

/// Whether `ch` can alter terminal state or visual ordering when printed.
// Not `const`: `char::is_control` is only const since Rust 1.97, above the MSRV.
fn is_terminal_unsafe(ch: char) -> bool {
    ch.is_control()
        || matches!(
            ch,
            // Arabic letter mark, LRM and RLM: invisible direction marks.
            '\u{061C}'
                | '\u{200E}'..='\u{200F}'
                // LRE, RLE, PDF, LRO, RLO: embeddings and overrides.
                | '\u{202A}'..='\u{202E}'
                // LRI, RLI, FSI, PDI: isolates.
                | '\u{2066}'..='\u{2069}'
        )
}

pub(super) fn display_width(value: &str) -> usize {
    let mut width = 0;
    let mut chars = value.chars().peekable();

    while let Some(ch) = chars.next() {
        // CRLF is a line ending with zero horizontal display width.
        // Consume the pair so `\r` is not counted independently.
        if ch == '\r' && chars.peek() == Some(&'\n') {
            chars.next();
            continue;
        }

        width += char_display_width(ch);
    }

    width
}

pub(super) fn truncate_to_width(value: &str, width: usize) -> String {
    if display_width(value) <= width {
        return value.to_string();
    }

    if width == 0 {
        return String::new();
    }

    let ellipsis_width = char_display_width(ELLIPSIS);
    if width <= ellipsis_width {
        return ELLIPSIS.to_string();
    }

    let available_width = width - ellipsis_width;
    let mut visible_width = 0;
    let mut end = 0;

    for (index, ch) in value.char_indices() {
        let char_width = char_display_width(ch);
        if visible_width + char_width > available_width {
            break;
        }

        visible_width += char_width;
        end = index + ch.len_utf8();
    }

    let mut truncated = value[..end].to_string();
    truncated.push(ELLIPSIS);
    truncated
}

const fn char_display_width(ch: char) -> usize {
    if is_zero_width(ch) || is_control(ch) {
        return 0;
    }

    if is_wide(ch) {
        return 2;
    }

    1
}

const fn is_control(ch: char) -> bool {
    matches!(ch, '\u{0000}'..='\u{001F}' | '\u{007F}'..='\u{009F}')
}

const fn is_zero_width(ch: char) -> bool {
    ch == '\u{034F}' || in_ranges(ch, ZERO_WIDTH_RANGES)
}

const fn is_wide(ch: char) -> bool {
    matches!(
        ch,
        '\u{231A}'..='\u{231B}'
            | '\u{23E9}'..='\u{23EC}'
            | '\u{23F0}'
            | '\u{23F3}'
            | '\u{25FD}'..='\u{25FE}'
            | '\u{2614}'..='\u{2615}'
            | '\u{2648}'..='\u{2653}'
            | '\u{267F}'
            | '\u{2693}'
            | '\u{26A1}'
            | '\u{26AA}'..='\u{26AB}'
            | '\u{26BD}'..='\u{26BE}'
            | '\u{26C4}'..='\u{26C5}'
            | '\u{26CE}'
            | '\u{26D4}'
            | '\u{26EA}'
            | '\u{26F2}'..='\u{26F3}'
            | '\u{26F5}'
            | '\u{26FA}'
            | '\u{26FD}'
            | '\u{2705}'
            | '\u{270A}'..='\u{270B}'
            | '\u{2728}'
            | '\u{274C}'
            | '\u{274E}'
            | '\u{2753}'..='\u{2755}'
            | '\u{2757}'
            | '\u{2795}'..='\u{2797}'
            | '\u{27B0}'
            | '\u{27BF}'
            | '\u{2B1B}'..='\u{2B1C}'
            | '\u{2B50}'
            | '\u{2B55}'
    ) || in_ranges(ch, WIDE_RANGES)
}

const fn in_ranges(ch: char, ranges: &[(char, char)]) -> bool {
    let mut index = 0;
    while index < ranges.len() {
        let (start, end) = ranges[index];
        if ch >= start && ch <= end {
            return true;
        }
        index += 1;
    }

    false
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

pub(super) fn reduce_widths_to_fit(
    widths: &mut [usize],
    min_widths: &[usize],
    shrink_order: &[usize],
    compact: bool,
    available_width: usize,
) {
    debug_assert_eq!(widths.len(), min_widths.len());

    let mut overage = rendered_table_width(widths, compact).saturating_sub(available_width);
    if overage == 0 {
        return;
    }

    for &index in shrink_order {
        if overage == 0 {
            break;
        }

        let reducible = widths[index].saturating_sub(min_widths[index]);
        let reduction = reducible.min(overage);
        widths[index] -= reduction;
        overage -= reduction;
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

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::{display_width, sanitize_for_terminal, truncate_to_width};

    #[test]
    fn sanitize_replaces_csi_escape_sequence() {
        let result = sanitize_for_terminal("a\x1b[2Jb");
        assert_eq!(result, "a\u{FFFD}[2Jb", "ESC must be replaced");
        assert!(!result.contains('\x1b'), "no raw ESC may survive");
    }

    #[test]
    fn sanitize_replaces_osc_title_sequence_with_bel() {
        let result = sanitize_for_terminal("\x1b]0;pwn\x07");
        assert_eq!(
            result, "\u{FFFD}]0;pwn\u{FFFD}",
            "ESC and BEL terminator must both be replaced"
        );
    }

    #[test]
    fn sanitize_replaces_c1_csi_and_del() {
        let result = sanitize_for_terminal("x\u{009B}2Jy\u{007F}");
        assert_eq!(
            result, "x\u{FFFD}2Jy\u{FFFD}",
            "single-byte C1 CSI and DEL must be replaced"
        );
    }

    #[test]
    fn sanitize_replaces_newlines_and_tabs() {
        assert_eq!(
            sanitize_for_terminal("a\r\nb\tc"),
            "a\u{FFFD}\u{FFFD}b\u{FFFD}c",
            "line breaks and tabs would break row layout"
        );
    }

    #[test]
    fn sanitize_replaces_bidi_overrides_and_isolates() {
        for ch in [
            '\u{061C}', '\u{200E}', '\u{200F}', '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}',
            '\u{202E}', '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}',
        ] {
            let input = format!("ab{ch}cd");
            assert_eq!(
                sanitize_for_terminal(&input),
                "ab\u{FFFD}cd",
                "bidi control U+{:04X} must be replaced",
                u32::from(ch)
            );
        }
    }

    #[test]
    fn sanitize_borrows_clean_input() {
        for clean in ["node", "my-app", "Next.js", "世界", "e\u{0301}", ""] {
            assert!(
                matches!(sanitize_for_terminal(clean), Cow::Borrowed(s) if s == clean),
                "clean input {clean:?} should be borrowed without allocating"
            );
        }
    }

    #[test]
    fn sanitized_text_has_one_column_per_replaced_char() {
        assert_eq!(
            display_width(&sanitize_for_terminal("a\x1b[2Jb")),
            6,
            "each replaced control character must occupy exactly one column"
        );
    }

    #[test]
    fn display_width_treats_cjk_as_double_width() {
        assert_eq!(
            display_width("世界"),
            4,
            "CJK glyphs should consume two columns each"
        );
    }

    #[test]
    fn display_width_treats_crlf_as_zero_width() {
        assert_eq!(
            display_width("hello\r\nworld"),
            10,
            "CRLF line endings should have zero horizontal display width"
        );
    }

    #[test]
    fn display_width_ignores_combining_marks() {
        assert_eq!(
            display_width("e\u{0301}"),
            1,
            "combining marks should not add columns"
        );
    }

    #[test]
    fn truncate_to_width_respects_cjk_column_width() {
        let result = truncate_to_width("世界abc", 4);

        assert_eq!(result, "世…");
        assert!(
            display_width(&result) <= 4,
            "truncated CJK text must fit the requested width"
        );
    }

    #[test]
    fn truncate_to_width_preserves_combining_sequences() {
        let result = truncate_to_width("e\u{0301}abc", 3);

        assert_eq!(result, "e\u{0301}a…");
        assert_eq!(
            display_width(&result),
            3,
            "combining sequences should retain the correct width"
        );
    }
}
