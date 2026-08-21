//! A DEC sixel encoder, including colour quantisation.
//!
//! Sixel encodes six vertically stacked pixels per character, one bit each, and
//! addresses colour through a palette of at most 256 registers. So an encoder is
//! really two problems: reducing an arbitrary image to 256 colours without it
//! looking like 1987, and packing the result compactly.
//!
//! * Quantisation uses median cut over a 15-bit colour histogram, which keeps
//!   the work proportional to the number of distinct colours rather than the
//!   number of pixels.
//! * Floyd-Steinberg dithering spreads the residual error into neighbouring
//!   pixels, which is what stops gradients from banding.
//! * Output is run-length encoded, because a photograph's background is mostly
//!   long runs of the same sixel.
//!
//! Written against the DEC sixel specification; no code was taken from libsixel
//! or any other implementation.

use image::RgbaImage;

/// Sixel supports 256 colour registers; some terminals have fewer, but 256 is
//  the value they all accept.
const MAX_COLORS: usize = 256;
/// Alpha below this is treated as fully transparent and left undrawn.
const ALPHA_THRESHOLD: u8 = 128;
/// A run shorter than this costs more to encode than to repeat.
const MIN_RUN: usize = 4;

/// Encodes an image as a sixel escape sequence.
pub fn encode(image: &RgbaImage) -> Option<String> {
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return None;
    }
    let quantized = quantize(image, MAX_COLORS);
    Some(emit(&quantized, width as usize, height as usize))
}

/// An image reduced to a palette, with `None` marking transparent pixels.
struct Quantized {
    palette: Vec<[u8; 3]>,
    /// One entry per pixel, row-major.
    indices: Vec<Option<u8>>,
}

/// Builds the escape sequence from quantised pixels.
fn emit(q: &Quantized, width: usize, height: usize) -> String {
    let mut out = String::with_capacity(width * height / 4 + 256);

    // DCS with P2=1: bit-zero pixels leave the background untouched, which is
    // what makes transparency work.
    out.push_str("\x1bP0;1;0q");
    // Raster attributes: 1:1 pixel aspect, and the image's true size, so the
    // terminal reserves the right area before drawing.
    out.push_str(&format!("\"1;1;{width};{height}"));

    for (i, c) in q.palette.iter().enumerate() {
        // Colour components are percentages in sixel, not 0-255.
        out.push_str(&format!(
            "#{};2;{};{};{}",
            i,
            to_percent(c[0]),
            to_percent(c[1]),
            to_percent(c[2])
        ));
    }

    let bands = height.div_ceil(6);
    let mut used = vec![false; q.palette.len()];
    for band in 0..bands {
        let y0 = band * 6;
        let rows = 6.min(height - y0);

        used.iter_mut().for_each(|u| *u = false);
        for row in 0..rows {
            for x in 0..width {
                if let Some(i) = q.indices[(y0 + row) * width + x] {
                    used[i as usize] = true;
                }
            }
        }

        let mut first = true;
        for (color, _) in used.iter().enumerate().filter(|(_, u)| **u) {
            if !first {
                // Carriage return: draw the next colour over the same band.
                out.push('$');
            }
            first = false;
            out.push_str(&format!("#{color}"));
            emit_band(&mut out, q, width, height, y0, rows, color as u8);
        }
        if band + 1 < bands {
            out.push('-');
        }
    }

    out.push_str("\x1b\\");
    out
}

/// Writes one colour's contribution to one band, run-length encoded.
fn emit_band(
    out: &mut String,
    q: &Quantized,
    width: usize,
    _height: usize,
    y0: usize,
    rows: usize,
    color: u8,
) {
    let mut run_char = 0u8;
    let mut run_len = 0usize;
    // Trailing empty sixels need not be written at all, so they are held back
    // until something visible follows.
    let mut pending_empty = 0usize;

    let flush = |out: &mut String, ch: u8, len: usize| {
        if len == 0 {
            return;
        }
        let c = (0x3f + ch) as char;
        if len >= MIN_RUN {
            out.push_str(&format!("!{len}{c}"));
        } else {
            for _ in 0..len {
                out.push(c);
            }
        }
    };

    for x in 0..width {
        let mut bits = 0u8;
        for row in 0..rows {
            if q.indices[(y0 + row) * width + x] == Some(color) {
                bits |= 1 << row;
            }
        }
        if bits == 0 {
            if run_char == 0 {
                run_len += 1;
            } else {
                flush(out, run_char, run_len);
                run_char = 0;
                run_len = 1;
            }
            continue;
        }
        // Something visible: any empty run we were holding must be written now.
        if run_char == 0 && run_len > 0 {
            pending_empty = run_len;
            run_len = 0;
        }
        if pending_empty > 0 {
            flush(out, 0, pending_empty);
            pending_empty = 0;
        }
        if bits == run_char {
            run_len += 1;
        } else {
            flush(out, run_char, run_len);
            run_char = bits;
            run_len = 1;
        }
    }
    if run_char != 0 {
        flush(out, run_char, run_len);
    }
}

