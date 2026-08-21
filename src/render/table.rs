//! Table layout.
//!
//! Tables are the thing terminal Markdown viewers most often skip, because the
//! hard part is not drawing the box -- it is deciding column widths when the
//! content does not fit. The approach here: measure each column's natural and
//! minimum width, spend any spare room on the natural widths, and when there is
//! not enough, take columns down one cell at a time starting with the widest.
//! That keeps narrow columns (`Yes`, `1.2.0`) intact and makes the prose column
//! absorb the wrapping, which is what a person would do by hand.
//!
//! Below a certain width a grid stops being readable at all, so we switch to a
//! record layout instead of emitting a mangled box.

use super::blocks::{Ctx, inline_tokens};
use super::inline::{Align, display_width, wrap};
use super::{Line, Span};
use crate::markdown::{Alignment, Cell, Table};
use crate::term::style::Style;

pub(super) fn render(table: &Table, ctx: &mut Ctx, width: usize) -> Vec<Line> {
    let columns = table
        .head
        .len()
        .max(table.rows.iter().map(Vec::len).max().unwrap_or(0));
    if columns == 0 {
        return Vec::new();
    }

    // Borders and padding: "│ x │ y │" costs three cells per column plus one.
    let overhead = columns * 3 + 1;
    let available = width.saturating_sub(overhead);

    // Two cells of content per column is the floor for anything grid-shaped.
    if available < columns * 2 {
        return render_as_records(table, ctx, width, columns);
    }

    let measures = measure_columns(table, ctx, columns);
    let widths = fit_columns(&measures, available);
    let alignments = resolve_alignments(table, columns);

    let g = ctx.glyphs();
    let border = ctx.theme.table_border;
    let mut out = Vec::new();

    out.push(rule_line(
        &widths,
        border,
        g.top_left,
        g.top_mid,
        g.top_right,
        g.h,
    ));
    if !table.head.is_empty() {
        out.extend(render_row(
            &table.head,
            &widths,
            &alignments,
            ctx,
            ctx.theme.table_header,
            columns,
        ));
        out.push(rule_line(
            &widths,
            border,
            g.mid_left,
            g.mid_mid,
            g.mid_right,
            g.h,
        ));
    }
    for (i, row) in table.rows.iter().enumerate() {
        let style = match ctx.theme.table_stripe {
            Some(stripe) if i % 2 == 1 => ctx.theme.text.merge(stripe),
            _ => ctx.theme.text,
        };
        out.extend(render_row(row, &widths, &alignments, ctx, style, columns));
    }
    out.push(rule_line(
        &widths,
        border,
        g.bottom_left,
        g.bottom_mid,
        g.bottom_right,
        g.h,
    ));
    out
}

/// Natural and minimum width for each column.
struct Measure {
    natural: usize,
    minimum: usize,
}

fn measure_columns(table: &Table, ctx: &mut Ctx, columns: usize) -> Vec<Measure> {
    let mut measures: Vec<Measure> = (0..columns)
        .map(|_| Measure {
            natural: 0,
            minimum: 0,
        })
        .collect();

    let visit = |cells: &[Cell], ctx: &mut Ctx, measures: &mut Vec<Measure>| {
        for (i, cell) in cells.iter().take(columns).enumerate() {
            let tokens = inline_tokens(cell, ctx, Style::PLAIN);
            let natural: usize = tokens.iter().map(|t| t.width()).sum();
            // The minimum is the longest single word: below this we would have
            // to break words mid-token, which we only do as a last resort.
            let minimum = tokens.iter().map(|t| t.width()).max().unwrap_or(0);
            measures[i].natural = measures[i].natural.max(natural);
            measures[i].minimum = measures[i].minimum.max(minimum);
        }
    };
    visit(&table.head, ctx, &mut measures);
    for row in &table.rows {
        visit(row, ctx, &mut measures);
    }
    for m in &mut measures {
        m.natural = m.natural.max(1);
        m.minimum = m.minimum.max(1).min(m.natural);
    }
    measures
}

/// Distributes `available` columns of space across the table's columns.
fn fit_columns(measures: &[Measure], available: usize) -> Vec<usize> {
    let mut widths: Vec<usize> = measures.iter().map(|m| m.natural).collect();
    let mut total: usize = widths.iter().sum();
    if total <= available {
        return widths;
    }

    // Shave the widest column that is still above its minimum, repeatedly. This
    // converges on every column sitting at its minimum, at worst.
    while total > available {
        let candidate = widths
            .iter()
            .enumerate()
            .filter(|(i, w)| **w > measures[*i].minimum)
            .max_by_key(|(_, w)| **w)
            .map(|(i, _)| i);
        match candidate {
            Some(i) => {
                widths[i] -= 1;
                total -= 1;
            }
            None => break,
        }
    }

    if total > available {
        // Even the minimums do not fit: share the space out proportionally and
        // accept that some words will be broken.
        let sum: usize = widths.iter().sum::<usize>().max(1);
        let mut scaled: Vec<usize> = widths
            .iter()
            .map(|w| {
                ((*w as f64 / sum as f64) * available as f64)
                    .floor()
                    .max(1.0) as usize
            })
            .collect();
        // Hand out any rounding remainder left to left.
        let mut used: usize = scaled.iter().sum();
        let n = scaled.len();
        let mut i = 0;
        while used < available && n > 0 {
            scaled[i % n] += 1;
            used += 1;
            i += 1;
        }
        while used > available {
            let j = scaled
                .iter()
                .enumerate()
                .max_by_key(|(_, w)| **w)
                .map(|(i, _)| i)
                .unwrap_or(0);
            if scaled[j] <= 1 {
                break;
            }
            scaled[j] -= 1;
            used -= 1;
        }
        return scaled;
    }
    widths
}

