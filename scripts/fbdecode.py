#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Harsh Nikarsa

"""Read text back out of an fbterm screendump by matching cells to glyphs.

    scripts/fbdecode.py <screendump.ppm> [fbterm-source-dir]

scripts/fbshot.py answers "what does this screen say", by recognising the shapes
in the framebuffer. That is the right question for the installer's own screens,
which are drawn with a font this script has never seen and only has to read
loosely.

It is the wrong question for fbterm. When the terminal draws text with a broken
font, OCR reports garbage -- and garbage is indistinguishable from a broken
framebuffer, a wrong palette, a corrupted pixmap, or text that was never drawn.
That is exactly the ambiguity that made a font bug look like a rendering bug: the
screenshot said "static", and there was no way to tell from the screenshot whether
the glyphs were wrong or the pixels were.

So this does not recognise shapes. It reads the font's own glyph table out of the
fbterm source and matches each cell's pixels against it, nearest match by
Hamming distance. That turns the question into one with a decidable answer: if
the cells match the glyphs that were supposed to be drawn, the font and the
framebuffer are both fine and whatever is unreadable is somewhere else. If they
do not, the mismatch distance says how far off they are, which separates "shifted
by a row" from "entirely different data".

The cost is that it only reads fbterm's screen, in fbterm's cell geometry, with
fbterm's font. That is a narrower tool than fbshot.py and deliberately so: it is
the one that can adjudicate a font bug, and a tool that works everywhere cannot.
"""

import os
import re
import sys

# fbterm's cell geometry, from ports/fbterm/patches/0004. The glyph is 8x8
# placed `CELL_TOP` rows down inside a taller cell, so lines are spaced out and a
# descender has somewhere to go. Duplicated here rather than parsed out of the
# patch because the patch is a diff and these are four numbers in it.
CELL_W = 8
CELL_H = 16
GLYPH_H = 8
CELL_TOP = 4

# Above this many differing bits a cell is reported as `?` rather than as its
# nearest glyph. The threshold is not a guess: a correct cell scores 0, and a
# cell that differs by a whole row of pixels scores about 8, so anything past
# this is a cell whose bits do not correspond to any glyph at all.
MAX_DISTANCE = 14

# A mirrored cell still scores 0 against a mirrored glyph, which is the whole
# problem: matching pixels against a table cannot tell a correct render from a
# consistently flipped one unless the table is read the same way the renderer
# wrote it. So the check that matters is not "does this match" but "does this
# match *and* would it also match mirrored", and a cell that satisfies both is
# symmetric and tells us nothing. Asymmetric cells are where a flip shows up, and
# they are common enough in ordinary text to be worth testing for on every run.
SYMMETRIC_TOLERANCE = 0

# Pixels this much brighter than black count as "ink". fbterm draws text in
# palette colours on a black background, and the sum of the three channels is used
# so the test does not care which channel a given colour happens to weight.
INK_THRESHOLD = 300


def load_ppm(path):
    """Read a binary PPM, skipping the comment lines `screendump` may emit."""
    data = open(path, "rb").read()
    fields, i = [], 0
    while len(fields) < 4:
        while data[i:i + 1].isspace():
            i += 1
        if data[i:i + 1] == b"#":
            while data[i:i + 1] not in (b"\n", b""):
                i += 1
            continue
        j = i
        while not data[j:j + 1].isspace():
            j += 1
        fields.append(data[i:j])
        i = j
    i += 1  # single whitespace byte after the maxval
    width, height = int(fields[1]), int(fields[2])
    return width, height, data[i:i + width * height * 3]


def load_font(fbterm_src):
    """The 8x8 ASCII glyph table, parsed out of the port's font.cpp."""
    path = os.path.join(fbterm_src, "src", "font.cpp")
    try:
        src = open(path, encoding="utf-8").read()
    except OSError:
        sys.exit("cannot read %s -- pass the fbterm source directory" % path)
    m = re.search(r"static const u8 ascii8x8\[95\]\[GLYPH_H\] = \{(.*?)\n\};", src, re.S)
    if not m:
        sys.exit("no ascii8x8 table in %s" % path)
    glyphs = []
    for row in re.findall(r"\{([^{}]*)\}", m.group(1)):
        vals = [int(x, 16) for x in re.findall(r"0x([0-9A-Fa-f]{2})", row)]
        if len(vals) == GLYPH_H:
            glyphs.append(vals)
    if len(glyphs) != 95:
        sys.exit("parsed %d glyphs from %s, expected 95" % (len(glyphs), path))
    return glyphs


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    ppm = sys.argv[1]
    fbterm_src = sys.argv[2] if len(sys.argv) > 2 else "build/fbterm-src"
    width, height, px = load_ppm(ppm)
    font = load_font(fbterm_src)

    def ink(x, y):
        o = (y * width + x) * 3
        return px[o] + px[o + 1] + px[o + 2] > INK_THRESHOLD

    def cell_bits(cx, cy):
        # Bit 0 is the leftmost pixel, matching the font data and both renderers
        # that already draw it (kernel/src/console.rs, user/nutcracker-rt/src/fb.rs).
        return [sum((1 << c) for c in range(CELL_W) if ink(cx + c, cy + CELL_TOP + r))
                for r in range(GLYPH_H)]

    def distance(a, b):
        return sum(bin(x ^ y).count("1") for x, y in zip(a, b))

    def mirror(bits):
        return [sum(((b >> c) & 1) << (CELL_W - 1 - c) for c in range(CELL_W))
                for b in bits]

    flipped = 0
    for cy in range(0, height - CELL_H, CELL_H):
        line = ""
        for ci in range(width // CELL_W):
            bits = cell_bits(ci * CELL_W, cy)
            if not any(bits):
                line += " "
                continue
            best = min(range(95), key=lambda i: distance(bits, font[i]))
            d = distance(bits, font[best])
            if d < MAX_DISTANCE:
                # A mirrored cell still scores 0 against a mirrored glyph, which
                # is the trap this check exists to close: matching pixels against
                # a table cannot tell a correct render from a consistently
                # flipped one unless the table is read the way the renderer wrote
                # it. So the test is whether the cell *also* matches when
                # mirrored, and whether that match is a different glyph. Both
                # together mean a horizontal flip -- legible text, correct
                # spacing, every character backwards, and invisible to every
                # other check here.
                mirrored = mirror(bits)
                mbest = min(range(95), key=lambda i: distance(mirrored, font[i]))
                if (distance(mirrored, font[mbest]) <= SYMMETRIC_TOLERANCE
                        and font[best] != font[mbest]):
                    flipped += 1
            line += chr(0x20 + best) if d < MAX_DISTANCE else "?"
        line = line.rstrip()
        if line:
            print("%3d |%s" % (cy, line))

    if flipped:
        print()
        print("warning: %d cell(s) match a mirrored glyph exactly while the real"
              " glyph differs; the font is being drawn flipped" % flipped)


if __name__ == "__main__":
    main()