fn to_percent(v: u8) -> u32 {
    (v as u32 * 100 + 127) / 255
}

/// A histogram entry: one 15-bit colour bucket and the pixels that fell in it.
#[derive(Clone, Copy)]
struct Entry {
    count: u32,
    sum: [u32; 3],
}

impl Entry {
    fn channel(&self, c: usize) -> u8 {
        // Reconstruct the bucket's representative channel value.
        (self.sum[c] / self.count.max(1)) as u8
    }
}

/// Reduces an image to at most `max_colors` colours.
fn quantize(image: &RgbaImage, max_colors: usize) -> Quantized {
    let entries = histogram(image);
    if entries.is_empty() {
        return Quantized {
            palette: vec![[0, 0, 0]],
            indices: vec![None; image.pixels().len()],
        };
    }
    let palette = median_cut(entries, max_colors.max(1));
    let indices = map_pixels(image, &palette);
    Quantized { palette, indices }
}

/// Buckets colours to 5 bits per channel and counts them.
///
/// 32768 buckets is a big enough net to keep distinct colours apart and small
/// enough that the median cut below is cheap regardless of image size.
fn histogram(image: &RgbaImage) -> Vec<Entry> {
    let mut table: Vec<Option<Entry>> = vec![None; 1 << 15];
    for px in image.pixels() {
        let [r, g, b, a] = px.0;
        if a < ALPHA_THRESHOLD {
            continue;
        }
        let slot = &mut table[key555(r, g, b) as usize];
        match slot {
            Some(e) => {
                e.count += 1;
                e.sum[0] += r as u32;
                e.sum[1] += g as u32;
                e.sum[2] += b as u32;
            }
            None => {
                *slot = Some(Entry {
                    count: 1,
                    sum: [r as u32, g as u32, b as u32],
                });
            }
        }
    }
    table.into_iter().flatten().collect()
}

fn key555(r: u8, g: u8, b: u8) -> u16 {
    ((r as u16 >> 3) << 10) | ((g as u16 >> 3) << 5) | (b as u16 >> 3)
}

/// Median cut: repeatedly split the most promising box along its widest channel.
fn median_cut(mut entries: Vec<Entry>, max_colors: usize) -> Vec<[u8; 3]> {
    if entries.len() <= max_colors {
        return entries.iter().map(average_of_one).collect();
    }

    // Boxes are contiguous ranges of `entries`, which lets a split be a sort
    // plus an index rather than a reallocation.
    let mut boxes: Vec<(usize, usize)> = vec![(0, entries.len())];

    while boxes.len() < max_colors {
        // Split the box with the largest colour volume weighted by population:
        // a wide box holding one pixel matters less than a narrow, busy one.
        let target = boxes
            .iter()
            .enumerate()
            .filter(|(_, (s, e))| e - s > 1)
            .max_by_key(|(_, (s, e))| {
                let slice = &entries[*s..*e];
                let (_, range) = widest_channel(slice);
                let count: u32 = slice.iter().map(|x| x.count).sum();
                (range as u64) * (count as u64).max(1)
            })
            .map(|(i, _)| i);

        let Some(i) = target else { break };
        let (start, end) = boxes[i];
        let slice = &mut entries[start..end];
        let (channel, _) = widest_channel(slice);
        slice.sort_unstable_by_key(|e| e.channel(channel));

        // Cut at the population median so both halves carry similar weight.
        let total: u32 = slice.iter().map(|e| e.count).sum();
        let mut acc = 0u32;
        let mut split = 1;
        for (j, e) in slice.iter().enumerate() {
            acc += e.count;
            if acc * 2 >= total {
                split = (j + 1).clamp(1, slice.len() - 1);
                break;
            }
        }

        boxes[i] = (start, start + split);
        boxes.push((start + split, end));
    }

    boxes
        .iter()
        .map(|(s, e)| average(&entries[*s..*e]))
        .collect()
}

/// The channel with the widest spread in a box, and that spread.
fn widest_channel(entries: &[Entry]) -> (usize, u8) {
    let mut best = (0usize, 0u8);
    for c in 0..3 {
        let mut lo = u8::MAX;
        let mut hi = 0u8;
        for e in entries {
            let v = e.channel(c);
            lo = lo.min(v);
            hi = hi.max(v);
        }
        let range = hi.saturating_sub(lo);
        if range > best.1 {
            best = (c, range);
        }
    }
    best
}

