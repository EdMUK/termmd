//! The document tree.
//!
//! `pulldown-cmark` hands us a flat event stream, which is ideal for streaming
//! to HTML and awkward for laying out a terminal page: to wrap a table column or
//! indent a nested list we need to know the whole subtree first. So the parser
//! folds events into this tree once, and every later stage works on it.

/// A parsed document plus the bits of metadata worth keeping.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Document {
    pub blocks: Vec<Block>,
    /// Footnote bodies, in first-reference order.
    pub footnotes: Vec<(String, Vec<Block>)>,
    /// Title from YAML front matter, or the first level-1 heading.
    pub title: Option<String>,
    /// Raw front matter, kept so the CLI can expose it.
    pub front_matter: Option<String>,
}

impl Document {
    /// Headings in document order, for the table of contents.
    pub fn headings(&self) -> Vec<(u8, String, String)> {
        let mut out = Vec::new();
        collect_headings(&self.blocks, &mut out);
        out
    }
}

fn collect_headings(blocks: &[Block], out: &mut Vec<(u8, String, String)>) {
    for b in blocks {
        match b {
            Block::Heading { level, id, content } => {
                out.push((*level, plain_text(content), id.clone()));
            }
            Block::BlockQuote { blocks, .. } => collect_headings(blocks, out),
            Block::List(list) => {
                for item in &list.items {
                    collect_headings(&item.blocks, out);
                }
            }
            _ => {}
        }
    }
}

/// Flattens inline content to plain text, for slugs, titles and search.
pub fn plain_text(inlines: &[Inline]) -> String {
    let mut s = String::new();
    push_plain(inlines, &mut s);
    s
}

fn push_plain(inlines: &[Inline], out: &mut String) {
    for i in inlines {
        match i {
            Inline::Text(t) | Inline::Code(t) | Inline::Math { text: t, .. } => out.push_str(t),
            Inline::Emph(c) | Inline::Strong(c) | Inline::Strike(c) => push_plain(c, out),
            Inline::Superscript(c) | Inline::Subscript(c) => push_plain(c, out),
            Inline::Link { content, .. } => push_plain(content, out),
            Inline::Image(img) => out.push_str(&img.alt),
            Inline::SoftBreak | Inline::HardBreak => out.push(' '),
            Inline::FootnoteRef(label) => {
                out.push('[');
                out.push_str(label);
                out.push(']');
            }
            Inline::Html(_) => {}
        }
    }
}

/// A block-level element.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Heading {
        level: u8,
        id: String,
        content: Inlines,
    },
    Paragraph(Inlines),
    CodeBlock {
        language: Option<String>,
        code: String,
    },
    /// A quote, or one of GitHub's alert callouts when `kind` is set.
    BlockQuote {
        kind: Option<AlertKind>,
        blocks: Vec<Block>,
    },
    List(List),
    Table(Table),
    /// A paragraph whose only content is an image, which we can render large.
    Figure {
        image: ImageRef,
        caption: Inlines,
    },
    Rule,
    /// Raw HTML we choose to show rather than silently drop.
    Html(String),
    /// `$$ ... $$`
    DisplayMath(String),
    /// Terms and their definitions.
    DefinitionList(Vec<(Inlines, Vec<Vec<Block>>)>),
    /// A footnote body, rendered at the end of the document.
    FootnoteDefinition {
        label: String,
        blocks: Vec<Block>,
    },
}

/// GitHub's alert callouts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertKind {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

