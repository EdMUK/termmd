---
title: termmd demo
---

# termmd

Everything below is here to be looked at rather than read. Run it with

```sh
termmd doc/demo.md
```

and press `H` for the key bindings.

## Images

This is the part most terminal Markdown viewers skip. On kitty, Ghostty, WezTerm,
iTerm2, or anything with sixel, the picture below is real pixels. Everywhere else
it is drawn from half blocks, and it is still a picture.

![Gradients, discs and hard-edged bars](images/demo.png)

PNG, JPEG, GIF, WebP, BMP, TIFF and SVG all work. SVG is worth calling out: most
badges in a README are SVG, and they are rendered at the exact size the terminal
will show them rather than scaled from a bitmap.

An image on a line of its own becomes a figure: centred, given room, and captioned
with its alt text. An image ![badge](images/badge.png) written mid-sentence stays
inline as its alt text instead, because there is nowhere to put a picture in the
middle of a line without breaking it.

Press `i` to toggle images off and back on, and try resizing the window: images
are re-encoded at the new size, and scroll off the top edge a row at a time
rather than disappearing.

## Inline styling

Text can be **bold**, *italic*, ***both***, ~~struck through~~, or `monospaced`.
Links are [clickable](https://github.com/EdMUK/termmd) -- with the mouse, or from
the link list on `L` -- and show their destination where the terminal has no
hyperlink support. A link to another local Markdown file, like
[the README](../README.md), opens in termmd itself; `backspace` comes back. Autolinks like
<https://example.com> are not repeated. Footnotes[^why] collect at the end.

[^why]: Like this one.

Wide text wraps by display column, so 日本語のテキスト and emoji 🎉 stay inside
the margin instead of spilling past it.

## Headings

### Level three

#### Level four

##### Level five

###### Level six

## Lists

- Unordered, which nests:
  - Second level
    - Third level
- With a long item that wraps, so you can see that continuation lines sit under
  the text rather than under the bullet.

1. Ordered lists
2. Number their items
3. And align the text
   when it wraps

10. Numbers past nine
11. Still line up

- [x] Task lists render their state
- [ ] And the ones still to do

## Quotes and alerts

> A quote keeps a bar down the left of the whole block, however many lines it
> runs to, and however much text you put in it.
>
> > Nested quotes stack their bars.

> [!NOTE]
> Useful information that users should know.

> [!TIP]
> Helpful advice for doing things better.

> [!IMPORTANT]
> Key information users need to know.

> [!WARNING]
> Urgent info that needs immediate attention.

> [!CAUTION]
> Advises about risks or negative outcomes.

## Code

```rust
/// Nearest index in the xterm 256-colour cube.
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
    16 + 36 * ri as u8 + 6 * gi as u8 + bi as u8
}
```

```python
def quantize(image, max_colors=256):
    """Median cut over a 15-bit histogram."""
    entries = histogram(image)
    return median_cut(entries, max_colors)
```

```console
$ termmd --caps
terminal
  TERM              xterm-ghostty
  size              120 x 40 cells
```

A block with no language at all still gets highlighted, because the first line
gives it away:

```
#!/bin/sh
set -eu
echo "sniffed as shell"
```

## Tables

| Column | Alignment | Notes |
|:-------|:---------:|------:|
| Left   | Centre    | Right |
| `code` | **bold**  | *italic* |

Numeric columns are right aligned without being asked:

| Package | Downloads | Size |
|---------|-----------|------|
| termmd  | 1,204     | 2.4  |
| example | 89        | 15.0 |

And a table whose content does not fit shrinks its widest column first, so the
narrow ones stay intact:

| Version | Description |
|---------|-------------|
| 1.2.0 | A description long enough that it has to wrap inside its own cell, while the version column beside it is left exactly as wide as it needs to be. |

## Definitions

Median cut
: A colour quantisation algorithm that repeatedly splits the busiest box of
  colours along its widest channel.

Sixel
: A terminal graphics format that encodes six vertical pixels per character.

## Maths

Inline $E = mc^2$ and display:

$$\int_{0}^{\infty} e^{-x^2}\,dx = \frac{\sqrt{\pi}}{2}$$

## Rules

---

That was a horizontal rule.
