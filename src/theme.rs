//! Named style roles, and the built-in light and dark themes.
//!
//! Every visual decision the renderer makes goes through a role on [`Theme`]
//! rather than a literal colour, which is what makes the whole appearance
//! swappable from a config file, and what lets the same document render sensibly
//! against a white background without a second code path.
//!
//! Themes are authored in truecolor. Degrading to 256 or 16 colours happens once,
//! at output time, in [`crate::term::style::StyleWriter`].

use std::path::Path;

use serde::Deserialize;

use crate::markdown::AlertKind;
use crate::term::style::{Color, Rgb, Style};

/// A complete visual specification.
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub name: String,
    pub dark: bool,

    /// Body text. `Color::Default` keeps the user's own foreground.
    pub text: Style,
    /// Secondary text: captions, footnote bodies, HTML remnants.
    pub muted: Style,

    /// Heading styles, indexed by level minus one.
    pub headings: [Style; 6],
    /// Drawn under a level-1 heading when the terminal has box characters.
    pub heading_rule: Style,

    pub emphasis: Style,
    pub strong: Style,
    pub strikethrough: Style,
    pub inline_code: Style,

    pub link_text: Style,
    /// The URL shown beside a link when hyperlinks are unavailable.
    pub link_url: Style,

    pub quote_bar: Style,
    pub quote_text: Style,

    pub bullet: Style,
    pub number: Style,
    pub task_done: Style,
    pub task_todo: Style,
    /// Applied to the text of a completed task.
    pub task_done_text: Style,

    pub table_border: Style,
    pub table_header: Style,
    /// Optional zebra striping for alternate rows.
    pub table_stripe: Option<Style>,

    pub rule: Style,
    pub code_block_bg: Option<Rgb>,
    /// Line numbers in code blocks.
    pub code_gutter: Style,
    /// The syntect theme used for code, one for each background.
    pub syntax_theme: String,

    pub footnote_ref: Style,
    pub caption: Style,
    pub math: Style,

    /// One style per alert kind: note, tip, important, warning, caution.
    pub alerts: [Style; 5],

    // Pager chrome.
    pub status_bar: Style,
    pub status_key: Style,
    pub search_match: Style,
    pub search_current: Style,
}

/// A small, deliberately flat palette. Themes are built from these so the
/// relationships between roles stay consistent when one colour changes.
struct Palette {
    fg: Rgb,
    muted: Rgb,
    faint: Rgb,
    blue: Rgb,
    cyan: Rgb,
    green: Rgb,
    yellow: Rgb,
    orange: Rgb,
    red: Rgb,
    magenta: Rgb,
    surface: Rgb,
    line: Rgb,
}

const DARK: Palette = Palette {
    fg: Rgb(0xdc, 0xe3, 0xea),
    muted: Rgb(0x8a, 0x94, 0xa3),
    faint: Rgb(0x5b, 0x64, 0x72),
    blue: Rgb(0x6f, 0xb3, 0xff),
    cyan: Rgb(0x4f, 0xd0, 0xdd),
    green: Rgb(0x7b, 0xdc, 0x8a),
    yellow: Rgb(0xf0, 0xc9, 0x5c),
    orange: Rgb(0xff, 0xa2, 0x5c),
    red: Rgb(0xff, 0x7a, 0x70),
    magenta: Rgb(0xcf, 0xa4, 0xff),
    surface: Rgb(0x1b, 0x1f, 0x27),
    line: Rgb(0x3a, 0x42, 0x50),
};

const LIGHT: Palette = Palette {
    fg: Rgb(0x1f, 0x24, 0x30),
    muted: Rgb(0x5c, 0x66, 0x73),
    faint: Rgb(0x8d, 0x96, 0xa3),
    blue: Rgb(0x1a, 0x63, 0xc9),
    cyan: Rgb(0x0e, 0x7f, 0x8c),
    green: Rgb(0x1f, 0x7a, 0x3d),
    yellow: Rgb(0x8a, 0x6d, 0x00),
    orange: Rgb(0xa8, 0x57, 0x00),
    red: Rgb(0xc0, 0x34, 0x2b),
    magenta: Rgb(0x7a, 0x3f, 0xb5),
    surface: Rgb(0xf2, 0xf3, 0xf5),
    line: Rgb(0xd3, 0xd7, 0xdd),
};

