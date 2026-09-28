// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa
//
// Framebuffer rasterization shared by the display-owning user-space
// programs (the setup wizard and the native terminal emulator). All
// renderable text is ASCII: the 8x8 console font has no non-ASCII glyphs.

use alloc::string::String;

extern crate alloc;

use crate::syscall::{self, map_flags, FbInfo};

// --- VGA palette (mirrors kernel framebuffer PALETTE_RGB) -----------------
pub const BLACK: u8 = 0;
pub const BLUE: u8 = 1;
pub const CYAN: u8 = 3;
pub const DGREY: u8 = 8;
pub const LGREY: u8 = 7;
pub const LGREEN: u8 = 10;
pub const WHITE: u8 = 15;
pub const YELLOW: u8 = 14;

pub const PALETTE_RGB: [(u8, u8, u8); 16] = [
    (0x00, 0x00, 0x00),
    (0x00, 0x00, 0xAA),
    (0x00, 0xAA, 0x00),
    (0x00, 0xAA, 0xAA),
    (0xAA, 0x00, 0x00),
    (0xAA, 0x00, 0xAA),
    (0xAA, 0x55, 0x00),
    (0xAA, 0xAA, 0xAA),
    (0x55, 0x55, 0x55),
    (0x55, 0x55, 0xFF),
    (0x55, 0xFF, 0x55),
    (0x55, 0xFF, 0xFF),
    (0xFF, 0x55, 0x55),
    (0xFF, 0x55, 0xFF),
    (0xFF, 0xFF, 0x55),
    (0xFF, 0xFF, 0xFF),
];

