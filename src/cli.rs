//! Command-line interface, and the settings resolution behind it.
//!
//! Three sources feed into one [`Settings`]: the flags, the config file, and
//! what we detected about the terminal. Flags win, then config, then detection.
//! Keeping that resolution in one place -- rather than checking `Option`s at each
//! use site -- is what keeps the rest of the crate free of "is this overridden?"
//! branches.

use std::path::PathBuf;

use clap::{Parser, ValueEnum};

use crate::config::Config;
use crate::markdown::ParseOptions;
use crate::render::{Glyphs, LinkMode, RenderOptions};
use crate::term::caps::{Capabilities, ColorDepth, GraphicsProtocol, UnicodeLevel};
use crate::theme::Theme;

/// A Markdown viewer for terminals that can do more than plain text.
#[derive(Debug, Parser)]
#[command(
    name = "termmd",
    version,
    about,
    long_about = None,
    after_help = "Reads from stdin when no file is given.\n\
                  Run `termmd --caps` to see what termmd detected about your terminal."
)]
pub struct Cli {
    /// Markdown files to view. Use `-` for stdin.
    #[arg(value_name = "FILE")]
    pub files: Vec<PathBuf>,

    /// Maximum text width in columns [default: terminal width, capped at 100].
    #[arg(short, long, value_name = "COLS")]
    pub width: Option<usize>,

    /// Left and right margin in columns.
    #[arg(long, value_name = "COLS")]
    pub margin: Option<usize>,

    /// Force the interactive pager, even when piping.
    #[arg(short, long, conflicts_with = "no_pager")]
    pub pager: bool,

    /// Print the document and exit, without the pager.
    #[arg(short = 'P', long)]
    pub no_pager: bool,

    /// Strip all styling and print plain text.
    #[arg(long)]
    pub plain: bool,

    /// Theme name (dark, light, mono) or path to a theme file.
    #[arg(short, long, value_name = "NAME|PATH")]
    pub theme: Option<String>,

    /// Syntax highlighting theme for code blocks.
    #[arg(long, value_name = "NAME")]
    pub syntax_theme: Option<String>,

    /// When to use colour.
    #[arg(long, value_name = "WHEN", default_value = "auto")]
    pub color: ColorChoice,

    /// Image protocol to use.
    #[arg(long, value_name = "PROTOCOL", default_value = "auto")]
    pub images: ImageChoice,

    /// Allow fetching images over http(s).
    #[arg(long)]
    pub remote_images: bool,

    /// Maximum height of an image, in terminal rows.
    #[arg(long, value_name = "ROWS")]
    pub max_image_rows: Option<u16>,

    /// How to present link destinations.
    #[arg(long, value_name = "MODE", default_value = "auto")]
    pub links: LinkChoice,

    /// Show line numbers beside code blocks.
    #[arg(short = 'n', long)]
    pub line_numbers: bool,

    /// Restrict drawing to ASCII characters.
    #[arg(long)]
    pub ascii: bool,

    /// Re-render when the file changes. Implies the pager.
    #[arg(long, conflicts_with = "no_pager")]
    pub watch: bool,

    /// Print the document's table of contents and exit.
    #[arg(long)]
    pub toc: bool,

    /// Print what termmd detected about this terminal and exit.
    #[arg(long)]
    pub caps: bool,

    /// List the available syntax highlighting themes and exit.
    #[arg(long)]
    pub list_themes: bool,

    /// List the languages that can be highlighted and exit.
    #[arg(long)]
    pub list_languages: bool,

    /// Use a specific config file, or `none` to ignore any config.
    #[arg(long, value_name = "PATH")]
    pub config: Option<String>,

