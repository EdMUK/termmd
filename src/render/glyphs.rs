//! Two drawing alphabets: one that assumes a capable terminal, one that assumes
//! nothing. Every glyph the renderer uses comes from here, so an ASCII-only
//! terminal degrades uniformly instead of ending up with half a box drawn.

use crate::term::caps::UnicodeLevel;

#[derive(Debug, Clone, Copy)]
pub struct Glyphs {
    pub bullets: [&'static str; 3],
    pub quote_bar: &'static str,
    pub rule: &'static str,
    pub heading_rule: &'static str,
    pub task_done: &'static str,
    pub task_todo: &'static str,
    pub ellipsis: &'static str,
    pub continuation: &'static str,

    // Table borders, in the order used by `table.rs`.
    pub h: &'static str,
    pub v: &'static str,
    pub top_left: &'static str,
    pub top_mid: &'static str,
    pub top_right: &'static str,
    pub mid_left: &'static str,
    pub mid_mid: &'static str,
    pub mid_right: &'static str,
    pub bottom_left: &'static str,
    pub bottom_mid: &'static str,
    pub bottom_right: &'static str,

    /// Marks an image we could not draw.
    pub image_marker: &'static str,
    pub link_marker: &'static str,
    /// Half block used by the blocks image backend.
    pub upper_half: &'static str,
}

pub const UNICODE: Glyphs = Glyphs {
    bullets: ["•", "◦", "▪"],
    quote_bar: "▌",
    rule: "─",
    heading_rule: "─",
    task_done: "✔",
    task_todo: "☐",
    ellipsis: "…",
    continuation: "↪",
    h: "─",
    v: "│",
    top_left: "┌",
    top_mid: "┬",
    top_right: "┐",
    mid_left: "├",
    mid_mid: "┼",
    mid_right: "┤",
    bottom_left: "└",
    bottom_mid: "┴",
    bottom_right: "┘",
    image_marker: "🖼",
    link_marker: "↗",
    upper_half: "▀",
};

pub const ASCII: Glyphs = Glyphs {
    bullets: ["*", "-", "+"],
    quote_bar: "|",
    rule: "-",
    heading_rule: "=",
    task_done: "x",
    task_todo: " ",
    ellipsis: "...",
    continuation: ">",
    h: "-",
    v: "|",
    top_left: "+",
    top_mid: "+",
    top_right: "+",
    mid_left: "+",
    mid_mid: "+",
    mid_right: "+",
    bottom_left: "+",
    bottom_mid: "+",
    bottom_right: "+",
    image_marker: "[img]",
    link_marker: "->",
    upper_half: "#",
};

impl Glyphs {
    pub fn for_level(level: UnicodeLevel) -> Self {
        match level {
            UnicodeLevel::Ascii => ASCII,
            // The extended set avoids emoji, which are double-width and not
            // universally available; only `Full` gets the picture frame.
            UnicodeLevel::Extended => Glyphs {
                image_marker: "[image]",
                ..UNICODE
            },
            UnicodeLevel::Full => UNICODE,
        }
    }

    /// The bullet for a list nesting depth, cycling for deep nesting.
    pub fn bullet(&self, depth: usize) -> &'static str {
        self.bullets[depth % self.bullets.len()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_glyphs_are_ascii() {
        let g = ASCII;
        for s in [
            g.quote_bar,
            g.rule,
            g.task_done,
            g.v,
            g.top_left,
            g.bullets[0],
        ] {
            assert!(s.is_ascii(), "{s:?} should be ASCII");
        }
    }

    #[test]
    fn bullets_cycle_with_depth() {
        let g = UNICODE;
        assert_eq!(g.bullet(0), "•");
        assert_eq!(g.bullet(3), g.bullet(0));
    }

    #[test]
    fn extended_avoids_emoji() {
        assert_eq!(
            Glyphs::for_level(UnicodeLevel::Extended).image_marker,
            "[image]"
        );
    }
}
