//! Syntax highlighting for fenced code blocks, via `syntect`.
//!
//! Loading Sublime grammars is not cheap, so a [`Highlighter`] is built once per
//! process and shared. Language lookup tries the fence's info string as a name
//! and as an extension, then falls back to sniffing the first line -- which is
//! what rescues the `#!/bin/sh` blocks that carry no fence language at all.

use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Style as SynStyle, Theme as SynTheme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

use crate::term::style::{Color, Rgb, Style};

/// Owns the grammar and theme sets.
pub struct Highlighter {
    syntaxes: SyntaxSet,
    themes: ThemeSet,
    theme_name: String,
}

impl std::fmt::Debug for Highlighter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Highlighter")
            .field("theme", &self.theme_name)
            .finish_non_exhaustive()
    }
}

impl Highlighter {
    /// Builds a highlighter using the extended grammar set.
    pub fn new(theme_name: &str) -> Self {
        let syntaxes = two_face::syntax::extra_newlines();
        let themes = two_face::theme::extra().into();
        let mut hl = Self {
            syntaxes,
            themes,
            theme_name: String::new(),
        };
        hl.theme_name = hl.resolve_theme_name(theme_name);
        hl
    }

    /// A highlighter with no grammars, for tests and `--no-highlight`.
    pub fn plain() -> Self {
        Self {
            syntaxes: SyntaxSet::new(),
            themes: ThemeSet::new(),
            theme_name: String::new(),
        }
    }

    /// The theme actually in use, which may differ from the one requested.
    pub fn theme_name(&self) -> &str {
        &self.theme_name
    }

    pub fn theme_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.themes.themes.keys().cloned().collect();
        v.sort();
        v
    }

    pub fn language_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .syntaxes
            .syntaxes()
            .iter()
            .map(|s| s.name.clone())
            .collect();
        v.sort();
        v.dedup();
        v
    }

    fn resolve_theme_name(&self, requested: &str) -> String {
        if self.themes.themes.contains_key(requested) {
            return requested.to_string();
        }
        // Case-insensitive second chance before giving up on the request.
        if let Some(k) = self
            .themes
            .themes
            .keys()
            .find(|k| k.eq_ignore_ascii_case(requested))
        {
            return k.clone();
        }
        for fallback in ["base16-ocean.dark", "Monokai Extended", "InspiredGitHub"] {
            if self.themes.themes.contains_key(fallback) {
                return fallback.to_string();
            }
        }
        self.themes
            .themes
            .keys()
            .next()
            .cloned()
            .unwrap_or_default()
    }

    fn theme(&self) -> Option<&SynTheme> {
        self.themes.themes.get(&self.theme_name)
    }

    /// The background colour the syntax theme wants, if it has one.
    pub fn background(&self) -> Option<Rgb> {
        let bg = self.theme()?.settings.background?;
        (bg.a > 1).then_some(Rgb(bg.r, bg.g, bg.b))
    }

    /// Finds a grammar for a fence language, falling back to first-line sniffing.
    fn syntax_for(&self, language: Option<&str>, code: &str) -> Option<&SyntaxReference> {
        if let Some(lang) = language {
            let lang = lang.trim();
            if !lang.is_empty() {
                if let Some(s) = self.syntaxes.find_syntax_by_token(lang) {
                    return Some(s);
                }
                // `console`, `shell-session`, `text` and friends: try some
                // common aliases the grammar set does not know by that name.
                let alias = match lang.to_ascii_lowercase().as_str() {
                    "sh" | "shell" | "zsh" | "console" | "shell-session" | "shellsession" => "bash",
                    "js" | "jsx" | "node" => "javascript",
                    "ts" | "tsx" => "typescript",
                    "yml" => "yaml",
                    "rs" => "rust",
                    "py" | "python3" => "python",
                    "golang" => "go",
                    "cmd" | "batch" => "dosbatch",
                    "docker" | "containerfile" => "dockerfile",
                    "md" | "mdown" => "markdown",
                    "tf" | "hcl" => "terraform",
                    _ => return self.syntaxes.find_syntax_by_extension(lang),
                };
                return self.syntaxes.find_syntax_by_token(alias);
            }
        }
        self.syntaxes
            .find_syntax_by_first_line(code.lines().next().unwrap_or(""))
    }

    /// True when we have a grammar and a theme and can actually colour something.
    pub fn can_highlight(&self, language: Option<&str>, code: &str) -> bool {
        self.theme().is_some() && self.syntax_for(language, code).is_some()
    }

    /// Highlights `code`, returning one vector of styled pieces per line.
    ///
    /// Falls back to a single unstyled piece per line when no grammar matches,
    /// so callers always get well-formed output.
    pub fn highlight(&self, code: &str, language: Option<&str>) -> Vec<Vec<(Style, String)>> {
        let plain = || {
            code.lines()
                .map(|l| vec![(Style::PLAIN, l.to_string())])
                .collect()
        };
        let (Some(theme), Some(syntax)) = (self.theme(), self.syntax_for(language, code)) else {
            return plain();
        };

        let mut h = HighlightLines::new(syntax, theme);
        let mut out = Vec::new();
        for line in LinesWithEndings::from(code) {
            match h.highlight_line(line, &self.syntaxes) {
                Ok(ranges) => out.push(
                    ranges
                        .into_iter()
                        .map(|(style, text)| {
                            (
                                convert_style(style),
                                text.trim_end_matches(['\n', '\r']).to_string(),
                            )
                        })
                        .filter(|(_, t)| !t.is_empty())
                        .collect(),
                ),
                // A grammar that fails mid-file should cost that line's colour,
                // not the whole block.
                Err(_) => out.push(vec![(Style::PLAIN, line.trim_end().to_string())]),
            }
        }
        if out.is_empty() { plain() } else { out }
    }
}

