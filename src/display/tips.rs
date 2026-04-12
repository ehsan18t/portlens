//! Tips panel rendering.
//!
//! Draws the "Quick Actions" footer box that appears below the port table
//! when stdout is a terminal. Adapts between wide (3-column table) and
//! narrow (stacked card) layouts based on terminal width.

use std::io::Write;

use anyhow::{Context, Result};

use super::terminal::{stderr_terminal_width, terminal_supports_utf8_borders};
use super::{
    Alignment, BorderStyle, ascii_border_style, display_width, format_cell, render_border_line,
    render_bordered_cells, rendered_table_width, truncate_to_width, utf8_border_style,
};

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

struct ActionItem {
    name: &'static str,
    flag: &'static str,
    detail: &'static str,
}

pub(super) fn write_tips(writer: &mut impl Write) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
