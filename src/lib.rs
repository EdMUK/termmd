//! `termmd` renders Markdown for terminals that are better than we usually give
//! them credit for.
//!
//! The crate is organised as a pipeline. Each stage has a single job and hands a
//! plain data structure to the next one, which keeps the interesting parts
//! (layout, images, the pager) testable without a terminal attached:
//!
//! ```text
//! source text -> markdown::parse -> Document (IR)
//!             -> render::render   -> Screen (styled lines + image placements)
//!             -> output           -> bytes on a terminal
//! ```
//!
//! The [`Screen`](render::Screen) in the middle is the important boundary: both
//! the "just print it" path and the interactive pager consume the same value, so
//! what you see when you scroll is exactly what you get when you pipe.

pub mod cli;
pub mod config;
pub mod images;
pub mod markdown;
pub mod pager;
pub mod render;
pub mod source;
pub mod term;
pub mod theme;

pub use markdown::Document;
// pub use render::Screen;
pub use theme::Theme;