/// Column alignments, inferring right alignment for numeric columns.
fn resolve_alignments(table: &Table, columns: usize) -> Vec<Align> {
    (0..columns)
        .map(
            |i| match table.alignments.get(i).copied().unwrap_or(Alignment::None) {
                Alignment::Left => Align::Left,
                Alignment::Center => Align::Center,
                Alignment::Right => Align::Right,
                // Numbers read better right aligned, and a column of them is
                // unambiguous enough to detect.
                Alignment::None if is_numeric_column(table, i) => Align::Right,
                Alignment::None => Align::Left,
            },
        )
        .collect()
}

fn is_numeric_column(table: &Table, index: usize) -> bool {
    let mut seen = 0;
    for row in &table.rows {
        let Some(cell) = row.get(index) else { continue };
        let text = crate::markdown::ir_plain_text(cell);
        let t = text
            .trim()
            .trim_start_matches(['$', '£', '€'])
            .trim_end_matches('%')
            .replace(',', "");
        if t.is_empty() {
            continue;
        }
        // A real float parse, so version strings like `1.2.0` and identifiers
        // like `3a` stay left aligned -- they are labels, not quantities.
        if t.parse::<f64>().is_ok() {
            seen += 1;
        } else {
            return false;
        }
    }
    seen > 0
}

fn render_row(
    cells: &[Cell],
    widths: &[usize],
    alignments: &[Align],
    ctx: &mut Ctx,
    style: Style,
    columns: usize,
) -> Vec<Line> {
    // Wrap every cell first: the row is as tall as its tallest cell.
    let mut wrapped: Vec<Vec<Line>> = Vec::with_capacity(columns);
    for i in 0..columns {
        let width = widths[i];
        let lines = match cells.get(i) {
            Some(cell) => {
                let tokens = inline_tokens(cell, ctx, style);
                wrap(&tokens, width, alignments[i])
            }
            None => vec![Line::default()],
        };
        wrapped.push(lines);
    }
    let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);

    let v = ctx.glyphs().v;
    let border = ctx.theme.table_border;
    let mut out = Vec::with_capacity(height);
    for row in 0..height {
        let mut spans = vec![Span::new(v, border, None)];
        for (i, cell_lines) in wrapped.iter().enumerate() {
            spans.push(Span::new(" ", style, None));
            let width = widths[i];
            match cell_lines.get(row) {
                Some(line) => {
                    let used = line.width();
                    spans.extend(line.spans.iter().cloned());
                    if used < width {
                        spans.push(Span::new(" ".repeat(width - used), style, None));
                    }
                }
                None => spans.push(Span::new(" ".repeat(width), style, None)),
            }
            spans.push(Span::new(" ", style, None));
            spans.push(Span::new(v, border, None));
        }
        out.push(Line::from_spans(spans));
    }
    out
}

fn rule_line(widths: &[usize], style: Style, left: &str, mid: &str, right: &str, h: &str) -> Line {
    let mut s = String::from(left);
    for (i, w) in widths.iter().enumerate() {
        s.push_str(&h.repeat(w + 2));
        s.push_str(if i + 1 == widths.len() { right } else { mid });
    }
    Line::from_spans(vec![Span::new(s, style, None)])
}