    /// Do not query the terminal for its capabilities at startup.
    #[arg(long)]
    pub no_probe: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum ColorChoice {
    #[default]
    Auto,
    Always,
    Never,
    #[value(name = "16")]
    Ansi16,
    #[value(name = "256")]
    Ansi256,
    #[value(name = "truecolor", alias = "24bit")]
    TrueColor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum ImageChoice {
    #[default]
    Auto,
    Kitty,
    #[value(name = "iterm2")]
    ITerm2,
    Sixel,
    Blocks,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum LinkChoice {
    #[default]
    Auto,
    Hyperlink,
    Inline,
    Reference,
    Hide,
}

impl From<LinkChoice> for LinkMode {
    fn from(c: LinkChoice) -> Self {
        match c {
            LinkChoice::Auto => LinkMode::Auto,
            LinkChoice::Hyperlink => LinkMode::Hyperlink,
            LinkChoice::Inline => LinkMode::Inline,
            LinkChoice::Reference => LinkMode::Reference,
            LinkChoice::Hide => LinkMode::Hide,
        }
    }
}

/// Everything resolved: what to render, how, and where.
#[derive(Debug, Clone)]
pub struct Settings {
    pub caps: Capabilities,
    pub theme: Theme,
    pub render: RenderOptions,
    pub parse: ParseOptions,
    pub use_pager: bool,
    pub mouse: bool,
    /// Whether to emit escape sequences at all.
    ///
    /// Distinct from colour depth: a terminal with no colour still gets bold and
    /// underline, but a pipe gets neither. Redirected output should be text a
    /// script can read, which is why this is off unless stdout is a terminal or
    /// colour was explicitly forced.
    pub styled: bool,
    pub watch: bool,
    pub remote_images: bool,
    pub protocol: GraphicsProtocol,
    pub syntax_theme: String,
    /// The width the user asked for, if any.
    ///
    /// Kept separate from the resolved width so that a resize can recompute
    /// from the original intent: a terminal that is widened again should get
    /// its full width back, which it cannot if the only record of the request
    /// is the value derived from the previous size.
    pub requested_width: Option<usize>,
}

/// The terminal width we will not exceed even on a very wide window.
///
/// Long lines are hard to read; newspapers settled on something near this for
/// the same reason. Users who disagree can pass `--width`.
const COMFORTABLE_WIDTH: usize = 100;

impl Settings {
    /// Resolves flags, config and detected capabilities into one settings value.
    pub fn resolve(cli: &Cli, config: &Config) -> anyhow::Result<Self> {
        let mut caps = Capabilities::detect(!cli.no_probe && config.probe != Some(false));

        // --- colour -------------------------------------------------------
        caps.color = match cli.color {
            ColorChoice::Auto => match config.color.as_deref() {
                Some("never") => ColorDepth::None,
                Some("always") => caps.color.max(ColorDepth::Ansi256),
                Some("16") => ColorDepth::Ansi16,
                Some("256") => ColorDepth::Ansi256,
                Some("truecolor" | "24bit") => ColorDepth::TrueColor,
                _ => caps.color,
            },
            ColorChoice::Never => ColorDepth::None,
            ColorChoice::Always => caps.color.max(ColorDepth::Ansi256),
            ColorChoice::Ansi16 => ColorDepth::Ansi16,
            ColorChoice::Ansi256 => ColorDepth::Ansi256,
            ColorChoice::TrueColor => ColorDepth::TrueColor,
        };
        if cli.plain {
            caps.color = ColorDepth::None;
        }

        // --- unicode ------------------------------------------------------
        if cli.ascii || config.unicode.as_deref() == Some("ascii") {
            caps.unicode = UnicodeLevel::Ascii;
        } else {
            match config.unicode.as_deref() {
                Some("extended") => caps.unicode = UnicodeLevel::Extended,
                Some("full") => caps.unicode = UnicodeLevel::Full,
                _ => {}
            }
        }

        // --- images -------------------------------------------------------
        let protocol = match cli.images {
            ImageChoice::Auto => match config.image_protocol.as_deref() {
                Some("kitty") => GraphicsProtocol::Kitty,
                Some("iterm2") => GraphicsProtocol::ITerm2,
                Some("sixel") => GraphicsProtocol::Sixel,
                Some("blocks") => GraphicsProtocol::Blocks,
                Some("none") => GraphicsProtocol::None,
                _ if config.images == Some(false) => GraphicsProtocol::None,
                _ => caps.graphics,
            },
            ImageChoice::Kitty => GraphicsProtocol::Kitty,
            ImageChoice::ITerm2 => GraphicsProtocol::ITerm2,
            ImageChoice::Sixel => GraphicsProtocol::Sixel,
            ImageChoice::Blocks => GraphicsProtocol::Blocks,
            ImageChoice::None => GraphicsProtocol::None,
        };
        let protocol = if cli.plain {
            GraphicsProtocol::None
        } else {
            protocol
        };
        caps.graphics = protocol;

        // --- theme --------------------------------------------------------
        let theme_name = cli
            .theme
            .clone()
            .or_else(|| {
                // A background-specific theme only applies when we know the
                // background, which needs a successful probe.
                if caps.prefers_dark() {
                    config.dark_theme.clone()
                } else {
                    config.light_theme.clone()
                }
            })
            .or_else(|| config.theme.clone())
            .unwrap_or_else(|| {
                if caps.prefers_dark() {
                    "dark".into()
                } else {
                    "light".into()
                }
            });

        let mut theme = load_theme(&theme_name)?;
        if caps.color == ColorDepth::None {
            theme = Theme::mono();
        }
        let syntax_theme = cli
            .syntax_theme
            .clone()
            .or_else(|| config.syntax_theme.clone())
            .unwrap_or_else(|| {
                if caps.color == ColorDepth::Ansi16 {
                    // A 16-colour terminal is better served by a theme built for
                    // the user's own palette than by squashed RGB.
                    "ansi".to_string()
                } else {
                    theme.syntax_theme.clone()
                }
            });

        // --- geometry -----------------------------------------------------
        let terminal_width = caps.cols as usize;
        let requested_width = cli.width.or(config.width);
        let width = requested_width
            .unwrap_or_else(|| terminal_width.min(COMFORTABLE_WIDTH))
            .clamp(8, 4096.max(terminal_width));
        let margin = cli.margin.or(config.margin).unwrap_or(1).min(width / 4);

        let links = if cli.links == LinkChoice::Auto {
            match config.links.as_deref() {
                Some("hyperlink") => LinkMode::Hyperlink,
                Some("inline") => LinkMode::Inline,
                Some("reference") => LinkMode::Reference,
                Some("hide") => LinkMode::Hide,
                _ => LinkMode::Auto,
            }
        } else {
            cli.links.into()
        };

        let render = RenderOptions {
            width,
            margin,
            links,
            images: protocol != GraphicsProtocol::None,
            max_image_rows: cli
                .max_image_rows
                .or(config.max_image_rows)
                .unwrap_or(24)
                .max(1),
            line_numbers: cli.line_numbers || config.line_numbers.unwrap_or(false),
            center_figures: config.center_figures.unwrap_or(true),
            tab_width: config.tab_width.unwrap_or(4).clamp(1, 16),
            glyphs: Glyphs::for_level(caps.unicode),
            heading_rules: config.heading_rules.unwrap_or(true),
        };

        let parse = ParseOptions {
            smart_punctuation: config.smart_punctuation.unwrap_or(true),
            ..Default::default()
        };

        // --- output mode --------------------------------------------------
        let use_pager = if cli.pager {
            true
        } else if cli.no_pager || cli.plain || cli.toc {
            false
        } else if cli.watch {
            // Watching without the pager would print once and then have nowhere
            // to put the updates.
            caps.is_tty
        } else {
            caps.is_tty && config.pager.unwrap_or(true)
        };

        // Forcing colour or a specific image protocol also forces styling, so
        // that `termmd --color=always doc.md | less -R` keeps its colour and
        // `termmd --images=kitty doc.md > frame` keeps its pictures. Left on
        // `auto`, a redirect still gets plain text.
        let images_forced = !matches!(cli.images, ImageChoice::Auto | ImageChoice::None);
        let styled = !cli.plain && (caps.is_tty || caps.color != ColorDepth::None || images_forced);

        Ok(Self {
            caps,
            theme,
            render,
            parse,
            use_pager,
            mouse: config.mouse.unwrap_or(false),
            styled,
            watch: cli.watch,
            remote_images: cli.remote_images || config.remote_images.unwrap_or(false),
            protocol,
            syntax_theme,
            requested_width,
        })
    }
}

/// Resolves a theme by built-in name, config-directory name, or path.
fn load_theme(name: &str) -> anyhow::Result<Theme> {
    if let Some(theme) = Theme::builtin(name) {
        return Ok(theme);
    }
    let path = PathBuf::from(name);
    if path.exists() {
        return Theme::load(&path);
    }
    if let Some(dir) = crate::config::theme_dir_for_lookup() {
        let candidate = dir.join(format!("{name}.toml"));
        if candidate.exists() {
            return Theme::load(&candidate);
        }
    }
    anyhow::bail!(
        "unknown theme {name:?}; built-in themes are {:?}, or give a path to a theme file",
        Theme::builtin_names()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn cli(args: &[&str]) -> Cli {
        Cli::parse_from(std::iter::once("termmd").chain(args.iter().copied()))
    }

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_files_and_flags() {
        let c = cli(&["a.md", "b.md", "--width", "72", "-n"]);
        assert_eq!(c.files.len(), 2);
        assert_eq!(c.width, Some(72));
        assert!(c.line_numbers);
    }

    #[test]
    fn flags_beat_config() {
        let config = Config {
            width: Some(50),
            ..Default::default()
        };
        let s = Settings::resolve(&cli(&["--width", "72"]), &config).unwrap();
        assert_eq!(s.render.width, 72);
    }

    #[test]
    fn config_beats_defaults() {
        let config = Config {
            width: Some(50),
            margin: Some(3),
            ..Default::default()
        };
        let s = Settings::resolve(&cli(&[]), &config).unwrap();
        assert_eq!(s.render.width, 50);
        assert_eq!(s.render.margin, 3);
    }

    #[test]
    fn plain_disables_colour_and_images() {
        let s = Settings::resolve(&cli(&["--plain"]), &Config::default()).unwrap();
        assert_eq!(s.caps.color, ColorDepth::None);
        assert_eq!(s.protocol, GraphicsProtocol::None);
        assert!(!s.render.images);
        assert!(!s.use_pager);
        assert!(!s.styled);
    }

    #[test]
    fn forcing_an_image_protocol_keeps_images_through_a_pipe() {
        let s = Settings::resolve(&cli(&["--images", "kitty"]), &Config::default()).unwrap();
        assert!(s.styled, "an explicit protocol should survive a redirect");
        assert!(s.render.images);

        // `--images none` is a request for fewer escapes, not more.
        let s = Settings::resolve(&cli(&["--images", "none"]), &Config::default()).unwrap();
        assert!(!s.styled);
    }

    #[test]
    fn redirected_output_is_unstyled_unless_colour_is_forced() {
        // The test process has no terminal on stdout, which is exactly the case
        // this rule exists for.
        let s = Settings::resolve(&cli(&[]), &Config::default()).unwrap();
        assert!(!s.caps.is_tty, "test precondition");
        assert!(!s.styled, "a pipe should get plain text");

        let s = Settings::resolve(&cli(&["--color", "always"]), &Config::default()).unwrap();
        assert!(s.styled, "forcing colour should re-enable styling");
    }

    #[test]
    fn colour_can_be_forced_and_disabled() {
        assert_eq!(
            Settings::resolve(&cli(&["--color", "never"]), &Config::default())
                .unwrap()
                .caps
                .color,
            ColorDepth::None
        );
        assert_eq!(
            Settings::resolve(&cli(&["--color", "256"]), &Config::default())
                .unwrap()
                .caps
                .color,
            ColorDepth::Ansi256
        );
    }

    #[test]
    fn image_protocol_can_be_forced() {
        let s = Settings::resolve(&cli(&["--images", "sixel"]), &Config::default()).unwrap();
        assert_eq!(s.protocol, GraphicsProtocol::Sixel);
        assert!(s.render.images);

        let s = Settings::resolve(&cli(&["--images", "none"]), &Config::default()).unwrap();
        assert!(!s.render.images);
    }

    #[test]
    fn ascii_flag_downgrades_glyphs() {
        let s = Settings::resolve(&cli(&["--ascii"]), &Config::default()).unwrap();
        assert_eq!(s.caps.unicode, UnicodeLevel::Ascii);
        assert_eq!(s.render.glyphs.quote_bar, "|");
    }

    #[test]
    fn width_is_clamped_to_something_sane() {
        let s = Settings::resolve(&cli(&["--width", "1"]), &Config::default()).unwrap();
        assert!(s.render.width >= 8, "unusably narrow widths are clamped");
    }

    #[test]
    fn margin_never_eats_the_page() {
        let s = Settings::resolve(
            &cli(&["--width", "20", "--margin", "40"]),
            &Config::default(),
        )
        .unwrap();
        assert!(s.render.margin * 2 < s.render.width);
    }

    #[test]
    fn unknown_theme_is_a_helpful_error() {
        let err =
            Settings::resolve(&cli(&["--theme", "neon-dreams"]), &Config::default()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("neon-dreams"), "{msg}");
        assert!(msg.contains("dark"), "should list the built-ins: {msg}");
    }

    #[test]
    fn watch_turns_the_pager_on_when_there_is_a_terminal() {
        let config = Config {
            pager: Some(false),
            ..Default::default()
        };
        let s = Settings::resolve(&cli(&["--watch"]), &config).unwrap();
        // No terminal in the test process, so the pager stays off; what matters
        // is that the config's `pager = false` is no longer the deciding factor.
        assert_eq!(s.use_pager, s.caps.is_tty);
        assert!(s.watch);
    }

    #[test]
    fn an_explicit_width_is_remembered_separately() {
        let s = Settings::resolve(&cli(&["--width", "72"]), &Config::default()).unwrap();
        assert_eq!(s.requested_width, Some(72));

        let s = Settings::resolve(&cli(&[]), &Config::default()).unwrap();
        assert_eq!(s.requested_width, None, "an unset width must stay unset");
    }

    #[test]
    fn link_mode_is_resolved_from_either_source() {
        let s = Settings::resolve(&cli(&["--links", "reference"]), &Config::default()).unwrap();
        assert_eq!(s.render.links, LinkMode::Reference);

        let config = Config {
            links: Some("hide".into()),
            ..Default::default()
        };
        let s = Settings::resolve(&cli(&[]), &config).unwrap();
        assert_eq!(s.render.links, LinkMode::Hide);
    }
}
