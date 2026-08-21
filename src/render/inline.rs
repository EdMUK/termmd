//! Inline content: styling, tokenising, and line breaking.
//!
//! Wrapping is where terminal Markdown viewers usually give themselves away.
//! Counting `char`s makes CJK text and emoji overflow the right margin; counting
//! bytes makes accented Latin wrap early. We measure in display columns over
//! grapheme clusters, which is the only measure the terminal itself agrees with.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::{Line, Span};
use crate::term::style::Style;

/// Horizontal placement within the available width.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

/// A unit of inline content for the line breaker.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub text: String,
    pub style: Style,
    /// Index into the document's link table, for OSC 8 output.
    pub link: Option<usize>,
    pub kind: Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Breakable whitespace.
    Space,
    /// An atom that we would rather not split.
    Word,
    /// An unconditional line break.
    Break,
}

impl Token {
    pub fn word(text: impl Into<String>, style: Style, link: Option<usize>) -> Self {
        Self {
            text: text.into(),
            style,
            link,
            kind: Kind::Word,
        }
    }
    pub fn space(style: Style) -> Self {
        Self {
            text: " ".into(),
            style,
            link: None,
            kind: Kind::Space,
        }
    }
    pub fn hard_break() -> Self {
        Self {
            text: String::new(),
            style: Style::PLAIN,
            link: None,
            kind: Kind::Break,
        }
    }
    pub fn width(&self) -> usize {
        display_width(&self.text)
    }
}

/// Display width of a string in terminal cells.
///
/// Control characters are given zero width -- they should never reach here, but
/// if one does we would rather mis-measure by nothing than by a negative.
pub fn display_width(s: &str) -> usize {
    s.width()
}

/// Splits a run of text into word and space tokens.
pub fn tokenize_text(text: &str, style: Style, link: Option<usize>, out: &mut Vec<Token>) {
    let mut word = String::new();
    for g in text.graphemes(true) {
        let is_space = g.chars().all(|c| c.is_whitespace() && c != '\u{a0}');
        if is_space {
            if !word.is_empty() {
                out.push(Token::word(std::mem::take(&mut word), style, link));
            }
            // Collapse runs of whitespace: Markdown treats them as one space.
            if !matches!(out.last(), Some(t) if t.kind == Kind::Space) {
                out.push(Token::space(style));
            }
        } else {
            word.push_str(g);
        }
    }
    if !word.is_empty() {
        out.push(Token::word(word, style, link));
    }
}

/// Greedy line breaking.
///
/// `width` is the space available for text. `hanging` is added to every line
/// after the first, which is how list continuation lines stay under their text
/// rather than under their bullet.
pub fn wrap(tokens: &[Token], width: usize, align: Align) -> Vec<Line> {
    let width = width.max(1);
    let mut lines: Vec<Line> = Vec::new();
    let mut current: Vec<Span> = Vec::new();
    let mut used = 0usize;

    let flush = |current: &mut Vec<Span>, used: &mut usize, lines: &mut Vec<Line>| {
        // Trailing spaces would show up as stray background colour.
        while matches!(current.last(), Some(s) if s.text.trim().is_empty() && s.style.bg == crate::term::style::Color::Default)
        {
            current.pop();
        }
        let mut line = Line::from_spans(std::mem::take(current));
        line.align(width, align);
        lines.push(line);
        *used = 0;
    };

    for token in tokens {
        match token.kind {
            Kind::Break => flush(&mut current, &mut used, &mut lines),
            Kind::Space => {
                // A space at the start of a line is dropped, not carried over.
                if used > 0 {
                    current.push(Span::new(" ", token.style, None));
                    used += 1;
                }
            }
            Kind::Word => {
                let w = token.width();
                if used + w > width && used > 0 {
                    flush(&mut current, &mut used, &mut lines);
                }
                if w > width {
                    // A single token too wide for the line: split it across
                    // lines on grapheme boundaries. URLs and CJK runs land here.
                    for chunk in split_to_width(&token.text, width) {
                        let cw = display_width(&chunk);
                        if used + cw > width && used > 0 {
                            flush(&mut current, &mut used, &mut lines);
                        }
                        current.push(Span::new(chunk, token.style, token.link));
                        used += cw;
                    }
                } else {
                    current.push(Span::new(token.text.clone(), token.style, token.link));
                    used += w;
                }
            }
        }
    }
    if !current.is_empty() {
        flush(&mut current, &mut used, &mut lines);
    }
    if lines.is_empty() {
        lines.push(Line::default());
    }
    lines
}