/// 8x8 cell glyphs for ASCII 0x20..=0x7E, copied from the kernel console
/// font (kernel/src/console.rs) so user-space renders identically.
pub const FONT: [[u8; 8]; 95] = {
    let rows = [
        // 0x20 space
        [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        [0x18, 0x3C, 0x3C, 0x18, 0x18, 0x00, 0x18, 0x00], // !
        [0x36, 0x36, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], // "
        [0x36, 0x36, 0x7F, 0x36, 0x7F, 0x36, 0x36, 0x00], // #
        [0x0C, 0x3E, 0x03, 0x1E, 0x30, 0x1F, 0x0C, 0x00], // $
        [0x00, 0x63, 0x33, 0x18, 0x0C, 0x66, 0x63, 0x00], // %
        [0x1C, 0x36, 0x1C, 0x6E, 0x3B, 0x33, 0x6E, 0x00], // &
        [0x06, 0x06, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00], // '
        [0x18, 0x0C, 0x06, 0x06, 0x06, 0x0C, 0x18, 0x00], // (
        [0x06, 0x0C, 0x18, 0x18, 0x18, 0x0C, 0x06, 0x00], // )
        [0x00, 0x66, 0x3C, 0xFF, 0x3C, 0x66, 0x00, 0x00], // *
        [0x00, 0x0C, 0x0C, 0x3F, 0x0C, 0x0C, 0x00, 0x00], // +
        [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C, 0x06], // ,
        [0x00, 0x00, 0x00, 0x3F, 0x00, 0x00, 0x00, 0x00], // -
        [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C, 0x00], // .
        [0x60, 0x30, 0x18, 0x0C, 0x06, 0x03, 0x01, 0x00], // /
        [0x3E, 0x63, 0x73, 0x7B, 0x6F, 0x67, 0x3E, 0x00], // 0
        [0x0C, 0x0E, 0x0C, 0x0C, 0x0C, 0x0C, 0x3F, 0x00], // 1
        [0x1E, 0x33, 0x30, 0x1C, 0x06, 0x33, 0x3F, 0x00], // 2
        [0x1E, 0x33, 0x30, 0x1C, 0x30, 0x33, 0x1E, 0x00], // 3
        [0x38, 0x3C, 0x36, 0x33, 0x7F, 0x30, 0x78, 0x00], // 4
        [0x3F, 0x03, 0x1F, 0x30, 0x30, 0x33, 0x1E, 0x00], // 5
        [0x1C, 0x06, 0x03, 0x1F, 0x33, 0x33, 0x1E, 0x00], // 6
        [0x3F, 0x33, 0x30, 0x18, 0x0C, 0x0C, 0x0C, 0x00], // 7
        [0x1E, 0x33, 0x33, 0x1E, 0x33, 0x33, 0x1E, 0x00], // 8
        [0x1E, 0x33, 0x33, 0x3E, 0x30, 0x18, 0x0E, 0x00], // 9
        [0x00, 0x0C, 0x0C, 0x00, 0x0C, 0x0C, 0x00, 0x00], // :
        [0x00, 0x0C, 0x0C, 0x00, 0x0C, 0x0C, 0x06, 0x00], // ;
        [0x18, 0x0C, 0x06, 0x03, 0x06, 0x0C, 0x18, 0x00], // <
        [0x00, 0x00, 0x3F, 0x00, 0x3F, 0x00, 0x00, 0x00], // =
        [0x06, 0x0C, 0x18, 0x30, 0x18, 0x0C, 0x06, 0x00], // >
        [0x1E, 0x33, 0x30, 0x18, 0x0C, 0x00, 0x0C, 0x00], // ?
        [0x3E, 0x63, 0x7B, 0x7B, 0x7B, 0x03, 0x1E, 0x00], // @
        [0x0C, 0x1E, 0x33, 0x33, 0x3F, 0x33, 0x33, 0x00], // A
        [0x3F, 0x66, 0x66, 0x3E, 0x66, 0x66, 0x3F, 0x00], // B
        [0x3C, 0x66, 0x03, 0x03, 0x03, 0x66, 0x3C, 0x00], // C
        [0x1F, 0x36, 0x66, 0x66, 0x66, 0x36, 0x1F, 0x00], // D
        [0x7F, 0x46, 0x16, 0x1E, 0x16, 0x46, 0x7F, 0x00], // E
        [0x7F, 0x46, 0x16, 0x1E, 0x16, 0x06, 0x0F, 0x00], // F
        [0x3C, 0x66, 0x03, 0x03, 0x73, 0x66, 0x7C, 0x00], // G
        [0x33, 0x33, 0x33, 0x3F, 0x33, 0x33, 0x33, 0x00], // H
        [0x1E, 0x0C, 0x0C, 0x0C, 0x0C, 0x0C, 0x1E, 0x00], // I
        [0x78, 0x30, 0x30, 0x30, 0x33, 0x33, 0x1E, 0x00], // J
        [0x67, 0x66, 0x36, 0x1E, 0x36, 0x66, 0x67, 0x00], // K
        [0x0F, 0x06, 0x06, 0x06, 0x46, 0x66, 0x7F, 0x00], // L
        [0x63, 0x77, 0x7F, 0x7F, 0x6B, 0x63, 0x63, 0x00], // M
        [0x63, 0x67, 0x6F, 0x7B, 0x73, 0x63, 0x63, 0x00], // N
        [0x1C, 0x36, 0x63, 0x63, 0x63, 0x36, 0x1C, 0x00], // O
        [0x3F, 0x66, 0x66, 0x3E, 0x06, 0x06, 0x0F, 0x00], // P
        [0x1E, 0x33, 0x33, 0x33, 0x3B, 0x1E, 0x38, 0x00], // Q
        [0x3F, 0x66, 0x66, 0x3E, 0x36, 0x66, 0x67, 0x00], // R
        [0x1E, 0x33, 0x07, 0x0E, 0x38, 0x33, 0x1E, 0x00], // S
        [0x3F, 0x2D, 0x0C, 0x0C, 0x0C, 0x0C, 0x1E, 0x00], // T
        [0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x3F, 0x00], // U
        [0x33, 0x33, 0x33, 0x33, 0x33, 0x1E, 0x0C, 0x00], // V
        [0x63, 0x63, 0x63, 0x6B, 0x7F, 0x77, 0x63, 0x00], // W
        [0x63, 0x63, 0x36, 0x1C, 0x1C, 0x36, 0x63, 0x00], // X
        [0x33, 0x33, 0x33, 0x1E, 0x0C, 0x0C, 0x1E, 0x00], // Y
        [0x7F, 0x63, 0x31, 0x18, 0x4C, 0x66, 0x7F, 0x00], // Z
        [0x1E, 0x06, 0x06, 0x06, 0x06, 0x06, 0x1E, 0x00], // [
        [0x03, 0x06, 0x0C, 0x18, 0x30, 0x60, 0x40, 0x00], // backslash
        [0x1E, 0x18, 0x18, 0x18, 0x18, 0x18, 0x1E, 0x00], // ]
        [0x08, 0x1C, 0x36, 0x63, 0x00, 0x00, 0x00, 0x00], // ^
        [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF], // _
        [0x0C, 0x0C, 0x18, 0x00, 0x00, 0x00, 0x00, 0x00], // `
        [0x00, 0x00, 0x1E, 0x30, 0x3E, 0x33, 0x6E, 0x00], // a
        [0x07, 0x06, 0x06, 0x3E, 0x66, 0x66, 0x3B, 0x00], // b
        [0x00, 0x00, 0x1E, 0x33, 0x03, 0x33, 0x1E, 0x00], // c
        [0x38, 0x30, 0x30, 0x3E, 0x33, 0x33, 0x6E, 0x00], // d
        [0x00, 0x00, 0x1E, 0x33, 0x3F, 0x03, 0x1E, 0x00], // e
        [0x1C, 0x36, 0x06, 0x0F, 0x06, 0x06, 0x0F, 0x00], // f
        [0x00, 0x00, 0x6E, 0x33, 0x33, 0x3E, 0x30, 0x1F], // g
        [0x07, 0x06, 0x36, 0x6E, 0x66, 0x66, 0x67, 0x00], // h
        [0x0C, 0x00, 0x0E, 0x0C, 0x0C, 0x0C, 0x1E, 0x00], // i
        [0x30, 0x00, 0x30, 0x30, 0x30, 0x33, 0x1E, 0x00], // j
        [0x07, 0x06, 0x66, 0x36, 0x1E, 0x36, 0x67, 0x00], // k
        [0x0E, 0x0C, 0x0C, 0x0C, 0x0C, 0x0C, 0x1E, 0x00], // l
        [0x00, 0x00, 0x33, 0x7F, 0x7F, 0x6B, 0x63, 0x00], // m
        [0x00, 0x00, 0x1F, 0x33, 0x33, 0x33, 0x33, 0x00], // n
        [0x00, 0x00, 0x1E, 0x33, 0x33, 0x33, 0x1E, 0x00], // o
        [0x00, 0x00, 0x3B, 0x66, 0x66, 0x3E, 0x06, 0x0F], // p
        [0x00, 0x00, 0x6E, 0x33, 0x33, 0x3E, 0x30, 0x78], // q
        [0x00, 0x00, 0x3B, 0x6E, 0x66, 0x06, 0x0F, 0x00], // r
        [0x00, 0x00, 0x3E, 0x03, 0x1E, 0x30, 0x1F, 0x00], // s
        [0x08, 0x0C, 0x3E, 0x0C, 0x0C, 0x2C, 0x18, 0x00], // t
        [0x00, 0x00, 0x33, 0x33, 0x33, 0x33, 0x6E, 0x00], // u
        [0x00, 0x00, 0x33, 0x33, 0x33, 0x1E, 0x0C, 0x00], // v
        [0x00, 0x00, 0x63, 0x63, 0x6B, 0x7F, 0x36, 0x00], // w
        [0x00, 0x00, 0x63, 0x36, 0x1C, 0x36, 0x63, 0x00], // x
        [0x00, 0x00, 0x33, 0x33, 0x33, 0x3E, 0x30, 0x1F], // y
        [0x00, 0x00, 0x3F, 0x19, 0x0C, 0x26, 0x3F, 0x00], // z
        [0x38, 0x0C, 0x0C, 0x07, 0x0C, 0x0C, 0x38, 0x00], // {
        [0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x00], // |
        [0x07, 0x0C, 0x0C, 0x38, 0x0C, 0x0C, 0x07, 0x00], // }
        [0x00, 0x00, 0x6E, 0x3B, 0x00, 0x00, 0x00, 0x00], // ~
    ];
    let mut out = [[0u8; 8]; 95];
    let mut i = 0;
    while i < 95 {
        out[i] = rows[i];
        i += 1;
    }
    out
};

/// Pack one 8-bit channel into the format's mask; mirrors the kernel's
/// `PixelFormat::fit_channel` exactly so colors match the console output.
fn fit_channel(value: u8, pos: u32, size: u32) -> u32 {
    let v = value as u32;
    let mask = (1u32 << size) - 1;
    let scaled = if size >= 8 {
        v
    } else {
        // Round to nearest so mid-tones don't systematically darken.
        (v * mask + 127) / 255
    };
    (scaled & mask) << pos
}

/// A direct view of the framebuffer. All draws are volatile writes to the
/// mapped MMIO range (see `Display::new`).
pub struct Display {
    /// Virtual address of the first visible pixel (mapping base + byte
    /// offset reported by the kernel).
    pub first: usize,
    pub width: usize,
    pub height: usize,
    pub pitch: usize,
    pub pxsize: usize,
    pub red_pos: u32,
    pub red_size: u32,
    pub green_pos: u32,
    pub green_size: u32,
    pub blue_pos: u32,
    pub blue_size: u32,
}

impl Display {
    /// Query the framebuffer geometry and map the (registered) device range
    /// into this address space so we can paint it directly.
    pub fn new() -> Option<Display> {
        let mut info = FbInfo::default();
        syscall::fb_info(&mut info).ok()?;
        if info.width == 0 || info.height == 0 || info.pitch == 0 {
            return None;
        }
        let pxsize = (info.bpp / 8) as usize;
        if pxsize != 3 && pxsize != 4 {
            return None;
        }
        let total = info.offset as u64 + info.size;
        let frames = total.div_ceil(4096);
        let flags = map_flags::WRITABLE
            | map_flags::WRITE_THROUGH
            | map_flags::CACHE_DISABLE
            | map_flags::NO_EXECUTE;
        let base = syscall::map_phys(info.phys, frames, flags).ok()?;
        Some(Display {
            first: base + info.offset as usize,
            width: info.width as usize,
            height: info.height as usize,
            pitch: info.pitch as usize,
            pxsize,
            red_pos: info.red_pos,
            red_size: info.red_size,
            green_pos: info.green_pos,
            green_size: info.green_size,
            blue_pos: info.blue_pos,
            blue_size: info.blue_size,
        })
    }

    /// Packed native color of one of the sixteen VGA palette slots.
    pub fn packed(&self, idx: u8) -> u32 {
        let (r, g, b) = PALETTE_RGB[(idx & 0x0F) as usize];
        self.pack_rgb(r, g, b)
    }

    pub fn pack_rgb(&self, r: u8, g: u8, b: u8) -> u32 {
        fit_channel(r, self.red_pos, self.red_size)
            | fit_channel(g, self.green_pos, self.green_size)
            | fit_channel(b, self.blue_pos, self.blue_size)
    }

    fn px_addr(&self, x: usize, y: usize) -> usize {
        self.first + y * self.pitch + x * self.pxsize
    }

    pub fn put_pixel(&self, x: usize, y: usize, color: u32) {
        if x >= self.width || y >= self.height {
            return;
        }
        let p = self.px_addr(x, y) as *mut u8;
        // SAFETY: the mapping covers `pitch * height` writable bytes starting
        // at `first`; these addresses stay in range.
        unsafe {
            match self.pxsize {
                4 => core::ptr::write_volatile(p as *mut u32, color),
                3 => {
                    let b = color.to_le_bytes();
                    core::ptr::write_volatile(p, b[0]);
                    core::ptr::write_volatile(p.add(1), b[1]);
                    core::ptr::write_volatile(p.add(2), b[2]);
                }
                _ => unreachable!(),
            }
        }
    }

    pub fn fill_rect(&self, x: usize, y: usize, w: usize, h: usize, color: u32) {
        let x2 = (x + w).min(self.width);
        let y2 = (y + h).min(self.height);
        let mut yy = y;
        while yy < y2 {
            let mut xx = x;
            while xx < x2 {
                self.put_pixel(xx, yy, color);
                xx += 1;
            }
            yy += 1;
        }
    }

    /// Blit a 32bpp toolkit-format (`0x00RRGGBB`) image rectangle into the
    /// framebuffer at `(dx, dy)`, converting channel-wise to the native pixel
    /// format. This is the compositor's damage-flush path.
    pub fn put_image(
        &self,
        dx: usize,
        dy: usize,
        src: &[u32],
        stride: usize,
        w: usize,
        h: usize,
    ) {
        let x0 = dx.min(self.width);
        let y0 = dy.min(self.height);
        let x1 = (dx + w).min(self.width);
        let y1 = (dy + h).min(self.height);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let mut yy = y0;
        while yy < y1 {
            let row_src = (yy - y0) * stride;
            let mut xx = x0;
            while xx < x1 {
                let px = src[row_src + (xx - x0)];
                self.put_pixel(xx, yy, self.pack_xrgb(px));
                xx += 1;
            }
            yy += 1;
        }
    }

    /// Convert a toolkit-format pixel (`0x00RRGGBB`) to this display's native
    /// packed format.
    fn pack_xrgb(&self, px: u32) -> u32 {
        self.pack_rgb((px >> 16) as u8, (px >> 8) as u8, px as u8)
    }

    pub fn clear(&self, color: u32) {
        self.fill_rect(0, 0, self.width, self.height, color);
    }

    pub fn cols(&self) -> usize {
        self.width / 8
    }

    pub fn rows(&self) -> usize {
        self.height / 8
    }

    /// Draw one ASCII character at cell (col, row) with fg/bg palette colors.
    pub fn put_char(&self, col: usize, row: usize, ch: u8, fg: u8, bg: u8) {
        let cols = self.cols();
        let rows = self.rows();
        if col >= cols || row >= rows {
            return;
        }
        let fx = col * 8;
        let fy = row * 8;
        let glyph = if (0x20..=0x7E).contains(&ch) {
            FONT[(ch - 0x20) as usize]
        } else {
            FONT[('?' as u8 - 0x20) as usize]
        };
        let fgc = self.packed(fg);
        let bgc = self.packed(bg);
        // The kernel font stores bit 0 as the leftmost pixel (see
        // `kernel/src/console.rs`); use the same bit order so glyphs are not
        // mirror-flipped on the display.
        for gy in 0..8 {
            let rowbits = glyph[gy];
            for gx in 0..8 {
                if rowbits & (1 << gx) != 0 {
                    self.put_pixel(fx + gx, fy + gy, fgc);
                } else {
                    self.put_pixel(fx + gx, fy + gy, bgc);
                }
            }
        }
    }

    /// Draw `text` starting at cell column `col` of row `row`.
    pub fn put_text(&self, col: usize, row: usize, text: &str, fg: u8, bg: u8) {
        let cols = self.cols();
        let mut c = col;
        for &b in text.as_bytes() {
            if c >= cols {
                break;
            }
            self.put_char(c, row, b, fg, bg);
            c += 1;
        }
    }

    /// Fill `n` cells with spaces.
    pub fn blank(&self, col: usize, row: usize, n: usize, bg: u8) {
        let cols = self.cols();
        let mut c = col;
        let mut left = n;
        while left > 0 && c < cols {
            self.put_char(c, row, b' ', 0, bg);
            c += 1;
            left -= 1;
        }
    }

    /// Center `text` within cell columns col0..=col1.
    pub fn center(&self, col0: usize, col1: usize, row: usize, text: &str, fg: u8, bg: u8) {
        let width = col1 + 1 - col0;
        if text.len() >= width {
            self.put_text(col0, row, text, fg, bg);
        } else {
            self.put_text(col0 + (width - text.len()) / 2, row, text, fg, bg);
        }
    }

    /// Bordered panel with a blue title bar.
    pub fn panel(&self, col0: usize, row0: usize, col1: usize, row1: usize, title: &str) {
        if col1 <= col0 + 1 || row1 <= row0 + 1 {
            return;
        }
        let fg = LGREY;
        let bg = BLACK;
        for c in col0 + 1..col1 {
            self.put_char(c, row0, b'-', fg, bg);
            self.put_char(c, row1, b'-', fg, bg);
        }
        for r in row0 + 1..row1 {
            self.put_char(col0, r, b'|', fg, bg);
            self.put_char(col1, r, b'|', fg, bg);
        }
        self.put_char(col0, row0, b'+', fg, bg);
        self.put_char(col1, row0, b'+', fg, bg);
        self.put_char(col0, row1, b'+', fg, bg);
        self.put_char(col1, row1, b'+', fg, bg);
        // Title bar.
        self.put_text(col0 + 1, row0, title, WHITE, BLUE);
        for c in col0 + 2 + title.len()..col1 {
            self.put_char(c, row0, b'-', fg, bg);
        }
    }

    /// Menu inside a panel; the selected row is inverted. Items are laid out
    /// every two cell rows so the text does not press against the neighbour
    /// (each glyph is an 8px-tall cell). A zero hotkey renders without a
    /// number.
    pub fn menu(
        &self,
        col0: usize,
        row0: usize,
        col1: usize,
        row1: usize,
        title: &str,
        items: &[(u8, &str, &str)],
        sel: usize,
        hint: &str,
    ) {
        self.panel(col0, row0, col1, row1, title);
        let inner_left = col0 + 3;
        let inner_right = col1 - 1;
        let mut row = row0 + 2;
        for (i, (hotkey, label, value)) in items.iter().enumerate() {
            if row >= row1 - 1 {
                break;
            }
            let selected = i == sel;
            let (fg, bg) = if selected { (WHITE, BLUE) } else { (LGREY, BLACK) };
            self.blank(inner_left, row, inner_right - inner_left, bg);
            let mut text_col = inner_left;
            if *hotkey != 0 {
                self.put_char(text_col, row, *hotkey, if selected { WHITE } else { YELLOW }, bg);
                self.put_char(text_col + 1, row, b'.', fg, bg);
                text_col += 3;
            }
            // Left-align the label within the space left over by the value.
            let avail = inner_right - text_col;
            let vlen = value.len().min(avail);
            let space = avail.saturating_sub(vlen);
            let lbl = if label.len() > space {
                &label[..space]
            } else {
                label
            };
            self.put_text(text_col, row, lbl, fg, bg);
            if vlen > 0 {
                self.put_text(
                    inner_right - vlen,
                    row,
                    value,
                    if selected { WHITE } else { YELLOW },
                    bg,
                );
            }
            row += 2;
        }
        if row < row1 - 1 && !hint.is_empty() {
            self.blank(inner_left, row, inner_right - inner_left, DGREY);
            self.put_text(inner_left, row, hint, WHITE, DGREY);
        }
    }

    /// Wrapped text lines in a panel.
    pub fn text_block(&self, col0: usize, row0: usize, col1: usize, row1: usize, title: &str, lines: &[String]) {
        self.panel(col0, row0, col1, row1, title);
        let inner_left = col0 + 3;
        let width = col1 - 1 - inner_left;
        let mut row = row0 + 2;
        for line in lines {
            if row >= row1 - 1 {
                break;
            }
            let text = if line.len() > width { &line[..width] } else { line.as_str() };
            self.put_text(inner_left, row, text, LGREY, BLACK);
            row += 1;
            // A blank line in the source becomes a real blank row on screen,
            // and every line gets a little more vertical air between blocks.
        }
    }
}