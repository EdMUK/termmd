//! Folds `pulldown-cmark`'s event stream into the [`Document`] tree.
//!
//! The stream is flat -- `Start(Tag)` / content / `End(TagEnd)` -- so we keep a
//! stack of half-built nodes. Each `Start` pushes a frame, each `End` pops one
//! and hands the finished node to whatever is underneath it.
//!
//! Two details are worth knowing about. First, tight list items have no
//! `Paragraph` events at all, so inline content can arrive with a block
//! container on top of the stack; we open an implicit paragraph in that case and
//! close it when the container does. Second, footnote definitions arrive inline
//! wherever the author wrote them, but belong at the end of the rendered
//! document, so they are lifted out of the block tree as we go.

use pulldown_cmark::{
    Alignment as CmAlignment, BlockQuoteKind, CodeBlockKind, Event, HeadingLevel, Options, Parser,
    Tag, TagEnd,
};

use super::html::{self, HtmlPiece};
use super::ir::{
    AlertKind, Alignment, Block, Document, ImageRef, Inline, Inlines, List, ListItem, Table,
    plain_text,
};
use super::slug::Slugger;

/// Which Markdown extensions to enable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseOptions {
    /// Tables, task lists, strikethrough, footnotes, alerts.
    pub gfm: bool,
    /// `$inline$` and `$$display$$` math.
    pub math: bool,
    /// Curly quotes, en/em dashes, ellipses.
    pub smart_punctuation: bool,
    /// `[[wikilinks]]`.
    pub wikilinks: bool,
    /// Definition lists.
    pub definition_lists: bool,
    /// Treat a leading YAML block as front matter rather than content.
    pub front_matter: bool,
}

impl Default for ParseOptions {
    fn default() -> Self {
        Self {
            gfm: true,
            math: true,
            smart_punctuation: true,
            wikilinks: true,
            definition_lists: true,
            front_matter: true,
        }
    }
}

impl ParseOptions {
    fn to_cmark(self) -> Options {
        let mut o = Options::empty();
        if self.gfm {
            o |= Options::ENABLE_TABLES
                | Options::ENABLE_FOOTNOTES
                | Options::ENABLE_STRIKETHROUGH
                | Options::ENABLE_TASKLISTS
                | Options::ENABLE_GFM;
        }
        if self.math {
            o |= Options::ENABLE_MATH;
        }
        if self.smart_punctuation {
            o |= Options::ENABLE_SMART_PUNCTUATION;
        }
        if self.wikilinks {
            o |= Options::ENABLE_WIKILINKS;
        }
        if self.definition_lists {
            o |= Options::ENABLE_DEFINITION_LIST;
        }
        if self.front_matter {
            o |= Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
                | Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS;
        }
        o | Options::ENABLE_HEADING_ATTRIBUTES
            | Options::ENABLE_SUPERSCRIPT
            | Options::ENABLE_SUBSCRIPT
    }
}

/// Parses Markdown into a [`Document`].
pub fn parse(source: &str, options: ParseOptions) -> Document {
    Builder::new(options).run(source)
}

/// A half-built node on the stack.
/// How deep a document may nest before the tree stops getting deeper.
///
/// A hundred is far past anything written on purpose -- a list inside a quote
/// inside a footnote is four -- and far short of what a recursive walk over the
/// result cannot survive.
const MAX_DEPTH: usize = 100;

