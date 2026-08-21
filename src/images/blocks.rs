//! The universal fallback: pictures made of half blocks.
//!
//! `U+2580 UPPER HALF BLOCK` fills the top half of a cell with the foreground
//! colour and leaves the bottom half showing the background, so one cell carries
//! two vertically stacked pixels. That doubles the vertical resolution for free
//! and is why terminal image viewers reach for this character rather than a full
//! block.
//!
//! It is only coloured text, so it works anywhere with 256 colours -- including
//! inside tmux and over ssh, where the pixel protocols usually cannot go.

use image::RgbaImage;

use crate::term::caps::ColorDepth;
use crate::term::style::{Color, Rgb, Style, StyleWriter};

/// Pixels below this alpha are treated as fully transparent.
const ALPHA_THRESHOLD: u8 = 128;

/// Renders an image as half blocks.
///
/// The image should already be sized in pixels to `cols` x `rows * 2`. Lines
/// after the first are indented by `indent` columns so the picture stays in its
/// column when it sits inside a list or a quote.
pub fn encode(image: &RgbaImage, indent: u16) -> String {
    encode_with_depth(image, indent, ColorDepth::TrueColor)
}

/// As [`encode`], with an explicit colour depth for the output.
pub fn encode_with_depth(image: &RgbaImage, indent: u16, depth: ColorDepth) -> String {
    let (w, h) = image.dimensions();
    if w == 0 || h == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut sw = StyleWriter::new(depth);
    let pad = " ".repeat(indent as usize);

    for row in 0..h.div_ceil(2) {
        if row > 0 {
            sw.reset(&mut out);
            out.push('\n');
            out.push_str(&pad);
        }
        for x in 0..w {
            let top = sample(image, x, row * 2);
            let bottom = sample(image, x, row * 2 + 1);
            let (text, style) = cell(top, bottom);
            sw.transition(style, &mut out);
            out.push_str(text);
        }
    }
    sw.reset(&mut out);
    out
}

/// Chooses the character and colours for one cell from its two pixels.
fn cell(top: Option<Rgb>, bottom: Option<Rgb>) -> (&'static str, Style) {
    match (top, bottom) {
        // Both halves visible: foreground paints the top, background the bottom.
        (Some(t), Some(b)) => ("\u{2580}", Style::PLAIN.fg(Color::Rgb(t)).bg(Color::Rgb(b))),
        // Only one half visible: draw that half and leave the other transparent,
        // so the terminal's own background shows through.
        (Some(t), None) => ("\u{2580}", Style::PLAIN.fg(Color::Rgb(t))),
        (None, Some(b)) => ("\u{2584}", Style::PLAIN.fg(Color::Rgb(b))),
        (None, None) => (" ", Style::PLAIN),
    }
}

/// Reads a pixel, returning `None` for transparency and out-of-range rows.
fn sample(image: &RgbaImage, x: u32, y: u32) -> Option<Rgb> {
    if y >= image.height() {
        return None;
    }
    let px = image.get_pixel(x, y).0;
    (px[3] >= ALPHA_THRESHOLD).then(|| Rgb(px[0], px[1], px[2]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    fn solid(w: u32, h: u32, color: [u8; 4]) -> RgbaImage {
        RgbaImage::from_pixel(w, h, Rgba(color))
    }

    /// Counts the visible cells, ignoring escape sequences.
    fn visible(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c in chars.by_ref() {
                    if c == 'm' || c == '\\' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn packs_two_pixel_rows_into_one_line() {
        let img = solid(4, 4, [255, 0, 0, 255]);
        let out = encode(&img, 0);
        assert_eq!(
            visible(&out).lines().count(),
            2,
            "4 pixel rows make 2 text rows"
        );
        assert_eq!(visible(&out).lines().next().unwrap().chars().count(), 4);
    }

    #[test]
    fn uses_foreground_and_background_for_the_two_halves() {
        let mut img = solid(1, 2, [255, 0, 0, 255]);
        img.put_pixel(0, 1, Rgba([0, 0, 255, 255]));
        let out = encode(&img, 0);
        assert!(
            out.contains("38;2;255;0;0"),
            "top half is the foreground: {out:?}"
        );
        assert!(
            out.contains("48;2;0;0;255"),
            "bottom half is the background: {out:?}"
        );
        assert!(out.contains('\u{2580}'));
    }

    #[test]
    fn an_odd_number_of_rows_leaves_the_last_half_transparent() {
        let img = solid(2, 3, [10, 20, 30, 255]);
        let out = encode(&img, 0);
        let shown = visible(&out);
        assert_eq!(shown.lines().count(), 2);
        // The final row has no bottom pixel, so no background is set for it.
        assert!(out.lines().next_back().unwrap().contains('\u{2580}'));
    }

    #[test]
    fn transparent_pixels_produce_no_colour() {
        let img = solid(2, 2, [0, 0, 0, 0]);
        let out = encode(&img, 0);
        assert_eq!(visible(&out), "  ", "transparent cells are spaces");
        assert!(!out.contains("38;2;"), "nothing should be painted: {out:?}");
    }

    #[test]
    fn a_transparent_top_half_draws_the_lower_block() {
        let mut img = solid(1, 2, [0, 0, 0, 0]);
        img.put_pixel(0, 1, Rgba([1, 2, 3, 255]));
        let out = encode(&img, 0);
        assert!(
            out.contains('\u{2584}'),
            "expected a lower half block: {out:?}"
        );
    }

    #[test]
    fn indents_continuation_rows() {
        let img = solid(2, 4, [255, 255, 255, 255]);
        let out = encode(&img, 3);
        let second = visible(&out).lines().nth(1).unwrap().to_string();
        assert!(
            second.starts_with("   "),
            "second row should be indented: {second:?}"
        );
    }

    #[test]
    fn degrades_to_the_available_colour_depth() {
        let img = solid(1, 2, [255, 255, 255, 255]);
        let out = encode_with_depth(&img, 0, ColorDepth::Ansi256);
        assert!(out.contains("38;5;"), "should use palette colours: {out:?}");
        assert!(!out.contains("38;2;"));
    }

    #[test]
    fn resets_style_at_the_end() {
        let out = encode(&solid(1, 2, [1, 2, 3, 255]), 0);
        assert!(
            out.ends_with("\x1b[0m"),
            "must not leak style into later text"
        );
    }

    #[test]
    fn empty_images_produce_nothing() {
        assert_eq!(encode(&RgbaImage::new(0, 0), 0), "");
    }
}