/// The narrow-terminal fallback: one record per row, `header: value` per line.
///
/// A three-column table squeezed into 30 columns is unreadable as a grid but
/// perfectly readable as records, and no content has to be dropped.
fn render_as_records(table: &Table, ctx: &mut Ctx, width: usize, columns: usize) -> Vec<Line> {
    let headers: Vec<String> = (0..columns)
        .map(|i| {
            table
                .head
                .get(i)
                .map(|c| crate::markdown::ir_plain_text(c))
                .unwrap_or_else(|| format!("{}", i + 1))
        })
        .collect();

    let mut out = Vec::new();
    for (r, row) in table.rows.iter().enumerate() {
        if r > 0 {
            out.push(Line::default());
        }
        for (i, header) in headers.iter().enumerate() {
            let label = format!("{header}: ");
            let label_width = display_width(&label);
            let body_width = width.saturating_sub(label_width).max(4);
            let tokens = match row.get(i) {
                Some(cell) => inline_tokens(cell, ctx, ctx.theme.text),
                None => Vec::new(),
            };
            let mut lines = wrap(&tokens, body_width, Align::Left);
            for (j, line) in lines.iter_mut().enumerate() {
                if j == 0 {
                    line.prefix(vec![Span::new(label.clone(), ctx.theme.table_header, None)]);
                } else {
                    line.prefix(vec![Span::plain(" ".repeat(label_width))]);
                }
            }
            out.extend(lines);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::{ParseOptions, parse};
    use crate::render::code::Highlighter;
    use crate::render::{NoImages, RenderOptions};
    use crate::term::caps::Capabilities;
    use crate::theme::Theme;

    fn table_text(src: &str, width: usize) -> String {
        let doc = parse(src, ParseOptions::default());
        let opts = RenderOptions {
            width,
            margin: 0,
            images: false,
            glyphs: crate::render::glyphs::ASCII,
            ..Default::default()
        };
        crate::render::render(
            &doc,
            &opts,
            &Theme::mono(),
            &Capabilities::default(),
            &mut NoImages,
            &Highlighter::plain(),
        )
        .plain_text()
    }

    const SIMPLE: &str = "| Name | Qty |\n|------|----:|\n| Apple | 3 |\n| Banana | 12 |\n";

    #[test]
    fn draws_a_grid() {
        let out = table_text(SIMPLE, 40);
        let lines: Vec<&str> = out.lines().collect();
        assert!(
            lines[0].starts_with('+'),
            "expected a top border: {:?}",
            lines[0]
        );
        assert!(lines.iter().any(|l| l.contains("Apple")));
        assert!(lines.last().unwrap().starts_with('+'));
    }

    #[test]
    fn every_row_has_the_same_width() {
        let out = table_text(SIMPLE, 40);
        let widths: Vec<usize> = out.lines().map(display_width).collect();
        assert!(
            widths.windows(2).all(|w| w[0] == w[1]),
            "ragged table: {widths:?}"
        );
    }

    #[test]
    fn never_exceeds_the_available_width() {
        let long = "| Column | Description |\n|---|---|\n| x | a very long description that must be wrapped inside its cell |\n";
        for width in [30usize, 40, 60, 100] {
            let out = table_text(long, width);
            for line in out.lines() {
                assert!(display_width(line) <= width, "width {width}: {line:?}");
            }
        }
    }

    #[test]
    fn wraps_the_prose_column_and_keeps_narrow_ones_intact() {
        let src = "| Ver | Notes |\n|---|---|\n| 1.2.0 | a long note that will need to wrap across lines |\n";
        let out = table_text(src, 40);
        assert!(
            out.contains("1.2.0"),
            "narrow column should not wrap: {out}"
        );
        let body_rows = out.lines().filter(|l| l.starts_with('|')).count();
        assert!(
            body_rows >= 3,
            "the prose column should have wrapped: {out}"
        );
    }

    #[test]
    fn respects_explicit_alignment() {
        let src = "| L | C | R |\n|:--|:-:|--:|\n| a | b | c |\n";
        let out = table_text(src, 30);
        let row = out.lines().find(|l| l.contains('a')).unwrap();
        let cells: Vec<&str> = row.split('|').filter(|s| !s.trim().is_empty()).collect();
        assert!(cells[0].starts_with(" a"), "left: {:?}", cells[0]);
        assert!(cells[2].ends_with("c "), "right: {:?}", cells[2]);
    }

    #[test]
    fn right_aligns_numeric_columns_automatically() {
        let src = "| Item | Count |\n|---|---|\n| a | 5 |\n| b | 1000 |\n";
        let out = table_text(src, 30);
        let row = out.lines().find(|l| l.contains(" 5 ")).unwrap();
        assert!(row.contains("   5 "), "expected a right-aligned 5: {row:?}");
    }

    #[test]
    fn falls_back_to_records_when_too_narrow() {
        let out = table_text("| A | B | C |\n|---|---|---|\n| 1 | 2 | 3 |\n", 12);
        assert!(!out.contains('+'), "should not draw a grid: {out}");
        assert!(out.contains("A: 1"), "got {out}");
        assert!(out.contains("C: 3"));
    }

    #[test]
    fn handles_ragged_rows() {
        let out = table_text("| A | B |\n|---|---|\n| 1 |\n| 1 | 2 |\n", 30);
        let widths: Vec<usize> = out.lines().map(display_width).collect();
        assert!(
            widths.windows(2).all(|w| w[0] == w[1]),
            "ragged: {widths:?}"
        );
    }

    #[test]
    fn handles_wide_characters_in_cells() {
        let out = table_text("| Name | 説明 |\n|---|---|\n| x | 日本語のテキスト |\n", 40);
        let widths: Vec<usize> = out.lines().map(display_width).collect();
        assert!(
            widths.windows(2).all(|w| w[0] == w[1]),
            "CJK broke alignment: {out}"
        );
    }

    #[test]
    fn table_with_no_body_rows_still_renders() {
        let out = table_text("| A | B |\n|---|---|\n", 30);
        assert!(out.contains('A') && out.contains('B'));
    }
}
