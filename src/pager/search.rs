//! Incremental search over a rendered [`Screen`].
//!
//! Matching runs against each line's plain text and records positions in display
//! columns, not byte offsets. That is what lets a match be highlighted after the
//! line has been sliced for horizontal scrolling, and it is the difference
//! between highlighting the right characters and highlighting the right number
//! of bytes starting in the wrong place.

use unicode_segmentation::UnicodeSegmentation;

use crate::render::inline::display_width;
use crate::render::{Line, Screen, Span, merge_spans};
use crate::term::style::Style;

/// One match, in display columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

/// The state of a search: what was asked for, and what was found.
#[derive(Debug, Clone, Default)]
pub struct Search {
    pub query: String,
    pub matches: Vec<Match>,
    /// Index into `matches` of the one currently focused.
    pub current: usize,
}

impl Search {
    /// Runs a query over a screen.
    ///
    /// Case handling follows the "smart case" convention: an all-lowercase query
    /// matches any case, and a query with a capital in it is taken literally.
    pub fn new(query: &str, screen: &Screen) -> Self {
        let matches = if query.is_empty() {
            Vec::new()
        } else {
            find_all(query, &screen.lines)
        };
        Self {
            query: query.to_string(),
            matches,
            current: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.matches.is_empty()
    }

    pub fn len(&self) -> usize {
        self.matches.len()
    }

    /// Focuses the first match at or after `line`, wrapping around the end.
    pub fn focus_from(&mut self, line: usize) -> Option<Match> {
        if self.matches.is_empty() {
            return None;
        }
        let index = self
            .matches
            .iter()
            .position(|m| m.line >= line)
            .unwrap_or(0);
        self.current = index;
        Some(self.matches[index])
    }

    /// Moves to the next match, wrapping.
    pub fn advance(&mut self) -> Option<Match> {
        if self.matches.is_empty() {
            return None;
        }
        self.current = (self.current + 1) % self.matches.len();
        Some(self.matches[self.current])
    }

    /// Moves to the previous match, wrapping.
    pub fn retreat(&mut self) -> Option<Match> {
        if self.matches.is_empty() {
            return None;
        }
        self.current = if self.current == 0 {
            self.matches.len() - 1
        } else {
            self.current - 1
        };
        Some(self.matches[self.current])
    }

    pub fn focused(&self) -> Option<Match> {
        self.matches.get(self.current).copied()
    }

    /// The matches that fall on one line.
    pub fn on_line(&self, line: usize) -> impl Iterator<Item = (Match, bool)> + '_ {
        let focused = self.focused();
        self.matches
            .iter()
            .filter(move |m| m.line == line)
            .map(move |m| (*m, Some(*m) == focused))
    }
}

/// Finds every non-overlapping occurrence of `query`.
fn find_all(query: &str, lines: &[Line]) -> Vec<Match> {
    let case_sensitive = query.chars().any(char::is_uppercase);
    let needle = if case_sensitive {
        query.to_string()
    } else {
        query.to_lowercase()
    };
    let mut out = Vec::new();

    for (index, line) in lines.iter().enumerate() {
        let text = line.text();
        let haystack = if case_sensitive {
            text.clone()
        } else {
            text.to_lowercase()
        };
        // Lowercasing can change byte lengths (for example 'İ'), so positions
        // are mapped back through the original text by grapheme count rather
        // than by byte offset arithmetic.
        let mut from = 0usize;
        while let Some(found) = haystack[from..].find(&needle) {
            let start_byte = from + found;
            let end_byte = start_byte + needle.len();
            let start = column_of(&haystack, start_byte);
            let end = column_of(&haystack, end_byte);
            out.push(Match {
                line: index,
                start,
                end,
            });
            from = end_byte.max(start_byte + 1);
            if from >= haystack.len() {
                break;
            }
        }
    }
    out
}

/// The display column at a byte offset, rounding down to a char boundary.
fn column_of(text: &str, byte: usize) -> usize {
    let mut end = byte.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    display_width(&text[..end])
}

