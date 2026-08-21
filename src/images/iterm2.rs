//! iTerm2's inline images protocol.
//!
//! Implemented from <https://iterm2.com/documentation-images.html>: an OSC 1337
//! `File=` sequence carrying arguments and a base64 payload. We give the size in
//! cells and keep `preserveAspectRatio=1`, so a terminal that rounds differently
//! than we do letterboxes the picture rather than stretching it.
//!
//! WezTerm and mintty implement the same sequence, which is why this is a useful
//! backend beyond iTerm2 itself.

use std::io::Cursor;

use base64::Engine as _;
use image::{DynamicImage, ImageFormat};

/// Encodes an image for display at `cols` x `rows` cells.
pub fn encode(image: &DynamicImage, cols: u16, rows: u16, source: &str) -> Option<String> {
    let mut png = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .ok()?;
    Some(encode_bytes(&png, cols, rows, source))
}

/// Wraps already-encoded image bytes in the protocol's escape sequence.
///
/// `source` is where the picture came from; its basename is passed on as the
/// `name` argument. That is not decoration. The protocol carries no content
/// type, so a receiver works out what it has been handed partly from this
/// name's extension, and the default when it is omitted -- "Unnamed file", with
/// no extension at all -- is enough to make iTerm2 show a file chip instead of
/// the picture. We always transmit PNG, so the name always ends in `.png`.
pub fn encode_bytes(bytes: &[u8], cols: u16, rows: u16, source: &str) -> String {
    let engine = base64::engine::general_purpose::STANDARD;
    let payload = engine.encode(bytes);
    let name = engine.encode(png_name(source));
    // BEL rather than ST as the terminator, matching iTerm2's own `imgcat`.
    // Both are documented, but this is the form every implementation is
    // actually tested against, and graphics are off under a multiplexer anyway.
    format!(
        "\x1b]1337;File=name={name};size={};inline=1;width={cols};height={rows};preserveAspectRatio=1:{payload}\x07",
        bytes.len()
    )
}

/// A `.png` filename derived from wherever the image came from.
fn png_name(source: &str) -> String {
    let base = source
        .split(['?', '#'])
        .next()
        .unwrap_or(source)
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("image");
    let stem = base
        .rsplit_once('.')
        .map(|(name, _)| name)
        .unwrap_or(base)
        .trim();
    if stem.is_empty() {
        "image.png".to_string()
    } else {
        format!("{stem}.png")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_well_formed_sequence() {
        let seq = encode_bytes(b"hello", 12, 6, "logo.png");
        assert!(seq.starts_with("\x1b]1337;File="));
        assert!(seq.ends_with('\x07'), "BEL terminator, as imgcat uses");
        assert!(seq.contains("inline=1"));
        assert!(seq.contains("width=12"));
        assert!(seq.contains("height=6"));
        assert!(seq.contains("preserveAspectRatio=1"));
        assert!(
            seq.contains("size=5"),
            "size is the byte count, not the base64 length"
        );
    }

    #[test]
    fn payload_round_trips() {
        let data = b"\x89PNG\r\n\x1a\narbitrary";
        let seq = encode_bytes(data, 1, 1, "x.png");
        let payload = seq.split(':').next_back().unwrap().trim_end_matches('\x07');
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(payload)
                .unwrap(),
            data
        );
    }

    #[test]
    fn carries_a_png_filename() {
        // Without a name, iTerm2 has nothing to identify the payload by and
        // shows a file chip rather than the picture.
        let seq = encode_bytes(b"x", 1, 1, "https://example.com/assets/badge.svg?v=2");
        let name = seq
            .split("name=")
            .nth(1)
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(name)
            .unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), "badge.png");
    }

    #[test]
    fn names_awkward_sources_sensibly() {
        assert_eq!(png_name("logo.png"), "logo.png");
        assert_eq!(png_name("/a/b/diagram.jpeg"), "diagram.png");
        assert_eq!(png_name("no-extension"), "no-extension.png");
        assert_eq!(png_name(""), "image.png");
        assert_eq!(png_name("dir/"), "image.png");
    }

    #[test]
    fn encodes_a_real_image() {
        let img = DynamicImage::ImageRgba8(image::RgbaImage::new(4, 4));
        assert!(encode(&img, 1, 1, "x.png").unwrap().contains("File="));
    }
}
