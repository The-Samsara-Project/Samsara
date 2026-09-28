// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! Linear framebuffer driver.
//!
//! The bootloader's Multiboot2 framebuffer tag describes a linear graphics
//! mode; this driver maps that physical range into the kernel address space
//! and exposes pixel-level drawing primitives plus a precomputed palette for
//! the classic sixteen VGA/ANSI colors.
//!
//! Only direct-RGB 24/32 bpp modes are supported. Without one, the console
//! falls back to the debug UART.

use crate::memory::{pmm, vmm};
use crate::multiboot2::FramebufferInfo;
use crate::sync::OnceCell;

/// Virtual address the bootloader's framebuffer is mapped at.
///
/// Must not collide with anything the boot stub pre-maps: it fills the first
/// GiB of physical memory (2 MiB pages) at `0xffff_ff80_0000_0000`
/// (`PML4[511]`, `PDPT[0]`), the kernel image/higher-half physmap at
/// `0xffff_ffff_8000_0000`, and the kernel heap at `0xffff_ff00_0000_0000`.
/// This window lives under `PML4[509]`, which nothing else maps.
const FRAMEBUFFER_VIRT: usize = 0xFFFF_FE80_0000_0000;

/// The sixteen classic VGA/ANSI colors as 8-bit RGB triples.
const PALETTE_RGB: [(u8, u8, u8); 16] = [
    (0x00, 0x00, 0x00), //  0 black
    (0x00, 0x00, 0xAA), //  1 blue
    (0x00, 0xAA, 0x00), //  2 green
    (0x00, 0xAA, 0xAA), //  3 cyan
    (0xAA, 0x00, 0x00), //  4 red
    (0xAA, 0x00, 0xAA), //  5 magenta
    (0xAA, 0x55, 0x00), //  6 brown
    (0xAA, 0xAA, 0xAA), //  7 light grey
    (0x55, 0x55, 0x55), //  8 dark grey
    (0x55, 0x55, 0xFF), //  9 light blue
    (0x55, 0xFF, 0x55), // 10 light green
    (0x55, 0xFF, 0xFF), // 11 light cyan
    (0xFF, 0x55, 0x55), // 12 light red
    (0xFF, 0x55, 0xFF), // 13 light magenta
    (0xFF, 0xFF, 0x55), // 14 yellow
    (0xFF, 0xFF, 0xFF), // 15 white
];

/// 32-bit RGB packed in the native format of one framebuffer.
#[derive(Clone, Copy)]
struct PixelFormat {
    bpp: u8,
    red_pos: u8,
    red_size: u8,
    green_pos: u8,
    green_size: u8,
    blue_pos: u8,
    blue_size: u8,
}

