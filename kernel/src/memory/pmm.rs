// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! Physical memory management: a bitmap frame allocator seeded from the
//! Multiboot2 memory map.

use super::{MemRegion, RegionKind, MAX_MANAGED_PHYS};
use crate::sync::{OnceCell, Spinlock};

/// Page/frame size in bytes.
pub const FRAME_SIZE: usize = 4096;

struct FrameAllocator {
    /// Bitmap storage (physical-mapped), one bit per managed frame.
    bitmap: &'static mut [u8],
    /// Number of frames covered by the bitmap.
    total_frames: usize,
    /// Frames currently allocated.
    used_frames: usize,
    /// Scan hint to keep allocations roughly front-to-back.
    hint: usize,
}

impl FrameAllocator {
    fn test(&self, index: usize) -> bool {
        self.bitmap[index / 8] & (1 << (index % 8)) != 0
    }

    fn set(&mut self, index: usize) {
        self.bitmap[index / 8] |= 1 << (index % 8);
    }

    fn clear(&mut self, index: usize) {
        self.bitmap[index / 8] &= !(1 << (index % 8));
    }

    fn mark_range(&mut self, start: usize, size: usize) {
        let first = start / FRAME_SIZE;
        let last = (start + size + FRAME_SIZE - 1) / FRAME_SIZE;
        for i in first..last.min(self.total_frames) {
            if !self.test(i) {
                self.set(i);
                self.used_frames += 1;
            }
        }
    }

    fn alloc(&mut self) -> Option<usize> {
        for _ in 0..self.total_frames {
            let i = self.hint % self.total_frames;
            if !self.test(i) {
                self.set(i);
                self.used_frames += 1;
                self.hint = (i + 1) % self.total_frames;
                return Some(i * FRAME_SIZE);
            }
            self.hint += 1;
        }
        None
    }

    fn alloc_contiguous(&mut self, count: usize) -> Option<usize> {
        'outer: for base in 0..self.total_frames.saturating_sub(count) {
            for k in 0..count {
                if self.test(base + k) {
                    continue 'outer;
                }
            }
            for k in 0..count {
                self.set(base + k);
            }
            self.used_frames += count;
            self.hint = (base + count) % self.total_frames.max(1);
            return Some(base * FRAME_SIZE);
        }
        None
    }

    fn free(&mut self, phys: usize) {
        debug_assert_eq!(phys % FRAME_SIZE, 0, "unaligned frame free");
        let index = phys / FRAME_SIZE;
        debug_assert!(self.test(index), "double free of frame {:#x}", phys);
        self.clear(index);
        self.used_frames -= 1;
    }
}

static PMM: OnceCell<Spinlock<FrameAllocator>> = OnceCell::new();

/// Initialize the frame allocator from the firmware memory map.
///
/// `kernel_start`/`kernel_end` delimit the loaded kernel image in physical
/// memory and are permanently reserved, along with the bitmap itself.
pub fn init(regions: &[MemRegion], kernel_start: usize, kernel_end: usize) {
    // Clamp every usable region to what the current physical map addresses.
    let mut usable: [(u64, u64); 16] = [(0, 0); 16];
    let mut usable_count = 0usize;
    let mut total_bytes = 0u64;

    for r in regions {
        if r.kind != RegionKind::Usable {
            continue;
        }
        let start = r.start.min(MAX_MANAGED_PHYS);
        let end = r.end().min(MAX_MANAGED_PHYS);
        if end <= start {
            continue;
        }
        if usable_count == usable.len() {
            break;
        }
        total_bytes += end - start;
        usable[usable_count] = (start, end);
        usable_count += 1;
    }

    let total_frames = (total_bytes / FRAME_SIZE as u64) as usize;
    assert!(total_frames > 0, "no usable physical memory below 1 GiB");
    let bitmap_size = total_frames.div_ceil(8);

    // Place the bitmap just past the kernel image inside a usable region.
    let mut bitmap_phys = None;
    let candidate = ((kernel_end + FRAME_SIZE - 1) / FRAME_SIZE * FRAME_SIZE) as u64;
    for &(start, end) in &usable[..usable_count] {
        if candidate >= start && candidate + bitmap_size as u64 <= end {
            bitmap_phys = Some(candidate as usize);
            break;
        }
    }
    if bitmap_phys.is_none() {
        panic!("pmm: no room for {} byte frame bitmap", bitmap_size);
    }
    let bitmap_phys = bitmap_phys.unwrap();

    // Zero the bitmap and hand ownership to the global allocator.
    let bitmap_slice = unsafe {
        let ptr = super::phys_to_virt(bitmap_phys) as *mut u8;
        core::ptr::write_bytes(ptr, 0, bitmap_size);
        core::slice::from_raw_parts_mut(ptr, bitmap_size)
    };

    let fa = FrameAllocator {
        bitmap: bitmap_slice,
        total_frames,
        used_frames: 0,
        hint: 0,
    };

    if PMM.set(Spinlock::new(fa)).is_err() {
        panic!("pmm initialized twice");
    }

    // Reserve the kernel image and the bitmap itself.
    {
        let mut guard = lock();
        // First MiB: IVT/BDA/EBDA and loader remnants — never hand out.
        guard.mark_range(0, 0x100000);
        guard.mark_range(kernel_start, kernel_end - kernel_start);
        guard.mark_range(bitmap_phys, bitmap_size);
        crate::log::kdebug!(
            "pmm: {} frames ({} KiB), bitmap at phys {:#x}, kernel [{:#x}..{:#x})",
            guard.total_frames,
            guard.total_frames * FRAME_SIZE / 1024,
            bitmap_phys,
            kernel_start,
            kernel_end,
        );
    }
}

fn lock() -> crate::sync::SpinlockGuard<'static, FrameAllocator> {
    match PMM.get() {
        Some(l) => l.lock(),
        None => panic!("pmm used before initialization"),
    }
}

/// Allocate one physical frame, returning its physical address.
pub fn alloc_frame() -> Option<usize> {
    let mut a = lock();
    match a.alloc() {
        Some(f) => Some(f),
        None => {
            let (total, used) = (a.total_frames, a.used_frames);
            drop(a);
            crate::log::kwarn!("pmm: out of frames ({} of {} in use)", used, total);
            None
        }
    }
}

/// Allocate `count` contiguous frames, returning the first physical address.
pub fn alloc_contiguous(count: usize) -> Option<usize> {
    lock().alloc_contiguous(count)
}

/// Return a previously allocated frame to the pool.
pub fn free_frame(phys: usize) {
    lock().free(phys);
}

/// Snapshot of allocator statistics: `(total, used)` frame counts.
pub fn stats() -> (usize, usize) {
    let g = lock();
    (g.total_frames, g.used_frames)
}

/// Allocate physically contiguous pages for DMA.
/// Returns the physical address of the first page.
pub fn alloc_dma_pages(pages: usize) -> Option<u64> {
    alloc_contiguous(pages).map(|p| p as u64)
}

/// Free DMA pages previously allocated with `alloc_dma_pages`.
pub fn free_dma_pages(phys: u64, pages: usize) {
    let start = phys as usize;
    for i in 0..pages {
        free_frame(start + i * FRAME_SIZE);
    }
}
