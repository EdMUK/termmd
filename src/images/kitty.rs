//! The kitty graphics protocol.
//!
//! Implemented from the specification at
//! <https://sw.kovidgoyal.net/kitty/graphics-protocol/>.
//!
//! We transmit PNG (`f=100`) rather than raw pixels: the terminal reads the
//! dimensions from the file itself, and a compressed payload means far less data
//! through the pty. Placement is given in cells (`c`/`r`) so the image lands
//! exactly in the space the layout reserved for it, and `C=1` stops the terminal
//! from moving the cursor -- the writer positions everything itself, because
//! where an image leaves the cursor differs between terminals.

use std::io::Cursor;

use base64::Engine as _;
use image::{DynamicImage, ImageFormat};

/// Escape sequences have a maximum length, so the payload is chunked. Chunks
/// must be a multiple of 4 bytes so that base64 quanta are never split.
const CHUNK: usize = 4096;

/// Encodes an image for display at `cols` x `rows` cells.
pub fn encode(image: &DynamicImage, cols: u16, rows: u16, id: u32) -> Option<String> {
    let mut png = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .ok()?;
    Some(encode_png(&png, cols, rows, id))
}

/// Wraps already-encoded PNG bytes in the protocol's escape sequences.
pub fn encode_png(png: &[u8], cols: u16, rows: u16, id: u32) -> String {
    let payload = base64::engine::general_purpose::STANDARD.encode(png);
    let mut out = String::with_capacity(payload.len() + 64);

    let chunks: Vec<&str> = payload
        .as_bytes()
        .chunks(CHUNK)
        .map(|c| std::str::from_utf8(c).expect("base64 is ASCII"))
        .collect();

    for (i, chunk) in chunks.iter().enumerate() {
        let more = u8::from(i + 1 < chunks.len());
        out.push_str("\x1b_G");
        if i == 0 {
            // a=T: transmit and display. q=2: stay quiet, so that a terminal
            // that does not understand us cannot spray a reply into the page.
            //
            // z=-1 places the image above the background but below the text.
            // kitty's default, 0, draws it *over* the text, so anything the
            // pager draws on top of a picture -- the contents panel, a search
            // prompt -- would disappear behind it.
            out.push_str(&format!(
                "a=T,f=100,i={id},c={cols},r={rows},z=-1,C=1,q=2,m={more}"
            ));
        } else {
            // Every chunk repeats q=2, not just the first. The suppression key
            // applies per escape code, and a terminal that treats each one
            // independently will happily answer a continuation chunk that has
            // not asked for silence -- iTerm2 replies `i=0;OK` to the final
            // one. In the pager that reply is swallowed as input, but when
            // printing, the terminal is not in raw mode and echoes it into the
            // middle of the document. Only images large enough to be chunked
            // showed the problem, which is what made it look intermittent.
            out.push_str(&format!("i={id},q=2,m={more}"));
        }
        out.push(';');
        out.push_str(chunk);
        out.push_str("\x1b\\");
    }
    out
}

/// Deletes every image placement on screen.
///
/// The pager calls this before redrawing so that images from the previous frame
/// do not linger over the new one.
pub fn clear_all() -> &'static str {
    "\x1b_Ga=d,d=A,q=2\x1b\\"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_keys(seq: &str) -> std::collections::HashMap<String, String> {
        let body = seq.strip_prefix("\x1b_G").unwrap();
        let control = body.split(';').next().unwrap();
        control
            .split(',')
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn emits_a_single_sequence_for_a_small_image() {
        let seq = encode_png(b"tiny", 4, 2, 7);
        assert_eq!(seq.matches("\x1b_G").count(), 1);
        assert!(seq.ends_with("\x1b\\"));

        let keys = parse_keys(&seq);
        assert_eq!(keys["a"], "T");
        assert_eq!(keys["f"], "100", "PNG passthrough");
        assert_eq!(keys["c"], "4");
        assert_eq!(keys["r"], "2");
        assert_eq!(keys["i"], "7");
        assert_eq!(keys["C"], "1", "the cursor must not move");
        assert_eq!(
            keys["z"], "-1",
            "text must draw over the image, not under it"
        );
        assert_eq!(keys["m"], "0", "a single chunk is also the last chunk");
    }

    #[test]
    fn chunks_large_payloads_correctly() {
        let big = vec![0u8; CHUNK * 3];
        let seq = encode_png(&big, 10, 5, 1);
        let parts: Vec<&str> = seq.split("\x1b_G").filter(|s| !s.is_empty()).collect();
        assert!(parts.len() > 1, "expected several chunks");

        // Only the first chunk carries the image metadata.
        assert!(parts[0].contains("a=T"));
        assert!(!parts[1].contains("a=T"));

        // Every chunk but the last says "more to come".
        for part in &parts[..parts.len() - 1] {
            assert!(
                part.contains("m=1"),
                "chunk should continue: {}",
                &part[..20.min(part.len())]
            );
        }
        assert!(
            parts.last().unwrap().contains("m=0"),
            "last chunk must end the transfer"
        );
    }

    #[test]
    fn every_chunk_asks_for_silence() {
        // Regression: only the first chunk carried q=2, so a terminal that
        // treats each escape code on its own answered the last one. The reply
        // landed in the middle of the document, because printing does not put
        // the terminal in raw mode and the reply was echoed.
        let big = vec![0u8; CHUNK * 3];
        let seq = encode_png(&big, 10, 5, 1);
        for part in seq.split("\x1b_G").filter(|s| !s.is_empty()) {
            let control = part.split(';').next().unwrap();
            assert!(control.contains("q=2"), "chunk not silenced: {control}");
            assert!(control.contains("i=1"), "chunk not attributable: {control}");
        }
    }

    #[test]
    fn chunk_payloads_stay_on_base64_boundaries() {
        let big = vec![7u8; CHUNK * 2 + 17];
        let seq = encode_png(&big, 10, 5, 1);
        for part in seq.split("\x1b_G").filter(|s| !s.is_empty()) {
            let payload = part.split(';').nth(1).unwrap().trim_end_matches("\x1b\\");
            if payload.len() == CHUNK {
                assert_eq!(
                    payload.len() % 4,
                    0,
                    "chunks must not split a base64 quantum"
                );
            }
        }
    }

    #[test]
    fn round_trips_the_payload() {
        let png = b"\x89PNG\r\n\x1a\n some bytes here";
        let seq = encode_png(png, 1, 1, 1);
        let payload: String = seq
            .split("\x1b_G")
            .filter(|s| !s.is_empty())
            .map(|p| p.split(';').nth(1).unwrap().trim_end_matches("\x1b\\"))
            .collect();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .unwrap();
        assert_eq!(decoded, png);
    }

    #[test]
    fn encodes_a_real_image() {
        let img = image::DynamicImage::ImageRgba8(image::RgbaImage::new(8, 8));
        let seq = encode(&img, 2, 1, 3).unwrap();
        assert!(seq.starts_with("\x1b_G"));
    }
}