impl Theme {
    /// The default dark theme.
    pub fn dark() -> Self {
        Self::from_palette("dark", true, &DARK)
    }

    /// The default light theme.
    pub fn light() -> Self {
        Self::from_palette("light", false, &LIGHT)
    }

    /// Picks a built-in theme by name, or `None` if there is no such theme.
    pub fn builtin(name: &str) -> Option<Self> {
        match name {
            "dark" => Some(Self::dark()),
            "light" => Some(Self::light()),
            "mono" => Some(Self::mono()),
            _ => None,
        }
    }

    pub fn builtin_names() -> &'static [&'static str] {
        &["dark", "light", "mono"]
    }

    /// Attributes only: for terminals or users that want no colour at all, while
    /// keeping the structural cues that bold and underline provide.
    pub fn mono() -> Self {
        let plain = Style::PLAIN;
        let mut t = Self::from_palette("mono", true, &DARK);
        t.text = plain;
        t.muted = plain.dim();
        t.headings = [
            plain.bold().underline(),
            plain.bold(),
            plain.bold(),
            plain.italic(),
            plain.italic(),
            plain.dim().italic(),
        ];
        t.heading_rule = plain.dim();
        t.emphasis = plain.italic();
        t.strong = plain.bold();
        t.strikethrough = plain.strike();
        t.inline_code = plain.reverse();
        t.link_text = plain.underline();
        t.link_url = plain.dim();
        t.quote_bar = plain.dim();
        t.quote_text = plain.italic();
        t.bullet = plain.bold();
        t.number = plain.bold();
        t.task_done = plain.bold();
        t.task_todo = plain;
        t.task_done_text = plain.dim().strike();
        t.table_border = plain.dim();
        t.table_header = plain.bold();
        t.table_stripe = None;
        t.rule = plain.dim();
        t.code_block_bg = None;
        t.code_gutter = plain.dim();
        t.syntax_theme = "ansi".into();
        t.footnote_ref = plain.bold();
        t.caption = plain.italic().dim();
        t.math = plain.italic();
        t.alerts = [plain.bold(); 5];
        t.status_bar = plain.reverse();
        t.status_key = plain.reverse().bold();
        t.search_match = plain.reverse();
        t.search_current = plain.reverse().bold();
        t
    }

    fn from_palette(name: &str, dark: bool, p: &Palette) -> Self {
        let c = Color::Rgb;
        Self {
            name: name.to_string(),
            dark,
            text: Style::PLAIN,
            muted: Style::PLAIN.fg(c(p.muted)),
            // Headings step down in weight rather than cycling hues at random:
            // the first two carry the accent, the rest recede.
            headings: [
                Style::PLAIN.fg(c(p.blue)).bold(),
                Style::PLAIN.fg(c(p.magenta)).bold(),
                Style::PLAIN.fg(c(p.cyan)).bold(),
                Style::PLAIN.fg(c(p.green)).bold(),
                Style::PLAIN.fg(c(p.muted)).bold(),
                Style::PLAIN.fg(c(p.muted)).italic(),
            ],
            heading_rule: Style::PLAIN.fg(c(p.line)),
            emphasis: Style::PLAIN.italic(),
            strong: Style::PLAIN.bold(),
            strikethrough: Style::PLAIN.strike().fg(c(p.muted)),
            inline_code: Style::PLAIN.fg(c(p.orange)).bg(c(p.surface)),
            link_text: Style::PLAIN.fg(c(p.blue)).underline(),
            link_url: Style::PLAIN.fg(c(p.faint)),
            quote_bar: Style::PLAIN.fg(c(p.faint)),
            quote_text: Style::PLAIN.fg(c(p.muted)).italic(),
            bullet: Style::PLAIN.fg(c(p.blue)),
            number: Style::PLAIN.fg(c(p.blue)),
            task_done: Style::PLAIN.fg(c(p.green)),
            task_todo: Style::PLAIN.fg(c(p.muted)),
            task_done_text: Style::PLAIN.fg(c(p.muted)),
            table_border: Style::PLAIN.fg(c(p.line)),
            table_header: Style::PLAIN.bold().fg(c(p.fg)),
            table_stripe: None,
            rule: Style::PLAIN.fg(c(p.line)),
            code_block_bg: Some(p.surface),
            code_gutter: Style::PLAIN.fg(c(p.faint)),
            syntax_theme: if dark {
                "base16-ocean.dark".into()
            } else {
                "InspiredGitHub".into()
            },
            footnote_ref: Style::PLAIN.fg(c(p.cyan)),
            caption: Style::PLAIN.fg(c(p.muted)).italic(),
            math: Style::PLAIN.fg(c(p.cyan)),
            alerts: [
                Style::PLAIN.fg(c(p.blue)).bold(),
                Style::PLAIN.fg(c(p.green)).bold(),
                Style::PLAIN.fg(c(p.magenta)).bold(),
                Style::PLAIN.fg(c(p.yellow)).bold(),
                Style::PLAIN.fg(c(p.red)).bold(),
            ],
            status_bar: Style::PLAIN.fg(c(p.fg)).bg(c(p.surface)),
            status_key: Style::PLAIN.fg(c(p.blue)).bg(c(p.surface)).bold(),
            search_match: Style::PLAIN.fg(c(p.surface)).bg(c(p.yellow)),
            search_current: Style::PLAIN.fg(c(p.surface)).bg(c(p.orange)).bold(),
        }
    }

    /// The style for an alert kind.
    pub fn alert(&self, kind: AlertKind) -> Style {
        let i = match kind {
            AlertKind::Note => 0,
            AlertKind::Tip => 1,
            AlertKind::Important => 2,
            AlertKind::Warning => 3,
            AlertKind::Caution => 4,
        };
        self.alerts[i]
    }

    /// The style for a heading level, clamping out-of-range levels.
    pub fn heading(&self, level: u8) -> Style {
        self.headings[(level.clamp(1, 6) - 1) as usize]
    }

    /// Loads a theme file, starting from a built-in base.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("reading theme {}: {e}", path.display()))?;
        Self::from_toml(&text)
    }

    /// Parses a theme from TOML, layering it over the base it names.
    ///
    /// Keys may be written with hyphens or underscores; both reach the same
    /// role. An unrecognised key is an error, because a theme that silently
    /// ignores a misspelled role just looks broken.
    pub fn from_toml(text: &str) -> anyhow::Result<Self> {
        let file: ThemeFile = toml::from_str(text)?;
        let mut theme = match file.base.as_deref() {
            Some(b) => Self::builtin(b).ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown base theme {b:?}; try one of {:?}",
                    Self::builtin_names()
                )
            })?,
            None => Self::dark(),
        };
        if let Some(name) = file.name {
            theme.name = name;
        }
        if let Some(dark) = file.dark {
            theme.dark = dark;
        }
        if let Some(s) = file.syntax_theme {
            theme.syntax_theme = s;
        }
        if let Some(bg) = file.code_block_bg {
            theme.code_block_bg = if bg.eq_ignore_ascii_case("none") {
                None
            } else {
                Some(Rgb::parse(&bg).ok_or_else(|| anyhow::anyhow!("bad colour {bg:?}"))?)
            };
        }

        for (key, spec) in &file.styles {
            let role = key.replace('-', "_");
            theme.set_role(&role, spec).map_err(|e| {
                anyhow::anyhow!("{e} (in key {key:?}; known roles are {})", ROLES.join(", "))
            })?;
        }
        Ok(theme)
    }

    /// Applies one `role = "style"` entry.
    fn set_role(&mut self, role: &str, spec: &str) -> anyhow::Result<()> {
        // `table_stripe` is the one role that can be switched off entirely.
        if role == "table_stripe" {
            self.table_stripe = if spec.eq_ignore_ascii_case("none") {
                None
            } else {
                Some(parse_style(spec)?)
            };
            return Ok(());
        }
        if let Some(index) = ["h1", "h2", "h3", "h4", "h5", "h6"]
            .iter()
            .position(|h| *h == role)
        {
            self.headings[index] = parse_style(spec)?;
            return Ok(());
        }
        if let Some(index) = ["note", "tip", "important", "warning", "caution"]
            .iter()
            .position(|a| *a == role)
        {
            self.alerts[index] = parse_style(spec)?;
            return Ok(());
        }

        let target: &mut Style = match role {
            "text" => &mut self.text,
            "muted" => &mut self.muted,
            "emphasis" => &mut self.emphasis,
            "strong" => &mut self.strong,
            "strikethrough" => &mut self.strikethrough,
            "inline_code" => &mut self.inline_code,
            "link_text" => &mut self.link_text,
            "link_url" => &mut self.link_url,
            "quote_bar" => &mut self.quote_bar,
            "quote_text" => &mut self.quote_text,
            "bullet" => &mut self.bullet,
            "number" => &mut self.number,
            "task_done" => &mut self.task_done,
            "task_todo" => &mut self.task_todo,
            "task_done_text" => &mut self.task_done_text,
            "table_border" => &mut self.table_border,
            "table_header" => &mut self.table_header,
            "rule" => &mut self.rule,
            "code_gutter" => &mut self.code_gutter,
            "footnote_ref" => &mut self.footnote_ref,
            "caption" => &mut self.caption,
            "math" => &mut self.math,
            "heading_rule" => &mut self.heading_rule,
            "status_bar" => &mut self.status_bar,
            "status_key" => &mut self.status_key,
            "search_match" => &mut self.search_match,
            "search_current" => &mut self.search_current,
            other => anyhow::bail!("unknown theme role {other:?}"),
        };
        *target = parse_style(spec)?;
        Ok(())
    }
}

