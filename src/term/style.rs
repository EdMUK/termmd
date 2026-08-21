//! Colours, text attributes, and the SGR encoder that turns them into bytes.
//!
//! Styles are authored once in truecolor and degraded on the way out, so a theme
//! never has to be written twice for a 256-colour terminal.

use std::fmt::Write as _;

use super::caps::ColorDepth;

/// A 24-bit colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// Parses `#rgb`, `#rrggbb`, or `rrggbb`.
    pub fn parse(s: &str) -> Option<Self> {
        let h = s.strip_prefix('#').unwrap_or(s);
        let v = |i: usize, n: usize| u8::from_str_radix(&h[i..i + n], 16).ok();
        match h.len() {
            3 => {
                let (r, g, b) = (v(0, 1)?, v(1, 1)?, v(2, 1)?);
                Some(Rgb(r * 17, g * 17, b * 17))
            }
            6 => Some(Rgb(v(0, 2)?, v(2, 2)?, v(4, 2)?)),
            _ => None,
        }
    }

    /// Relative luminance per WCAG, used to decide whether a background is dark.
    pub fn luminance(self) -> f32 {
        let f = |c: u8| {
            let c = c as f32 / 255.0;
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * f(self.0) + 0.7152 * f(self.1) + 0.0722 * f(self.2)
    }

    /// Mixes two colours, `t` = 0.0 yields `self`.
    pub fn blend(self, other: Rgb, t: f32) -> Rgb {
        let m = |a: u8, b: u8| {
            (a as f32 + (b as f32 - a as f32) * t)
                .round()
                .clamp(0.0, 255.0) as u8
        };
        Rgb(m(self.0, other.0), m(self.1, other.1), m(self.2, other.2))
    }

    /// Nearest index in the xterm 256-colour cube.
    ///
    /// The palette is a 6x6x6 cube plus a 24-step grey ramp. Greys in the cube
    /// are coarse, so we compare against the ramp too and keep whichever is
    /// closer -- this is what stops dark greys from snapping to pure black.
    pub fn to_xterm256(self) -> u8 {
        const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
        let nearest = |c: u8| {
            LEVELS
                .iter()
                .enumerate()
                .min_by_key(|&(_, l)| (*l as i32 - c as i32).abs())
                .map(|(i, _)| i)
                .unwrap_or(0)
        };
        let (ri, gi, bi) = (nearest(self.0), nearest(self.1), nearest(self.2));
        let cube = Rgb(LEVELS[ri], LEVELS[gi], LEVELS[bi]);
        let cube_idx = 16 + 36 * ri as u8 + 6 * gi as u8 + bi as u8;

        // Grey ramp: indices 232..=255 run from 8 to 238 in steps of 10.
        let avg = (self.0 as u32 + self.1 as u32 + self.2 as u32) / 3;
        let step = ((avg as i32 - 8).clamp(0, 238) as f32 / 10.0)
            .round()
            .clamp(0.0, 23.0) as u8;
        let grey_val = 8 + step * 10;
        let grey = Rgb(grey_val, grey_val, grey_val);

        if self.distance2(grey) < self.distance2(cube) {
            232 + step
        } else {
            cube_idx
        }
    }

    /// Nearest of the 16 ANSI colours, assuming a conventional palette.
    pub fn to_ansi16(self) -> u8 {
        const PALETTE: [(u8, Rgb); 16] = [
            (0, Rgb(0, 0, 0)),
            (1, Rgb(170, 0, 0)),
            (2, Rgb(0, 170, 0)),
            (3, Rgb(170, 85, 0)),
            (4, Rgb(0, 0, 170)),
            (5, Rgb(170, 0, 170)),
            (6, Rgb(0, 170, 170)),
            (7, Rgb(170, 170, 170)),
            (8, Rgb(85, 85, 85)),
            (9, Rgb(255, 85, 85)),
            (10, Rgb(85, 255, 85)),
            (11, Rgb(255, 255, 85)),
            (12, Rgb(85, 85, 255)),
            (13, Rgb(255, 85, 255)),
            (14, Rgb(85, 255, 255)),
            (15, Rgb(255, 255, 255)),
        ];
        PALETTE
            .iter()
            .min_by_key(|(_, col)| self.distance2(*col))
            .map(|(i, _)| *i)
            .unwrap_or(7)
    }

    fn distance2(self, other: Rgb) -> u32 {
        // Weighted to approximate perceived difference; cheap and good enough
        // for palette snapping.
        let dr = self.0 as i32 - other.0 as i32;
        let dg = self.1 as i32 - other.1 as i32;
        let db = self.2 as i32 - other.2 as i32;
        (2 * dr * dr + 4 * dg * dg + 3 * db * db) as u32
    }
}

/// A colour slot in a style: either the terminal default or a concrete colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Color {
    #[default]
    Default,
    Rgb(Rgb),
    /// An explicit palette index, used when a theme wants the user's own colours.
    Indexed(u8),
}