impl PixelFormat {
    /// Scale one 8-bit channel into the channel's mask and shift it in place.
    fn fit_channel(value: u8, pos: u8, size: u8) -> u32 {
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

    fn pack(&self, r: u8, g: u8, b: u8) -> u32 {
        Self::fit_channel(r, self.red_pos, self.red_size)
            | Self::fit_channel(g, self.green_pos, self.green_size)
            | Self::fit_channel(b, self.blue_pos, self.blue_size)
    }
}

/// A mapped linear framebuffer.
///
/// Pixel access is `&self`-safe: draws go through volatile writes to the
/// framebuffer's backing memory, so a shared reference is enough and no lock
/// is needed to render.
pub struct Framebuffer {
    /// Virtual address of the first pixel byte.
    base: usize,
    /// Page-aligned physical base of the mapped range.
    phys: usize,
    /// Byte offset of the first pixel from [`Framebuffer::phys`].
    offset: usize,
    /// Distance in bytes between scanlines.
    pitch: usize,
    /// Visible width in pixels.
    width: usize,
    /// Visible height in pixels.
    height: usize,
    /// Native pixel layout of the display.
    format: PixelFormat,
    /// Precomputed packed form of [`PALETTE_RGB`] for this mode.
    palette: [u32; 16],
}

/// A snapshot of the active framebuffer's geometry, used to hand the mode to
/// user space (see the `FB_INFO` syscall).
#[derive(Clone, Copy)]
pub struct FbGeometry {
    /// Page-aligned physical base of the mapped range.
    pub phys: usize,
    /// Byte offset of the first visible pixel from [`FbGeometry::phys`].
    pub offset: usize,
    /// Total mapped length in bytes.
    pub size: usize,
    /// Visible width in pixels.
    pub width: usize,
    /// Visible height in pixels.
    pub height: usize,
    /// Distance in bytes between scanlines.
    pub pitch: usize,
    /// Bits per pixel (24 or 32).
    pub bpp: u8,
    /// Bit position of the red channel.
    pub red_pos: u8,
    /// Width of the red channel in bits.
    pub red_size: u8,
    /// Bit position of the green channel.
    pub green_pos: u8,
    /// Width of the green channel in bits.
    pub green_size: u8,
    /// Bit position of the blue channel.
    pub blue_pos: u8,
    /// Width of the blue channel in bits.
    pub blue_size: u8,
}

static FRAMEBUFFER: OnceCell<Framebuffer> = OnceCell::new();

/// Bring up the framebuffer from the Multiboot2 tag, if the mode is usable.
///
/// Returns whether a framebuffer is now active.
pub fn init(info: Option<&FramebufferInfo>) -> bool {
    let Some(info) = info else {
        crate::log::kdebug!("framebuffer: no mode provided by the bootloader");
        return false;
    };
    // Only linear direct-RGB modes with 24 or 32 bits per pixel are handled.
    if info.fb_type != 1 || (info.bpp != 24 && info.bpp != 32) {
        crate::log::kwarn!("framebuffer: unsupported mode type {} bpp {}", info.fb_type, info.bpp);
        return false;
    }
    if info.phys_addr == 0 || info.width == 0 || info.height == 0 || info.pitch == 0 {
        crate::log::kwarn!("framebuffer: bootloader reported an empty mode");
        return false;
    }

    install(
        info.phys_addr as usize,
        info.width as usize,
        info.height as usize,
        info.pitch as usize,
        PixelFormat {
            bpp: info.bpp,
            red_pos: info.red_pos,
            red_size: info.red_size,
            green_pos: info.green_pos,
            green_size: info.green_size,
            blue_pos: info.blue_pos,
            blue_size: info.blue_size,
        },
    )
}

/// Bring up the framebuffer from explicit parameters, for device drivers
/// (such as the Bochs DISPI VGA) that program a linear mode themselves
/// instead of relying on the bootloader's tag.
///
/// `bpp` selects the packed channel layout: 24 and 32 both map to the
/// xxRRGGBB / xRGB byte order used by BIOS and DISPI linear modes. The scan
/// line length is `width * bpp / 8` with no padding.
///
/// Returns whether a framebuffer is now active.
pub fn init_mode(phys: usize, width: usize, height: usize, bpp: u8) -> bool {
    let pitch = match width.checked_mul(bpp as usize / 8) {
        Some(pitch) => pitch,
        None => {
            crate::log::kwarn!("framebuffer: mode pitch overflow");
            return false;
        }
    };
    init_mode_with_pitch(phys, width, height, pitch, bpp)
}

/// Bring up a framebuffer with an explicit scanline pitch.
///
/// Video adapters are permitted to use a virtual scanline wider than the
/// visible width.  Callers which program such an adapter must pass the
/// reported pitch instead of assuming tightly packed rows.
pub fn init_mode_with_pitch(
    phys: usize,
    width: usize,
    height: usize,
    pitch: usize,
    bpp: u8,
) -> bool {
    let format = match bpp {
        32 => PixelFormat {
            bpp: 32,
            red_pos: 16, red_size: 8,
            green_pos: 8, green_size: 8,
            blue_pos: 0, blue_size: 8,
        },
        24 => PixelFormat {
            bpp: 24,
            red_pos: 16, red_size: 8,
            green_pos: 8, green_size: 8,
            blue_pos: 0, blue_size: 8,
        },
        _ => {
            crate::log::kwarn!("framebuffer: unsupported bpp {}", bpp);
            return false;
        }
    };
    let bytes_per_pixel = bpp as usize / 8;
    if phys == 0 || width == 0 || height == 0 || pitch < width.saturating_mul(bytes_per_pixel) {
        crate::log::kwarn!("framebuffer: empty mode");
        return false;
    }
    install(phys, width, height, pitch, format)
}

/// Map, palette and publish a framebuffer described by raw parameters.
fn install(phys: usize, width: usize, height: usize, pitch: usize, format: PixelFormat) -> bool {
    let Some(size) = pitch.checked_mul(height) else {
        crate::log::kwarn!("framebuffer: mode size overflow");
        return false;
    };
    let base_page = phys & !(pmm::FRAME_SIZE - 1);
    let offset = phys - base_page;
    let frames = (offset + size).div_ceil(pmm::FRAME_SIZE);
    vmm::map_physical(
        FRAMEBUFFER_VIRT,
        base_page,
        frames,
        // A framebuffer is MMIO, not normal RAM.  A cached alias lets the CPU
        // retain stale pixels or merge writes in ways the display engine does
        // not observe reliably.
        vmm::PRESENT | vmm::WRITABLE | vmm::NO_EXECUTE | vmm::CACHE_DISABLE | vmm::WRITE_THROUGH,
    );

    let mut palette = [0u32; 16];
    for (i, &(r, g, b)) in PALETTE_RGB.iter().enumerate() {
        palette[i] = format.pack(r, g, b);
    }

    let fb = Framebuffer {
        base: FRAMEBUFFER_VIRT + offset,
        phys: base_page,
        offset,
        pitch,
        width,
        height,
        format,
        palette,
    };
    fb.fill(0);

    if FRAMEBUFFER.set(fb).is_err() {
        crate::log::kwarn!("framebuffer: initialized twice");
        return false;
    }
    // Authorize user-space MMIO mapping of the same range so a ring-3
    // compositor/terminal can drive the display directly via `MAP_PHYS`.
    crate::memory::user_map::register_device_region(
        base_page as u64,
        (frames * pmm::FRAME_SIZE) as u64,
    );
    crate::log::kinfo!(
        "framebuffer: {:#x} {}x{} @{}bpp, {} bytes/line",
        phys,
        width,
        height,
        format.bpp,
        pitch
    );
    true
}

/// True once a usable framebuffer has been mapped.
pub fn active() -> bool {
    FRAMEBUFFER.get().is_some()
}

/// Access to the active framebuffer, if any.
pub fn get() -> Option<&'static Framebuffer> {
    FRAMEBUFFER.get()
}