/// Converts a syntect style to ours.
fn convert_style(s: SynStyle) -> Style {
    let mut style = Style::PLAIN.fg(convert_color(s.foreground));
    if s.font_style.contains(FontStyle::BOLD) {
        style = style.bold();
    }
    if s.font_style.contains(FontStyle::ITALIC) {
        style = style.italic();
    }
    if s.font_style.contains(FontStyle::UNDERLINE) {
        style = style.underline();
    }
    style
}

/// Maps a syntect colour, honouring the palette convention used by ANSI themes.
///
/// Themes designed for terminals encode "use the user's colour N" as alpha 0
/// with the index in the red channel, and "use the terminal default" as alpha 1.
/// Respecting that is what makes a 16-colour theme follow the user's own palette
/// instead of snapping to fixed RGB values.
fn convert_color(c: syntect::highlighting::Color) -> Color {
    match c.a {
        0 => Color::Indexed(c.r),
        1 => Color::Default,
        _ => Color::Rgb(Rgb(c.r, c.g, c.b)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hl() -> Highlighter {
        Highlighter::new("base16-ocean.dark")
    }

    #[test]
    fn highlights_rust_with_more_than_one_colour() {
        let h = hl();
        let lines = h.highlight("fn main() { let x = 1; }", Some("rust"));
        assert_eq!(lines.len(), 1);
        let colours: std::collections::HashSet<_> = lines[0]
            .iter()
            .map(|(s, _)| format!("{:?}", s.fg))
            .collect();
        assert!(
            colours.len() > 1,
            "expected several colours, got {colours:?}"
        );
    }

    #[test]
    fn round_trips_the_source_text() {
        let h = hl();
        let src = "def f(x):\n    return x * 2\n";
        let lines = h.highlight(src, Some("python"));
        let joined: String = lines
            .iter()
            .map(|l| l.iter().map(|(_, t)| t.as_str()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(joined.trim_end(), src.trim_end());
    }

    #[test]
    fn resolves_common_language_aliases() {
        let h = hl();
        for lang in [
            "rs", "py", "js", "ts", "yml", "sh", "zsh", "console", "golang",
        ] {
            assert!(
                h.can_highlight(Some(lang), ""),
                "{lang} should resolve to a grammar"
            );
        }
    }

    #[test]
    fn unknown_languages_fall_back_to_plain_text() {
        let h = hl();
        let lines = h.highlight("some text\nmore", Some("no-such-language-xyz"));
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0][0].1, "some text");
    }

    #[test]
    fn sniffs_shebangs_when_no_language_is_given() {
        let h = hl();
        assert!(h.can_highlight(None, "#!/bin/bash\necho hi"));
    }

    #[test]
    fn falls_back_when_the_theme_is_unknown() {
        let h = Highlighter::new("no-such-theme");
        assert!(!h.theme_name().is_empty());
        assert!(
            h.theme_names().len() > 5,
            "the extended theme set should be loaded"
        );
    }

    #[test]
    fn plain_highlighter_never_panics() {
        let h = Highlighter::plain();
        let lines = h.highlight("anything", Some("rust"));
        assert_eq!(lines[0][0].1, "anything");
    }

    #[test]
    fn honours_the_ansi_palette_convention() {
        use syntect::highlighting::Color as SynColor;
        assert_eq!(
            convert_color(SynColor {
                r: 4,
                g: 0,
                b: 0,
                a: 0
            }),
            Color::Indexed(4)
        );
        assert_eq!(
            convert_color(SynColor {
                r: 0,
                g: 0,
                b: 0,
                a: 1
            }),
            Color::Default
        );
        assert_eq!(
            convert_color(SynColor {
                r: 1,
                g: 2,
                b: 3,
                a: 255
            }),
            Color::rgb(1, 2, 3)
        );
    }

    #[test]
    fn empty_code_produces_no_lines() {
        // An empty block has nothing to highlight; the renderer draws the
        // surrounding panel and this returns nothing to put in it.
        assert!(hl().highlight("", Some("rust")).is_empty());
    }

    #[test]
    fn a_blank_line_is_preserved() {
        let lines = hl().highlight("a\n\nb", Some("text"));
        assert_eq!(lines.len(), 3, "the blank line must survive: {lines:?}");
    }
}