impl Color {
    pub fn rgb(r: u8, g: u8, b: u8) -> Self {
        Color::Rgb(Rgb(r, g, b))
    }
}

/// Text attributes plus foreground/background.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub reverse: bool,
}

impl Style {
    pub const PLAIN: Style = Style {
        fg: Color::Default,
        bg: Color::Default,
        bold: false,
        dim: false,
        italic: false,
        underline: false,
        strike: false,
        reverse: false,
    };

    pub fn fg(mut self, c: Color) -> Self {
        self.fg = c;
        self
    }
    pub fn bg(mut self, c: Color) -> Self {
        self.bg = c;
        self
    }
    pub fn bold(mut self) -> Self {
        self.bold = true;
        self
    }
    pub fn dim(mut self) -> Self {
        self.dim = true;
        self
    }
    pub fn italic(mut self) -> Self {
        self.italic = true;
        self
    }
    pub fn underline(mut self) -> Self {
        self.underline = true;
        self
    }
    pub fn strike(mut self) -> Self {
        self.strike = true;
        self
    }
    pub fn reverse(mut self) -> Self {
        self.reverse = true;
        self
    }

    /// Layers `over` on top of `self`; set attributes win, colours override.
    pub fn merge(self, over: Style) -> Style {
        Style {
            fg: if over.fg == Color::Default {
                self.fg
            } else {
                over.fg
            },
            bg: if over.bg == Color::Default {
                self.bg
            } else {
                over.bg
            },
            bold: self.bold || over.bold,
            dim: self.dim || over.dim,
            italic: self.italic || over.italic,
            underline: self.underline || over.underline,
            strike: self.strike || over.strike,
            reverse: self.reverse || over.reverse,
        }
    }

    pub fn is_plain(&self) -> bool {
        *self == Style::PLAIN
    }
}

/// Emits SGR sequences, degrading colour and attributes to fit the terminal.
///
/// The writer remembers the style it last emitted so that a run of identically
/// styled spans costs one escape sequence rather than one per span.
#[derive(Debug)]
pub struct StyleWriter {
    depth: ColorDepth,
    current: Style,
    /// Terminals that ignore SGR 3 (italic) get underline instead, which reads
    /// closer to the author's intent than silently dropping the emphasis.
    italic_as_underline: bool,
}

impl StyleWriter {
    pub fn new(depth: ColorDepth) -> Self {
        Self {
            depth,
            current: Style::PLAIN,
            italic_as_underline: false,
        }
    }

    pub fn with_italic_fallback(mut self, on: bool) -> Self {
        self.italic_as_underline = on;
        self
    }

    pub fn depth(&self) -> ColorDepth {
        self.depth
    }

    /// Resets internal bookkeeping; call when the caller has emitted its own reset.
    pub fn forget(&mut self) {
        self.current = Style::PLAIN;
    }

    /// Appends the escape sequence that moves from the current style to `next`.
    ///
    /// Turning an attribute *off* requires a full reset on most terminals, so we
    /// only take the incremental path when the new style is strictly additive.
    pub fn transition(&mut self, next: Style, out: &mut String) {
        let next = self.normalize(next);
        if next == self.current {
            return;
        }
        if next.is_plain() {
            out.push_str("\x1b[0m");
            self.current = next;
            return;
        }

        let drops_attr = (self.current.bold && !next.bold)
            || (self.current.dim && !next.dim)
            || (self.current.italic && !next.italic)
            || (self.current.underline && !next.underline)
            || (self.current.strike && !next.strike)
            || (self.current.reverse && !next.reverse)
            || (self.current.fg != Color::Default && next.fg == Color::Default)
            || (self.current.bg != Color::Default && next.bg == Color::Default);

        let mut params: Vec<String> = Vec::with_capacity(6);
        let base = if drops_attr {
            params.push("0".into());
            Style::PLAIN
        } else {
            self.current
        };

        if next.bold && !base.bold {
            params.push("1".into());
        }
        if next.dim && !base.dim {
            params.push("2".into());
        }
        if next.italic && !base.italic {
            params.push("3".into());
        }
        if next.underline && !base.underline {
            params.push("4".into());
        }
        if next.reverse && !base.reverse {
            params.push("7".into());
        }
        if next.strike && !base.strike {
            params.push("9".into());
        }
        if next.fg != base.fg {
            params.push(self.color_params(next.fg, false));
        }
        if next.bg != base.bg {
            params.push(self.color_params(next.bg, true));
        }

        if !params.is_empty() {
            let _ = write!(out, "\x1b[{}m", params.join(";"));
        }
        self.current = next;
    }

    /// Writes a reset if anything is currently set.
    pub fn reset(&mut self, out: &mut String) {
        if !self.current.is_plain() {
            out.push_str("\x1b[0m");
            self.current = Style::PLAIN;
        }
    }

