//! Everything that depends on what the terminal on the other end can actually do.

pub mod caps;
pub mod clipboard;
pub mod probe;
pub mod style;
pub mod tmux;

pub use caps::{Capabilities, ColorDepth, GraphicsProtocol, UnicodeLevel};
pub use style::{Color, Rgb, Style, StyleWriter};