impl AlertKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Note => "NOTE",
            Self::Tip => "TIP",
            Self::Important => "IMPORTANT",
            Self::Warning => "WARNING",
            Self::Caution => "CAUTION",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct List {
    /// `Some(start)` for ordered lists.
    pub start: Option<u64>,
    /// Tight lists set their items solid; loose lists get blank lines between.
    pub tight: bool,
    pub items: Vec<ListItem>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ListItem {
    /// `Some(checked)` when this is a task list item.
    pub task: Option<bool>,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Table {
    pub alignments: Vec<Alignment>,
    pub head: Vec<Cell>,
    pub rows: Vec<Vec<Cell>>,
}

/// Cell contents; a cell is inline-only in GFM.
pub type Cell = Inlines;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Alignment {
    #[default]
    None,
    Left,
    Center,
    Right,
}

pub type Inlines = Vec<Inline>;

#[derive(Debug, Clone, PartialEq)]
pub enum Inline {
    Text(String),
    Code(String),
    Emph(Inlines),
    Strong(Inlines),
    Strike(Inlines),
    Superscript(Inlines),
    Subscript(Inlines),
    Link {
        dest: String,
        title: Option<String>,
        content: Inlines,
    },
    Image(ImageRef),
    SoftBreak,
    HardBreak,
    FootnoteRef(String),
    Math {
        text: String,
        display: bool,
    },
    Html(String),
}

/// A reference to an image, before we have gone looking for the bytes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImageRef {
    pub url: String,
    pub alt: String,
    pub title: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_flattens_nested_inlines() {
        let inlines = vec![
            Inline::Text("a ".into()),
            Inline::Strong(vec![Inline::Emph(vec![Inline::Text("b".into())])]),
            Inline::SoftBreak,
            Inline::Code("c".into()),
        ];
        assert_eq!(plain_text(&inlines), "a b c");
    }

    #[test]
    fn headings_are_collected_in_document_order() {
        let doc = Document {
            blocks: vec![
                Block::Heading {
                    level: 1,
                    id: "one".into(),
                    content: vec![Inline::Text("One".into())],
                },
                Block::BlockQuote {
                    kind: None,
                    blocks: vec![Block::Heading {
                        level: 2,
                        id: "two".into(),
                        content: vec![Inline::Text("Two".into())],
                    }],
                },
            ],
            ..Default::default()
        };
        let h = doc.headings();
        assert_eq!(h.len(), 2);
        assert_eq!(h[0], (1, "One".to_string(), "one".to_string()));
        assert_eq!(h[1].0, 2);
    }
}

/// Rewrites relative image and link URLs so they resolve against `base`.
///
/// Done once after parsing, which means later stages never need to know which
/// file a node came from -- and a document assembled from several files keeps
/// each part's own idea of where `./diagram.png` lives.
pub fn rebase_urls(doc: &mut Document, base: &std::path::Path) {
    rebase_blocks(&mut doc.blocks, base);
    for (_, blocks) in &mut doc.footnotes {
        rebase_blocks(blocks, base);
    }
}

fn rebase_blocks(blocks: &mut [Block], base: &std::path::Path) {
    for block in blocks {
        match block {
            Block::Heading { content, .. } | Block::Paragraph(content) => {
                rebase_inlines(content, base)
            }
            Block::BlockQuote { blocks, .. } | Block::FootnoteDefinition { blocks, .. } => {
                rebase_blocks(blocks, base)
            }
            Block::List(list) => {
                for item in &mut list.items {
                    rebase_blocks(&mut item.blocks, base);
                }
            }
            Block::Table(table) => {
                for cell in table.head.iter_mut().chain(table.rows.iter_mut().flatten()) {
                    rebase_inlines(cell, base);
                }
            }
            Block::Figure { image, caption } => {
                rebase_url(&mut image.url, base);
                rebase_inlines(caption, base);
            }
            Block::DefinitionList(items) => {
                for (term, defs) in items {
                    rebase_inlines(term, base);
                    for blocks in defs {
                        rebase_blocks(blocks, base);
                    }
                }
            }
            Block::CodeBlock { .. } | Block::Rule | Block::Html(_) | Block::DisplayMath(_) => {}
        }
    }
}

fn rebase_inlines(inlines: &mut [Inline], base: &std::path::Path) {
    for inline in inlines {
        match inline {
            Inline::Image(img) => rebase_url(&mut img.url, base),
            Inline::Link { dest, content, .. } => {
                rebase_url(dest, base);
                rebase_inlines(content, base);
            }
            Inline::Emph(c) | Inline::Strong(c) | Inline::Strike(c) => rebase_inlines(c, base),
            Inline::Superscript(c) | Inline::Subscript(c) => rebase_inlines(c, base),
            _ => {}
        }
    }
}

/// Resolves one relative URL against the document's directory.
///
/// The result is an absolute path. That matters more than it looks: later
/// stages resolve paths too, and a relative result can be joined a second time
/// against a different base, quietly turning `doc/x.png` into `doc/doc/x.png`.
/// An absolute path cannot be re-based by accident.
fn rebase_url(url: &mut String, base: &std::path::Path) {
    // Absolute URLs, fragments, and anything with a scheme stay as they are.
    if url.is_empty()
        || url.starts_with('#')
        || url.starts_with('/')
        || url.starts_with("data:")
        || url.split_once("://").is_some()
        || url.split_once(':').is_some_and(|(scheme, _)| {
            !scheme.is_empty()
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-')
        })
    {
        return;
    }
    let joined = base.join(&*url);
    let absolute = std::path::absolute(&joined).unwrap_or(joined);
    if let Some(s) = absolute.to_str() {
        *url = s.to_string();
    }
}

#[cfg(test)]
mod rebase_tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// An absolute directory for whatever platform this is: on Windows a path
    /// needs a drive letter to count as absolute, so `/docs` will not do.
    fn base() -> PathBuf {
        std::path::absolute("docs").expect("a working directory")
    }

    fn url_after_rebase(url: &str) -> String {
        rebase_from(&base(), url)
    }

    fn rebase_from(base: &Path, url: &str) -> String {
        let mut doc = Document {
            blocks: vec![Block::Figure {
                image: ImageRef {
                    url: url.into(),
                    ..Default::default()
                },
                caption: Vec::new(),
            }],
            ..Default::default()
        };
        rebase_urls(&mut doc, base);
        match &doc.blocks[0] {
            Block::Figure { image, .. } => image.url.clone(),
            _ => unreachable!(),
        }
    }

    #[test]
    fn makes_relative_paths_absolute() {
        // Compared as paths, not strings, so the separator does not matter.
        assert_eq!(
            Path::new(&url_after_rebase("img/logo.png")),
            base().join("img/logo.png")
        );
        assert_eq!(
            Path::new(&url_after_rebase("./logo.png")),
            base().join("logo.png")
        );
    }

    #[test]
    fn leaves_everything_else_alone() {
        for url in [
            "https://example.com/a.png",
            "http://example.com/a.png",
            "/absolute/a.png",
            "data:image/png;base64,AAAA",
            "#anchor",
            "mailto:someone@example.com",
        ] {
            assert_eq!(url_after_rebase(url), url, "{url} should not be rebased");
        }
    }

    #[test]
    fn a_relative_base_still_yields_an_absolute_path() {
        // The guard against a second base being applied further down the line.
        let url = rebase_from(Path::new("doc"), "images/x.png");
        let path = Path::new(&url);
        assert!(path.is_absolute(), "got {url}");
        assert!(path.ends_with("doc/images/x.png"), "got {url}");
    }

    #[test]
    fn rebases_inside_nested_blocks() {
        let mut doc = Document {
            blocks: vec![Block::BlockQuote {
                kind: None,
                blocks: vec![Block::Paragraph(vec![Inline::Link {
                    dest: "other.md".into(),
                    title: None,
                    content: vec![Inline::Text("x".into())],
                }])],
            }],
            ..Default::default()
        };
        rebase_urls(&mut doc, &base());
        let Block::BlockQuote { blocks, .. } = &doc.blocks[0] else {
            panic!()
        };
        let Block::Paragraph(inlines) = &blocks[0] else {
            panic!()
        };
        let Inline::Link { dest, .. } = &inlines[0] else {
            panic!("expected a link")
        };
        assert_eq!(Path::new(dest), base().join("other.md"));
    }
}
