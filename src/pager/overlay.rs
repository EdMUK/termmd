//! Overlay panels: the table of contents, the link list, and help.
//!
//! One panel implementation serves all three. It is a centred, bordered box with
//! a title and a scrolling list, drawn over whatever is already on screen.

use crate::render::Glyphs;
use crate::render::inline::{display_width, pad_to, truncate};
use crate::term::style::StyleWriter;
use crate::theme::Theme;

/// One row in a panel.
#[derive(Debug, Clone)]
pub struct Item {
    pub text: String,
    /// Right-hand column, used by the help panel for descriptions.
    pub detail: String,
}

/// Everything needed to draw a panel.
pub struct Panel<'a> {
    pub title: &'a str,
    pub items: &'a [Item],
    pub selection: usize,
    pub cols: u16,
    pub rows: u16,
    pub theme: &'a Theme,
    pub glyphs: Glyphs,
}

/// The keys the help panel lists.
pub const HELP: &[(&str, &str)] = &[
    ("j / k / arrows", "scroll a line"),
    ("space / b", "scroll a page"),
    ("d / u", "scroll half a page"),
    ("g / G", "go to the start or end"),
    ("h / l", "scroll sideways"),
    ("0", "back to the left edge"),
    ("/", "search forwards"),
    ("? ", "search backwards"),
    ("n / N", "next or previous match"),
    ("t", "table of contents"),
    ("L", "links"),
    ("backspace", "back to the previous document"),
    ("m", "give termmd the mouse, or give it back"),
    ("drag", "select text (the terminal's own selection)"),
    ("i", "toggle images"),
    ("r", "reload"),
    ("H / F1", "this help"),
    ("q / esc", "quit"),
];

pub fn title(panel: super::Panel) -> &'static str {
    match panel {
        super::Panel::Contents => "Contents",
        super::Panel::Links => "Links",
        super::Panel::Help => "Keys",
    }
}

/// Draws the panel into `out`.
pub fn draw(out: &mut String, writer: &mut StyleWriter, panel: &Panel<'_>) {
    let g = panel.glyphs;
    let theme = panel.theme;

    // Size the box to the content, within what the screen can hold.
    let widest = panel
        .items
        .iter()
        .map(|i| {
            display_width(&i.text)
                + if i.detail.is_empty() {
                    0
                } else {
                    display_width(&i.detail) + 3
                }
        })
        .max()
        .unwrap_or(0)
        .max(display_width(panel.title) + 2);

    let inner_width = widest.clamp(16, (panel.cols as usize).saturating_sub(6).max(16));
    let max_rows = (panel.rows as usize).saturating_sub(6).max(3);
    let visible = panel.items.len().min(max_rows).max(1);

    let box_width = inner_width + 4;
    let box_height = visible + 2;
    let left = ((panel.cols as usize).saturating_sub(box_width)) / 2;
    let top = ((panel.rows as usize).saturating_sub(box_height)) / 2;

    // Keep the selection on screen.
    let start = panel
        .selection
        .saturating_sub(visible.saturating_sub(1))
        .min(panel.items.len().saturating_sub(visible));

    // Every cell the panel draws carries its background, so the panel is
    // opaque over whatever is beneath it -- including an image.
    let panel_bg = theme.panel;
    let border = panel_bg.merge(theme.table_border);
    let mut row = top;

    // Top edge, with the title set into it.
    let title = format!(" {} ", panel.title);
    let filled = inner_width + 2;
    let title_width = display_width(&title).min(filled);
    let after = filled - title_width;
    place(out, row, left);
    writer.transition(border, out);
    out.push_str(g.top_left);
    out.push_str(&title);
    out.push_str(&g.h.repeat(after));
    out.push_str(g.top_right);
    row += 1;

    for offset in 0..visible {
        let index = start + offset;
        place(out, row, left);
        writer.transition(border, out);
        out.push_str(g.v);

        match panel.items.get(index) {
            Some(item) => {
                let selected = index == panel.selection;
                let style = if selected {
                    theme.search_current
                } else {
                    panel_bg.merge(theme.text)
                };
                let marker = if selected { ">" } else { " " };

                let text = if item.detail.is_empty() {
                    truncate(&item.text, inner_width, g.ellipsis)
                } else {
                    // Two columns: keys on the left, description on the right.
                    let key_width = inner_width / 3;
                    format!(
                        "{}  {}",
                        pad_to(&truncate(&item.text, key_width, g.ellipsis), key_width),
                        truncate(&item.detail, inner_width - key_width - 2, g.ellipsis)
                    )
                };
                writer.transition(style, out);
                out.push_str(marker);
                out.push_str(&pad_to(&text, inner_width));
                out.push(' ');
            }
            None => {
                writer.transition(panel_bg, out);
                out.push_str(&" ".repeat(inner_width + 2));
            }
        }
        writer.transition(border, out);
        out.push_str(g.v);
        row += 1;
    }

    place(out, row, left);
    writer.transition(border, out);
    out.push_str(g.bottom_left);
    out.push_str(&g.h.repeat(inner_width + 2));
    out.push_str(g.bottom_right);
    writer.reset(out);
}

