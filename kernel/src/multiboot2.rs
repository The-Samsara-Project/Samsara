// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! Minimal, dependency-free Multiboot2 information structure parser.
//!
//! During early boot the kernel keeps an identity map of the first GiB, so
//! the physical-addressed MBI handed over by GRUB is read through the
//! higher-half physical map (`phys_to_virt`).

use crate::memory::{MemRegion, RegionKind};

/// A graphics mode handed over by the bootloader's framebuffer tag.
#[derive(Debug, Clone, Copy, Default)]
pub struct FramebufferInfo {
    /// Physical address of the linear framebuffer.
    pub phys_addr: u64,
    /// Distance in bytes between two vertically adjacent scanlines.
    pub pitch: u32,
    /// Visible width in pixels.
    pub width: u32,
    /// Visible height in pixels.
    pub height: u32,
    /// Bits per pixel.
    pub bpp: u8,
    /// Pixel layout: 0 = indexed, 1 = direct RGB, 2 = EGA text.
    pub fb_type: u8,
    /// Bit offset of the red channel (direct color only).
    pub red_pos: u8,
    /// Width in bits of the red channel mask.
    pub red_size: u8,
    /// Bit offset of the green channel (direct color only).
    pub green_pos: u8,
    /// Width in bits of the green channel mask.
    pub green_size: u8,
    /// Bit offset of the blue channel (direct color only).
    pub blue_pos: u8,
    /// Width in bits of the blue channel mask.
    pub blue_size: u8,
}

/// Boot-time information extracted from the Multiboot2 structure.
#[derive(Debug, Default)]
pub struct BootInfo {
    /// Name reported by the bootloader, if the tag was present.
    pub bootloader_name: Option<&'static str>,
    /// Kernel command line, if the tag was present.
    pub command_line: Option<&'static str>,
    /// Usable and reserved memory regions from the firmware memory map.
    pub regions: [MemRegion; MAX_REGIONS],
    /// Number of valid entries in `regions`.
    pub region_count: usize,
    /// Bootloader-provided graphics mode, if any.
    pub framebuffer: Option<FramebufferInfo>,
}

impl BootInfo {
    fn push_region(&mut self, region: MemRegion) {
        if self.region_count < MAX_REGIONS {
            self.regions[self.region_count] = region;
            self.region_count += 1;
        }
    }
}

const MAX_REGIONS: usize = 32;

// Multiboot2 tag types we understand.
const TAG_END: u32 = 0;
const TAG_CMDLINE: u32 = 1;
const TAG_BOOTLOADER_NAME: u32 = 2;
const TAG_MEMORY_MAP: u32 = 6;
const TAG_FRAMEBUFFER: u32 = 8;

// Memory map entry types.
const MMAP_AVAILABLE: u32 = 1;

/// Parse the MBI at `mbi_phys` into a [`BootInfo`].
pub fn parse(mbi_phys: u64) -> BootInfo {
    let mut info = BootInfo::default();
    let base = mbi_phys as usize;
    let total_size = unsafe { read_u32(base + 0) } as usize;

    // Tags start 8 bytes in; each tag is 8-byte aligned.
    let mut offset = 8;
    while offset + 8 <= total_size {
        let tag_type = unsafe { read_u32(base + offset) };
        let tag_size = unsafe { read_u32(base + offset + 4) } as usize;
        if tag_type == TAG_END || tag_size < 8 {
            break;
        }

        match tag_type {
            TAG_BOOTLOADER_NAME => {
                if let Some(s) = unsafe { read_cstr(base + offset + 8) } {
                    info.bootloader_name = Some(s);
                }
            }
            TAG_CMDLINE => {
                if let Some(s) = unsafe { read_cstr(base + offset + 8) } {
                    info.command_line = Some(s);
                }
            }
            TAG_MEMORY_MAP => {
                // entries: base(8) len(8) type(4) reserved(4)
                let mut entry = offset + 16;
                let end = offset + tag_size;
                while entry + 24 <= end {
                    let start = unsafe { read_u64(base + entry) };
                    let length = unsafe { read_u64(base + entry + 8) };
                    let kind = unsafe { read_u32(base + entry + 16) };
                    if length > 0 {
                        let kind = if kind == MMAP_AVAILABLE {
                            RegionKind::Usable
                        } else {
                            RegionKind::Reserved
                        };
                        info.push_region(MemRegion {
                            start,
                            size: length,
                            kind,
                        });
                    }
                    entry += 24;
                }
            }
            TAG_FRAMEBUFFER => {
                // header(8) | addr(8) pitch(4) width(4) height(4) |
                // bpp(1) type(1) reserved(2) | color info(6)
                let phys_addr = unsafe { read_u64(base + offset + 8) };
                let pitch = unsafe { read_u32(base + offset + 16) };
                let width = unsafe { read_u32(base + offset + 20) };
                let height = unsafe { read_u32(base + offset + 24) };
                let bpp = unsafe { read_u8(base + offset + 28) };
                let fb_type = unsafe { read_u8(base + offset + 29) };
                let fb = FramebufferInfo {
                    phys_addr,
                    pitch,
                    width,
                    height,
                    bpp,
                    fb_type,
                    red_pos: unsafe { read_u8(base + offset + 32) },
                    red_size: unsafe { read_u8(base + offset + 33) },
                    green_pos: unsafe { read_u8(base + offset + 34) },
                    green_size: unsafe { read_u8(base + offset + 35) },
                    blue_pos: unsafe { read_u8(base + offset + 36) },
                    blue_size: unsafe { read_u8(base + offset + 37) },
                };
                info.framebuffer = Some(fb);
                crate::log::kdebug!(
                    "multiboot2: framebuffer {:#x} {}x{} pitch {} bpp {} type {}",
                    phys_addr,
                    width,
                    height,
                    pitch,
                    bpp,
                    fb_type
                );
            }
            _ => {}
        }

        offset = align_up(offset + tag_size, 8);
    }

    info
}

fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}

/// # Safety
/// `addr` must point at readable low memory during early boot.
unsafe fn read_u8(addr: usize) -> u8 {
    core::ptr::read_volatile(crate::memory::phys_to_virt(addr) as *const u8)
}

/// # Safety
/// `addr` must point at readable low memory during early boot.
unsafe fn read_u32(addr: usize) -> u32 {
    core::ptr::read_volatile(crate::memory::phys_to_virt(addr) as *const u32)
}

/// # Safety
/// `addr` must point at readable low memory during early boot.
unsafe fn read_u64(addr: usize) -> u64 {
    core::ptr::read_volatile(crate::memory::phys_to_virt(addr) as *const u64)
}

/// Read a NUL-terminated ASCII string from early-boot memory.
///
/// # Safety
/// `addr` must point at a NUL-terminated string in readable low memory.
unsafe fn read_cstr(addr: usize) -> Option<&'static str> {
    let virt = crate::memory::phys_to_virt(addr) as *const u8;
    let mut len = 0usize;
    while *virt.add(len) != 0 && len < 256 {
        len += 1;
    }
    if len == 0 {
        return None;
    }
    let slice = core::slice::from_raw_parts(virt, len);
    core::str::from_utf8(slice).ok()
}
