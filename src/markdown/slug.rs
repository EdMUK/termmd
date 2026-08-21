//! GitHub-compatible heading slugs, so `[link](#some-heading)` resolves.

use std::collections::HashMap;

/// Generates slugs, disambiguating repeats the way GitHub does (`-1`, `-2`).
#[derive(Debug, Default)]
pub struct Slugger {
    seen: HashMap<String, usize>,
}

impl Slugger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Slugifies `text` and guarantees the result is unique for this document.
    pub fn slug(&mut self, text: &str) -> String {
        let base = slugify(text);
        match self.seen.get_mut(&base) {
            Some(n) => {
                *n += 1;
                format!("{base}-{n}")
            }
            None => {
                self.seen.insert(base.clone(), 0);
                base
            }
        }
    }
}

/// Lowercases, drops punctuation, and turns runs of whitespace into hyphens.
pub fn slugify(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if c == '-' || c == '_' {
            out.push(c);
        } else if c.is_whitespace() {
            // Collapse runs so "a   b" and "a b" agree.
            if !out.ends_with('-') {
                out.push('-');
            }
        }
        // Everything else -- punctuation, emoji -- is dropped, as GitHub does.
    }
    out.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_github_slug_rules() {
        assert_eq!(slugify("Hello, World!"), "hello-world");
        assert_eq!(slugify("  Spaced   Out  "), "spaced-out");
        assert_eq!(
            slugify("snake_case and kebab-case"),
            "snake_case-and-kebab-case"
        );
        assert_eq!(slugify("C++ vs C#"), "c-vs-c");
        assert_eq!(slugify("Ünïcödé Ok"), "ünïcödé-ok");
    }

    #[test]
    fn repeated_headings_get_suffixes() {
        let mut s = Slugger::new();
        assert_eq!(s.slug("Usage"), "usage");
        assert_eq!(s.slug("Usage"), "usage-1");
        assert_eq!(s.slug("Usage"), "usage-2");
    }
}