/// Every role a theme file may set, for error messages.
const ROLES: &[&str] = &[
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "heading_rule",
    "text",
    "muted",
    "emphasis",
    "strong",
    "strikethrough",
    "inline_code",
    "link_text",
    "link_url",
    "quote_bar",
    "quote_text",
    "bullet",
    "number",
    "task_done",
    "task_todo",
    "task_done_text",
    "table_border",
    "table_header",
    "table_stripe",
    "rule",
    "code_gutter",
    "footnote_ref",
    "caption",
    "math",
    "note",
    "tip",
    "important",
    "warning",
    "caution",
    "status_bar",
    "status_key",
    "search_match",
    "search_current",
];

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct ThemeFile {
    name: Option<String>,
    base: Option<String>,
    dark: Option<bool>,
    #[serde(alias = "syntax_theme")]
    syntax_theme: Option<String>,
    #[serde(alias = "code_block_bg")]
    code_block_bg: Option<String>,
    /// Everything else is a role; validated in `from_toml`.
    #[serde(default, flatten)]
    styles: std::collections::BTreeMap<String, String>,
}

/// Parses a style spec: a colour and any number of attributes, in any order.
///
/// ```text
/// "#6fb3ff bold"          foreground and an attribute
/// "bold underline"        attributes only, inheriting the colour
/// "#fff on #333 italic"   explicit background
/// "none"                  no styling at all
/// ```
pub fn parse_style(spec: &str) -> anyhow::Result<Style> {
    let mut style = Style::PLAIN;
    let mut tokens = spec.split_whitespace().peekable();
    while let Some(tok) = tokens.next() {
        match tok.to_ascii_lowercase().as_str() {
            "none" | "default" => {}
            "bold" => style = style.bold(),
            "dim" | "faint" => style = style.dim(),
            "italic" => style = style.italic(),
            "underline" => style = style.underline(),
            "strike" | "strikethrough" => style = style.strike(),
            "reverse" | "inverse" => style = style.reverse(),
            "on" => {
                let next = tokens.next().ok_or_else(|| {
                    anyhow::anyhow!("`on` must be followed by a colour in {spec:?}")
                })?;
                style = style.bg(parse_color(next)?);
            }
            _ => style = style.fg(parse_color(tok)?),
        }
    }
    Ok(style)
}