    /// Strips what this terminal cannot render, so comparisons stay meaningful.
    fn normalize(&self, mut s: Style) -> Style {
        if self.depth == ColorDepth::None {
            s.fg = Color::Default;
            s.bg = Color::Default;
        }
        if self.italic_as_underline && s.italic {
            s.italic = false;
            s.underline = true;
        }
        s
    }

    fn color_params(&self, c: Color, background: bool) -> String {
        let base = if background { 48 } else { 38 };
        let default = if background { "49" } else { "39" };
        match c {
            Color::Default => default.to_string(),
            Color::Indexed(i) => Self::indexed_params(i, background),
            Color::Rgb(rgb) => match self.depth {
                ColorDepth::TrueColor => format!("{base};2;{};{};{}", rgb.0, rgb.1, rgb.2),
                ColorDepth::Ansi256 => format!("{base};5;{}", rgb.to_xterm256()),
                ColorDepth::Ansi16 => Self::indexed_params(rgb.to_ansi16(), background),
                ColorDepth::None => default.to_string(),
            },
        }
    }

    fn indexed_params(i: u8, background: bool) -> String {
        // 0-7 and 8-15 have dedicated short codes that respect the user's own
        // palette more reliably than the 256-colour form on some terminals.
        match (i, background) {
            (0..=7, false) => format!("{}", 30 + i),
            (0..=7, true) => format!("{}", 40 + i),
            (8..=15, false) => format!("{}", 90 + (i - 8)),
            (8..=15, true) => format!("{}", 100 + (i - 8)),
            (_, false) => format!("38;5;{i}"),
            (_, true) => format!("48;5;{i}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_colors() {
        assert_eq!(Rgb::parse("#ff8800"), Some(Rgb(255, 136, 0)));
        assert_eq!(Rgb::parse("f80"), Some(Rgb(255, 136, 0)));
        assert_eq!(Rgb::parse("#zzz"), None);
        assert_eq!(Rgb::parse("#ff88"), None);
    }

    #[test]
    fn truecolor_roundtrip_is_exact() {
        let mut w = StyleWriter::new(ColorDepth::TrueColor);
        let mut out = String::new();
        w.transition(Style::PLAIN.fg(Color::rgb(1, 2, 3)), &mut out);
        assert_eq!(out, "\x1b[38;2;1;2;3m");
    }

    #[test]
    fn degrades_to_256_and_16() {
        let mut out = String::new();
        StyleWriter::new(ColorDepth::Ansi256)
            .transition(Style::PLAIN.fg(Color::rgb(255, 255, 255)), &mut out);
        assert_eq!(out, "\x1b[38;5;231m");

        // Nearest-neighbour in RGB space: pure red is closer to the palette's
        // red (170,0,0) than to its bright red (255,85,85).
        let mut out = String::new();
        StyleWriter::new(ColorDepth::Ansi16)
            .transition(Style::PLAIN.fg(Color::rgb(255, 0, 0)), &mut out);
        assert_eq!(out, "\x1b[31m");

        let mut out = String::new();
        StyleWriter::new(ColorDepth::Ansi16)
            .transition(Style::PLAIN.fg(Color::rgb(255, 122, 112)), &mut out);
        assert_eq!(out, "\x1b[91m", "a soft red should reach the bright slot");
    }

    #[test]
    fn mono_emits_attributes_but_no_color() {
        let mut out = String::new();
        StyleWriter::new(ColorDepth::None)
            .transition(Style::PLAIN.fg(Color::rgb(255, 0, 0)).bold(), &mut out);
        assert_eq!(out, "\x1b[1m");
    }

    #[test]
    fn additive_transition_avoids_reset() {
        let mut w = StyleWriter::new(ColorDepth::TrueColor);
        let mut out = String::new();
        w.transition(Style::PLAIN.bold(), &mut out);
        out.clear();
        w.transition(Style::PLAIN.bold().italic(), &mut out);
        assert_eq!(out, "\x1b[3m", "adding an attribute should not reset");
    }

    #[test]
    fn removing_an_attribute_resets_first() {
        let mut w = StyleWriter::new(ColorDepth::TrueColor);
        let mut out = String::new();
        w.transition(Style::PLAIN.bold().italic(), &mut out);
        out.clear();
        w.transition(Style::PLAIN.italic(), &mut out);
        assert_eq!(out, "\x1b[0;3m");
    }

    #[test]
    fn grey_prefers_the_ramp_over_the_cube() {
        // 0x30 grey is much closer to ramp entry 236 than to cube black.
        assert_eq!(Rgb(48, 48, 48).to_xterm256(), 236);
    }

    #[test]
    fn merge_prefers_the_overlay() {
        let base = Style::PLAIN.fg(Color::rgb(1, 1, 1)).bold();
        let over = Style::PLAIN.fg(Color::rgb(2, 2, 2)).italic();
        let m = base.merge(over);
        assert_eq!(m.fg, Color::rgb(2, 2, 2));
        assert!(m.bold && m.italic);
    }
}
