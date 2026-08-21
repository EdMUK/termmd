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
pub fn encode(image: &DynamicImage, cols: u16, rows: u16) -> Option<String> {
    let mut png = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .ok()?;
    Some(encode_bytes(&png, cols, rows))
}

/// Wraps already-encoded image bytes in the protocol's escape sequence.
pub fn encode_bytes(bytes: &[u8], cols: u16, rows: u16) -> String {
    let payload = base64::engine::general_purpose::STANDARD.encode(bytes);
    // ST rather than BEL as the terminator: both are accepted, and ST is the
    // standard form, which multiplexers are more likely to pass through intact.
    format!(
        "\x1b]1337;File=inline=1;size={};width={cols};height={rows};preserveAspectRatio=1:{payload}\x1b\\",
        bytes.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_well_formed_sequence() {
        let seq = encode_bytes(b"hello", 12, 6);
        assert!(seq.starts_with("\x1b]1337;File="));
        assert!(seq.ends_with("\x1b\\"));
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
        let seq = encode_bytes(data, 1, 1);
        let payload = seq
            .split(':')
            .next_back()
            .unwrap()
            .trim_end_matches("\x1b\\");
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(payload)
                .unwrap(),
            data
        );
    }

    #[test]
    fn encodes_a_real_image() {
        let img = DynamicImage::ImageRgba8(image::RgbaImage::new(4, 4));
        assert!(encode(&img, 1, 1).unwrap().contains("File="));
    }
}