/// Applies search highlighting to spans that have already been sliced for the
/// viewport.
///
/// `base_col` is the display column the first span starts at, so the ranges --
/// which are in document columns -- line up with what is on screen.
pub fn highlight(
    spans: Vec<Span>,
    base_col: usize,
    ranges: &[(Match, bool)],
    normal: Style,
    focused: Style,
) -> Vec<Span> {
    if ranges.is_empty() {
        return spans;
    }
    let mut out: Vec<Span> = Vec::with_capacity(spans.len() + ranges.len() * 2);
    let mut column = base_col;

    for span in spans {
        let mut buffer = String::new();
        let mut buffer_style = span.style;

        for grapheme in span.text.graphemes(true) {
            let width = display_width(grapheme).max(1);
            let overlay = ranges
                .iter()
                .find(|(m, _)| column >= m.start && column < m.end)
                .map(|(_, is_focused)| if *is_focused { focused } else { normal });
            let style = match overlay {
                Some(highlight) => span.style.merge(highlight),
                None => span.style,
            };
            if style != buffer_style && !buffer.is_empty() {
                out.push(Span::new(
                    std::mem::take(&mut buffer),
                    buffer_style,
                    span.link,
                ));
            }
            buffer_style = style;
            buffer.push_str(grapheme);
            column += width;
        }
        if !buffer.is_empty() {
            out.push(Span::new(buffer, buffer_style, span.link));
        }
    }
    merge_spans(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::Line;

    fn screen_of(lines: &[&str]) -> Screen {
        Screen {
            lines: lines
                .iter()
                .map(|t| Line::from_spans(vec![Span::plain(*t)]))
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn finds_every_occurrence() {
        let s = Search::new(
            "the",
            &screen_of(&["the cat", "sat on the mat", "no match here"]),
        );
        assert_eq!(s.len(), 2);
        assert_eq!(
            s.matches[0],
            Match {
                line: 0,
                start: 0,
                end: 3
            }
        );
        assert_eq!(
            s.matches[1],
            Match {
                line: 1,
                start: 7,
                end: 10
            }
        );
    }

    #[test]
    fn finds_repeats_on_one_line() {
        let s = Search::new("ab", &screen_of(&["ababab"]));
        assert_eq!(s.len(), 3);
        assert_eq!(s.matches[2].start, 4);
    }

    #[test]
    fn smart_case_matches_any_case_when_the_query_is_lowercase() {
        let s = Search::new("cat", &screen_of(&["Cat", "CAT", "cat"]));
        assert_eq!(s.len(), 3);
    }

    #[test]
    fn an_uppercase_query_is_taken_literally() {
        let s = Search::new("Cat", &screen_of(&["Cat", "CAT", "cat"]));
        assert_eq!(s.len(), 1);
        assert_eq!(s.matches[0].line, 0);
    }

    #[test]
    fn positions_are_display_columns_not_bytes() {
        // Each CJK character is two columns wide and three bytes long.
        let s = Search::new("x", &screen_of(&["日本語x"]));
        assert_eq!(
            s.matches[0].start, 6,
            "should be columns, not the byte offset 9"
        );
    }

    #[test]
    fn navigation_wraps_in_both_directions() {
        let mut s = Search::new("a", &screen_of(&["a", "a", "a"]));
        assert_eq!(s.focused().unwrap().line, 0);
        s.advance();
        s.advance();
        assert_eq!(s.focused().unwrap().line, 2);
        assert_eq!(s.advance().unwrap().line, 0, "should wrap to the start");
        assert_eq!(s.retreat().unwrap().line, 2, "should wrap to the end");
    }

    #[test]
    fn focus_from_finds_the_next_match_below() {
        let mut s = Search::new("a", &screen_of(&["a", "b", "a"]));
        assert_eq!(s.focus_from(1).unwrap().line, 2);
        assert_eq!(s.focus_from(99).unwrap().line, 0, "wraps when past the end");
    }

    #[test]
    fn handles_text_whose_case_changes_length() {
        // 'İ' lowercases to two code points, so byte offsets shift.
        let s = Search::new("x", &screen_of(&["İx"]));
        assert_eq!(s.len(), 1, "should still find the match");
        assert!(s.matches[0].start <= 2, "column should stay in range");
    }

    #[test]
    fn overlapping_candidates_do_not_double_count() {
        let s = Search::new("aa", &screen_of(&["aaaa"]));
        assert_eq!(s.len(), 2, "matches must not overlap");
    }

    #[test]
    fn empty_queries_match_nothing() {
        let s = Search::new("", &screen_of(&["anything"]));
        assert!(s.is_empty());
        assert_eq!(s.focused(), None);
    }

    #[test]
    fn highlighting_splits_spans_at_match_boundaries() {
        let spans = vec![Span::plain("hello world")];
        let m = Match {
            line: 0,
            start: 6,
            end: 11,
        };
        let out = highlight(
            spans,
            0,
            &[(m, true)],
            Style::PLAIN.reverse(),
            Style::PLAIN.bold(),
        );
        let text: String = out.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(text, "hello world", "text must survive unchanged");
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].text, "world");
        assert!(
            out[1].style.bold,
            "the focused match takes the focused style"
        );
    }

    #[test]
    fn highlighting_accounts_for_horizontal_scrolling() {
        // The viewport starts at column 6, so the visible text is "world".
        let spans = vec![Span::plain("world")];
        let m = Match {
            line: 0,
            start: 6,
            end: 11,
        };
        let out = highlight(
            spans,
            6,
            &[(m, false)],
            Style::PLAIN.reverse(),
            Style::PLAIN.bold(),
        );
        assert_eq!(out.len(), 1);
        assert!(out[0].style.reverse);
    }

    #[test]
    fn highlighting_without_matches_is_a_no_op() {
        let spans = vec![Span::plain("unchanged")];
        let out = highlight(spans.clone(), 0, &[], Style::PLAIN, Style::PLAIN);
        assert_eq!(out, spans);
    }
}
