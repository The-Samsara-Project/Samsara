#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-or-later
# Copyright (C) 2026 Harsh Nikarsa

"""Read a QEMU framebuffer screendump back as text.

The installer draws its menus and the self-test results into the framebuffer and
reports nothing to the serial console, so a headless `make run` produces a picture
of the results rather than the results. That is not much use: a test can fail and
the only evidence is a word on a screen nobody reads.

So this decodes the screenshot. The font is not guessed at -- it is parsed out of
user/nutcracker-rt/src/fb.rs, the same table `put_char` draws from, and the bit
order is that table's own (bit 0 leftmost, per the comment there). Reading the
font from the source rather than hardcoding a copy means the two cannot drift: if
a glyph changes, the decoder changes with it.

Usage:
    scripts/fbshot.py shots/00-diag.ppm
    scripts/fbshot.py --invert shots/00-diag.ppm      # for light-on-dark UIs
"""

import re
import sys
from pathlib import Path

FONT_SRC = Path(__file__).resolve().parent.parent / "user/nutcracker-rt/src/fb.rs"

CELL_W = CELL_H = 8
# How far a decoded cell may be from the best-matching glyph and still count as
# that glyph. Exactly 0 would mean only a pixel-perfect match decodes, and
# antialiasing or a one-bit palette mistake would blank the whole screen;
# anything loose enough to matter would turn a block of text into plausible
# nonsense, which is worse than leaving it blank. A few bits is the honest
# middle: enough for noise, not enough to confuse a letter with its neighbour.
MAX_DIST = 6


def load_font():
    """Parse the 8x8 glyph table out of the framebuffer's own source."""
    src = FONT_SRC.read_text()
    start = src.index("pub const FONT")
    body = src[start:]
    # The table is built by a `const` block wrapping a `let rows = [ ... ]`, so
    # each glyph is one `[0x.., ...]` literal, optionally commented with the
    # character it draws.
    glyphs = []
    # The trailing comma is optional on the *last* value: the source writes
    # `[0x00, 0x18, ..., 0x00]` with no comma before the bracket, so a pattern
    # that demands one after every value matches nothing at all. That failure is
    # silent -- zero glyphs, no error -- which is why the count is checked below.
    pat = r"\[((?:0x[0-9A-Fa-f]{2}(?:,\s*)?){%d})\]" % CELL_H
    for m in re.finditer(pat, body):
        glyphs.append([int(v, 16) for v in re.findall(r"0x([0-9A-Fa-f]{2})", m.group(1))])
        if len(glyphs) == 95:
            break
    if len(glyphs) != 95:
        raise SystemExit(f"fbshot: parsed {len(glyphs)} glyphs from {FONT_SRC}, expected 95")
    return glyphs


def read_ppm(path):
    data = Path(path).read_bytes()
    if not data.startswith(b"P6"):
        raise SystemExit("fbshot: not a binary PPM (P6); screendump -format ppm only")
    # Header: magic, width, height, maxval -- whitespace separated, '#' comments
    # allowed, and the pixel data begins right after the maxval's newline.
    fields, i = [], 2
    while len(fields) < 3:
        while i < len(data) and data[i : i + 1].isspace():
            i += 1
        if data[i : i + 1] == b"#":
            while data[i : i + 1] not in (b"\n", b""):
                i += 1
            continue
        j = i
        while j < len(data) and not data[j : j + 1].isspace():
            j += 1
        fields.append(int(data[i:j]))
        i = j
    i += 1  # exactly one whitespace byte terminates the maxval
    w, h, _maxval = fields
    return w, h, data[i : i + w * h * 3]


def decode(ppm, invert=False):
    glyphs = load_font()
    w, h, px = ppm
    cols, rows = w // CELL_W, h // CELL_H
    out = []
    for r in range(rows):
        line = []
        for c in range(cols):
            bits = []
            for gy in range(CELL_H):
                b = 0
                for gx in range(CELL_W):
                    o = ((r * CELL_H + gy) * w + (c * CELL_W + gx)) * 3
                    # A cell is "ink" if any of its pixels differs from the
                    # cell's own background, taken as the most common colour in
                    # the cell. That makes the decoder independent of the
                    # palette: it works for light-on-dark and dark-on-light
                    # without being told which is which.
                    bits.append(px[o : o + 3])
            bg = max(set(bits), key=bits.count)
            cell = []
            for gy in range(CELL_H):
                b = 0
                for gx in range(CELL_W):
                    p = bits[gy * CELL_W + gx]
                    on = p != bg
                    if invert:
                        on = not on
                    if on:
                        b |= 1 << gx
                cell.append(b)
            best, bestd = 0x20, 1 << 30
            for gi, g in enumerate(glyphs):
                d = sum(bin(a ^ b).count("1") for a, b in zip(cell, g))
                if d < bestd:
                    bestd, best = d, 0x20 + gi
            line.append(chr(best) if bestd <= MAX_DIST else " ")
        out.append("".join(line).rstrip())
    return out


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    invert = "--invert" in sys.argv
    if len(args) != 1:
        raise SystemExit(__doc__)
    for line in decode(read_ppm(args[0]), invert):
        print(line.rstrip())


if __name__ == "__main__":
    main()