fn parse_color(tok: &str) -> anyhow::Result<Color> {
    if let Some(rgb) = Rgb::parse(tok) {
        return Ok(Color::Rgb(rgb));
    }
    if let Some(idx) = tok.strip_prefix('@') {
        // @0..@255 addresses the user's own palette rather than a fixed colour.
        return idx
            .parse::<u8>()
            .map(Color::Indexed)
            .map_err(|_| anyhow::anyhow!("bad palette index {tok:?}"));
    }
    const NAMED: &[(&str, u8)] = &[
        ("black", 0),
        ("red", 1),
        ("green", 2),
        ("yellow", 3),
        ("blue", 4),
        ("magenta", 5),
        ("cyan", 6),
        ("white", 7),
        ("bright-black", 8),
        ("gray", 8),
        ("grey", 8),
        ("bright-red", 9),
        ("bright-green", 10),
        ("bright-yellow", 11),
        ("bright-blue", 12),
        ("bright-magenta", 13),
        ("bright-cyan", 14),
        ("bright-white", 15),
    ];
    NAMED
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(tok))
        .map(|(_, i)| Color::Indexed(*i))
        .ok_or_else(|| anyhow::anyhow!("unknown colour {tok:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_exist_and_differ() {
        let d = Theme::dark();
        let l = Theme::light();
        assert!(d.dark && !l.dark);
        assert_ne!(d.headings[0], l.headings[0]);
        for name in Theme::builtin_names() {
            assert!(
                Theme::builtin(name).is_some(),
                "{name} should be a built-in"
            );
        }
        assert!(Theme::builtin("nope").is_none());
    }

    #[test]
    fn parses_style_specs() {
        assert_eq!(parse_style("bold").unwrap(), Style::PLAIN.bold());
        assert_eq!(
            parse_style("#ff8800").unwrap(),
            Style::PLAIN.fg(Color::rgb(255, 136, 0))
        );
        assert_eq!(
            parse_style("#fff on #000 italic").unwrap(),
            Style::PLAIN
                .fg(Color::rgb(255, 255, 255))
                .bg(Color::rgb(0, 0, 0))
                .italic()
        );
        assert_eq!(
            parse_style("red").unwrap(),
            Style::PLAIN.fg(Color::Indexed(1))
        );
        assert_eq!(
            parse_style("@42").unwrap(),
            Style::PLAIN.fg(Color::Indexed(42))
        );
        assert_eq!(parse_style("none").unwrap(), Style::PLAIN);
    }

    #[test]
    fn rejects_nonsense_specs() {
        assert!(parse_style("chartreuse").is_err());
        assert!(parse_style("bold on").is_err());
    }

    #[test]
    fn theme_files_layer_over_a_base() {
        let t = Theme::from_toml(
            r##"
            name = "custom"
            base = "light"
            h1 = "#ff0000 bold underline"
            inline_code = "bold"
            table_stripe = "none"
            "##,
        )
        .unwrap();
        assert_eq!(t.name, "custom");
        assert!(!t.dark, "should inherit light from the base");
        assert_eq!(
            t.headings[0],
            Style::PLAIN.fg(Color::rgb(255, 0, 0)).bold().underline()
        );
        assert_eq!(t.inline_code, Style::PLAIN.bold());
        // Untouched roles keep the base values.
        assert_eq!(t.headings[1], Theme::light().headings[1]);
    }

    #[test]
    fn unknown_base_is_an_error() {
        assert!(Theme::from_toml("base = \"neon\"").is_err());
    }

    #[test]
    fn unknown_roles_are_rejected_with_a_hint() {
        let err = Theme::from_toml("headline = \"bold\"")
            .unwrap_err()
            .to_string();
        assert!(err.contains("headline"), "{err}");
        assert!(
            err.contains("h1"),
            "should list the roles that do exist: {err}"
        );
    }

    #[test]
    fn roles_accept_hyphens_or_underscores() {
        let a = Theme::from_toml("link-text = \"bold\"").unwrap();
        let b = Theme::from_toml("link_text = \"bold\"").unwrap();
        assert_eq!(a.link_text, Style::PLAIN.bold());
        assert_eq!(a.link_text, b.link_text);
    }

    #[test]
    fn syntax_theme_accepts_either_spelling() {
        for text in ["syntax-theme = \"Nord\"", "syntax_theme = \"Nord\""] {
            assert_eq!(Theme::from_toml(text).unwrap().syntax_theme, "Nord");
        }
    }

    #[test]
    fn a_bad_style_spec_names_the_key() {
        let err = Theme::from_toml("h1 = \"chartreuse\"")
            .unwrap_err()
            .to_string();
        assert!(err.contains("h1"), "{err}");
    }

    #[test]
    fn the_shipped_example_theme_is_valid() {
        // Keeps the documented example honest: if a role is renamed, this fails.
        let text = include_str!("../doc/themes/example.toml");
        let theme = Theme::from_toml(text).expect("doc/themes/example.toml should parse");
        assert_eq!(theme.name, "example");
    }

    #[test]
    fn alert_and_heading_lookups_are_in_range() {
        let t = Theme::dark();
        assert_eq!(t.heading(0), t.headings[0], "level 0 clamps to h1");
        assert_eq!(t.heading(9), t.headings[5], "level 9 clamps to h6");
        assert_eq!(t.alert(AlertKind::Caution), t.alerts[4]);
    }
}