enum Frame {
    Paragraph {
        inlines: Inlines,
        implicit: bool,
    },
    Heading {
        level: u8,
        id: Option<String>,
        inlines: Inlines,
    },
    BlockQuote {
        kind: Option<AlertKind>,
        blocks: Vec<Block>,
    },
    CodeBlock {
        language: Option<String>,
        code: String,
    },
    List {
        start: Option<u64>,
        items: Vec<ListItem>,
        tight: bool,
    },
    /// `loose` records whether cmark emitted a real `Paragraph` directly
    /// inside this item, which is exactly the signal that the source had a
    /// blank line -- our own implicit paragraphs must not be mistaken for it.
    Item {
        task: Option<bool>,
        blocks: Vec<Block>,
        loose: bool,
    },
    Table {
        alignments: Vec<Alignment>,
        head: Vec<Inlines>,
        rows: Vec<Vec<Inlines>>,
        in_head: bool,
    },
    TableRow {
        cells: Vec<Inlines>,
    },
    TableCell {
        inlines: Inlines,
    },
    Emphasis(Inlines),
    Strong(Inlines),
    Strike(Inlines),
    Superscript(Inlines),
    Subscript(Inlines),
    Link {
        dest: String,
        title: Option<String>,
        content: Inlines,
    },
    Image {
        dest: String,
        title: Option<String>,
        alt: Inlines,
    },
    FootnoteDefinition {
        label: String,
        blocks: Vec<Block>,
    },
    DefinitionList {
        items: Vec<(Inlines, Vec<Vec<Block>>)>,
    },
    DefinitionTitle(Inlines),
    DefinitionBody(Vec<Block>),
    /// Raw HTML accumulated across the events of one block.
    HtmlBlock(String),
    Metadata(String),
}

struct Builder {
    options: ParseOptions,
    stack: Vec<Frame>,
    /// Opened containers we declined to nest, waiting for their closes.
    suppressed: usize,
    blocks: Vec<Block>,
    footnotes: Vec<(String, Vec<Block>)>,
    slugger: Slugger,
    front_matter: Option<String>,
}

impl Builder {
    fn new(options: ParseOptions) -> Self {
        Self {
            options,
            stack: Vec::new(),
            suppressed: 0,
            blocks: Vec::new(),
            footnotes: Vec::new(),
            slugger: Slugger::new(),
            front_matter: None,
        }
    }