fn average(entries: &[Entry]) -> [u8; 3] {
    let mut sum = [0u64; 3];
    let mut count = 0u64;
    for e in entries {
        for (c, total) in sum.iter_mut().enumerate() {
            *total += e.sum[c] as u64;
        }
        count += e.count as u64;
    }
    let count = count.max(1);
    [
        (sum[0] / count) as u8,
        (sum[1] / count) as u8,
        (sum[2] / count) as u8,
    ]
}

fn average_of_one(e: &Entry) -> [u8; 3] {
    [e.channel(0), e.channel(1), e.channel(2)]
}

/// Maps every pixel to a palette entry, diffusing the error as it goes.
fn map_pixels(image: &RgbaImage, palette: &[[u8; 3]]) -> Vec<Option<u8>> {
    let (w, h) = (image.width() as usize, image.height() as usize);
    let mut out = vec![None; w * h];
    // Working copy in i32 so diffused error can push a channel out of range
    // temporarily without wrapping.
    let mut buf: Vec<[i32; 3]> = image
        .pixels()
        .map(|p| [p.0[0] as i32, p.0[1] as i32, p.0[2] as i32])
        .collect();
    let mut cache = LookupCache::new(palette.len());

    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if image.as_raw()[i * 4 + 3] < ALPHA_THRESHOLD {
                continue;
            }
            let want = buf[i];
            let clamped = [
                want[0].clamp(0, 255) as u8,
                want[1].clamp(0, 255) as u8,
                want[2].clamp(0, 255) as u8,
            ];
            let index = cache.nearest(palette, clamped);
            out[i] = Some(index as u8);

            let chosen = palette[index];
            let error = [
                want[0] - chosen[0] as i32,
                want[1] - chosen[1] as i32,
                want[2] - chosen[2] as i32,
            ];
            // Floyd-Steinberg weights: 7/16 right, 3/16 down-left, 5/16 down,
            // 1/16 down-right.
            let mut spread = |dx: isize, dy: usize, num: i32| {
                let nx = x as isize + dx;
                let ny = y + dy;
                if nx < 0 || nx >= w as isize || ny >= h {
                    return;
                }
                let j = ny * w + nx as usize;
                for (c, channel) in buf[j].iter_mut().enumerate() {
                    *channel += error[c] * num / 16;
                }
            };
            spread(1, 0, 7);
            spread(-1, 1, 3);
            spread(0, 1, 5);
            spread(1, 1, 1);
        }
    }
    out
}

/// Caches nearest-palette lookups on 15-bit colour keys.
///
/// Without this, every pixel costs a linear scan of 256 palette entries; with
/// it, all but the first pixel of each colour bucket costs an array read.
struct LookupCache {
    table: Vec<u16>,
    palette_len: usize,
}

impl LookupCache {
    const EMPTY: u16 = u16::MAX;

    fn new(palette_len: usize) -> Self {
        Self {
            table: vec![Self::EMPTY; 1 << 15],
            palette_len,
        }
    }

