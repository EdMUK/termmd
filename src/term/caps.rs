//! What the terminal on the other end can do, and how we found out.
//!
//! Detection runs in two tiers. Environment variables give a usable answer
//! instantly and work over pipes and in CI; a live probe (see [`super::probe`])
//! asks the terminal directly and overrides the guesses when it answers. Every
//! field can be forced from the CLI or the config file, because auto-detection
//! is a heuristic and users know their setup better than we do.

use std::env;
use std::io::IsTerminal;

use super::style::Rgb;

/// How many colours we may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum ColorDepth {
    /// Attributes only: bold, italic, underline.
    None,
    Ansi16,
    #[default]
    Ansi256,
    TrueColor,
}

/// The image protocol we will use, in descending order of fidelity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GraphicsProtocol {
    /// kitty's graphics protocol: PNG passthrough, precise cell placement.
    Kitty,
    /// iTerm2's `OSC 1337;File=` inline images.
    ITerm2,
    /// DEC sixel, encoded by [`crate::images::sixel`].
    Sixel,
    /// Unicode half blocks with foreground/background colour pairs.
    Blocks,
    /// Show a captioned placeholder instead.
    #[default]
    None,
}

impl GraphicsProtocol {
    pub fn name(self) -> &'static str {
        match self {
            Self::Kitty => "kitty",
            Self::ITerm2 => "iterm2",
            Self::Sixel => "sixel",
            Self::Blocks => "blocks",
            Self::None => "none",
        }
    }

    /// True for protocols that draw real pixels rather than coloured cells.
    pub fn is_pixel_exact(self) -> bool {
        matches!(self, Self::Kitty | Self::ITerm2 | Self::Sixel)
    }
}

/// How adventurous we can be with non-ASCII drawing characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum UnicodeLevel {
    /// Pure ASCII: `-`, `|`, `+`, `*`.
    Ascii,
    /// Box drawing, arrows, common symbols.
    #[default]
    Extended,
    /// Everything above plus emoji and quadrant blocks.
    Full,
}

/// The resolved capability set for one run.
#[derive(Debug, Clone)]
pub struct Capabilities {
    pub color: ColorDepth,
    pub graphics: GraphicsProtocol,
    pub hyperlinks: bool,
    pub unicode: UnicodeLevel,
    /// Terminal size in cells.
    pub cols: u16,
    pub rows: u16,
    /// Size of one cell in pixels, when the terminal told us.
    pub cell_px: Option<(u16, u16)>,
    /// Background colour, used to pick a light or dark theme.
    pub background: Option<Rgb>,
    /// Whether stdout is a terminal at all.
    pub is_tty: bool,
    /// Terminals that ignore SGR 3.
    pub italic_broken: bool,
    /// True when we are inside a multiplexer that may swallow passthrough codes.
    pub multiplexed: bool,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            color: ColorDepth::None,
            graphics: GraphicsProtocol::None,
            hyperlinks: false,
            unicode: UnicodeLevel::Ascii,
            cols: 80,
            rows: 24,
            cell_px: None,
            background: None,
            is_tty: false,
            italic_broken: false,
            multiplexed: false,
        }
    }
}

impl Capabilities {
    /// Detects capabilities from the environment, then refines them by probing.
    ///
    /// `probe` is skipped when stdout is not a terminal: there is nobody to
    /// answer, and writing query sequences into a pipe would corrupt output.
    pub fn detect(allow_probe: bool) -> Self {
        let mut caps = Self::from_env();
        if caps.is_tty && allow_probe {
            super::probe::refine(&mut caps);
        }
        caps
    }

    /// The environment-only tier. Deterministic and safe to unit test.
    pub fn from_env() -> Self {
        let is_tty = std::io::stdout().is_terminal();
        let term = env::var("TERM").unwrap_or_default();
        let term_program = env::var("TERM_PROGRAM").unwrap_or_default();
        let dumb = term == "dumb" || term.is_empty();
        let multiplexed = env::var_os("TMUX").is_some() || term.starts_with("screen");

        let color = Self::detect_color(is_tty, &term, &term_program, dumb);
        let (cols, rows) = terminal_size();

        let mut caps = Self {
            color,
            graphics: GraphicsProtocol::None,
            hyperlinks: is_tty && !dumb && Self::hyperlinks_from_env(&term, &term_program),
            unicode: Self::unicode_from_env(dumb),
            cols,
            rows,
            cell_px: cell_size_px(),
            background: None,
            is_tty,
            // Terminal.app draws SGR 3 as reverse video, which is worse than
            // an underline; linux console ignores it entirely.
            italic_broken: term_program == "Apple_Terminal" || term == "linux",
            multiplexed,
        };
        caps.graphics = Self::graphics_from_env(&caps, &term, &term_program);
        caps
    }

