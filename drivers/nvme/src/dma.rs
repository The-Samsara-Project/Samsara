// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! Page-aligned, physically-contiguous DMA buffers for NVMe transfers.

#![no_std]

use driver_common::{DriverError, DriverResult, alloc_dma_pages, free_dma_pages, phys_to_virt};

/// A DMA buffer: page aligned, physically contiguous, mapped in the kernel
/// higher half and zeroed on allocation.
///
/// NVMe PRPs demand physical addresses that are page aligned and form a chain
/// of *contiguous* pages. The kernel heap hands out arbitrary byte-aligned
/// blocks whose pages come from independent PMM allocations, so a heap `Vec`
/// can never back a PRP: the controller would either reject the buffer (PRP
/// offset not page aligned) or scribble across unrelated physical pages.
/// Every transfer and identify buffer goes through this instead.
pub struct DmaBuf {
    phys: u64,
    len: usize,
    pages: usize,
}

impl DmaBuf {
    /// Allocate a zeroed DMA buffer holding at least `len` bytes.
    pub fn new(len: usize) -> DriverResult<Self> {
        let pages = len.div_ceil(0x1000);
        if pages == 0 {
            return Err(DriverError::InvalidArgument);
        }
        let phys = unsafe { alloc_dma_pages(pages) }.ok_or(DriverError::OutOfMemory)?;
        let virt = unsafe { phys_to_virt(phys) } as *mut u8;
        // SAFETY: `[virt, virt + pages*0x1000)` is kernel-mapped physical-map
        // memory dedicated to this buffer. Zero it so Identify/transfer
        // semantics are well defined no matter what the controller writes.
        unsafe { core::ptr::write_bytes(virt, 0, pages * 0x1000) };
        Ok(Self { phys, len, pages })
    }

    /// Physical (bus) address of the buffer, for PRP entries.
    pub fn phys(&self) -> u64 {
        self.phys
    }

    /// Kernel virtual pointer to the buffer start.
    fn ptr(&self) -> *mut u8 {
        unsafe { phys_to_virt(self.phys) as *mut u8 }
    }

    /// Contents as an immutable slice.
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: the buffer is kernel-mapped and only dropped after the
        // transfer it backs has completed.
        unsafe { core::slice::from_raw_parts(self.ptr(), self.len) }
    }

    /// Contents as a mutable slice.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: the buffer is kernel-mapped and uniquely borrowed here.
        unsafe { core::slice::from_raw_parts_mut(self.ptr(), self.len) }
    }
}

impl Drop for DmaBuf {
    fn drop(&mut self) {
        unsafe { free_dma_pages(self.phys, self.pages) };
    }
}