fn place(out: &mut String, row: usize, col: usize) {
    out.push_str(&format!("\x1b[{};{}H", row + 1, col + 1));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::glyphs;
    use crate::term::caps::ColorDepth;

    fn render(items: &[Item], selection: usize, cols: u16, rows: u16) -> String {
        let theme = Theme::mono();
        let panel = Panel {
            title: "Contents",
            items,
            selection,
            cols,
            rows,
            theme: &theme,
            glyphs: glyphs::ASCII,
        };
        let mut out = String::new();
        let mut writer = StyleWriter::new(ColorDepth::None);
        draw(&mut out, &mut writer, &panel);
        out
    }

    fn items(names: &[&str]) -> Vec<Item> {
        names
            .iter()
            .map(|n| Item {
                text: (*n).into(),
                detail: String::new(),
            })
            .collect()
    }

    #[test]
    fn draws_a_bordered_box_with_a_title() {
        let out = render(&items(&["one", "two"]), 0, 60, 20);
        assert!(out.contains("Contents"));
        assert!(out.contains('+'), "expected ASCII corners: {out:?}");
        assert!(out.contains("one") && out.contains("two"));
    }

    #[test]
    fn marks_the_selection() {
        let out = render(&items(&["one", "two"]), 1, 60, 20);
        assert!(
            out.contains(">two"),
            "selected row should be marked: {out:?}"
        );
        assert!(!out.contains(">one"));
    }

    #[test]
    fn scrolls_to_keep_the_selection_visible() {
        let many: Vec<String> = (0..50).map(|i| format!("item{i}")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let out = render(&items(&refs), 49, 60, 12);
        assert!(out.contains("item49"), "the selection must be on screen");
        assert!(
            !out.contains("item0 "),
            "the top of the list should have scrolled away"
        );
    }

    #[test]
    fn truncates_long_entries_to_the_box() {
        let long = "x".repeat(200);
        let out = render(&items(&[&long]), 0, 40, 20);
        for line in out.split("\x1b[").filter(|s| s.contains('x')) {
            assert!(line.len() < 60, "row was not truncated: {line:?}");
        }
    }

    #[test]
    fn survives_a_tiny_terminal() {
        let out = render(&items(&["a", "b", "c"]), 0, 10, 5);
        assert!(!out.is_empty());
    }

    #[test]
    fn empty_panels_still_draw_a_box() {
        let out = render(&[], 0, 40, 20);
        assert!(out.contains('+'));
    }

    #[test]
    fn help_entries_are_two_columns() {
        let entries: Vec<Item> = HELP
            .iter()
            .map(|(k, d)| Item {
                text: (*k).into(),
                detail: (*d).into(),
            })
            .collect();
        let out = render(&entries, 0, 70, 30);
        assert!(
            out.contains("quit"),
            "descriptions should be drawn: {out:?}"
        );
    }
}
