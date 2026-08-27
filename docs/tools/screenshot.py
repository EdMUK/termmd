#!/usr/bin/env python3
"""Render real termmd output to a PNG, for the README and the website.

This is not a mockup. It runs termmd on a pty, parses the escape sequences it
actually emits -- colours, attributes, cursor movement -- into a grid of cells,
and draws that grid with a monospace font. Images are decoded from the escape
stream -- kitty graphics, and sixel through the decoder below -- and composited
at the exact cell position termmd placed them, so what you see is what a terminal
shows.

Decoding sixel here is worth the hundred lines: it is the one protocol termmd
encodes itself, pixel by pixel and colour by colour, so a screenshot taken with
`--protocol sixel` is a picture of what its own encoder produced.

The pty is given pixel dimensions matching the font metrics, so termmd sizes its
images for the same cell geometry the renderer draws with.

Usage:
    python3 docs/tools/screenshot.py docs/showcase.md docs/images/screenshot.png
    python3 docs/tools/screenshot.py --pager --keys 't' docs/showcase.md out.png

Requires Pillow and a monospace font with box-drawing coverage.
"""

from __future__ import annotations

import argparse
import base64
import fcntl
import io
import os
import pty
import re
import select
import struct
import sys
import termios
import time
from dataclasses import dataclass, field, replace

from PIL import Image, ImageDraw, ImageFont

# --- appearance -------------------------------------------------------------

FONT_SIZE = 26
FONT_CANDIDATES = {
    "regular": ["~/Library/Fonts/HackNerdFontMono-Regular.ttf", "/System/Library/Fonts/Menlo.ttc"],
    "bold": ["~/Library/Fonts/HackNerdFontMono-Bold.ttf", "/System/Library/Fonts/Menlo.ttc"],
    "italic": ["~/Library/Fonts/HackNerdFontMono-Italic.ttf", "/System/Library/Fonts/Menlo.ttc"],
    "bolditalic": ["~/Library/Fonts/HackNerdFontMono-BoldItalic.ttf", "/System/Library/Fonts/Menlo.ttc"],
}
# Matches the dark theme termmd ships, so the frame and the content agree.
BACKGROUND = (0x14, 0x17, 0x1D)
FOREGROUND = (0xDC, 0xE3, 0xEA)
CHROME = (0x1D, 0x21, 0x29)
PADDING = 26
TITLE_BAR = 46
RADIUS = 12

# The 16 ANSI colours, in the same spirit as the built-in dark theme.
ANSI16 = [
    (0x22, 0x26, 0x2E), (0xE0, 0x6C, 0x63), (0x7B, 0xDC, 0x8A), (0xF0, 0xC9, 0x5C),
    (0x6F, 0xB3, 0xFF), (0xCF, 0xA4, 0xFF), (0x4F, 0xD0, 0xDD), (0xC7, 0xCE, 0xD6),
    (0x5B, 0x64, 0x72), (0xFF, 0x7A, 0x70), (0x9A, 0xEC, 0xA6), (0xFF, 0xDD, 0x7A),
    (0x8F, 0xC6, 0xFF), (0xDC, 0xBB, 0xFF), (0x7B, 0xE2, 0xEC), (0xFF, 0xFF, 0xFF),
]


