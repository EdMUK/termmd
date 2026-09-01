//! Enough TeX to read the mathematics in a README.
//!
//! Documents that carry `$x^2$` are not asking for typesetting, they are asking
//! to be read, and `x^2` printed literally is the one thing the terminal can do
//! that a person cannot easily undo in their head. Unicode has the characters
//! for the common cases -- superscripts, roots, Greek, the comparison operators
//! -- so the common cases are translated and everything else is left exactly as
//! it was written.
//!
//! That last part is the whole design. This is not a TeX engine and will not
//! become one: an unknown command, a script that has no Unicode form, a
//! construct with more structure than a line of text can hold, all pass through
//! untouched. A reader who sees `\begin{pmatrix}` has lost nothing, and a reader
//! who sees `x²` has gained something.

/// How deep a formula may nest before we stop reading and start copying.
///
/// Groups nest by recursion here, and a formula is a string rather than part
/// of the document tree, so it needs its own floor. Sixty-four is past any
/// formula and short of any stack.
const MAX_DEPTH: usize = 64;

/// Translates the parts of TeX that a line of terminal text can hold.
pub(super) fn to_unicode(source: &str) -> String {
    convert(&source.chars().collect::<Vec<_>>(), MAX_DEPTH)
}

fn convert(src: &[char], depth: usize) -> String {
    // Deeper than anyone means, so the rest is left exactly as it was written,
    // which is what this module does with everything it will not translate.
    if depth == 0 {
        return src.iter().collect();
    }
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        match src[i] {
            '\\' => i = command(src, i, &mut out, depth),
            '^' => i = script(src, i, &mut out, superscript, depth),
            '_' => i = script(src, i, &mut out, subscript, depth),
            // A group that is not attached to anything keeps only its contents:
            // `{a}` and `a` mean the same thing, and one of them is readable.
            '{' => match group(src, i) {
                Some((body, next)) => {
                    out.push_str(&convert(body, depth - 1));
                    i = next;
                }
                None => {
                    out.push('{');
                    i += 1;
                }
            },
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// Handles a `\command`, returning where to carry on from.
fn command(src: &[char], start: usize, out: &mut String, depth: usize) -> usize {
    let mut end = start + 1;
    while end < src.len() && src[end].is_ascii_alphabetic() {
        end += 1;
    }
    let name: String = src[start + 1..end].iter().collect();

    match name.as_str() {
        // A fraction is the one construct worth restructuring rather than
        // replacing: written on one line it needs its parts kept apart.
        "frac" | "dfrac" | "tfrac" => {
            if let Some((numerator, after)) = group(src, skip_spaces(src, end)) {
                if let Some((denominator, next)) = group(src, skip_spaces(src, after)) {
                    out.push_str(&fraction(
                        &convert(numerator, depth - 1),
                        &convert(denominator, depth - 1),
                    ));
                    return next;
                }
            }
        }
        "sqrt" => {
            let mut at = skip_spaces(src, end);
            // The optional index: \sqrt[3]{x} is a cube root.
            let mut sign = '√';
            if let Some((index, after)) = bracketed(src, at) {
                sign = match index.iter().collect::<String>().as_str() {
                    "3" => '∛',
                    "4" => '∜',
                    // Any other index has no character of its own, and a root
                    // whose index has been dropped is a lie.
                    _ => return passthrough(src, start, after, out),
                };
                at = skip_spaces(src, after);
            }
            if let Some((body, next)) = group(src, at) {
                let body = convert(body, depth - 1);
                out.push(sign);
                out.push_str(&wrap_if_compound(&body));
                return next;
            }
        }
        // Font commands that Unicode can honour, and those it cannot but whose
        // contents are still worth reading.
        "mathbb" => {
            if let Some((body, next)) = group(src, skip_spaces(src, end)) {
                let text: String = body.iter().collect();
                out.push_str(&blackboard(&text).unwrap_or_else(|| convert(body, depth - 1)));
                return next;
            }
        }
        "text" | "mathrm" | "mathbf" | "mathit" | "mathsf" | "operatorname" => {
            if let Some((body, next)) = group(src, skip_spaces(src, end)) {
                out.push_str(&convert(body, depth - 1));
                return next;
            }
        }
        // Sizing commands wrap a delimiter we are going to print anyway.
        "left" | "right" | "big" | "Big" | "bigg" | "Bigg" => return end,
        "quad" | "qquad" => {
            out.push(' ');
            return end;
        }
        // A backslash followed by something that is not a letter is an escape
        // or a piece of spacing, and the character itself is the whole command.
        "" => {
            return match src.get(end) {
                // Thin, medium and thick spaces, all of them one space here --
                // and none at all next to a space that is already written.
                Some(',' | ';' | ':' | ' ') => {
                    if !out.ends_with(' ') {
                        out.push(' ');
                    }
                    end + 1
                }
                // Negative space, which has nothing to become.
                Some('!') => end + 1,
                // `\{`, `\%`, `\$`: the character, without its escape.
                Some(c) => {
                    out.push(*c);
                    end + 1
                }
                None => {
                    out.push('\\');
                    end
                }
            };
        }
        _ => {
            if let Some(symbol) = SYMBOLS
                .binary_search_by_key(&name.as_str(), |(k, _)| k)
                .ok()
                .map(|i| SYMBOLS[i].1)
            {
                out.push_str(symbol);
                return end;
            }
        }
    }
    passthrough(src, start, end, out)
}

/// Writes a command back exactly as it was written, arguments included.
///
/// Keeping the braces matters: `\begin{pmatrix}` says something, and
/// `\beginpmatrix` says it worse.
fn passthrough(src: &[char], start: usize, end: usize, out: &mut String) -> usize {
    let mut at = end;
    while let Some((_, next)) = group(src, at) {
        at = next;
    }
    out.extend(&src[start..at]);
    at
}

/// Handles `^` or `_`, which take a group, a command, or the next character.
fn script(
    src: &[char],
    start: usize,
    out: &mut String,
    map: fn(char) -> Option<char>,
    depth: usize,
) -> usize {
    let at = start + 1;
    let (body, next) = if let Some((body, next)) = group(src, at) {
        (body.to_vec(), next)
    } else if src.get(at) == Some(&'\\') {
        // A command is a single thing, arguments and all: the script in
        // `x^\alpha` is a letter, and the one in `y^\frac{1}{2}` is a
        // fraction. Taking only the backslash would leave the name behind as
        // ordinary text and strip the braces off what followed.
        let end = command_extent(src, at);
        (src[at..end].to_vec(), end)
    } else if at < src.len() {
        (vec![src[at]], at + 1)
    } else {
        return at;
    };

    // Convert first: `x^\alpha` has a script that is a command.
    let converted = convert(&body, depth - 1);
    match converted.chars().map(map).collect::<Option<String>>() {
        Some(scripted) => out.push_str(&scripted),
        // No character for it, so keep the notation the author used rather than
        // dropping the distinction between x2 and x².
        None => {
            out.push(src[start]);
            if converted.chars().count() > 1 {
                out.push('{');
                out.push_str(&converted);
                out.push('}');
            } else {
                out.push_str(&converted);
            }
        }
    }
    next
}

/// Where the command starting at `start` ends, arguments included.
fn command_extent(src: &[char], start: usize) -> usize {
    let mut end = start + 1;
    if src.get(end).is_some_and(char::is_ascii_alphabetic) {
        while src.get(end).is_some_and(char::is_ascii_alphabetic) {
            end += 1;
        }
    } else if end < src.len() {
        // A command that is one character of punctuation, like `\,`.
        end += 1;
    }
    while let Some((_, next)) = bracketed(src, end).or_else(|| group(src, end)) {
        end = next;
    }
    end
}

/// The contents of a `{...}` at `at`, and the index after its closing brace.
fn group(src: &[char], at: usize) -> Option<(&[char], usize)> {
    delimited(src, at, '{', '}')
}

/// The same for `[...]`, which is how an optional argument is written.
fn bracketed(src: &[char], at: usize) -> Option<(&[char], usize)> {
    delimited(src, at, '[', ']')
}

fn delimited(src: &[char], at: usize, open: char, close: char) -> Option<(&[char], usize)> {
    if src.get(at) != Some(&open) {
        return None;
    }
    let mut depth = 0usize;
    for (offset, c) in src[at..].iter().enumerate() {
        match c {
            _ if *c == open => depth += 1,
            _ if *c == close => {
                depth -= 1;
                if depth == 0 {
                    return Some((&src[at + 1..at + offset], at + offset + 1));
                }
            }
            _ => {}
        }
    }
    // Unbalanced, so there is no group here and the brace is just a brace.
    None
}

fn skip_spaces(src: &[char], mut at: usize) -> usize {
    while src.get(at) == Some(&' ') {
        at += 1;
    }
    at
}

/// Writes a fraction on one line.
fn fraction(numerator: &str, denominator: &str) -> String {
    if let Some(vulgar) = VULGAR
        .iter()
        .find(|(n, d, _)| *n == numerator && *d == denominator)
    {
        return vulgar.2.to_string();
    }
    format!(
        "{}/{}",
        wrap_if_compound(numerator),
        wrap_if_compound(denominator)
    )
}

/// Parenthesises a part that would otherwise bind wrongly: `(a+b)/c` is the
/// same fraction as `\frac{a+b}{c}`, where `a+b/c` is a different one.
fn wrap_if_compound(part: &str) -> String {
    let compound = part
        .chars()
        .any(|c| matches!(c, '+' | '-' | '−' | '±' | '/' | '*' | '·' | '×' | ' '));
    let wrapped = part.starts_with('(') && part.ends_with(')');
    if compound && !wrapped {
        format!("({part})")
    } else {
        part.to_string()
    }
}

fn superscript(c: char) -> Option<char> {
    Some(match c {
        '0' => '⁰',
        '1' => '¹',
        '2' => '²',
        '3' => '³',
        '4' => '⁴',
        '5' => '⁵',
        '6' => '⁶',
        '7' => '⁷',
        '8' => '⁸',
        '9' => '⁹',
        '+' => '⁺',
        '-' | '−' => '⁻',
        '=' => '⁼',
        '(' => '⁽',
        ')' => '⁾',
        'n' => 'ⁿ',
        'i' => 'ⁱ',
        _ => return None,
    })
}

fn subscript(c: char) -> Option<char> {
    Some(match c {
        '0' => '₀',
        '1' => '₁',
        '2' => '₂',
        '3' => '₃',
        '4' => '₄',
        '5' => '₅',
        '6' => '₆',
        '7' => '₇',
        '8' => '₈',
        '9' => '₉',
        '+' => '₊',
        '-' | '−' => '₋',
        '=' => '₌',
        '(' => '₍',
        ')' => '₎',
        'a' => 'ₐ',
        'e' => 'ₑ',
        'h' => 'ₕ',
        'i' => 'ᵢ',
        'j' => 'ⱼ',
        'k' => 'ₖ',
        'l' => 'ₗ',
        'm' => 'ₘ',
        'n' => 'ₙ',
        'o' => 'ₒ',
        'p' => 'ₚ',
        'r' => 'ᵣ',
        's' => 'ₛ',
        't' => 'ₜ',
        'u' => 'ᵤ',
        'v' => 'ᵥ',
        'x' => 'ₓ',
        _ => return None,
    })
}

/// `\mathbb{R}` and the handful of others that have a character.
fn blackboard(text: &str) -> Option<String> {
    let mut out = String::new();
    for c in text.chars() {
        out.push(match c {
            'C' => 'ℂ',
            'H' => 'ℍ',
            'N' => 'ℕ',
            'P' => 'ℙ',
            'Q' => 'ℚ',
            'R' => 'ℝ',
            'Z' => 'ℤ',
            _ => return None,
        });
    }
    Some(out)
}

/// Fractions with a character of their own, which read better than `1/2`.
static VULGAR: &[(&str, &str, &str)] = &[
    ("1", "2", "½"),
    ("1", "3", "⅓"),
    ("2", "3", "⅔"),
    ("1", "4", "¼"),
    ("3", "4", "¾"),
    ("1", "5", "⅕"),
    ("2", "5", "⅖"),
    ("3", "5", "⅗"),
    ("4", "5", "⅘"),
    ("1", "6", "⅙"),
    ("5", "6", "⅚"),
    ("1", "8", "⅛"),
    ("3", "8", "⅜"),
    ("5", "8", "⅝"),
    ("7", "8", "⅞"),
];

/// Commands that are simply a character, sorted for a binary search.
static SYMBOLS: &[(&str, &str)] = &[
    ("Delta", "Δ"),
    ("Gamma", "Γ"),
    ("Im", "ℑ"),
    ("Lambda", "Λ"),
    ("Leftarrow", "⇐"),
    ("Leftrightarrow", "⇔"),
    ("Omega", "Ω"),
    ("Phi", "Φ"),
    ("Pi", "Π"),
    ("Psi", "Ψ"),
    ("Re", "ℜ"),
    ("Rightarrow", "⇒"),
    ("Sigma", "Σ"),
    ("Theta", "Θ"),
    ("Upsilon", "Υ"),
    ("Xi", "Ξ"),
    ("aleph", "ℵ"),
    ("alpha", "α"),
    ("angle", "∠"),
    ("approx", "≈"),
    ("ast", "∗"),
    ("beta", "β"),
    ("bullet", "•"),
    ("cap", "∩"),
    ("cdot", "·"),
    ("cdots", "⋯"),
    ("chi", "χ"),
    ("circ", "∘"),
    ("cong", "≅"),
    ("cup", "∪"),
    ("dagger", "†"),
    ("dashv", "⊣"),
    ("ddots", "⋱"),
    ("deg", "°"),
    ("delta", "δ"),
    ("div", "÷"),
    ("dots", "…"),
    ("downarrow", "↓"),
    ("ell", "ℓ"),
    ("emptyset", "∅"),
    ("epsilon", "ε"),
    ("equiv", "≡"),
    ("eta", "η"),
    ("exists", "∃"),
    ("forall", "∀"),
    ("gamma", "γ"),
    ("ge", "≥"),
    ("geq", "≥"),
    ("gg", "≫"),
    ("hbar", "ℏ"),
    ("iff", "⇔"),
    ("in", "∈"),
    ("infty", "∞"),
    ("int", "∫"),
    ("iota", "ι"),
    ("kappa", "κ"),
    ("lambda", "λ"),
    ("land", "∧"),
    ("langle", "⟨"),
    ("lceil", "⌈"),
    ("ldots", "…"),
    ("le", "≤"),
    ("leftarrow", "←"),
    ("leftrightarrow", "↔"),
    ("leq", "≤"),
    ("lfloor", "⌊"),
    ("ll", "≪"),
    ("lor", "∨"),
    ("mapsto", "↦"),
    ("mid", "∣"),
    ("models", "⊨"),
    ("mp", "∓"),
    ("mu", "μ"),
    ("nabla", "∇"),
    ("ne", "≠"),
    ("neg", "¬"),
    ("neq", "≠"),
    ("ni", "∋"),
    ("notin", "∉"),
    ("nu", "ν"),
    ("odot", "⊙"),
    ("oint", "∮"),
    ("omega", "ω"),
    ("oplus", "⊕"),
    ("otimes", "⊗"),
    ("parallel", "∥"),
    ("partial", "∂"),
    ("perp", "⊥"),
    ("phi", "φ"),
    ("pi", "π"),
    ("pm", "±"),
    ("prime", "′"),
    ("prod", "∏"),
    ("propto", "∝"),
    ("psi", "ψ"),
    ("rangle", "⟩"),
    ("rceil", "⌉"),
    ("rfloor", "⌋"),
    ("rho", "ρ"),
    ("rightarrow", "→"),
    ("sigma", "σ"),
    ("sim", "∼"),
    ("simeq", "≃"),
    ("star", "⋆"),
    ("subset", "⊂"),
    ("subseteq", "⊆"),
    ("sum", "∑"),
    ("supset", "⊃"),
    ("supseteq", "⊇"),
    ("tau", "τ"),
    ("theta", "θ"),
    ("times", "×"),
    ("to", "→"),
    ("top", "⊤"),
    ("uparrow", "↑"),
    ("upsilon", "υ"),
    ("varepsilon", "ε"),
    ("varphi", "φ"),
    ("vdash", "⊢"),
    ("vdots", "⋮"),
    ("vee", "∨"),
    ("wedge", "∧"),
    ("xi", "ξ"),
    ("zeta", "ζ"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_symbol_table_is_sorted() {
        assert!(
            SYMBOLS.windows(2).all(|w| w[0].0 < w[1].0),
            "the table must be sorted for the binary search to work"
        );
    }

    #[test]
    fn scripts_become_scripts() {
        assert_eq!(to_unicode("E = mc^2"), "E = mc²");
        assert_eq!(to_unicode("x^{10}"), "x¹⁰");
        assert_eq!(to_unicode("a_1 + a_2"), "a₁ + a₂");
        assert_eq!(to_unicode("x^{-1}"), "x⁻¹");
        assert_eq!(to_unicode("H_2O"), "H₂O");
    }

    #[test]
    fn a_script_can_be_a_command() {
        // The whole command belongs to the script. Taking only the backslash
        // left the name as text and dropped the braces from what came after,
        // turning a half into `\frac12`.
        assert_eq!(to_unicode(r"x^\alpha"), "x^α");
        assert_eq!(to_unicode(r"y^\frac{1}{2}"), "y^½");
        assert_eq!(to_unicode(r"z_\beta"), "z_β");
        assert_eq!(to_unicode(r"e^{\alpha x}"), "e^{α x}");
        // Two characters, so the braces stay: x^√2 would read as x to the
        // power of a root sign.
        assert_eq!(to_unicode(r"x^\sqrt{2}"), "x^{√2}");
        // The text after the script is still text.
        assert_eq!(to_unicode(r"x^\alpha + 1"), "x^α + 1");
    }

    #[test]
    fn a_script_with_no_character_keeps_its_notation() {
        // No superscript 'q', and 'x^q' is clearer than 'xq'.
        assert_eq!(to_unicode("x^q"), "x^q");
        assert_eq!(to_unicode("x^{qq}"), "x^{qq}");
        assert_eq!(to_unicode("x_{yz}"), "x_{yz}");
    }

    #[test]
    fn symbols_and_greek() {
        assert_eq!(to_unicode(r"\alpha + \beta \leq \gamma"), "α + β ≤ γ");
        assert_eq!(to_unicode(r"\sum_{i=1}^{n} x_i"), "∑ᵢ₌₁ⁿ xᵢ");
        assert_eq!(to_unicode(r"a \times b \neq c"), "a × b ≠ c");
        assert_eq!(to_unicode(r"\infty"), "∞");
    }

    #[test]
    fn fractions_are_written_on_one_line() {
        assert_eq!(to_unicode(r"\frac{a}{b}"), "a/b");
        assert_eq!(to_unicode(r"\frac{a+b}{c}"), "(a+b)/c");
        assert_eq!(to_unicode(r"\frac{dx}{dt}"), "dx/dt");
        assert_eq!(to_unicode(r"\frac{1}{2}"), "½");
        assert_eq!(to_unicode(r"\frac{3}{4} \pi"), "¾ π");
    }

    #[test]
    fn roots_keep_their_index() {
        assert_eq!(to_unicode(r"\sqrt{2}"), "√2");
        assert_eq!(to_unicode(r"\sqrt{a+b}"), "√(a+b)");
        assert_eq!(to_unicode(r"\sqrt[3]{x}"), "∛x");
        // A fifth root has no sign, so nothing is claimed about it.
        assert_eq!(to_unicode(r"\sqrt[5]{x}"), r"\sqrt[5]{x}");
    }

    #[test]
    fn unknown_commands_pass_through() {
        assert_eq!(
            to_unicode(r"\begin{pmatrix} a \end{pmatrix}"),
            r"\begin{pmatrix} a \end{pmatrix}"
        );
        assert_eq!(to_unicode(r"\unheardof{x}"), r"\unheardof{x}");
    }

    #[test]
    fn nesting_has_a_floor() {
        // Groups recurse, and a formula is a string rather than part of the
        // document tree, so it needs a floor of its own.
        let deep = format!("{}a{}", "{".repeat(50_000), "}".repeat(50_000));
        let out = to_unicode(&deep);
        assert!(out.contains('a'), "the contents are still there");
        assert!(out.contains('{'), "past the floor it is copied as written");
    }

    #[test]
    fn plain_text_is_untouched() {
        assert_eq!(to_unicode("f(x) = 2x + 1"), "f(x) = 2x + 1");
        assert_eq!(to_unicode(""), "");
    }

    #[test]
    fn sizing_and_spacing_disappear() {
        assert_eq!(to_unicode(r"\left( x \right)"), "( x )");
        assert_eq!(to_unicode(r"a \, b"), "a  b");
        assert_eq!(to_unicode(r"a\,b"), "a b");
    }

    #[test]
    fn sets_and_text_come_through() {
        assert_eq!(to_unicode(r"x \in \mathbb{R}"), "x ∈ ℝ");
        assert_eq!(to_unicode(r"\text{if } x > 0"), "if  x > 0");
        // Not a blackboard letter, so the contents are kept as they are.
        assert_eq!(to_unicode(r"\mathbb{W}"), "W");
    }

    #[test]
    fn an_unbalanced_brace_is_just_a_brace() {
        assert_eq!(to_unicode("{a"), "{a");
        assert_eq!(to_unicode("a}"), "a}");
        assert_eq!(to_unicode(r"\frac{a"), r"\frac{a");
    }

    #[test]
    fn escaped_punctuation_survives() {
        assert_eq!(to_unicode(r"\{a\}"), "{a}");
        assert_eq!(to_unicode(r"50\%"), "50%");
    }
}
