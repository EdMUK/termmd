//! Just enough HTML understanding to not embarrass ourselves on real READMEs.
//!
//! A large fraction of Markdown in the wild contains raw HTML: `<img>` badges,
//! `<p align="center">` banners, `<br>`, `<details>` sections. Viewers that dump
//! the tags verbatim look broken, and viewers that drop the blocks silently lose
//! content. We do neither -- we pull out the parts that map onto something we can
//! draw and discard the scaffolding.
//!
//! This is deliberately not an HTML parser. It is a tag scanner with a short list
//! of tags it cares about, and anything it does not recognise becomes text.

use super::ir::ImageRef;

/// A fragment of raw HTML reduced to something renderable.
#[derive(Debug, Clone, PartialEq)]
pub enum HtmlPiece {
    Text(String),
    Image(ImageRef),
    /// `<br>` and friends.
    Break,
    /// `<summary>` content, which we show as the visible title of a `<details>`.
    Summary(String),
}

/// Reduces an HTML fragment to renderable pieces.
pub fn simplify(html: &str) -> Vec<HtmlPiece> {
    let mut out = Vec::new();
    let bytes = html.as_bytes();
    let mut i = 0;
    let mut text = String::new();
    let mut in_summary = false;
    let mut skip_depth = 0usize;

    while i < bytes.len() {
        if bytes[i] == b'<' {
            let Some(end) = html[i..].find('>').map(|e| i + e) else {
                // An unterminated '<' is just text.
                text.push('<');
                i += 1;
                continue;
            };
            let tag = &html[i + 1..end];
            let name = tag_name(tag);
            let closing = tag.starts_with('/');

            match name.as_str() {
                // Tags whose content is machinery, not prose.
                "script" | "style" => {
                    if closing {
                        skip_depth = skip_depth.saturating_sub(1);
                    } else if !tag.ends_with('/') {
                        skip_depth += 1;
                    }
                }
                "br" => {
                    flush(&mut text, &mut out, in_summary);
                    out.push(HtmlPiece::Break);
                }
                "img" if !closing => {
                    flush(&mut text, &mut out, in_summary);
                    if let Some(src) = attr(tag, "src") {
                        out.push(HtmlPiece::Image(ImageRef {
                            url: src,
                            alt: attr(tag, "alt").unwrap_or_default(),
                            title: attr(tag, "title"),
                        }));
                    }
                }
                "summary" => {
                    flush(&mut text, &mut out, in_summary);
                    in_summary = !closing;
                }
                "p" | "div" | "tr" | "li" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
                | "blockquote"
                    if closing =>
                {
                    flush(&mut text, &mut out, in_summary);
                    out.push(HtmlPiece::Break);
                }
                _ => {}
            }
            i = end + 1;
            continue;
        }

        if skip_depth == 0 {
            let ch = html[i..].chars().next().unwrap_or('\0');
            text.push(ch);
            i += ch.len_utf8();
        } else {
            i += 1;
        }
    }
    flush(&mut text, &mut out, in_summary);
    out
}

fn flush(text: &mut String, out: &mut Vec<HtmlPiece>, summary: bool) {
    let decoded = decode_entities(text);
    let trimmed = decoded.trim();
    if !trimmed.is_empty() {
        if summary {
            out.push(HtmlPiece::Summary(trimmed.to_string()));
        } else {
            out.push(HtmlPiece::Text(collapse_whitespace(trimmed)));
        }
    }
    text.clear();
}

fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            space = true;
        } else {
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            out.push(c);
        }
    }
    out
}

/// The lowercased element name from inside `<...>`.
fn tag_name(tag: &str) -> String {
    tag.trim_start_matches('/')
        .split(|c: char| c.is_whitespace() || c == '/' || c == '>')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Reads `name="value"`, `name='value'`, or a bare value.
fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(pos) = lower[from..].find(name) {
        let at = from + pos;
        // Must be preceded by whitespace so `data-src` does not match `src`.
        let preceded_ok = at > 0 && tag.as_bytes()[at - 1].is_ascii_whitespace();
        let rest = &tag[at + name.len()..];
        let after = rest.trim_start();
        if preceded_ok && after.starts_with('=') {
            let value = after[1..].trim_start();
            let v = match value.as_bytes().first() {
                Some(b'"') => value[1..].split('"').next(),
                Some(b'\'') => value[1..].split('\'').next(),
                _ => value
                    .split_whitespace()
                    .next()
                    .map(|v| v.trim_end_matches('/')),
            };
            return v.filter(|v| !v.is_empty()).map(decode_entities);
        }
        from = at + name.len();
    }
    None
}

/// Decodes the handful of entities that actually show up in Markdown files.
pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let Some(semi) = tail[..tail.len().min(12)].find(';') else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let entity = &tail[1..semi];
        let replacement = match entity {
            "amp" => Some("&".to_string()),
            "lt" => Some("<".to_string()),
            "gt" => Some(">".to_string()),
            "quot" => Some("\"".to_string()),
            "apos" | "#39" => Some("'".to_string()),
            "nbsp" => Some("\u{a0}".to_string()),
            "hellip" => Some("…".to_string()),
            "mdash" => Some("—".to_string()),
            "ndash" => Some("–".to_string()),
            e => e
                .strip_prefix('#')
                .and_then(|n| {
                    if let Some(hex) = n.strip_prefix(['x', 'X']) {
                        u32::from_str_radix(hex, 16).ok()
                    } else {
                        n.parse::<u32>().ok()
                    }
                })
                .and_then(char::from_u32)
                .map(|c| c.to_string()),
        };
        match replacement {
            Some(r) => {
                out.push_str(&r);
                rest = &tail[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_images_from_html_blocks() {
        let pieces =
            simplify(r#"<p align="center"><img src="logo.png" alt="Logo" width="100"></p>"#);
        assert!(
            matches!(&pieces[0], HtmlPiece::Image(i) if i.url == "logo.png" && i.alt == "Logo")
        );
    }

    #[test]
    fn does_not_confuse_similar_attributes() {
        assert_eq!(
            attr(r#"img data-src="a.png" src="b.png""#, "src"),
            Some("b.png".into())
        );
        assert_eq!(attr(r#"img data-src="a.png""#, "src"), None);
    }

    #[test]
    fn br_becomes_a_break() {
        let pieces = simplify("one<br/>two");
        assert_eq!(pieces[0], HtmlPiece::Text("one".into()));
        assert_eq!(pieces[1], HtmlPiece::Break);
        assert_eq!(pieces[2], HtmlPiece::Text("two".into()));
    }

    #[test]
    fn keeps_prose_and_drops_tags() {
        let pieces = simplify("<div><b>bold</b> text</div>");
        assert_eq!(pieces[0], HtmlPiece::Text("bold text".into()));
    }

    #[test]
    fn discards_script_contents() {
        let pieces = simplify("<script>alert('x')</script>keep");
        assert_eq!(pieces, vec![HtmlPiece::Text("keep".into())]);
    }

    #[test]
    fn summary_is_kept_separately() {
        let pieces = simplify("<details><summary>Click me</summary>body</details>");
        assert_eq!(pieces[0], HtmlPiece::Summary("Click me".into()));
        assert_eq!(pieces[1], HtmlPiece::Text("body".into()));
    }

    #[test]
    fn decodes_entities() {
        assert_eq!(
            decode_entities("a &amp; b &lt;c&gt; &#8212; &#x2014;"),
            "a & b <c> — —"
        );
        assert_eq!(decode_entities("100% & rising"), "100% & rising");
    }
}