    fn nearest(&mut self, palette: &[[u8; 3]], color: [u8; 3]) -> usize {
        let key = key555(color[0], color[1], color[2]) as usize;
        let cached = self.table[key];
        if cached != Self::EMPTY && (cached as usize) < self.palette_len {
            return cached as usize;
        }
        let mut best = 0usize;
        let mut best_d = i32::MAX;
        for (i, c) in palette.iter().enumerate() {
            let dr = color[0] as i32 - c[0] as i32;
            let dg = color[1] as i32 - c[1] as i32;
            let db = color[2] as i32 - c[2] as i32;
            // Green-weighted distance: the eye is most sensitive to it.
            let d = 2 * dr * dr + 4 * dg * dg + 3 * db * db;
            if d < best_d {
                best_d = d;
                best = i;
            }
        }
        self.table[key] = best as u16;
        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    fn gradient(w: u32, h: u32) -> RgbaImage {
        let mut img = RgbaImage::new(w, h);
        for (x, y, px) in img.enumerate_pixels_mut() {
            *px = Rgba([
                (x * 255 / w.max(1)) as u8,
                (y * 255 / h.max(1)) as u8,
                90,
                255,
            ]);
        }
        img
    }

    #[test]
    fn produces_a_well_formed_sequence() {
        let out = encode(&gradient(12, 12)).unwrap();
        assert!(out.starts_with("\x1bP"), "must open with a DCS");
        assert!(out.ends_with("\x1b\\"), "must close with ST");
        assert!(
            out.contains("\"1;1;12;12"),
            "raster attributes should state the size"
        );
        assert!(
            out.contains("#0;2;"),
            "at least one colour register must be defined"
        );
    }

    #[test]
    fn colour_components_are_percentages() {
        let img = RgbaImage::from_pixel(2, 2, Rgba([255, 0, 0, 255]));
        let out = encode(&img).unwrap();
        assert!(out.contains(";2;100;0;0"), "255 should become 100%: {out}");
    }

    #[test]
    fn every_data_byte_is_a_legal_sixel_character() {
        let out = encode(&gradient(20, 14)).unwrap();
        // Strip the header and terminator, then check the payload alphabet.
        let body = out
            .trim_start_matches("\x1bP0;1;0q")
            .trim_end_matches("\x1b\\");
        let body = &body[body.find('#').unwrap_or(0)..];
        for c in body.chars() {
            let ok = ('?'..='~').contains(&c)
                || c == '#'
                || c == '$'
                || c == '-'
                || c == '!'
                || c == ';'
                || c == '"'
                || c.is_ascii_digit();
            assert!(ok, "illegal sixel byte {c:?} in output");
        }
    }

    #[test]
    fn bands_are_separated_by_newlines() {
        // 13 rows spans three bands of six, so there should be two separators.
        let out = encode(&gradient(4, 13)).unwrap();
        assert_eq!(out.matches('-').count(), 2, "expected two band separators");
    }

    #[test]
    fn run_length_encodes_long_runs() {
        let img = RgbaImage::from_pixel(200, 6, Rgba([10, 200, 30, 255]));
        let out = encode(&img).unwrap();
        assert!(
            out.contains("!200"),
            "a uniform row should compress to one run: {out}"
        );
        assert!(
            out.len() < 400,
            "output should be compact, got {} bytes",
            out.len()
        );
    }

    #[test]
    fn transparent_pixels_are_left_undrawn() {
        let mut img = RgbaImage::from_pixel(8, 6, Rgba([255, 255, 255, 255]));
        for x in 0..4 {
            for y in 0..6 {
                img.put_pixel(x, y, Rgba([0, 0, 0, 0]));
            }
        }
        let out = encode(&img).unwrap();
        // The left half contributes no set bits, so it appears as empty sixels.
        assert!(
            out.contains('?'),
            "expected empty sixels for the transparent half: {out}"
        );
    }

    #[test]
    fn quantisation_stays_within_the_palette_limit() {
        let img = gradient(64, 64);
        let q = quantize(&img, 16);
        assert!(q.palette.len() <= 16, "got {} colours", q.palette.len());
        assert!(
            q.indices
                .iter()
                .flatten()
                .all(|&i| (i as usize) < q.palette.len())
        );
    }

    #[test]
    fn a_small_palette_is_preserved_exactly() {
        let mut img = RgbaImage::new(4, 1);
        img.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
        img.put_pixel(1, 0, Rgba([0, 255, 0, 255]));
        img.put_pixel(2, 0, Rgba([0, 0, 255, 255]));
        img.put_pixel(3, 0, Rgba([255, 0, 0, 255]));
        let q = quantize(&img, 256);
        assert_eq!(
            q.palette.len(),
            3,
            "three distinct colours, three registers"
        );
    }

    #[test]
    fn quantisation_keeps_colours_close_to_the_original() {
        let img = gradient(48, 48);
        let q = quantize(&img, 256);
        let mut worst = 0i32;
        for (i, px) in img.pixels().enumerate() {
            let chosen = q.palette[q.indices[i].unwrap() as usize];
            for (actual, want) in chosen.iter().zip(px.0.iter()) {
                worst = worst.max((*want as i32 - *actual as i32).abs());
            }
        }
        // Dithering trades per-pixel accuracy for perceived accuracy, so this is
        // a sanity bound rather than a tight one.
        assert!(worst < 96, "quantisation drifted too far: {worst}");
    }

    #[test]
    fn handles_a_single_pixel() {
        let img = RgbaImage::from_pixel(1, 1, Rgba([1, 2, 3, 255]));
        assert!(encode(&img).is_some());
    }

    #[test]
    fn handles_a_fully_transparent_image() {
        let img = RgbaImage::from_pixel(4, 4, Rgba([0, 0, 0, 0]));
        let out = encode(&img).unwrap();
        assert!(out.starts_with("\x1bP") && out.ends_with("\x1b\\"));
    }

    #[test]
    fn empty_images_encode_to_nothing() {
        assert!(encode(&RgbaImage::new(0, 0)).is_none());
    }
}
