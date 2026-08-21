//! Layout: from a [`Document`] to positioned, styled lines.
//!
//! The output of this module is a [`Screen`] -- a flat list of lines, each a run
//! of styled spans, plus the metadata the pager needs (heading anchors, link
//! targets, image placements). Nothing here touches a terminal, which is what
//! makes the layout testable: a test renders a document at a fixed width and
//! asserts on plain text.

mod blocks;
mod code;
pub mod glyphs;
pub mod inline;
mod table;

use std::collections::HashMap;
use std::io::Write;

use crate::markdown::{Document, ImageRef};
use crate::term::caps::Capabilities;
use crate::term::style::{Color, Style, StyleWriter};
use crate::theme::Theme;

pub use code::Highlighter;
pub use glyphs::Glyphs;
pub use inline::Align;

/// How to present link destinations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LinkMode {
    /// OSC 8 hyperlinks when the terminal supports them, otherwise inline URLs.
    #[default]
    Auto,
    /// Always use OSC 8, even if we did not detect support.
    Hyperlink,
    /// Always print the URL next to the text.
    Inline,
    /// Number links and list the destinations at the end of the document.
    Reference,
    /// Show link text only.
    Hide,
}

/// Knobs for a single render pass.
#[derive(Debug, Clone)]
pub struct RenderOptions {
    /// Total columns available, including margins.
    pub width: usize,
    /// Blank columns kept at the left and right of the text.
    pub margin: usize,
    pub links: LinkMode,
    /// Draw images, when a protocol is available.
    pub images: bool,
    /// Cap on how tall one image may be, in rows.
    pub max_image_rows: u16,
    /// Show line numbers beside code blocks.
    pub line_numbers: bool,
    /// Centre block images and their captions.
    pub center_figures: bool,
    /// Columns a tab expands to inside code blocks.
    pub tab_width: usize,
    pub glyphs: Glyphs,
    /// Add an underline rule beneath level-1 and level-2 headings.
    pub heading_rules: bool,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            width: 80,
            margin: 1,
            links: LinkMode::Auto,
            images: true,
            max_image_rows: 24,
            line_numbers: false,
            center_figures: true,
            tab_width: 4,
            glyphs: glyphs::UNICODE,
            heading_rules: true,
        }
    }
}

/// A styled run of text that shares one style and one link destination.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Span {
    pub text: String,
    pub style: Style,
    /// Index into [`Screen::links`].
    pub link: Option<usize>,
}

impl Span {
    pub fn new(text: impl Into<String>, style: Style, link: Option<usize>) -> Self {
        Self {
            text: text.into(),
            style,
            link,
        }
    }
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            style: Style::PLAIN,
            link: None,
        }
    }
    pub fn width(&self) -> usize {
        inline::display_width(&self.text)
    }
}

/// One rendered line.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Line {
    pub spans: Vec<Span>,
    /// An image anchored at this line, occupying `rows` lines downward.
    pub image: Option<ImagePlacement>,
    /// True for the blank lines an image reserves beneath its anchor, so the
    /// pager knows they are not really empty.
    pub image_filler: bool,
    /// Index into [`Screen::headings`] when this line starts a heading, which is
    /// how anchors get their final line numbers once layout is complete.
    pub anchor: Option<usize>,
}