/// Splits a string into pieces that each fit within `width` columns.
pub fn split_to_width(s: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    let mut chunk = String::new();
    let mut used = 0;
    for g in s.graphemes(true) {
        let gw = display_width(g).max(1);
        if used + gw > width && !chunk.is_empty() {
            out.push(std::mem::take(&mut chunk));
            used = 0;
        }
        chunk.push_str(g);
        used += gw;
    }
    if !chunk.is_empty() {
        out.push(chunk);
    }
    out
}

/// Truncates to `width` columns, appending `ellipsis` when it had to cut.
pub fn truncate(s: &str, width: usize, ellipsis: &str) -> String {
    if display_width(s) <= width {
        return s.to_string();
    }
    let ew = display_width(ellipsis);
    if width <= ew {
        return split_to_width(ellipsis, width)
            .into_iter()
            .next()
            .unwrap_or_default();
    }
    let mut out = String::new();
    let mut used = 0;
    for g in s.graphemes(true) {
        let gw = display_width(g).max(1);
        if used + gw > width - ew {
            break;
        }
        out.push_str(g);
        used += gw;
    }
    out.push_str(ellipsis);
    out
}

/// Pads a string on the right to exactly `width` columns.
pub fn pad_to(s: &str, width: usize) -> String {
    let w = display_width(s);
    if w >= width {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(width - w))
    }
}

/// True for characters that need a variation selector to render as text.
pub fn is_wide(c: char) -> bool {
    c.width().unwrap_or(0) > 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<Token> {
        let mut v = Vec::new();
        tokenize_text(text, Style::PLAIN, None, &mut v);
        v
    }

    fn rendered(lines: &[Line]) -> Vec<String> {
        lines.iter().map(|l| l.text()).collect()
    }

    #[test]
    fn measures_in_display_columns() {
        assert_eq!(display_width("hello"), 5);
        assert_eq!(display_width("日本語"), 6, "CJK is double width");
        assert_eq!(
            display_width("café"),
            4,
            "combining accents do not add width"
        );
    }

    #[test]
    fn wraps_on_word_boundaries() {
        let lines = wrap(&words("the quick brown fox jumps"), 10, Align::Left);
        assert_eq!(rendered(&lines), vec!["the quick", "brown fox", "jumps"]);
    }

    #[test]
    fn never_exceeds_the_width_with_cjk() {
        let lines = wrap(&words("日本語のテキストです"), 8, Align::Left);
        for l in &lines {
            assert!(l.width() <= 8, "line {:?} is {} wide", l.text(), l.width());
        }
        assert_eq!(rendered(&lines).concat(), "日本語のテキストです");
    }

    #[test]
    fn breaks_words_longer_than_the_line() {
        let lines = wrap(
            &words("https://example.com/a/very/long/path/indeed"),
            12,
            Align::Left,
        );
        assert!(lines.len() > 1);
        for l in &lines {
            assert!(l.width() <= 12);
        }
        assert_eq!(
            rendered(&lines).concat(),
            "https://example.com/a/very/long/path/indeed"
        );
    }

    #[test]
    fn collapses_whitespace_runs() {
        let lines = wrap(&words("a    b\n\tc"), 40, Align::Left);
        assert_eq!(rendered(&lines), vec!["a b c"]);
    }

    #[test]
    fn drops_trailing_and_leading_spaces() {
        let lines = wrap(&words("  padded  "), 40, Align::Left);
        assert_eq!(rendered(&lines), vec!["padded"]);
    }

    #[test]
    fn hard_breaks_split_lines() {
        let mut t = words("one");
        t.push(Token::hard_break());
        t.extend(words("two"));
        assert_eq!(rendered(&wrap(&t, 40, Align::Left)), vec!["one", "two"]);
    }

    #[test]
    fn centers_and_right_aligns() {
        let lines = wrap(&words("hi"), 6, Align::Center);
        assert_eq!(lines[0].text(), "  hi");
        let lines = wrap(&words("hi"), 6, Align::Right);
        assert_eq!(lines[0].text(), "    hi");
    }

    #[test]
    fn truncates_with_an_ellipsis() {
        assert_eq!(truncate("hello world", 8, "…"), "hello w…");
        assert_eq!(truncate("short", 8, "…"), "short");
        assert_eq!(truncate("日本語です", 5, "…"), "日本…");
    }

    #[test]
    fn pads_to_exact_width() {
        assert_eq!(display_width(&pad_to("日本", 6)), 6);
        assert_eq!(pad_to("abc", 2), "abc", "never truncates");
    }

    #[test]
    fn empty_input_still_produces_a_line() {
        assert_eq!(wrap(&[], 10, Align::Left).len(), 1);
    }
}