impl Framebuffer {
    /// Geometry snapshot for user-space handoff.
    pub fn geometry(&self) -> FbGeometry {
        FbGeometry {
            phys: self.phys,
            offset: self.offset,
            size: self.pitch * self.height,
            width: self.width,
            height: self.height,
            pitch: self.pitch,
            bpp: self.format.bpp,
            red_pos: self.format.red_pos,
            red_size: self.format.red_size,
            green_pos: self.format.green_pos,
            green_size: self.format.green_size,
            blue_pos: self.format.blue_pos,
            blue_size: self.format.blue_size,
        }
    }

    /// Visible width in pixels.
    pub fn width(&self) -> usize {
        self.width
    }

    /// Visible height in pixels.
    pub fn height(&self) -> usize {
        self.height
    }

    /// Scanline length in bytes (may exceed `width * bpp / 8`).
    pub fn pitch(&self) -> usize {
        self.pitch
    }

    /// Packed native color of palette entry `index` (0..16).
    pub fn packed(&self, index: u8) -> u32 {
        self.palette[(index & 0x0F) as usize]
    }

    /// Pack an explicit 8-bit RGB triple into the native pixel format.
    ///
    /// Used for truecolor escape sequences (`SGR 38;2`) so colors outside the
    /// sixteen-color palette, such as the console's timestamp ramp, render
    /// exactly as authored.
    pub fn pack_rgb(&self, r: u8, g: u8, b: u8) -> u32 {
        self.format.pack(r, g, b)
    }