impl Line {
    pub fn from_spans(spans: Vec<Span>) -> Self {
        Self {
            spans: merge_spans(spans),
            ..Default::default()
        }
    }

    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }

    pub fn width(&self) -> usize {
        self.spans.iter().map(Span::width).sum()
    }

    pub fn is_blank(&self) -> bool {
        self.image.is_none() && self.spans.iter().all(|s| s.text.trim().is_empty())
    }

    /// Inserts padding so the content sits according to `align` within `width`.
    pub fn align(&mut self, width: usize, align: Align) {
        let w = self.width();
        if w >= width {
            return;
        }
        let pad = match align {
            Align::Left => return,
            Align::Center => (width - w) / 2,
            Align::Right => width - w,
        };
        if pad > 0 {
            self.spans.insert(0, Span::plain(" ".repeat(pad)));
        }
    }

    /// Prepends a prefix to the line; used for quote bars and list indents.
    pub fn prefix(&mut self, spans: Vec<Span>) {
        let added: u16 = spans.iter().map(|s| s.width() as u16).sum();
        let mut new = spans;
        new.append(&mut self.spans);
        self.spans = merge_spans(new);
        // An image anchored here shifts right by the same amount as the text.
        if let Some(img) = &mut self.image {
            img.indent += added;
        }
    }

    /// Returns the portion of the line between two display columns.
    ///
    /// Used for horizontal scrolling. Double-width graphemes that straddle a
    /// boundary are replaced by a space so the total width stays exact.
    pub fn slice_columns(&self, from: usize, to: usize) -> Vec<Span> {
        let mut out = Vec::new();
        let mut col = 0usize;
        for span in &self.spans {
            let sw = span.width();
            if col + sw <= from {
                col += sw;
                continue;
            }
            if col >= to {
                break;
            }
            if col >= from && col + sw <= to {
                out.push(span.clone());
                col += sw;
                continue;
            }
            // Partially visible span: walk its graphemes.
            let mut text = String::new();
            let mut c = col;
            for g in unicode_segmentation::UnicodeSegmentation::graphemes(span.text.as_str(), true)
            {
                let gw = inline::display_width(g).max(1);
                if c + gw <= from || c >= to {
                    c += gw;
                    continue;
                }
                if c < from || c + gw > to {
                    text.push(' ');
                } else {
                    text.push_str(g);
                }
                c += gw;
            }
            if !text.is_empty() {
                out.push(Span::new(text, span.style, span.link));
            }
            col += sw;
        }
        merge_spans(out)
    }
}

/// Collapses adjacent spans that share a style, so the emitter writes one escape
/// sequence per run instead of one per word.
pub fn merge_spans(spans: Vec<Span>) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::with_capacity(spans.len());
    for span in spans {
        if span.text.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some(last) if last.style == span.style && last.link == span.link => {
                last.text.push_str(&span.text);
            }
            _ => out.push(span),
        }
    }
    out
}

/// An image to draw, with the size the layout reserved for it.
#[derive(Debug, Clone, PartialEq)]
pub struct ImagePlacement {
    pub image: ImageRef,
    pub cols: u16,
    pub rows: u16,
    /// Left offset in cells.
    pub indent: u16,
}

/// A heading, for the table of contents.
#[derive(Debug, Clone, PartialEq)]
pub struct HeadingEntry {
    pub level: u8,
    pub text: String,
    pub id: String,
    pub line: usize,
}

/// A fully laid out document.
#[derive(Debug, Clone, Default)]
pub struct Screen {
    pub lines: Vec<Line>,
    /// Link destinations, indexed by [`Span::link`].
    pub links: Vec<String>,
    pub headings: Vec<HeadingEntry>,
    /// Heading id to line number, for resolving `#anchor` links.
    pub anchors: HashMap<String, usize>,
    pub width: usize,
    pub title: Option<String>,
}

impl Screen {
    pub fn len(&self) -> usize {
        self.lines.len()
    }
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Plain text of the whole document, for `--plain`, tests and search.
    pub fn plain_text(&self) -> String {
        let mut s = String::new();
        for line in &self.lines {
            s.push_str(line.text().trim_end());
            s.push('\n');
        }
        s
    }

    /// Line number of a heading anchor, if the document has one.
    pub fn anchor(&self, id: &str) -> Option<usize> {
        self.anchors.get(id).copied()
    }
}

/// A request to draw one image.
#[derive(Debug, Clone, Copy)]
pub struct ImageRequest<'a> {
    pub image: &'a ImageRef,
    pub cols: u16,
    pub rows: u16,
    /// Rows to cut from the top, so an image scrolled half off the screen can
    /// still be drawn as the part that remains.
    pub skip_top: u16,
    /// Rows to cut from the bottom, to keep an image inside the viewport.
    pub skip_bottom: u16,
    /// Left offset in cells, needed by backends whose output spans lines.
    pub indent: u16,
}

impl<'a> ImageRequest<'a> {
    pub fn new(image: &'a ImageRef, cols: u16, rows: u16) -> Self {
        Self {
            image,
            cols,
            rows,
            skip_top: 0,
            skip_bottom: 0,
            indent: 0,
        }
    }
    /// The number of rows actually drawn once the crops are applied.
    pub fn visible_rows(&self) -> u16 {
        self.rows
            .saturating_sub(self.skip_top)
            .saturating_sub(self.skip_bottom)
    }
}

/// Anything that can supply image bytes and dimensions.
///
/// The renderer needs a picture's aspect ratio to reserve the right number of
/// rows, and the writer needs its encoded form. Keeping both behind a trait lets
/// layout be tested without decoding a single PNG.
pub trait ImageProvider {
    /// Pixel dimensions, or `None` if the image cannot be loaded.
    fn measure(&mut self, image: &ImageRef) -> Option<(u32, u32)>;