def xterm256(index: int) -> tuple[int, int, int]:
    """The xterm 256-colour palette entry for an index."""
    if index < 16:
        return ANSI16[index]
    if index < 232:
        index -= 16
        levels = [0, 95, 135, 175, 215, 255]
        return (levels[index // 36], levels[(index // 6) % 6], levels[index % 6])
    grey = 8 + (index - 232) * 10
    return (grey, grey, grey)


# --- the cell grid ----------------------------------------------------------

@dataclass(frozen=True)
class Style:
    fg: tuple | None = None
    bg: tuple | None = None
    bold: bool = False
    dim: bool = False
    italic: bool = False
    underline: bool = False
    reverse: bool = False
    strike: bool = False


@dataclass
class Placement:
    row: int
    col: int
    cols: int
    rows: int
    data: bytes
    # kitty's z-index: below the text when negative, over it otherwise.
    z: int = 0


@dataclass
class Grid:
    rows: int
    cols: int
    cells: dict = field(default_factory=dict)
    images: list = field(default_factory=list)

    def put(self, row: int, col: int, char: str, style: Style) -> None:
        if 0 <= row < self.rows and 0 <= col < self.cols:
            self.cells[(row, col)] = (char, style)


class Terminal:
    """Just enough of a terminal to reproduce what termmd draws."""

    def __init__(self, rows: int, cols: int, cell: tuple[int, int] = (1, 1)):
        self.grid = Grid(rows, cols)
        self.cell = cell
        self.row = 0
        self.col = 0
        self.style = Style()
        self.saved = (0, 0)
        self._kitty: dict[str, list[bytes]] = {}
        self._kitty_open: str | None = None

    # -- SGR ---------------------------------------------------------------

    def sgr(self, params: str) -> None:
        codes = [int(p) if p else 0 for p in (params or "0").split(";")]
        i = 0
        s = self.style
        while i < len(codes):
            c = codes[i]
            if c == 0:
                s = Style()
            elif c == 1:
                s = replace(s, bold=True)
            elif c == 2:
                s = replace(s, dim=True)
            elif c == 3:
                s = replace(s, italic=True)
            elif c == 4:
                s = replace(s, underline=True)
            elif c == 7:
                s = replace(s, reverse=True)
            elif c == 9:
                s = replace(s, strike=True)
            elif c in (22,):
                s = replace(s, bold=False, dim=False)
            elif c == 23:
                s = replace(s, italic=False)
            elif c == 24:
                s = replace(s, underline=False)
            elif c == 27:
                s = replace(s, reverse=False)
            elif c == 29:
                s = replace(s, strike=False)
            elif 30 <= c <= 37:
                s = replace(s, fg=ANSI16[c - 30])
            elif 90 <= c <= 97:
                s = replace(s, fg=ANSI16[c - 90 + 8])
            elif 40 <= c <= 47:
                s = replace(s, bg=ANSI16[c - 40])
            elif 100 <= c <= 107:
                s = replace(s, bg=ANSI16[c - 100 + 8])
            elif c == 39:
                s = replace(s, fg=None)
            elif c == 49:
                s = replace(s, bg=None)
            elif c in (38, 48):
                target = "fg" if c == 38 else "bg"
                if i + 1 < len(codes) and codes[i + 1] == 2:
                    colour = tuple(codes[i + 2 : i + 5])
                    s = replace(s, **{target: colour})
                    i += 4
                elif i + 1 < len(codes) and codes[i + 1] == 5:
                    s = replace(s, **{target: xterm256(codes[i + 2])})
                    i += 2
            i += 1
        self.style = s

    # -- the parser --------------------------------------------------------

    def feed(self, data: bytes) -> None:
        i = 0
        n = len(data)
        while i < n:
            b = data[i]
            if b == 0x1B:
                i = self._escape(data, i)
                continue
            if b == 0x0A:  # newline
                self.row += 1
                i += 1
                continue
            if b == 0x0D:  # carriage return
                self.col = 0
                i += 1
                continue
            if b == 0x08:  # backspace
                self.col = max(0, self.col - 1)
                i += 1
                continue
            # A UTF-8 character.
            length = 1
            if b >= 0xF0:
                length = 4
            elif b >= 0xE0:
                length = 3
            elif b >= 0xC0:
                length = 2
            char = data[i : i + length].decode("utf-8", "replace")
            self.grid.put(self.row, self.col, char, self.style)
            # Double-width characters occupy the following cell too.
            self.col += 2 if _is_wide(char) else 1
            i += length

    def _escape(self, data: bytes, i: int) -> int:
        nxt = data[i + 1 : i + 2]
        if nxt == b"[":
            match = re.compile(rb"\x1b\[([0-9;?]*)([A-Za-z])").match(data, i)
            if not match:
                return i + 1
            params, final = match.group(1).decode(), match.group(2).decode()
            self._csi(params, final)
            return match.end()
        if nxt == b"]":
            end = data.find(b"\x07", i)
            st = data.find(b"\x1b\\", i)
            if st != -1 and (end == -1 or st < end):
                return st + 2
            return end + 1 if end != -1 else len(data)
        if nxt == b"_":
            end = data.find(b"\x1b\\", i)
            body = data[i + 2 : end if end != -1 else len(data)]
            self._kitty_chunk(body)
            return (end + 2) if end != -1 else len(data)
        if nxt == b"P":  # DCS, which for termmd means a sixel image.
            end = data.find(b"\x1b\\", i)
            self._sixel(data[i + 2 : end if end != -1 else len(data)])
            return (end + 2) if end != -1 else len(data)
        if nxt == b"7":
            self.saved = (self.row, self.col)
            return i + 2
        if nxt == b"8":
            self.row, self.col = self.saved
            return i + 2
        return i + 2

    def _csi(self, params: str, final: str) -> None:
        def arg(default: int = 1) -> int:
            try:
                return int(params.split(";")[0]) or default
            except (ValueError, IndexError):
                return default

        if final == "m":
            self.sgr(params)
        elif final == "A":
            self.row = max(0, self.row - arg())
        elif final == "B":
            self.row += arg()
        elif final == "C":
            self.col += arg()
        elif final == "D":
            self.col = max(0, self.col - arg())
        elif final == "H":
            parts = (params or "1;1").split(";")
            self.row = int(parts[0] or 1) - 1
            self.col = int(parts[1] or 1) - 1 if len(parts) > 1 else 0
        elif final == "K":
            for c in range(self.col, self.grid.cols):
                self.grid.cells.pop((self.row, c), None)
        elif final == "J":
            self.grid.cells.clear()

    # -- sixel -------------------------------------------------------------

    _SIXEL_COLOUR = re.compile(rb"(\d+)(?:;(\d+);(\d+);(\d+);(\d+))?")
    _SIXEL_REPEAT = re.compile(rb"(\d+)([\x3f-\x7e])")

    def _sixel(self, body: bytes) -> None:
        """Decode a sixel image and place it where the cursor is.

        Six pixels to a character, one bit each, over a palette of colour
        registers: `#Pc` selects one and `#Pc;2;r;g;b` defines it in percent,
        `!Pn` repeats the next sixel, `$` returns to the left of the band and
        `-` starts the next one. Bits that are zero are left alone, which is
        what carries transparency.
        """
        _, _, data = body.partition(b"q")
        # Raster attributes give the true size; without them there is nothing
        # to allocate, since sixel data itself never states a width.
        match = re.match(rb'"(\d+);(\d+);(\d+);(\d+)', data)
        if not match:
            return
        width, height = int(match.group(3)), int(match.group(4))
        if width <= 0 or height <= 0:
            return

        picture = Image.new("RGBA", (width, height), (0, 0, 0, 0))
        pixels = picture.load()
        palette: dict[int, tuple] = {}
        colour = 0
        x = band = 0
        i = match.end()

        def put(bits: int, at: int) -> None:
            rgb = palette.get(colour)
            if rgb is None or at >= width:
                return
            for row in range(6):
                if bits & (1 << row):
                    y = band * 6 + row
                    if y < height:
                        pixels[at, y] = (*rgb, 255)

        while i < len(data):
            byte = data[i]
            if byte == 0x23:  # '#'
                match = self._SIXEL_COLOUR.match(data, i + 1)
                if not match:
                    break
                colour = int(match.group(1))
                if match.group(2) == b"2":
                    # Components are percentages on the wire.
                    palette[colour] = tuple(
                        round(int(v) * 255 / 100) for v in match.group(3, 4, 5)
                    )
                i = match.end()
            elif byte == 0x21:  # '!'
                match = self._SIXEL_REPEAT.match(data, i + 1)
                if not match:
                    break
                bits = match.group(2)[0] - 0x3F
                for _ in range(int(match.group(1))):
                    put(bits, x)
                    x += 1
                i = match.end()
            elif byte == 0x24:  # '$', graphics carriage return
                x = 0
                i += 1
            elif byte == 0x2D:  # '-', graphics newline
                x = 0
                band += 1
                i += 1
            elif 0x3F <= byte <= 0x7E:
                put(byte - 0x3F, x)
                x += 1
                i += 1
            else:
                i += 1

        buffer = io.BytesIO()
        picture.save(buffer, "PNG")
        cell_w, cell_h = self.cell
        self.grid.images.append(
            Placement(
                self.row,
                self.col,
                -(-width // max(cell_w, 1)),
                -(-height // max(cell_h, 1)),
                buffer.getvalue(),
                # Sixel pixels go down before the text, so anything the pager
                # draws over them -- a panel, a search prompt -- stays legible,
                # as it does on a terminal that redraws those cells afterwards.
                z=-1,
            )
        )

    def _kitty_chunk(self, body: bytes) -> None:
        """Reassemble a kitty graphics transmission and record its placement."""
        if body[:1] != b"G":
            return
        control, _, payload = body[1:].partition(b";")
        keys = dict(kv.split(b"=", 1) for kv in control.split(b",") if b"=" in kv)
        action = keys.get(b"a", b"")

        if action == b"d":
            # The pager deletes every placement before drawing a frame. Without
            # honouring that, each frame's image piles on top of the last one
            # and a scrolled pager renders as a smear.
            self.grid.images.clear()
            self._kitty_open = None
            return

        if action == b"T":
            ident = keys.get(b"i", b"0").decode()
            self._kitty_open = ident
            self._kitty[ident] = [payload]
            self._pending = (
                self.row,
                self.col,
                int(keys.get(b"c", b"0")),
                int(keys.get(b"r", b"0")),
                int(keys.get(b"z", b"0")),
            )
        elif self._kitty_open is not None:
            self._kitty[self._kitty_open].append(payload)
        else:
            return

        if keys.get(b"m", b"0") == b"0":
            data = base64.b64decode(b"".join(self._kitty[self._kitty_open]))
            row, col, cols, rows, z = self._pending
            self.grid.images.append(Placement(row, col, cols, rows, data, z))
            self._kitty_open = None


def _is_wide(char: str) -> bool:
    import unicodedata

    return unicodedata.east_asian_width(char) in ("W", "F")


# --- running termmd ---------------------------------------------------------

def capture(
    argv: list[str],
    rows: int,
    cols: int,
    cell: tuple[int, int],
    keys: bytes,
    wait: float,
    delay: float = 1.5,
) -> bytes:
    """Runs a command on a pty of a known cell *and* pixel size."""
    master, slave = pty.openpty()
    # Reporting pixel dimensions is what lets termmd size images for the same
    # cell geometry this script draws with.
    fcntl.ioctl(
        slave,
        termios.TIOCSWINSZ,
        struct.pack("HHHH", rows, cols, cols * cell[0], rows * cell[1]),
    )
    pid = os.fork()
    if pid == 0:
        os.setsid()
        fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
        os.dup2(slave, 0)
        os.dup2(slave, 1)
        os.dup2(slave, 2)
        os.close(master)
        os.close(slave)
        env = dict(
            os.environ,
            TERM="xterm-kitty",
            TERM_PROGRAM="kitty",
            COLORTERM="truecolor",
            TERMMD_CONFIG="/nonexistent",
        )
        os.execvpe(argv[0], argv, env)
    os.close(slave)

    out = b""
    sent = not keys
    first_output = None
    deadline = time.time() + wait
    while time.time() < deadline:
        ready, _, _ = select.select([master], [], [], 0.1)
        if ready:
            try:
                chunk = os.read(master, 1 << 16)
            except OSError:
                break
            if not chunk:
                break
            out += chunk
            if first_output is None:
                first_output = time.time()
        # Wait before typing. termmd's first act is to query the terminal and
        # read the replies, and a keystroke sent during that window is eaten by
        # the probe rather than reaching the pager.
        if not sent and first_output and time.time() - first_output > delay:
            os.write(master, keys)
            sent = True
        if os.waitpid(pid, os.WNOHANG)[0]:
            break
    try:
        os.kill(pid, 9)
        os.waitpid(pid, 0)
    except (ProcessLookupError, ChildProcessError):
        pass
    os.close(master)
    return out


# --- drawing ----------------------------------------------------------------

def load_font(kind: str, size: int) -> ImageFont.FreeTypeFont:
    for path in FONT_CANDIDATES[kind]:
        expanded = os.path.expanduser(path)
        if os.path.exists(expanded):
            return ImageFont.truetype(expanded, size)
    raise SystemExit("no monospace font found; edit FONT_CANDIDATES")


def render(grid: Grid, title: str, out_path: str, size: int = FONT_SIZE) -> None:
    fonts = {kind: load_font(kind, size) for kind in FONT_CANDIDATES}
    cell_w = int(round(fonts["regular"].getlength("M")))
    ascent, descent = fonts["regular"].getmetrics()
    cell_h = ascent + descent

    width = grid.cols * cell_w + PADDING * 2
    height = grid.rows * cell_h + PADDING * 2 + TITLE_BAR
    image = Image.new("RGB", (width, height), CHROME)
    draw = ImageDraw.Draw(image)

    # Window chrome: a title bar naming the command that produced this.
    draw.rounded_rectangle([0, 0, width - 1, height - 1], RADIUS, fill=CHROME)
    for i, colour in enumerate([(0xFF, 0x5F, 0x57), (0xFE, 0xBC, 0x2E), (0x28, 0xC8, 0x40)]):
        x = 20 + i * 20
        draw.ellipse([x, TITLE_BAR // 2 - 6, x + 12, TITLE_BAR // 2 + 6], fill=colour)
    bar_font = load_font("regular", int(size * 0.62))
    draw.text(
        (width // 2, TITLE_BAR // 2),
        title,
        font=bar_font,
        fill=(0x8A, 0x94, 0xA3),
        anchor="mm",
    )

    body = [PADDING - 8, TITLE_BAR, width - PADDING + 8, height - PADDING + 8]
    draw.rounded_rectangle(body, 8, fill=BACKGROUND)

    origin = (PADDING, TITLE_BAR + PADDING // 2)

    def blend(a, b, t):
        return tuple(int(a[i] + (b[i] - a[i]) * t) for i in range(3))

    def paste(placement: Placement) -> None:
        picture = Image.open(io.BytesIO(placement.data)).convert("RGBA")
        box = (placement.cols * cell_w, placement.rows * cell_h)
        picture.thumbnail(box, Image.LANCZOS)
        image.paste(
            picture,
            (origin[0] + placement.col * cell_w, origin[1] + placement.row * cell_h),
            picture,
        )

    # Images below the text go down first, so the text lands on top of them.
    for placement in grid.images:
        if placement.z < 0:
            paste(placement)

    for (row, col), (char, style) in sorted(grid.cells.items()):
        fg = style.fg or FOREGROUND
        bg = style.bg or BACKGROUND
        if style.reverse:
            fg, bg = bg, fg
        if style.dim:
            fg = blend(fg, BACKGROUND, 0.42)

        x = origin[0] + col * cell_w
        y = origin[1] + row * cell_h
        if bg != BACKGROUND:
            span = 2 if _is_wide(char) else 1
            draw.rectangle([x, y, x + cell_w * span - 1, y + cell_h - 1], fill=bg)
        if char.strip():
            kind = "bolditalic" if style.bold and style.italic else (
                "bold" if style.bold else ("italic" if style.italic else "regular")
            )
            draw.text((x, y), char, font=fonts[kind], fill=fg)
        if style.underline:
            draw.line([x, y + ascent + 2, x + cell_w, y + ascent + 2], fill=fg, width=2)
        if style.strike:
            draw.line([x, y + ascent // 2 + 4, x + cell_w, y + ascent // 2 + 4], fill=fg, width=2)

    for placement in grid.images:
        if placement.z >= 0:
            paste(placement)

    image.save(out_path, optimize=True)
    print(f"wrote {out_path} ({image.width}x{image.height})")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source")
    parser.add_argument("output")
    parser.add_argument("--rows", type=int, default=34)
    parser.add_argument("--cols", type=int, default=92)
    parser.add_argument("--pager", action="store_true", help="capture the interactive pager")
    parser.add_argument("--keys", default="", help="keys to send once it has drawn")
    parser.add_argument("--title", default=None)
    parser.add_argument(
        "--protocol",
        default="kitty",
        help="image protocol to force: kitty and sixel render as pixels, blocks as text",
    )
    parser.add_argument("--binary", default="./target/release/termmd")
    parser.add_argument("--wait", type=float, default=6.0)
    parser.add_argument(
        "--delay", type=float, default=1.5, help="seconds to wait before typing"
    )
    args = parser.parse_args()

    argv = [os.path.abspath(args.binary)]
    argv += ["--images", args.protocol, "--color", "truecolor", "--width", str(args.cols)]
    argv += [] if args.pager else ["--no-pager"]
    argv += [args.source]

    # The cell size the renderer will draw with, so termmd can match it.
    probe = load_font("regular", FONT_SIZE)
    cell = (int(round(probe.getlength("M"))), sum(probe.getmetrics()))

    keys = args.keys.encode().decode("unicode_escape").encode("latin1")
    data = capture(argv, args.rows, args.cols, cell, keys, args.wait, args.delay)

    terminal = Terminal(args.rows, args.cols, cell)
    terminal.feed(data)
    title = args.title or f"termmd {args.source}"
    render(terminal.grid, title, args.output)


if __name__ == "__main__":
    main()