    /// Index of the nearest colour in the sixteen-colour palette to `(r, g, b)`.
    ///
    /// Lets indexed (256-colour) escape codes map onto the console palette.
    pub fn closest_palette(&self, r: u8, g: u8, b: u8) -> u8 {
        let mut best = 0u8;
        let mut best_dist = u32::MAX;
        for (i, &(pr, pg, pb)) in PALETTE_RGB.iter().enumerate() {
            let dr = pr as i32 - r as i32;
            let dg = pg as i32 - g as i32;
            let db = pb as i32 - b as i32;
            let dist = (dr * dr + dg * dg + db * db) as u32;
            if dist < best_dist {
                best_dist = dist;
                best = i as u8;
            }
        }
        best
    }

    /// Bytes occupied by one pixel.
    fn bytes_per_pixel(&self) -> usize {
        self.format.bpp as usize / 8
    }

    /// Fill the entire visible area with `color`.
    pub fn fill(&self, color: u32) {
        self.fill_rect(0, 0, self.width, self.height, color);
    }

    /// Fill a rectangle with `color`, clamped to the visible area.
    pub fn fill_rect(&self, x: usize, y: usize, w: usize, h: usize, color: u32) {
        let x0 = x.min(self.width);
        let y0 = y.min(self.height);
        let x1 = x.saturating_add(w).min(self.width);
        let y1 = y.saturating_add(h).min(self.height);
        for py in y0..y1 {
            for px in x0..x1 {
                self.put_pixel(px, py, color);
            }
        }
    }

    /// Draw a single pixel at `(x, y)` in the native packed `color`.
    pub fn put_pixel(&self, x: usize, y: usize, color: u32) {
        if x >= self.width || y >= self.height {
            return;
        }
        let off = y * self.pitch + x * self.bytes_per_pixel();
        let ptr = (self.base + off) as *mut u8;
        unsafe {
            if self.format.bpp == 32 {
                core::ptr::write_volatile(ptr as *mut u32, color);
            } else {
                core::ptr::write_volatile(ptr, (color & 0xFF) as u8);
                core::ptr::write_volatile(ptr.add(1), ((color >> 8) & 0xFF) as u8);
                core::ptr::write_volatile(ptr.add(2), ((color >> 16) & 0xFF) as u8);
            }
        }
    }

    /// Shift the visible image up by `lines` pixels, leaving back-filled
    /// scanlines at the bottom.
    pub fn scroll_up(&self, lines: usize) {
        let lines = lines.min(self.height);
        if lines == 0 {
            return;
        }
        let row_bytes = self.pitch;
        let moved_rows = self.height - lines;
        let base = self.base as *mut u8;
        // Framebuffer memory is MMIO.  Do not use `copy_within` here: it
        // produces ordinary cached loads/stores and is not a valid operation
        // on device memory.  Forward copying is memmove-safe because every
        // source row is above its destination row.
        unsafe {
            for row in 0..moved_rows {
                let dst = base.add(row * row_bytes);
                let src = base.add((row + lines) * row_bytes);
                for byte in 0..row_bytes {
                    let value = core::ptr::read_volatile(src.add(byte));
                    core::ptr::write_volatile(dst.add(byte), value);
                }
            }
            for row in moved_rows..self.height {
                let dst = base.add(row * row_bytes);
                for byte in 0..row_bytes {
                    core::ptr::write_volatile(dst.add(byte), 0);
                }
            }
        }
    }
}
