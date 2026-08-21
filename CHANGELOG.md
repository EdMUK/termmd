# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project uses
[semantic versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- Half-block images drew a staircase of colour across the pager instead of a
  picture. The backend separated its rows with a bare line feed, and the pager
  runs the terminal in raw mode, where that moves down without returning to
  column one. Printing hid the bug, because cooked mode supplies the carriage
  return. This affected every terminal without a pixel protocol, macOS
  Terminal.app included.
- Chunked kitty transmissions sprayed a protocol reply into the document.
  Only the first chunk carried the `q=2` suppression key, so a terminal that
  treats each escape code independently answered the last one -- iTerm2 replies
  `i=0;OK` -- and printing, which does not use raw mode, echoed that reply onto
  the page. Only images large enough to be split showed it.
- `TERM_PROGRAM` now outranks the marker environment variables when choosing a
  graphics protocol. `KITTY_WINDOW_ID` and `GHOSTTY_RESOURCES_DIR` are exported
  into every child process and outlive the terminal that set them, so a stale
  one could send kitty escape codes to a terminal that had never heard of them.
- The iTerm2 sequence carried no `name`, so the receiver had nothing but a
  default of "Unnamed file" to identify the payload by, and could show a file
  chip rather than the image. It now sends a `.png` name derived from the
  source, and terminates with BEL as `imgcat` does.

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

[Unreleased]: https://github.com/EdMUK/termmd/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/EdMUK/termmd/releases/tag/v0.1.0