    /// The escape sequence that draws an image as requested.
    fn encode(&mut self, request: ImageRequest<'_>) -> Option<String>;

    /// Why an image is not showing, in a few words fit for a placeholder.
    ///
    /// Providers that cannot explain themselves return `None` and get a bare
    /// placeholder, which is what the no-image provider wants anyway.
    fn problem(&mut self, _image: &ImageRef) -> Option<String> {
        None
    }
}

/// An [`ImageProvider`] that never finds an image: used when images are off.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoImages;

impl ImageProvider for NoImages {
    fn measure(&mut self, _image: &ImageRef) -> Option<(u32, u32)> {
        None
    }
    fn encode(&mut self, _request: ImageRequest<'_>) -> Option<String> {
        None
    }
}

/// Lays out a document.
pub fn render(
    doc: &Document,
    options: &RenderOptions,
    theme: &Theme,
    caps: &Capabilities,
    images: &mut dyn ImageProvider,
    highlighter: &Highlighter,
) -> Screen {
    blocks::render_document(doc, options, theme, caps, images, highlighter)
}

/// Options for turning a [`Screen`] into bytes.
#[derive(Debug, Clone)]
pub struct WriteOptions {
    pub hyperlinks: bool,
    /// Emit image escape sequences (off when piping).
    pub images: bool,
}

impl Default for WriteOptions {
    fn default() -> Self {
        Self {
            hyperlinks: true,
            images: true,
        }
    }
}

impl Screen {
    /// Writes the document to `out` as ANSI.
    ///
    /// Images are drawn by reserving their rows first, jumping back up to the
    /// anchor, emitting the escape, and returning: the terminal's own idea of
    /// where an image leaves the cursor varies by protocol, so we never rely on
    /// it.
    pub fn write(
        &self,
        out: &mut impl Write,
        caps: &Capabilities,
        opts: &WriteOptions,
        images: &mut dyn ImageProvider,
    ) -> std::io::Result<()> {
        let mut sw = StyleWriter::new(caps.color).with_italic_fallback(caps.italic_broken);
        let mut buf = String::new();
        let mut current_link: Option<usize> = None;

        for line in &self.lines {
            if line.image_filler {
                continue;
            }
            if let (Some(placement), true) = (&line.image, opts.images) {
                sw.reset(&mut buf);
                let request = ImageRequest {
                    image: &placement.image,
                    cols: placement.cols,
                    rows: placement.rows,
                    skip_top: 0,
                    skip_bottom: 0,
                    indent: placement.indent,
                };
                if let Some(seq) = images.encode(request) {
                    // Reserve the rows, then paint into them.
                    for _ in 0..placement.rows {
                        buf.push('\n');
                    }
                    buf.push_str(&format!("\x1b[{}A", placement.rows));
                    buf.push_str("\x1b7"); // save cursor
                    if placement.indent > 0 {
                        buf.push_str(&" ".repeat(placement.indent as usize));
                    }
                    buf.push_str(&seq);
                    buf.push_str("\x1b8"); // restore cursor
                    buf.push_str(&format!("\x1b[{}B", placement.rows));
                    out.write_all(buf.as_bytes())?;
                    buf.clear();
                    continue;
                }
            }

            for span in &line.spans {
                if span.link != current_link {
                    // Close the previous hyperlink before opening another.
                    if current_link.is_some() {
                        buf.push_str("\x1b]8;;\x1b\\");
                    }
                    if let Some(i) = span.link {
                        if opts.hyperlinks {
                            if let Some(url) = self.links.get(i) {
                                buf.push_str(&format!("\x1b]8;;{}\x1b\\", hyperlink_uri(url)));
                            }
                        }
                    }
                    current_link = span.link;
                }
                sw.transition(span.style, &mut buf);
                buf.push_str(&span.text);
            }
            if current_link.is_some() {
                buf.push_str("\x1b]8;;\x1b\\");
                current_link = None;
            }
            sw.reset(&mut buf);
            buf.push('\n');

            if buf.len() > 8192 {
                out.write_all(buf.as_bytes())?;
                buf.clear();
            }
        }
        sw.reset(&mut buf);
        out.write_all(buf.as_bytes())?;
        out.flush()
    }
}

/// Turns a link target into something a terminal will accept in an OSC 8
/// sequence.
///
/// Local paths reach us absolute but without a scheme, and a terminal has no
/// reason to treat `/home/you/notes.md` as a link. Giving it a `file://` prefix
/// is what makes a document link clickable in the terminal as well as in the
/// pager.
pub fn hyperlink_uri(target: &str) -> std::borrow::Cow<'_, str> {
    if target.split_once("://").is_some()
        || target.starts_with('#')
        || !std::path::Path::new(target).is_absolute()
    {
        return std::borrow::Cow::Borrowed(target);
    }