    fn run(mut self, source: &str) -> Document {
        for event in Parser::new_ext(source, self.options.to_cmark()) {
            self.event(event);
        }
        // Unbalanced input (a truncated file, say) can leave frames open. Close
        // them rather than losing the content.
        while !self.stack.is_empty() {
            self.close_top();
        }
        let title = self
            .front_matter
            .as_deref()
            .and_then(front_matter_title)
            .or_else(|| first_h1(&self.blocks));

        Document {
            blocks: self.blocks,
            footnotes: self.footnotes,
            title,
            front_matter: self.front_matter,
        }
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => {
                // Past this depth the document stops becoming a deeper tree and
                // its contents join the block already open. Every consumer of
                // the tree walks it recursively -- rendering, rebasing URLs,
                // even dropping it -- so without a floor here a document of
                // fifty thousand nested quotes takes the stack with it, and
                // since a document can now arrive from a URL that is not only
                // a thing people do to themselves.
                if self.stack.len() >= MAX_DEPTH {
                    self.suppressed += 1;
                    return;
                }
                self.start(tag)
            }
            Event::End(tag) => {
                // The close of a start we did not take: they nest, so the ones
                // we skipped are the innermost and close first.
                if self.suppressed > 0 {
                    self.suppressed -= 1;
                    return;
                }
                self.end(tag)
            }
            Event::Text(t) => {
                if let Some(Frame::CodeBlock { code, .. }) = self.stack.last_mut() {
                    code.push_str(&t);
                } else if let Some(Frame::Metadata(s)) = self.stack.last_mut() {
                    s.push_str(&t);
                } else {
                    self.push_inline(Inline::Text(t.into_string()));
                }
            }
            Event::Code(t) => self.push_inline(Inline::Code(t.into_string())),
            Event::InlineMath(t) => self.push_inline(Inline::Math {
                text: t.into_string(),
                display: false,
            }),
            Event::DisplayMath(t) => self.push_block(Block::DisplayMath(t.into_string())),
            Event::SoftBreak => self.push_inline(Inline::SoftBreak),
            Event::HardBreak => self.push_inline(Inline::HardBreak),
            Event::Rule => self.push_block(Block::Rule),
            Event::FootnoteReference(label) => {
                self.push_inline(Inline::FootnoteRef(label.into_string()))
            }
            Event::TaskListMarker(checked) => {
                // Arrives just inside the item it belongs to, possibly after an
                // implicit paragraph has been opened.
                for frame in self.stack.iter_mut().rev() {
                    if let Frame::Item { task, .. } = frame {
                        *task = Some(checked);
                        break;
                    }
                }
            }
            Event::Html(h) => {
                // An HTML block arrives as one event per line, so a tag written
                // across lines -- `<img src=` with the URL underneath, which is
                // common in hand-written READMEs -- would never be seen whole if
                // each event were scanned on its own. Collect the block first.
                match self.stack.last_mut() {
                    Some(Frame::HtmlBlock(buffer)) => buffer.push_str(&h),
                    _ => self.html_block(&h),
                }
            }
            Event::InlineHtml(h) => self.inline_html(&h),
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {
                self.close_implicit();
                // A paragraph opened directly inside an item means the author
                // left a blank line, so the enclosing list is loose.
                if let Some(Frame::Item { loose, .. }) = self.stack.last_mut() {
                    *loose = true;
                }
                self.stack.push(Frame::Paragraph {
                    inlines: Vec::new(),
                    implicit: false,
                });
            }
            Tag::Heading { level, id, .. } => {
                self.close_implicit();
                self.stack.push(Frame::Heading {
                    level: heading_level(level),
                    id: id.map(|i| i.into_string()),
                    inlines: Vec::new(),
                });
            }
            Tag::BlockQuote(kind) => {
                self.close_implicit();
                self.stack.push(Frame::BlockQuote {
                    kind: kind.map(alert_kind),
                    blocks: Vec::new(),
                });
            }
            Tag::CodeBlock(kind) => {
                self.close_implicit();
                let language = match kind {
                    CodeBlockKind::Fenced(info) => {
                        // The info string may carry more than a language, e.g.
                        // ```rust,ignore or ```js {highlight=1}.
                        let lang = info
                            .split([',', ' ', '\t', '{'])
                            .next()
                            .unwrap_or("")
                            .trim()
                            .to_string();
                        (!lang.is_empty()).then_some(lang)
                    }
                    CodeBlockKind::Indented => None,
                };
                self.stack.push(Frame::CodeBlock {
                    language,
                    code: String::new(),
                });
            }
            Tag::List(start) => {
                self.close_implicit();
                self.stack.push(Frame::List {
                    start,
                    items: Vec::new(),
                    tight: true,
                });
            }
            Tag::Item => {
                self.stack.push(Frame::Item {
                    task: None,
                    blocks: Vec::new(),
                    loose: false,
                });
            }
            Tag::Table(aligns) => {
                self.close_implicit();
                self.stack.push(Frame::Table {
                    alignments: aligns.into_iter().map(convert_alignment).collect(),
                    head: Vec::new(),
                    rows: Vec::new(),
                    in_head: false,
                });
            }
            Tag::TableHead => {
                if let Some(Frame::Table { in_head, .. }) = self.stack.last_mut() {
                    *in_head = true;
                }
                self.stack.push(Frame::TableRow { cells: Vec::new() });
            }
            Tag::TableRow => self.stack.push(Frame::TableRow { cells: Vec::new() }),
            Tag::TableCell => self.stack.push(Frame::TableCell {
                inlines: Vec::new(),
            }),
            Tag::Emphasis => self.stack.push(Frame::Emphasis(Vec::new())),
            Tag::Strong => self.stack.push(Frame::Strong(Vec::new())),
            Tag::Strikethrough => self.stack.push(Frame::Strike(Vec::new())),
            Tag::Superscript => self.stack.push(Frame::Superscript(Vec::new())),
            Tag::Subscript => self.stack.push(Frame::Subscript(Vec::new())),
            Tag::Link {
                dest_url, title, ..
            } => self.stack.push(Frame::Link {
                dest: dest_url.into_string(),
                title: non_empty(title.into_string()),
                content: Vec::new(),
            }),
            Tag::Image {
                dest_url, title, ..
            } => self.stack.push(Frame::Image {
                dest: dest_url.into_string(),
                title: non_empty(title.into_string()),
                alt: Vec::new(),
            }),
            Tag::FootnoteDefinition(label) => {
                self.close_implicit();
                self.stack.push(Frame::FootnoteDefinition {
                    label: label.into_string(),
                    blocks: Vec::new(),
                });
            }
            Tag::DefinitionList => {
                self.close_implicit();
                self.stack.push(Frame::DefinitionList { items: Vec::new() });
            }
            Tag::DefinitionListTitle => self.stack.push(Frame::DefinitionTitle(Vec::new())),
            Tag::DefinitionListDefinition => self.stack.push(Frame::DefinitionBody(Vec::new())),
            Tag::HtmlBlock => {
                self.close_implicit();
                self.stack.push(Frame::HtmlBlock(String::new()));
            }
            Tag::MetadataBlock(_) => self.stack.push(Frame::Metadata(String::new())),
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            // Containers close their implicit paragraph before closing themselves.
            TagEnd::Item
            | TagEnd::BlockQuote(_)
            | TagEnd::FootnoteDefinition
            | TagEnd::DefinitionListDefinition => {
                self.close_implicit();
                self.close_top();
            }
            TagEnd::HtmlBlock => {
                self.close_implicit();
                self.close_top();
            }
            _ => self.close_top(),
        }
    }

    /// Pops the top frame and folds it into its parent.
    fn close_top(&mut self) {
        let Some(frame) = self.stack.pop() else {
            return;
        };
        match frame {
            Frame::Paragraph { inlines, .. } => {
                if let Some(block) = paragraph_block(inlines) {
                    self.push_block(block);
                }
            }
            Frame::Heading { level, id, inlines } => {
                let text = plain_text(&inlines);
                let id = id.unwrap_or_else(|| self.slugger.slug(&text));
                self.push_block(Block::Heading {
                    level,
                    id,
                    content: inlines,
                });
            }
            Frame::BlockQuote { kind, blocks } => {
                self.push_block(Block::BlockQuote { kind, blocks })
            }
            Frame::CodeBlock { language, code } => {
                // Fenced blocks always end with a newline the author did not type.
                let code = code.strip_suffix('\n').map(str::to_string).unwrap_or(code);
                self.push_block(Block::CodeBlock { language, code });
            }
            Frame::List {
                start,
                items,
                tight,
            } => self.push_block(Block::List(List {
                start,
                tight,
                items,
            })),
            Frame::Item {
                task,
                blocks,
                loose,
            } => {
                if let Some(Frame::List { items, tight, .. }) = self.stack.last_mut() {
                    if loose {
                        *tight = false;
                    }
                    items.push(ListItem { task, blocks });
                }
            }
            Frame::Table {
                alignments,
                head,
                rows,
                ..
            } => self.push_block(Block::Table(Table {
                alignments,
                head,
                rows,
            })),
            Frame::TableRow { cells } => {
                if let Some(Frame::Table {
                    head,
                    rows,
                    in_head,
                    ..
                }) = self.stack.last_mut()
                {
                    if *in_head {
                        *head = cells;
                        *in_head = false;
                    } else {
                        rows.push(cells);
                    }
                }
            }
            Frame::TableCell { inlines } => {
                if let Some(Frame::TableRow { cells }) = self.stack.last_mut() {
                    cells.push(inlines);
                }
            }
            Frame::Emphasis(c) => self.push_inline(Inline::Emph(c)),
            Frame::Strong(c) => self.push_inline(Inline::Strong(c)),
            Frame::Strike(c) => self.push_inline(Inline::Strike(c)),
            Frame::Superscript(c) => self.push_inline(Inline::Superscript(c)),
            Frame::Subscript(c) => self.push_inline(Inline::Subscript(c)),
            Frame::Link {
                dest,
                title,
                content,
            } => self.push_inline(Inline::Link {
                dest,
                title,
                content,
            }),
            Frame::Image { dest, title, alt } => {
                let alt = plain_text(&alt);
                self.push_inline(Inline::Image(ImageRef {
                    url: dest,
                    alt,
                    title,
                }));
            }
            Frame::FootnoteDefinition { label, blocks } => self.footnotes.push((label, blocks)),
            Frame::DefinitionList { items } => self.push_block(Block::DefinitionList(items)),
            Frame::DefinitionTitle(inlines) => {
                if let Some(Frame::DefinitionList { items }) = self.stack.last_mut() {
                    items.push((inlines, Vec::new()));
                }
            }
            Frame::DefinitionBody(blocks) => {
                if let Some(Frame::DefinitionList { items }) = self.stack.last_mut() {
                    if let Some(last) = items.last_mut() {
                        last.1.push(blocks);
                    }
                }
            }
            Frame::HtmlBlock(raw) => self.html_block(&raw),
            Frame::Metadata(text) => self.front_matter = Some(text),
        }
    }

    /// Closes a paragraph we opened ourselves for a tight list item.
    fn close_implicit(&mut self) {
        if matches!(
            self.stack.last(),
            Some(Frame::Paragraph { implicit: true, .. })
        ) {
            self.close_top();
        }
    }

    /// Adds an inline to the nearest frame that accepts one, opening an implicit
    /// paragraph if the nearest frame is a block container.
    fn push_inline(&mut self, inline: Inline) {
        match self.stack.last_mut() {
            Some(Frame::Paragraph { inlines, .. })
            | Some(Frame::Heading { inlines, .. })
            | Some(Frame::TableCell { inlines })
            | Some(Frame::Emphasis(inlines))
            | Some(Frame::Strong(inlines))
            | Some(Frame::Strike(inlines))
            | Some(Frame::Superscript(inlines))
            | Some(Frame::Subscript(inlines))
            | Some(Frame::DefinitionTitle(inlines)) => inlines.push(inline),
            Some(Frame::Link { content, .. }) => content.push(inline),
            Some(Frame::Image { alt, .. }) => alt.push(inline),
            _ => {
                self.stack.push(Frame::Paragraph {
                    inlines: vec![inline],
                    implicit: true,
                });
            }
        }
    }

    /// Adds a finished block to the nearest block container.
    fn push_block(&mut self, block: Block) {
        self.close_implicit();
        for frame in self.stack.iter_mut().rev() {
            match frame {
                Frame::Item { blocks, .. }
                | Frame::BlockQuote { blocks, .. }
                | Frame::FootnoteDefinition { blocks, .. }
                | Frame::DefinitionBody(blocks) => {
                    blocks.push(block);
                    return;
                }
                _ => {}
            }
        }
        self.blocks.push(block);
    }

    /// Turns a raw HTML block into whatever we can actually draw.
    fn html_block(&mut self, raw: &str) {
        let pieces = html::simplify(raw);
        if pieces.is_empty() {
            return;
        }
        let mut inlines = Vec::new();
        for piece in pieces {
            match piece {
                HtmlPiece::Text(t) => inlines.push(Inline::Text(t)),
                HtmlPiece::Summary(t) => inlines.push(Inline::Strong(vec![Inline::Text(t)])),
                HtmlPiece::Image(img) => {
                    // Each picture becomes its own figure. A terminal cannot put
                    // two images side by side on one row anyway, and a row of
                    // badges reads better stacked than merged into a sentence.
                    if let Some(block) = paragraph_block(std::mem::take(&mut inlines)) {
                        self.push_block(block);
                    }
                    if let Some(block) = paragraph_block(vec![Inline::Image(img)]) {
                        self.push_block(block);
                    }
                }
                HtmlPiece::Break => {
                    // Flush what we have; each break starts a fresh paragraph so
                    // banner-style HTML does not run together.
                    if let Some(block) = paragraph_block(std::mem::take(&mut inlines)) {
                        self.push_block(block);
                    }
                }
            }
        }
        if let Some(block) = paragraph_block(inlines) {
            self.push_block(block);
        }
    }

    fn inline_html(&mut self, raw: &str) {
        for piece in html::simplify(raw) {
            match piece {
                HtmlPiece::Text(t) | HtmlPiece::Summary(t) => self.push_inline(Inline::Text(t)),
                HtmlPiece::Image(img) => self.push_inline(Inline::Image(img)),
                HtmlPiece::Break => self.push_inline(Inline::HardBreak),
            }
        }
    }
}

