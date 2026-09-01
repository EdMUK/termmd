//! Markdown parsing: from source text to a tree we can lay out.

mod html;
mod ir;
mod parse;
mod slug;

pub use ir::plain_text as ir_plain_text;
pub use ir::{
    AlertKind, Alignment, Block, Cell, Document, ImageRef, Inline, Inlines, List, ListItem, Table,
};
pub use ir::{UrlBase, rebase_urls};
pub use parse::{ParseOptions, parse};
pub use slug::Slugger;