    // A file URI always uses forward slashes and always has a slash before the
    // path, so a Windows path becomes `file:///C:/Users/...` rather than the
    // `file://C:\Users\...` that a naive prefix would produce.
    let mut path = target.replace('\\', "/");
    if !path.starts_with('/') {
        path.insert(0, '/');
    }

    // Percent-encode the characters that would otherwise end the URI or be
    // read as part of its syntax.
    let mut encoded = String::with_capacity(path.len() + 8);
    for ch in path.chars() {
        match ch {
            ' ' => encoded.push_str("%20"),
            '"' => encoded.push_str("%22"),
            '#' => encoded.push_str("%23"),
            '?' => encoded.push_str("%3F"),
            '%' => encoded.push_str("%25"),
            _ => encoded.push(ch),
        }
    }
    std::borrow::Cow::Owned(format!("file://{encoded}"))
}

/// Fills a line's full width with a background colour, so themed code blocks and
/// tables look like solid panels rather than ragged text.
pub fn pad_line_background(line: &mut Line, width: usize, style: Style) {
    if style.bg == Color::Default {
        return;
    }
    let w = line.width();
    if w < width {
        line.spans
            .push(Span::new(" ".repeat(width - w), style, None));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_adjacent_spans_with_equal_style() {
        let spans = vec![
            Span::plain("a"),
            Span::plain("b"),
            Span::new("c", Style::PLAIN.bold(), None),
        ];
        let merged = merge_spans(spans);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].text, "ab");
    }

    #[test]
    fn drops_empty_spans() {
        assert_eq!(
            merge_spans(vec![Span::plain(""), Span::plain("x")]).len(),
            1
        );
    }

    #[test]
    fn slices_columns_for_horizontal_scrolling() {
        let line = Line::from_spans(vec![
            Span::plain("abcdef"),
            Span::new("ghij", Style::PLAIN.bold(), None),
        ]);
        let spans = line.slice_columns(4, 8);
        let text: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(text, "efgh");
    }

    #[test]
    fn slicing_keeps_width_exact_across_wide_chars() {
        let line = Line::from_spans(vec![Span::plain("あいう")]);
        // Cutting through the middle of a double-width cell yields a space.
        let spans = line.slice_columns(1, 4);
        let text: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(inline::display_width(&text), 3);
        assert!(text.starts_with(' '));
    }

    #[test]
    fn local_paths_become_file_uris_for_hyperlinks() {
        assert_eq!(hyperlink_uri("https://example.com"), "https://example.com");
        assert_eq!(hyperlink_uri("#anchor"), "#anchor");
        assert_eq!(hyperlink_uri("relative.md"), "relative.md");

        // Built for whatever platform this is, rather than assuming a leading
        // slash: on Windows an absolute path starts with a drive letter.
        let absolute = std::path::absolute("notes.md").unwrap();
        let uri = hyperlink_uri(absolute.to_str().unwrap());
        assert!(uri.starts_with("file:///"), "got {uri}");
        assert!(!uri.contains('\\'), "a URI uses forward slashes: {uri}");
        assert!(uri.ends_with("notes.md"), "got {uri}");
    }

    #[cfg(unix)]
    #[test]
    fn file_uris_encode_awkward_characters() {
        assert_eq!(hyperlink_uri("/docs/notes.md"), "file:///docs/notes.md");
        assert_eq!(
            hyperlink_uri("/docs/my notes.md"),
            "file:///docs/my%20notes.md"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_become_well_formed_file_uris() {
        assert_eq!(
            hyperlink_uri(r"C:\docs\notes.md"),
            "file:///C:/docs/notes.md"
        );
        assert_eq!(
            hyperlink_uri(r"C:\docs\my notes.md"),
            "file:///C:/docs/my%20notes.md"
        );
    }

    #[test]
    fn alignment_pads_the_left() {
        let mut line = Line::from_spans(vec![Span::plain("ab")]);
        line.align(6, Align::Right);
        assert_eq!(line.text(), "    ab");
    }
}