    fn detect_color(is_tty: bool, term: &str, term_program: &str, dumb: bool) -> ColorDepth {
        // https://no-color.org: any non-empty value disables colour.
        if env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            return ColorDepth::None;
        }
        let forced = env::var_os("CLICOLOR_FORCE").is_some_and(|v| v != "0")
            || env::var("FORCE_COLOR").is_ok_and(|v| v != "0" && v != "false");
        if !is_tty && !forced {
            return ColorDepth::None;
        }
        if dumb {
            return ColorDepth::None;
        }
        let colorterm = env::var("COLORTERM").unwrap_or_default();
        if colorterm == "truecolor" || colorterm == "24bit" {
            return ColorDepth::TrueColor;
        }
        // These emulators are truecolor-capable regardless of what TERM says,
        // which matters over ssh where TERM is often downgraded.
        const TRUECOLOR_PROGRAMS: &[&str] = &[
            "iTerm.app",
            "WezTerm",
            "ghostty",
            "vscode",
            "Hyper",
            "rio",
            "Tabby",
            "warp",
        ];
        if TRUECOLOR_PROGRAMS
            .iter()
            .any(|p| p.eq_ignore_ascii_case(term_program))
        {
            return ColorDepth::TrueColor;
        }
        if env::var_os("KITTY_WINDOW_ID").is_some() || term.contains("kitty") {
            return ColorDepth::TrueColor;
        }
        if term.contains("direct") {
            return ColorDepth::TrueColor;
        }
        if term.contains("256") {
            return ColorDepth::Ansi256;
        }
        if term_program == "Apple_Terminal" {
            return ColorDepth::Ansi256;
        }
        ColorDepth::Ansi16
    }

    fn graphics_from_env(caps: &Self, term: &str, term_program: &str) -> GraphicsProtocol {
        // Read the markers here and pass them down, so the decision itself is a
        // pure function that tests can drive without mutating process-wide
        // environment variables underneath each other.
        let kitty_marker = env::var_os("KITTY_WINDOW_ID").is_some()
            || env::var_os("GHOSTTY_RESOURCES_DIR").is_some();
        Self::choose_graphics(caps, term, term_program, kitty_marker)
    }

    fn choose_graphics(
        caps: &Self,
        term: &str,
        term_program: &str,
        kitty_marker: bool,
    ) -> GraphicsProtocol {
        if !caps.is_tty {
            return GraphicsProtocol::None;
        }
        // Inside tmux, graphics need passthrough that we cannot rely on; fall
        // back to blocks, which are just coloured text and always survive.
        if caps.multiplexed {
            return GraphicsProtocol::Blocks;
        }
        // `TERM_PROGRAM` first, because whichever terminal is actually running
        // sets it for itself. The marker variables below are weaker evidence:
        // they are exported into every child process and survive into places
        // their terminal did not follow, so a shell started under Ghostty still
        // carries GHOSTTY_RESOURCES_DIR even when something else is drawing the
        // screen. Letting those outrank an explicit TERM_PROGRAM sends kitty
        // escape codes to a terminal that has never heard of them.
        match term_program {
            "iTerm.app" => return GraphicsProtocol::ITerm2,
            "ghostty" | "Ghostty" => return GraphicsProtocol::Kitty,
            "WezTerm" => return GraphicsProtocol::Kitty,
            "Apple_Terminal" => return GraphicsProtocol::Blocks,
            _ => {}
        }

        if term.contains("kitty") || kitty_marker {
            return GraphicsProtocol::Kitty;
        }
        if term.contains("mlterm") || term.contains("yaft") || term.contains("foot") {
            return GraphicsProtocol::Sixel;
        }
        // Anything else gets blocks until a probe proves otherwise.
        if caps.color >= ColorDepth::Ansi256 {
            GraphicsProtocol::Blocks
        } else {
            GraphicsProtocol::None
        }
    }

    fn hyperlinks_from_env(term: &str, term_program: &str) -> bool {
        if term == "linux" {
            return false;
        }
        // VTE learned OSC 8 in 0.50.
        if let Ok(v) = env::var("VTE_VERSION") {
            if let Ok(n) = v.parse::<u32>() {
                return n >= 5000;
            }
        }
        const KNOWN: &[&str] = &[
            "iTerm.app",
            "WezTerm",
            "ghostty",
            "vscode",
            "Hyper",
            "rio",
            "kitty",
        ];
        KNOWN.iter().any(|p| p.eq_ignore_ascii_case(term_program))
            || term.contains("kitty")
            || term.contains("foot")
            || term.contains("alacritty")
            || env::var_os("WT_SESSION").is_some()
            || env::var_os("KITTY_WINDOW_ID").is_some()
    }

    fn unicode_from_env(dumb: bool) -> UnicodeLevel {
        if dumb {
            return UnicodeLevel::Ascii;
        }
        let utf8 = ["LC_ALL", "LC_CTYPE", "LANG"]
            .iter()
            .filter_map(|k| env::var(k).ok())
            .any(|v| {
                v.to_ascii_lowercase().contains("utf-8") || v.to_ascii_lowercase().contains("utf8")
            });
        // Windows terminals are UTF-8 capable without the POSIX locale vars.
        if utf8 || cfg!(windows) {
            UnicodeLevel::Full
        } else {
            UnicodeLevel::Ascii
        }
    }

    /// True when the detected background is dark (or unknown, which we treat as
    /// dark because that is the common case for terminals).
    pub fn prefers_dark(&self) -> bool {
        self.background
            .map(|b| b.luminance() < 0.35)
            .unwrap_or(true)
    }

    /// Pixel size of the usable text area, when known.
    pub fn cell_px_or_guess(&self) -> (u16, u16) {
        // A 1:2 cell is the usual shape; guessing keeps image aspect ratios
        // sane on terminals that will not report their metrics.
        self.cell_px.unwrap_or((8, 16))
    }
}

