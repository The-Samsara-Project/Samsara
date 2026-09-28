// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! Bochs VBE (DISPI) linear framebuffer driver.
//!
//! GRUB's video drivers frequently find no VBE modes under QEMU and hand the
//! kernel an unusable 80x25 *text* tag, so the graphics console never comes
//! up. The Bochs VBE DISPI interface is the native programming interface of
//! QEMU's stdvga/Bochs VGA cards: a handful of registers live on legacy I/O
//! ports `0x1CE`/`0x1CF` and work without any video BIOS calls. When the
//! bootloader left the display in text mode this driver switches the adapter
//! to a 32bpp linear mode itself and feeds the resulting range to
//! [`crate::framebuffer`].

use crate::framebuffer;
use crate::io::pci;
use crate::io::{inw, outw};

/// DISPI index register port.
const VBE_DISPI_INDEX: u16 = 0x1CE;
/// DISPI data register port.
const VBE_DISPI_DATA: u16 = 0x1CF;

/// Register 0x00: adapter ID (0xB0C0..0xB0C5 across revisions).
const REG_ID: u16 = 0x00;
/// Register 0x01: horizontal resolution.
const REG_XRES: u16 = 0x01;
/// Register 0x02: vertical resolution.
const REG_YRES: u16 = 0x02;
/// Register 0x03: bits per pixel.
const REG_BPP: u16 = 0x03;
/// Register 0x04: display enable / mode flags.
const REG_ENABLE: u16 = 0x04;
/// Register 0x06: virtual scanline width in pixels.
const REG_VIRT_WIDTH: u16 = 0x06;
/// Register 0x07: virtual framebuffer height in pixels.
const REG_VIRT_HEIGHT: u16 = 0x07;
/// Registers 0x08/0x09: display origin within the virtual framebuffer.
const REG_X_OFFSET: u16 = 0x08;
const REG_Y_OFFSET: u16 = 0x09;

/// `REG_ENABLE` value: display disabled (also resets the mode).
const DISABLED: u16 = 0x0000;
/// `REG_ENABLE` value: display enabled.
const ENABLED: u16 = 0x0001;
/// `REG_ENABLE` value: feed the linear framebuffer window.
const LFB_ENABLED: u16 = 0x0040;

/// PCI vendor ID of QEMU's Bochs-compatible VGA.
const BOCHS_VENDOR_ID: u16 = 0x1234;
/// PCI device ID of QEMU's stdvga (`-vga std`).
const BOCHS_DEVICE_ID: u16 = 0x1111;

/// Horizontal resolution requested when the bootloader leaves text mode.
const WIDTH: u16 = 1024;
/// Vertical resolution requested when the bootloader leaves text mode.
const HEIGHT: u16 = 768;

/// Write one DISPI register.
fn reg_write(index: u16, value: u16) {
    outw(VBE_DISPI_INDEX, index);
    outw(VBE_DISPI_DATA, value);
}

/// Read one DISPI register.
fn reg_read(index: u16) -> u16 {
    outw(VBE_DISPI_INDEX, index);
    inw(VBE_DISPI_DATA)
}

/// Switch the adapter into a linear `width`x`height`, `bpp`-bit mode.
fn set_mode(width: u16, height: u16, bpp: u16) {
    // A disable first resets the resolution/bpp latches.
    reg_write(REG_ENABLE, DISABLED);
    reg_write(REG_XRES, width);
    reg_write(REG_YRES, height);
    reg_write(REG_BPP, bpp);
    // The visible resolution does not define the scanline stride.  If a
    // previous mode left a larger virtual width behind, treating rows as
    // `width * bytes_per_pixel` makes every scanline begin at the wrong byte
    // and produces the characteristic scrambled text output.
    reg_write(REG_VIRT_WIDTH, width);
    reg_write(REG_VIRT_HEIGHT, height);
    reg_write(REG_X_OFFSET, 0);
    reg_write(REG_Y_OFFSET, 0);
    reg_write(REG_ENABLE, ENABLED | LFB_ENABLED);
}

/// Detect the stdvga, switch it to a linear 32bpp mode and hand the result to
/// `crate::framebuffer`, unless the bootloader already provided a usable one.
pub fn init() {
    if framebuffer::active() {
        return;
    }

    let Some(dev) = pci::find_device(BOCHS_VENDOR_ID, BOCHS_DEVICE_ID) else {
        crate::log::kdebug!("bochsfb: no stdvga device present");
        return;
    };
    let lfb = dev.bar[0] & !0xF;
    if lfb == 0 {
        crate::log::kdebug!("bochsfb: stdvga has no linear framebuffer window");
        return;
    }

    // Confirm a DISPI adapter actually answers at the legacy ports.
    reg_write(REG_ID, 0);
    let id = reg_read(REG_ID);
    if !(0xB0C0..=0xB0C5).contains(&id) {
        crate::log::kdebug!("bochsfb: no DISPI interface (id {:#x})", id);
        return;
    }

    set_mode(WIDTH, HEIGHT, 32);
    if (reg_read(REG_ENABLE) & LFB_ENABLED) == 0 {
        crate::log::kwarn!("bochsfb: stdvga rejected the linear mode request");
        return;
    }

    let width = reg_read(REG_XRES) as usize;
    let height = reg_read(REG_YRES) as usize;
    let bpp = reg_read(REG_BPP) as u8;
    let virtual_width = reg_read(REG_VIRT_WIDTH) as usize;
    let bytes_per_pixel = bpp as usize / 8;
    let Some(pitch) = virtual_width.checked_mul(bytes_per_pixel) else {
        crate::log::kwarn!("bochsfb: virtual scanline width overflow");
        return;
    };
    if width == 0 || height == 0 || virtual_width < width || pitch == 0 {
        crate::log::kwarn!(
            "bochsfb: invalid programmed mode {}x{} virtual width {} @{}bpp",
            width, height, virtual_width, bpp
        );
        return;
    }

    // Use the adapter's read-back virtual width rather than assuming the
    // visible row is tightly packed.
    if framebuffer::init_mode_with_pitch(lfb as usize, width, height, pitch, bpp) {
        crate::log::kinfo!(
            "bochsfb: stdvga switched to {}x{} @{}bpp (pitch {}, lfb {:#x})",
            width,
            height,
            bpp,
            pitch,
            lfb
        );
    }
}