/// Builds a paragraph, promoting an image-only paragraph to a figure.
///
/// Authors write `![alt](x.png)` on a line of its own to mean "show this
/// picture", not "here is a sentence containing a picture", and the distinction
/// decides how much room we give it.
fn paragraph_block(inlines: Inlines) -> Option<Block> {
    let mut meaningful = inlines.iter().filter(|i| !is_blank(i));
    if let Some(Inline::Image(img)) = meaningful.next() {
        if meaningful.next().is_none() {
            let caption = if img.alt.is_empty() {
                Vec::new()
            } else {
                vec![Inline::Text(img.alt.clone())]
            };
            return Some(Block::Figure {
                image: img.clone(),
                caption,
            });
        }
    }
    if inlines.iter().all(is_blank) {
        return None;
    }
    Some(Block::Paragraph(inlines))
}

fn is_blank(i: &Inline) -> bool {
    match i {
        Inline::Text(t) => t.trim().is_empty(),
        Inline::SoftBreak | Inline::HardBreak => true,
        _ => false,
    }
}

fn non_empty(s: String) -> Option<String> {
    (!s.is_empty()).then_some(s)
}

fn heading_level(l: HeadingLevel) -> u8 {
    match l {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn convert_alignment(a: CmAlignment) -> Alignment {
    match a {
        CmAlignment::None => Alignment::None,
        CmAlignment::Left => Alignment::Left,
        CmAlignment::Center => Alignment::Center,
        CmAlignment::Right => Alignment::Right,
    }
}

fn alert_kind(k: BlockQuoteKind) -> AlertKind {
    match k {
        BlockQuoteKind::Note => AlertKind::Note,
        BlockQuoteKind::Tip => AlertKind::Tip,
        BlockQuoteKind::Important => AlertKind::Important,
        BlockQuoteKind::Warning => AlertKind::Warning,
        BlockQuoteKind::Caution => AlertKind::Caution,
    }
}

/// Pulls `title:` out of YAML front matter without a YAML parser.
fn front_matter_title(fm: &str) -> Option<String> {
    fm.lines().find_map(|line| {
        let rest = line.strip_prefix("title:")?;
        let v = rest.trim().trim_matches(['"', '\'']).trim();
        (!v.is_empty()).then(|| v.to_string())
    })
}

fn first_h1(blocks: &[Block]) -> Option<String> {
    blocks.iter().find_map(|b| match b {
        Block::Heading {
            level: 1, content, ..
        } => Some(plain_text(content)),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(src: &str) -> Document {
        parse(src, ParseOptions::default())
    }

    #[test]
    fn nesting_has_a_floor() {
        // Every walk over the tree recurses -- rendering, rebasing URLs,
        // dropping it -- so the tree itself has to stop getting deeper. This
        // used to take the stack with it, and a document can arrive from a URL.
        let deep = format!("{}deep\n", "> ".repeat(50_000));
        let doc = parse(&deep, ParseOptions::default());

        // Nothing is lost: the text is still in there, just not 50,000 quotes
        // down.
        let text = crate::markdown::ir_plain_text(&collect_inlines(&doc.blocks));
        assert!(text.contains("deep"), "got {text:?}");
        // One more than the cap: the innermost paragraph counts as a level
        // here, where the parser's floor counts open frames.
        let depth = depth_of(&doc.blocks);
        assert!(depth <= MAX_DEPTH + 1, "{depth} levels deep");
    }

    #[test]
    fn inline_nesting_has_the_same_floor() {
        let deep = format!("{}a{}\n", "*".repeat(20_000), "*".repeat(20_000));
        let doc = parse(&deep, ParseOptions::default());
        assert!(depth_of(&doc.blocks) <= MAX_DEPTH + 1);
    }

    /// The deepest chain of blocks in a tree.
    fn depth_of(blocks: &[Block]) -> usize {
        blocks
            .iter()
            .map(|block| match block {
                Block::BlockQuote { blocks, .. } | Block::FootnoteDefinition { blocks, .. } => {
                    1 + depth_of(blocks)
                }
                Block::List(list) => {
                    1 + list
                        .items
                        .iter()
                        .map(|i| depth_of(&i.blocks))
                        .max()
                        .unwrap_or(0)
                }
                _ => 1,
            })
            .max()
            .unwrap_or(0)
    }

    /// Every inline in a tree, flattened, for asserting nothing was dropped.
    fn collect_inlines(blocks: &[Block]) -> Inlines {
        let mut out = Vec::new();
        for block in blocks {
            match block {
                Block::Paragraph(inlines)
                | Block::Heading {
                    content: inlines, ..
                } => out.extend(inlines.clone()),
                Block::BlockQuote { blocks, .. } | Block::FootnoteDefinition { blocks, .. } => {
                    out.extend(collect_inlines(blocks))
                }
                Block::List(list) => {
                    for item in &list.items {
                        out.extend(collect_inlines(&item.blocks));
                    }
                }
                _ => {}
            }
        }
        out
    }

    #[test]
    fn parses_headings_with_slugs() {
        let d = doc("# Hello World\n## Hello World\n");
        assert_eq!(d.blocks.len(), 2);
        match (&d.blocks[0], &d.blocks[1]) {
            (Block::Heading { id: a, .. }, Block::Heading { id: b, .. }) => {
                assert_eq!(a, "hello-world");
                assert_eq!(b, "hello-world-1", "duplicate headings must not collide");
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(d.title.as_deref(), Some("Hello World"));
    }

    #[test]
    fn explicit_heading_ids_win() {
        let d = doc("# Title {#custom}\n");
        assert!(matches!(&d.blocks[0], Block::Heading { id, .. } if id == "custom"));
    }

    #[test]
    fn distinguishes_tight_and_loose_lists() {
        let tight = doc("- a\n- b\n");
        assert!(matches!(&tight.blocks[0], Block::List(l) if l.tight));

        let loose = doc("- a\n\n- b\n");
        assert!(matches!(&loose.blocks[0], Block::List(l) if !l.tight));
    }

    #[test]
    fn tight_items_still_carry_their_text() {
        let d = doc("- hello\n");
        let Block::List(list) = &d.blocks[0] else {
            panic!("expected list")
        };
        assert_eq!(list.items.len(), 1);
        assert_eq!(
            plain_text(match &list.items[0].blocks[0] {
                Block::Paragraph(p) => p,
                other => panic!("unexpected {other:?}"),
            }),
            "hello"
        );
    }

    #[test]
    fn parses_task_lists() {
        let d = doc("- [x] done\n- [ ] todo\n");
        let Block::List(list) = &d.blocks[0] else {
            panic!("expected list")
        };
        assert_eq!(list.items[0].task, Some(true));
        assert_eq!(list.items[1].task, Some(false));
    }

    #[test]
    fn parses_nested_lists() {
        let d = doc("- a\n  - b\n    - c\n");
        let Block::List(l1) = &d.blocks[0] else {
            panic!()
        };
        let Block::List(l2) = &l1.items[0].blocks[1] else {
            panic!("expected nested list")
        };
        assert!(matches!(&l2.items[0].blocks[1], Block::List(_)));
    }

    #[test]
    fn parses_tables_with_alignment() {
        let d = doc("| a | b |\n|:--|--:|\n| 1 | 2 |\n");
        let Block::Table(t) = &d.blocks[0] else {
            panic!("expected table")
        };
        assert_eq!(t.alignments, vec![Alignment::Left, Alignment::Right]);
        assert_eq!(t.head.len(), 2);
        assert_eq!(t.rows.len(), 1);
        assert_eq!(plain_text(&t.rows[0][1]), "2");
    }

    #[test]
    fn parses_github_alerts() {
        let d = doc("> [!WARNING]\n> Careful.\n");
        let Block::BlockQuote { kind, blocks } = &d.blocks[0] else {
            panic!("expected quote")
        };
        assert_eq!(*kind, Some(AlertKind::Warning));
        assert_eq!(blocks.len(), 1);
    }

    #[test]
    fn collects_footnotes_separately() {
        let d = doc("Text[^1]\n\n[^1]: The note.\n");
        assert_eq!(d.footnotes.len(), 1);
        assert_eq!(d.footnotes[0].0, "1");
        assert!(
            !d.blocks
                .iter()
                .any(|b| matches!(b, Block::FootnoteDefinition { .. }))
        );
    }

    #[test]
    fn code_blocks_keep_language_and_drop_trailing_newline() {
        let d = doc("```rust,ignore\nfn main() {}\n```\n");
        let Block::CodeBlock { language, code } = &d.blocks[0] else {
            panic!()
        };
        assert_eq!(language.as_deref(), Some("rust"));
        assert_eq!(code, "fn main() {}");
    }

    #[test]
    fn lone_images_become_figures() {
        let d = doc("![A cat](cat.png)\n");
        let Block::Figure { image, caption } = &d.blocks[0] else {
            panic!("expected figure")
        };
        assert_eq!(image.url, "cat.png");
        assert_eq!(plain_text(caption), "A cat");

        // ...but an image with text beside it stays inline.
        let d = doc("see ![A cat](cat.png) here\n");
        assert!(matches!(&d.blocks[0], Block::Paragraph(_)));
    }

    #[test]
    fn extracts_front_matter_and_title() {
        let d = doc("---\ntitle: From Front Matter\n---\n\n# Body\n");
        assert_eq!(d.title.as_deref(), Some("From Front Matter"));
        assert!(d.front_matter.as_deref().unwrap().contains("title:"));
    }

    #[test]
    fn html_images_survive() {
        let d = doc("<p align=\"center\"><img src=\"banner.png\" alt=\"Banner\"></p>\n");
        assert!(
            d.blocks
                .iter()
                .any(|b| matches!(b, Block::Figure { image, .. } if image.url == "banner.png")),
            "expected the HTML img to become a figure, got {:?}",
            d.blocks
        );
    }

    #[test]
    fn html_tags_split_across_lines_still_work() {
        // Regression: cmark emits an HTML block one line at a time, so this tag
        // reaches the parser in three pieces.
        let d = doc("<p>\n<img src=\n\"pic.png\"\n width=\"300px\">\n</p>\n");
        assert!(
            d.blocks
                .iter()
                .any(|b| matches!(b, Block::Figure { image, .. } if image.url == "pic.png")),
            "expected the image to survive being split: {:?}",
            d.blocks
        );
    }

    #[test]
    fn several_html_images_in_one_block_are_all_kept() {
        let d = doc("<p>\n<img src=\"a.png\">\n<img src=\"b.png\">\n</p>\n");
        let urls: Vec<&str> = d
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Figure { image, .. } => Some(image.url.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(urls, vec!["a.png", "b.png"]);
    }

    #[test]
    fn parses_math() {
        let d = doc("Inline $x^2$ and\n\n$$\\int f$$\n");
        assert!(matches!(&d.blocks[1], Block::DisplayMath(_)));
    }

    #[test]
    fn parses_definition_lists() {
        let d = doc("Term\n\n: Definition here\n");
        assert!(matches!(&d.blocks[0], Block::DefinitionList(items) if items.len() == 1));
    }

    #[test]
    fn handles_truncated_input_without_losing_content() {
        let d = doc("> quote with no end\n\n- item");
        assert!(d.blocks.len() >= 2);
    }

    #[test]
    fn empty_input_yields_empty_document() {
        assert_eq!(doc(""), Document::default());
    }
}
