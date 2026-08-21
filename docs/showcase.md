# termmd

A **Markdown viewer** for terminals that can do more than plain text.

![Real pixels, drawn by the terminal](images/demo.png)

> [!NOTE]
> That picture is real pixels — kitty, iTerm2 or sixel — and half blocks
> anywhere else. SVG works too, which is what most README badges are.

## Tables that fit

| Feature | Notes | Since |
|:--------|:------|------:|
| Images | Four protocols, chosen by asking the terminal | 0.1 |
| Tables | Columns measured, widest one wraps | 0.1 |
| Pager | Search, contents, live reload | 0.1 |

## Code, highlighted

```rust
/// Nearest index in the xterm 256-colour cube.
pub fn to_xterm256(self) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    16 + 36 * self.nearest(0) + 6 * self.nearest(1) + self.nearest(2)
}
```

- [x] CommonMark and GFM
- [ ] Anything left to add
