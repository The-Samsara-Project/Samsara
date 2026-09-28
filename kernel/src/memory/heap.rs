// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! Kernel heap: a first-fit linked-list allocator implementing
//! [`GlobalAlloc`] over a VMM-mapped region, enabling `Box`, `Vec`, etc.
//!
//! Chunk model: every chunk starts with a header whose `total` counts the
//! whole chunk including the header. Free chunks are chained in an
//! address-ordered list and coalesced eagerly on free. Allocated chunks
//! additionally record the offset of the returned pointer so `dealloc` can
//! recover the chunk start even when alignment padding was inserted.

use super::vmm;
use crate::sync::{OnceCell, Spinlock};
use core::alloc::{GlobalAlloc, Layout};
use core::ptr::null_mut;

/// Header size; also the minimum alignment and allocation granularity.
const HDR: usize = 32;
/// Bytes reserved right before the user pointer for the back-offset.
const OFF_SLOT: usize = 8;
/// Smallest body a split-off free chunk may keep.
const MIN_SPLIT: usize = 64;

struct HeapAllocator {
    /// Address-ordered list of free chunks.
    free_head: usize,
    total_bytes: usize,
    used_bytes: usize,
}

// SAFETY: all state is confined behind the owning spinlock; raw pointers
// never escape critical sections.
unsafe impl Send for HeapAllocator {}

#[inline]
unsafe fn chunk_total(addr: usize) -> usize {
    (addr as *const usize).read_volatile()
}

#[inline]
unsafe fn set_total(addr: usize, total: usize) {
    (addr as *mut usize).write_volatile(total);
}

#[inline]
unsafe fn next_of(addr: usize) -> usize {
    ((addr + 8) as *const usize).read_volatile()
}

#[inline]
unsafe fn set_next(addr: usize, next: usize) {
    ((addr + 8) as *mut usize).write_volatile(next);
}

impl HeapAllocator {
    const fn empty() -> Self {
        Self {
            free_head: 0,
            total_bytes: 0,
            used_bytes: 0,
        }
    }

    /// Adopt a freshly mapped region as one large free chunk.
    unsafe fn grow(&mut self, base: usize, size: usize) {
        debug_assert_eq!(base % HDR, 0);
        debug_assert!(size >= HDR * 4);
        set_total(base, size);
        set_next(base, self.free_head);
        self.free_head = base;
        self.total_bytes += size;
    }

    unsafe fn alloc_layout(&mut self, layout: Layout) -> *mut u8 {
        let size = layout.size().max(1);
        let align = layout.align().max(HDR);

        let mut prev: usize = 0;
        let mut cur = self.free_head;

        while cur != 0 {
            let body_start = cur + HDR;
            // Place the user pointer so it is aligned to `align`, leaving room
            // for the 8-byte back-offset slot directly in front of it.
            let user = (body_start + OFF_SLOT + align - 1) & !(align - 1);
            let padding = user - body_start;
            let body = chunk_total(cur) - HDR;
            let required = padding + size;

            if body >= required {
                let remainder = body - required;
                if remainder >= HDR + MIN_SPLIT {
                    // Split: the tail becomes its own free chunk.
                    let tail = user + size;
                    set_total(tail, remainder);
                    set_next(tail, next_of(cur));
                    set_next(cur, tail);
                    set_total(cur, HDR + padding + size);
                }
                // Unlink.
                if prev == 0 {
                    self.free_head = next_of(cur);
                } else {
                    set_next(prev, next_of(cur));
                }
                ((user - OFF_SLOT) as *mut usize).write_volatile(user - cur);
                self.used_bytes += chunk_total(cur);
                return user as *mut u8;
            }

            prev = cur;
            cur = next_of(cur);
        }
        null_mut()
    }

    unsafe fn dealloc_layout(&mut self, ptr: *mut u8) {
        let p = ptr as usize;
        if p == 0 {
            return;
        }
        let base = super::HEAP_VIRT_BASE;
        if !(base..base + super::HEAP_SIZE).contains(&p) {
            crate::log::kemerg!(
                "heap: bad free {:#x} (heap {:#x}..{:#x})",
                p,
                base,
                base + super::HEAP_SIZE
            );
            loop {
                core::arch::asm!("cli; hlt");
            }
        }
        let user_off = ((p - OFF_SLOT) as *const usize).read_volatile();
        let chunk = p - user_off;
        let total = chunk_total(chunk);
        self.used_bytes = self.used_bytes.saturating_sub(total);

        // Address-ordered insertion.
        if self.free_head == 0 || self.free_head > chunk {
            set_next(chunk, self.free_head);
            self.free_head = chunk;
        } else {
            let mut cur = self.free_head;
            while next_of(cur) != 0 && next_of(cur) < chunk {
                cur = next_of(cur);
            }
            set_next(chunk, next_of(cur));
            set_next(cur, chunk);
        }

        // Coalesce with successor, then with predecessor.
        let succ = next_of(chunk);
        if succ != 0 && chunk + total == succ {
            set_total(chunk, total + chunk_total(succ));
            set_next(chunk, next_of(succ));
        }
        let head = self.free_head;
        if head != chunk && head + chunk_total(head) == chunk {
            set_total(head, chunk_total(head) + chunk_total(chunk));
            set_next(head, next_of(chunk));
        }
    }
}

/// Global heap handle handed to Rust's allocator machinery.
pub struct KernelHeap {
    inner: OnceCell<Spinlock<HeapAllocator>>,
}

impl KernelHeap {
    pub(crate) const fn new() -> Self {
        Self {
            inner: OnceCell::new(),
        }
    }

    fn lock(&self) -> crate::sync::SpinlockGuard<'_, HeapAllocator> {
        match self.inner.get() {
            Some(l) => l.lock(),
            None => panic!("heap used before heap::init"),
        }
    }
}

unsafe impl GlobalAlloc for KernelHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.lock().alloc_layout(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        self.lock().dealloc_layout(ptr)
    }
}

#[global_allocator]
static KERNEL_HEAP: KernelHeap = KernelHeap::new();

/// Map `[virt, virt+size)` through the VMM and seed the allocator with it.
pub fn init(virt: usize, size: usize) {
    vmm::map_range(virt, size, vmm::WRITABLE | vmm::NO_EXECUTE);
    if KERNEL_HEAP
        .inner
        .set(Spinlock::new(HeapAllocator::empty()))
        .is_err()
    {
        panic!("heap initialized twice");
    }

    // SAFETY: region was just mapped and is owned solely by the heap.
    unsafe {
        KERNEL_HEAP.lock().grow(virt, size);
    }
    crate::log::kdebug!("heap: {} MiB online at {:#x}", size >> 20, virt);
}