/// Terminal size in cells, honouring `COLUMNS`/`LINES` when set.
pub fn terminal_size() -> (u16, u16) {
    if let (Ok(c), Ok(r)) = (env::var("COLUMNS"), env::var("LINES")) {
        if let (Ok(c), Ok(r)) = (c.parse(), r.parse()) {
            return (c, r);
        }
    }
    crossterm::terminal::size().unwrap_or((80, 24))
}

/// Cell size in pixels derived from the kernel's window size, when available.
fn cell_size_px() -> Option<(u16, u16)> {
    let ws = crossterm::terminal::window_size().ok()?;
    if ws.width == 0 || ws.height == 0 || ws.columns == 0 || ws.rows == 0 {
        return None;
    }
    Some((ws.width / ws.columns, ws.height / ws.rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decides a protocol for a made-up terminal, with no environment involved.
    fn graphics_for(term: &str, term_program: &str, kitty_marker: bool) -> GraphicsProtocol {
        let caps = Capabilities {
            is_tty: true,
            color: ColorDepth::TrueColor,
            ..Default::default()
        };
        Capabilities::choose_graphics(&caps, term, term_program, kitty_marker)
    }

    #[test]
    fn term_program_outranks_the_marker_variables() {
        // Regression: KITTY_WINDOW_ID and GHOSTTY_RESOURCES_DIR are exported
        // into every child process, so a shell that started life under one
        // terminal keeps carrying them. TERM_PROGRAM says what is drawing now,
        // and letting a stale marker beat it sends kitty escape codes to a
        // terminal that has never heard of them.
        assert_eq!(
            graphics_for("xterm-256color", "iTerm.app", true),
            GraphicsProtocol::ITerm2
        );
        assert_eq!(
            graphics_for("xterm-256color", "Apple_Terminal", true),
            GraphicsProtocol::Blocks
        );
        // With nothing better to go on, the marker still counts.
        assert_eq!(
            graphics_for("xterm-256color", "", true),
            GraphicsProtocol::Kitty
        );
    }

    #[test]
    fn known_terminals_get_their_protocol() {
        assert_eq!(
            graphics_for("xterm-ghostty", "ghostty", false),
            GraphicsProtocol::Kitty
        );
        assert_eq!(
            graphics_for("xterm-kitty", "", false),
            GraphicsProtocol::Kitty
        );
        assert_eq!(
            graphics_for("wezterm", "WezTerm", false),
            GraphicsProtocol::Kitty
        );
        assert_eq!(graphics_for("foot", "", false), GraphicsProtocol::Sixel);
        assert_eq!(
            graphics_for("xterm-256color", "", false),
            GraphicsProtocol::Blocks
        );
    }

    #[test]
    fn a_multiplexer_falls_back_to_blocks() {
        let caps = Capabilities {
            is_tty: true,
            multiplexed: true,
            color: ColorDepth::TrueColor,
            ..Default::default()
        };
        assert_eq!(
            Capabilities::choose_graphics(&caps, "xterm-kitty", "ghostty", true),
            GraphicsProtocol::Blocks
        );
    }

    #[test]
    fn graphics_names_are_stable() {
        assert_eq!(GraphicsProtocol::Kitty.name(), "kitty");
        assert!(GraphicsProtocol::Sixel.is_pixel_exact());
        assert!(!GraphicsProtocol::Blocks.is_pixel_exact());
    }

    #[test]
    fn unknown_background_is_treated_as_dark() {
        let caps = Capabilities::default();
        assert!(caps.prefers_dark());
    }

    #[test]
    fn light_background_is_detected() {
        let caps = Capabilities {
            background: Some(Rgb(250, 250, 250)),
            ..Default::default()
        };
        assert!(!caps.prefers_dark());
    }

    #[test]
    fn color_depth_orders_by_fidelity() {
        assert!(ColorDepth::TrueColor > ColorDepth::Ansi256);
        assert!(ColorDepth::Ansi16 > ColorDepth::None);
    }
}
