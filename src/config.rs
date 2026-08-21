//! The config file: `~/.config/termmd/config.toml`.
//!
//! Every field is optional and mirrors a command-line flag. Precedence runs
//! flags > config > detected defaults, which is resolved in [`crate::cli`].

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// A parsed config file.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Config {
    /// Built-in theme name, or a path to a theme file.
    pub theme: Option<String>,
    /// Theme to use when the terminal background is light.
    pub light_theme: Option<String>,
    /// Theme to use when the terminal background is dark.
    pub dark_theme: Option<String>,
    /// `syntect` theme for code blocks.
    pub syntax_theme: Option<String>,

    /// Hard column limit for text.
    pub width: Option<usize>,
    /// Left and right margin, in columns.
    pub margin: Option<usize>,
    /// Columns a tab expands to in code blocks.
    pub tab_width: Option<usize>,

    /// Use the interactive pager when stdout is a terminal.
    pub pager: Option<bool>,
    /// Enable mouse wheel scrolling in the pager.
    pub mouse: Option<bool>,

    pub images: Option<bool>,
    /// `auto`, `kitty`, `iterm2`, `sixel`, `blocks`, `none`.
    pub image_protocol: Option<String>,
    /// Allow fetching `http(s)` images.
    pub remote_images: Option<bool>,
    pub max_image_rows: Option<u16>,
    pub center_figures: Option<bool>,

    /// `auto`, `hyperlink`, `inline`, `reference`, `hide`.
    pub links: Option<String>,
    /// `auto`, `always`, `never`, `16`, `256`, `truecolor`.
    pub color: Option<String>,
    /// `auto`, `ascii`, `extended`, `full`.
    pub unicode: Option<String>,

    pub line_numbers: Option<bool>,
    pub heading_rules: Option<bool>,
    pub smart_punctuation: Option<bool>,
    /// Probe the terminal for capabilities at startup.
    pub probe: Option<bool>,
}

impl Config {
    /// Loads the config from the standard location, or returns the defaults.
    ///
    /// A missing file is not an error; a malformed one is, because silently
    /// ignoring a typo in a config file is how people lose an afternoon.
    pub fn load() -> anyhow::Result<Self> {
        match Self::path() {
            Some(path) if path.exists() => Self::from_file(&path),
            _ => Ok(Self::default()),
        }
    }

    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;
        Self::from_toml(&text).map_err(|e| anyhow::anyhow!("in {}: {e}", path.display()))
    }

    pub fn from_toml(text: &str) -> anyhow::Result<Self> {
        Ok(toml::from_str(text)?)
    }

    /// Where the config file lives, honouring `TERMMD_CONFIG` and `XDG_CONFIG_HOME`.
    pub fn path() -> Option<PathBuf> {
        if let Some(explicit) = std::env::var_os("TERMMD_CONFIG") {
            return Some(PathBuf::from(explicit));
        }
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            return Some(PathBuf::from(xdg).join("termmd").join("config.toml"));
        }
        dirs::config_dir().map(|d| d.join("termmd").join("config.toml"))
    }

    /// The directory theme files are looked up in.
    pub fn theme_dir() -> Option<PathBuf> {
        Self::path().and_then(|p| p.parent().map(|d| d.join("themes")))
    }
}

/// The themes directory, used when resolving a theme by bare name.
pub fn theme_dir_for_lookup() -> Option<PathBuf> {
    Config::theme_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_is_all_defaults() {
        assert_eq!(Config::from_toml("").unwrap(), Config::default());
    }

    #[test]
    fn parses_kebab_case_keys() {
        let c = Config::from_toml(
            r#"
            theme = "light"
            width = 100
            line-numbers = true
            image-protocol = "kitty"
            remote-images = true
            "#,
        )
        .unwrap();
        assert_eq!(c.theme.as_deref(), Some("light"));
        assert_eq!(c.width, Some(100));
        assert_eq!(c.line_numbers, Some(true));
        assert_eq!(c.image_protocol.as_deref(), Some("kitty"));
        assert_eq!(c.remote_images, Some(true));
    }

    #[test]
    fn rejects_unknown_keys() {
        // A typo should be reported, not ignored.
        let err = Config::from_toml("thmee = \"dark\"").unwrap_err();
        assert!(err.to_string().contains("thmee"), "unhelpful error: {err}");
    }

    #[test]
    fn the_shipped_example_config_is_valid() {
        // If a key is renamed, the documented example must be updated with it.
        let text = include_str!("../doc/config.example.toml");
        Config::from_toml(text).expect("doc/config.example.toml should parse");
    }

    #[test]
    fn rejects_wrong_types() {
        assert!(Config::from_toml("width = \"wide\"").is_err());
    }
}
