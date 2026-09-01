# Contributing

Bug reports and patches are welcome.

## Before you start

For anything larger than a fix, open an issue first. It saves you writing code
that turns out to conflict with something planned, and it is a much better place
to argue about design than a pull request that already exists.

## Working on it

```sh
cargo test                          # unit and integration tests
cargo clippy --all-targets -- -D warnings
cargo fmt
```

All three must pass. CI runs them on Linux, macOS and Windows.

## What good looks like here

**Tests do not need a terminal.** Layout is tested by rendering a document at a
fixed width and asserting on plain text; capability detection is tested by
feeding recorded escape sequences to a parser. If a change can only be verified
by looking at a terminal, that usually means the logic wants pulling out from the
I/O around it.

The exception is `tests/terminal.rs`, for the few features whose entire point is
what reaches the terminal: the clipboard over OSC 52, and what tmux lets through.
Those run termmd on a pty or in a real tmux pane and inspect the bytes that come
out. They skip, saying so, on a machine without tmux. Add to that file only when
nothing short of a terminal can check the change.

**Say why, not what.** The code says what it does. Comments are for the reason:
why a table shrinks its widest column first, why the cursor is positioned by hand
around an image, why the Primary DA query goes last. If a comment restates the
line below it, delete it.

**Degrading is a feature.** Anything that assumes truecolor, or Unicode, or a
particular protocol needs a path for terminals that lack it. `--ascii`,
`--color=16` and `--images=none` should all still produce something worth
reading.

**No copied code.** Protocol support is written from published specifications.
If you are implementing something another project also implements, work from the
spec, not from their source. Cite the spec in a comment.

## Adding an image backend

1. A module in `src/images/` with an `encode` function and its own tests.
2. A variant in `GraphicsProtocol`, plus detection in `src/term/caps.rs` and,
   where the terminal can be asked, in `src/term/probe.rs`.
3. A CLI value in `ImageChoice`, so it can be forced.
4. A row in the README's terminal support table.

## Licence

Contributions are accepted under the [MIT licence](LICENSE), the same terms the
project is released under.

## Adding a theme

Themes are TOML files layered over a built-in base. Start from
`docs/themes/example.toml`. Built-in themes live in `src/theme.rs` and should stay
few: the point is that anyone can write their own without a rebuild.
