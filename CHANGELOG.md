# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project uses
[semantic versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- `--toc` and the contents panel showed a heading as written -- `:rocket:`,
  `$x^2$` -- while the page showed a rocket and `x²`. Both are expanded when
  the page is drawn, so the terminal can opt out with `--ascii`, and the table
  of contents was built from the parse tree instead. It now reads the way the
  page does, under `--ascii` too.
- `--watch` on a URL did nothing, silently. Nothing on the far side of a URL
  announces a change, so termmd now fetches it again every five seconds and
  reloads when the text differs.
- `--front-matter` over several files printed only the first file's. It now
  prints each file's, introduced by a `--- # name` line when there is more
  than one, so the output is still one YAML stream.
- termmd asked tmux what its client can draw with no limit on how long tmux
  might take to answer, so a wedged server held termmd at startup rather than
  leaving it on half blocks. The question is now given half a second, and
  handing a copy to `tmux load-buffer` two.

## [0.1.3] - 2026-09-02

More places a document can come from, more of it drawn, and one crash fixed:
a deeply nested document could take the viewer down, and now cannot.

### Fixed

- Deeply nested input crashed rather than rendering. Fifty thousand nested
  quotes, list items or emphasis markers took the stack down with them, and so
  did a formula with that many nested braces -- every walk over the document
  tree recurses, dropping it included. Both now have a floor: past a hundred
  levels a document stops nesting and its contents join the block already open,
  and past sixty-four a formula is copied as written. Nothing is lost either
  way, and since a document can now arrive from a URL, this is no longer only
  something people could do to themselves.
- A superscript or subscript that was a command took only the backslash, so
  `x^\alpha` came out unchanged and `y^\frac{1}{2}` came out as `y^\frac12`,
  with the braces dropped and the meaning with them. A command is now one
  thing, arguments and all: `x^α` and `y^½`.
- A directory index took a `#` comment inside a leading code block for the
  file's heading, so a document that opens with a shell snippet was listed
  under the wrong title.
- `holds_markdown` followed symlinks while deciding whether to list a
  subdirectory, and a cycle of them was stopped only by paths eventually
  growing too long to open. It descends into real directories now, eight deep.
- A protocol-relative URL in a fetched document (`//host/x.png`) resolved
  against the wrong host.
- A link to a URL whose *host* ends in `.md` was fetched as a document rather
  than handed to the browser.
- Square brackets in a filename broke the link the directory index generated.
- `termmd big.md | head` reported "Broken pipe (os error 32)" and exited
  non-zero. A reader who stops reading is not a failure.

### Added

- `y` in the pager copies the code block on screen to the system clipboard over
  OSC 52, which works over ssh. It copies the source as written -- no
  highlighting, no line numbers, no trailing newline, so a shell snippet lands
  at a prompt without running. In the links panel, `y` copies the selected URL.
  Inside tmux the text is handed to `tmux load-buffer -w`, since tmux does not
  forward an application's OSC 52 unless `set-clipboard` is `on`.
- A URL can be given instead of a file: `termmd https://example.com/README.md`
  fetches and renders it, resolving the relative links and images inside it
  against where it came from. Naming a URL is asking for it, so there is no flag
  to turn on; the images inside it still wait for `--remote-images`. In the
  pager, a link to a remote Markdown document opens in termmd rather than in a
  browser, on the same reasoning.
- A directory can be given too. `termmd docs/` lists the Markdown in it, with
  each file's first heading beside its name, as a document whose links the pager
  already knows how to follow -- so a directory tree is browsable, with
  `backspace` to come back, out of parts that were already there.
- `--front-matter` prints a document's YAML front matter. It was already parsed
  and kept out of the page, where it is metadata rather than content, but there
  was no way to ask for it.
- Maths is rendered into Unicode where Unicode has the characters: scripts
  (`x^2` becomes `x²`), Greek, the comparison and set operators, roots,
  fractions, and blackboard letters. Anything else -- an unknown command, a
  script with no character, a matrix -- passes through as written, which is
  still readable TeX.
- `:shortcode:` emoji, from the alias set GitHub accepts. Expanded at render
  time rather than at parse time, so a code span, a code block and `--ascii` all
  keep the name they were written with.
- Images and hyperlinks inside tmux, when the terminal tmux is drawing on can
  manage them. tmux answers a capability query on its own behalf -- it claims
  sixel whether or not its client could show one -- so termmd asks tmux what it
  decided the client supports instead. From tmux 3.4 a sixel is drawn by tmux
  itself, with no passthrough involved. Anything older, GNU screen, or a client
  without the feature keeps half blocks, which was the only option before.

## [0.1.2] - 2026-08-28

Nothing changes about how a document is drawn. This one is about getting termmd
onto a machine, and telling a shell what to do with it.

### Added

- `--completions <SHELL>` writes a completion script for bash, zsh, fish,
  PowerShell or elvish, and `--man` writes the man page as roff. Both are
  generated from the same definition the flags are parsed with, so neither can
  describe a termmd you do not have; both write to stdout without touching the
  terminal, so redirecting one into a file cannot capture a capability probe.
- Release archives carry the man page and the five completion scripts. They are
  generated once per release rather than per target, because the cross-compiled
  macOS build cannot run its own binary to produce them.
- `brew install EdMUK/tap/termmd` installs the release binary on macOS and
  Linux, from a [tap](https://github.com/EdMUK/homebrew-tap) that does not
  compile the crate.
- `cargo binstall termmd` fetches the release archive rather than compiling.
  The manifest pins the naming the release workflow uses, which none of
  binstall's defaults match.

## [0.1.1] - 2026-08-21

Bug fixes, all of them found by running termmd on terminals its author had not:
macOS Terminal.app and iTerm2.

### Fixed

- Half-block images drew a staircase of colour across the pager instead of a
  picture. The backend separated its rows with a bare line feed, and the pager
  runs the terminal in raw mode, where that moves down without returning to
  column one. Printing hid the bug, because cooked mode supplies the carriage
  return. This affected every terminal without a pixel protocol, macOS
  Terminal.app included.
- Chunked kitty transmissions sprayed a protocol reply into the document. Only
  the first chunk carried the `q=2` suppression key, so a terminal that treats
  each escape code independently answered the last one -- iTerm2 replies
  `i=0;OK` -- and printing, which does not use raw mode, echoed that reply onto
  the page. Only images large enough to be split showed it.
- kitty images were transmitted at the default z-index, which draws them over
  the text. Anything the pager drew on top of a picture -- the contents panel,
  a search prompt -- disappeared behind it. They now sit below the text.
- Overlay panels had no background of their own, so an image showed through the
  gaps between their text. Themes gained a `panel` role for it.
- `TERM_PROGRAM` now outranks the marker environment variables when choosing a
  graphics protocol. `KITTY_WINDOW_ID` and `GHOSTTY_RESOURCES_DIR` are exported
  into every child process and outlive the terminal that set them, so a stale
  one could send kitty escape codes to a terminal that had never heard of them.
- The iTerm2 sequence carried no `name`, so a receiver had nothing but a default
  of "Unnamed file" to identify the payload by, and could show a file chip
  rather than the image. It now sends a `.png` name derived from the source, and
  terminates with BEL as `imgcat` does.

### Changed

- WezTerm uses the kitty protocol rather than iTerm2's, which is what the
  documentation already claimed. iTerm2 3.5 speaks the kitty protocol too, and
  the probe now finds and prefers it there.
- The website and README explain that remote images are opt-in, which is the
  first thing anyone opening a README full of badges runs into.

## [0.1.0] - 2026-08-21

First release.

### Added

- CommonMark and GFM: tables, task lists, strikethrough, footnotes, autolinks,
  GitHub alerts, definition lists, math, wikilinks, and YAML front matter.
- Image rendering through the kitty graphics protocol, iTerm2 inline images,
  DEC sixel, and Unicode half blocks, chosen by terminal detection.
- A sixel encoder with median-cut quantisation and Floyd–Steinberg dithering,
  with no `libsixel` dependency.
- SVG rendering via `resvg`, behind the default `svg` feature. Vector images are
  rasterised at the size they will be displayed, so badge text stays legible.
- Images that cannot be shown explain why in their placeholder, rather than
  leaving an unexplained gap.
- Syntax highlighting via `syntect` and the extended `two-face` grammar set,
  with language aliases and first-line sniffing.
- Table layout that measures columns and wraps the widest, with a record layout
  for terminals too narrow for a grid.
- An interactive pager: search, table of contents, link list, horizontal
  scrolling, live reload, and mouse support.
- Clickable links in the pager. Links to other local Markdown files open in
  termmd, with `backspace` to go back; local paths are emitted as `file://`
  URIs so the terminal's own link handling works too.
- The mouse is left to the terminal by default, so click-drag selection works;
  the wheel still scrolls via alternate scroll. `m` hands the mouse to termmd
  for click-to-follow, and back again.
- Terminal capability detection by live query, with environment-based fallbacks
  and per-capability overrides.
- Themes in TOML, layered over built-in dark, light and monochrome bases.
- OSC 8 hyperlinks, with inline, reference and hidden URL modes as fallbacks.

[Unreleased]: https://github.com/EdMUK/termmd/compare/v0.1.3...HEAD
[0.1.3]: https://github.com/EdMUK/termmd/releases/tag/v0.1.3
[0.1.2]: https://github.com/EdMUK/termmd/releases/tag/v0.1.2
[0.1.1]: https://github.com/EdMUK/termmd/releases/tag/v0.1.1
[0.1.0]: https://github.com/EdMUK/termmd/releases/tag/v0.1.0
