// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa
//
// Ring-3 heap. A classic first-fit free list of header blocks, grown in
// multi-page chunks via the kernel's `MAP_ANON` grant. Processes are
// single-threaded, so the free list lives in plain static state.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr;

extern crate alloc;

use crate::syscall;

/// Frames requested per arena growth chunk (1 MiB).
const ARENA_FRAMES: u64 = 256;
/// Size of the `Block` header; payload starts `HDR` bytes in.
pub const HDR: usize = 32;
/// Free-list sanity marker.
const MAGIC: u64 = 0x5341_4d53_4152_4152; // "SAMSARAR"

/// Allocator block header (laid out to keep payload 16-byte aligned).
#[repr(C, align(16))]
struct Block {
    magic: u64,
    size: usize,
    next: *mut Block,
    _pad: u64,
}

static mut FREE: *mut Block = ptr::null_mut();

fn align_up(n: usize) -> usize {
    (n + 15) & !15usize
}

/// Extend the arena by one `MAP_ANON` chunk; true on success.
fn grow() -> bool {
    let va = match syscall::map_anon(ARENA_FRAMES) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let b = va as *mut Block;
    unsafe {
        (*b).magic = MAGIC;
        (*b).size = (ARENA_FRAMES as usize * (syscall::PAGE_SIZE as usize)) - HDR;
        (*b).next = FREE;
        FREE = b;
    }
    true
}

/// First-fit take of a block with at least `n` payload bytes.
unsafe fn take_from_free(n: usize) -> Option<*mut u8> {
    let mut cur = FREE;
    let mut prev: *mut Block = ptr::null_mut();
    while !cur.is_null() {
        if (*cur).size >= n {
            let rem = (*cur).size - n;
            if rem >= HDR + 16 {
                // Split; return the leading half, keep the tail free.
                let nb = (cur as usize + HDR + n) as *mut Block;
                (*nb).magic = MAGIC;
                (*nb).size = rem - HDR;
                (*nb).next = (*cur).next;
                // The front half no longer spans the tail; shrink its header
                // so a later free() of it does not resurrect the split region.
                (*cur).size = n;
                if prev.is_null() {
                    FREE = nb;
                } else {
                    (*prev).next = nb;
                }
            } else {
                // Take the whole block out of the free list.
                if prev.is_null() {
                    FREE = (*cur).next;
                } else {
                    (*prev).next = (*cur).next;
                }
            }
            return Some((cur as usize + HDR) as *mut u8);
        }
        prev = cur;
        cur = (*cur).next;
    }
    None
}

/// Allocate `size` payload bytes; returns null on failure.
pub fn allocate(size: usize) -> *mut u8 {
    let n = align_up(size.max(1));
    loop {
        // SAFETY: single-threaded process; free list only touched here.
        if let Some(p) = unsafe { take_from_free(n) } {
            return p;
        }
        if !grow() {
            return ptr::null_mut();
        }
    }
}

/// Release a previously allocated pointer.
pub fn deallocate(p: *mut u8) {
    if p.is_null() {
        return;
    }
    // SAFETY: `p` must have come from `allocate`; header preceeds the payload.
    unsafe {
        let b = (p as usize - HDR) as *mut Block;
        debug_assert_eq!((*b).magic, MAGIC, "double free in ring-3 heap");
        (*b).next = FREE;
        FREE = b;
    }
}

/// Diagnostics: snapshot the free list as `(block base address, payload
/// size)` pairs. Unsafe: walks the raw list; caller must trust the state.
pub unsafe fn free_walk() -> alloc::vec::Vec<(usize, usize)> {
    let mut v = alloc::vec::Vec::new();
    let mut cur = FREE;
    let mut n = 0usize;
    while !cur.is_null() && n < 64 {
        // Defensive: blocks live in arena chunks mapped at >= 0x40000000 and
        // are 16-aligned; a stray pointer is evidence of list corruption.
        let p = cur as usize;
        if p < 0x40000000 || p >= 0x50000000 || p & 15 != 0 {
            v.push((p, 0xFFFF));
            return v;
        }
        v.push((p, (*cur).size));
        cur = (*cur).next;
        n += 1;
    }
    v
}

/// Diagnostics: inspect the block header that precedes address `p`.
/// Returns `(magic, payload size, next, magic_ok)`.
pub unsafe fn probe(p: usize) -> (u64, usize, usize, bool) {
    let b = (p - HDR) as *mut Block;
    ((*b).magic, (*b).size, (*b).next as usize, (*b).magic == MAGIC)
}

/// Global allocator hook used via `#[global_allocator]` in `rt::HEAP`.
pub struct Heap;

unsafe impl GlobalAlloc for Heap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        allocate(layout.size())
    }

    unsafe fn dealloc(&self, ptr_: *mut u8, _layout: Layout) {
        deallocate(ptr_);
    }
}