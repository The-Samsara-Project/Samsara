// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! NVMe queue management with PRP list support.

#![no_std]
#![allow(missing_docs)]

extern crate alloc;

use core::fmt;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU16, Ordering};

use crate::registers::{CompletionQueueEntry, NvmeCommand, PrpEntry};

/// Maximum number of PRP entries per page (4096 / 8 = 512)
const PRP_PER_PAGE: usize = 512;

/// NVMe queue pair (submission + completion)
pub struct Queue {
    pub id: u16,
    pub depth: u16,
    pub submission_phys: u64,
    pub completion_phys: u64,
    sq_head: AtomicU16,
    sq_tail: AtomicU16,
    cq_head: AtomicU16,
    cq_phase: bool,
    /// Submission queue entries (mapped in kernel virtual memory)
    submission_queue: NonNull<[NvmeCommand]>,
    /// Completion queue entries (mapped in kernel virtual memory)
    completion_queue: NonNull<[CompletionQueueEntry]>,
    /// PRP list buffers for commands that need them
    prp_lists: alloc::vec::Vec<NonNull<[PrpEntry]>>,
}

impl Queue {
    /// Create a new queue pair with allocated memory
    pub fn new(depth: u16) -> driver_common::DriverResult<Self> {
        use driver_common::DriverError;
        
        let depth = depth as usize;
        
        // Allocate submission queue (aligned to 4K)
        let sq_size = depth * core::mem::size_of::<NvmeCommand>();
        let sq_pages = (sq_size + 4095) / 4096;
        let sq_phys = unsafe { driver_common::alloc_dma_pages(sq_pages) }
            .ok_or(DriverError::OutOfMemory)?;
        let sq_virt = unsafe { driver_common::phys_to_virt(sq_phys) } as *mut NvmeCommand;
        
        // Allocate completion queue (aligned to 4K)
        let cq_size = depth * core::mem::size_of::<CompletionQueueEntry>();
        let cq_pages = (cq_size + 4095) / 4096;
        let cq_phys = unsafe { driver_common::alloc_dma_pages(cq_pages) }
            .ok_or(DriverError::OutOfMemory)?;
        let cq_virt = unsafe { driver_common::phys_to_virt(cq_phys) } as *mut CompletionQueueEntry;
        
        // Zero both queues
        unsafe {
            core::ptr::write_bytes(sq_virt, 0, depth);
            core::ptr::write_bytes(cq_virt, 0, depth);
        }
        
        Ok(Self {
            id: 0,
            depth: depth as u16,
            submission_phys: sq_phys,
            completion_phys: cq_phys,
            sq_head: AtomicU16::new(0),
            sq_tail: AtomicU16::new(0),
            cq_head: AtomicU16::new(0),
            cq_phase: true,
            submission_queue: unsafe { NonNull::new_unchecked(core::slice::from_raw_parts_mut(sq_virt, depth)) },
            completion_queue: unsafe { NonNull::new_unchecked(core::slice::from_raw_parts_mut(cq_virt, depth)) },
            prp_lists: alloc::vec::Vec::new(),
        })
    }

    /// Get a mutable reference to a submission queue entry
    pub fn get_sq_entry(&mut self, index: u16) -> Option<&mut NvmeCommand> {
        if (index as usize) < self.depth as usize {
            unsafe { Some(&mut self.submission_queue.as_mut()[index as usize]) }
        } else {
            None
        }
    }

    /// Get a reference to a completion queue entry
    pub fn get_cq_entry(&mut self, index: u16) -> Option<&CompletionQueueEntry> {
        if (index as usize) < self.depth as usize {
            unsafe { Some(&self.completion_queue.as_ref()[index as usize]) }
        } else {
            None
        }
    }

    /// Submit a command to the submission queue
    pub fn submit(&mut self, mut cmd: NvmeCommand) -> driver_common::DriverResult<u16> {
        let tail = self.sq_tail.load(Ordering::Acquire);
        let next = (tail + 1) % self.depth;

        if next == self.sq_head.load(Ordering::Acquire) {
            return Err(driver_common::DriverError::Busy);
        }

        // Assign command ID
        cmd.cid = tail;
        
        // Store command
        unsafe {
            self.submission_queue.as_mut()[tail as usize] = cmd;
        }

        self.sq_tail.store(next, Ordering::Release);

        Ok(tail)
    }

    /// Ring the submission queue doorbell
    pub fn ring_doorbell(&self, controller: &crate::registers::ControllerRegisters) {
        let tail = self.sq_tail.load(Ordering::Acquire);
        controller.write_sq_doorbell(self.id, tail);
    }

    /// Poll for completion
    pub fn poll_completion(&mut self) -> Option<CompletionQueueEntry> {
        let head = self.cq_head.load(Ordering::Acquire);

        if (head as usize) >= self.depth as usize {
            return None;
        }

        let entry = unsafe { self.completion_queue.as_ref()[head as usize] };
        if entry.phase() != self.cq_phase {
            return None;
        }

        let next = (head + 1) % self.depth;
        self.cq_head.store(next, Ordering::Release);

        if next == 0 {
            self.cq_phase = !self.cq_phase;
        }

        Some(entry)
    }

    /// Get submission queue physical address
    pub fn submission_phys(&self) -> u64 {
        self.submission_phys
    }

    /// Get completion queue physical address
    pub fn completion_phys(&self) -> u64 {
        self.completion_phys
    }

    /// Build the PRP entries for a transfer buffer.
    ///
    /// Returns `(prp1, prp2)`. For a single page `prp2` is 0; for two pages
    /// `prp2` points at the second page; for larger transfers `prp2` points at
    /// a single page-sized PRP list (covering up to 512 pages) allocated from
    /// DMA memory.
    pub fn build_prp(
        &mut self,
        buffer_phys: u64,
        length: usize,
        page_size: usize,
    ) -> driver_common::DriverResult<(u64, u64)> {
        use driver_common::DriverError;

        if length == 0 {
            return Ok((0, 0));
        }

        let page_size = page_size as u64;
        let num_pages = (length as u64 + page_size - 1) / page_size;

        if num_pages <= 1 {
            return Ok((buffer_phys, 0));
        }
        if num_pages == 2 {
            return Ok((buffer_phys, buffer_phys + page_size));
        }

        if num_pages > (page_size / 8) {
            // One PRP list page holds at most `page_size/8` entries; chained
            // lists are not yet supported.
            return Err(DriverError::BufferTooSmall);
        }

        let list_phys = unsafe { driver_common::alloc_dma_pages(1) }
            .ok_or(DriverError::OutOfMemory)?;
        let list_virt = unsafe { driver_common::phys_to_virt(list_phys) } as *mut u64;

        unsafe {
            for i in 0..num_pages {
                *list_virt.add(i as usize) = buffer_phys + i * page_size;
            }
        }

        self.prp_lists.push(unsafe {
            NonNull::new_unchecked(core::slice::from_raw_parts_mut(
                list_virt as *mut PrpEntry,
                num_pages as usize,
            ))
        });

        Ok((buffer_phys, list_phys))
    }

    /// Get current tail index (for doorbell)
    pub fn tail(&self) -> u16 {
        self.sq_tail.load(Ordering::Acquire)
    }
}

impl fmt::Debug for Queue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Queue")
            .field("id", &self.id)
            .field("depth", &self.depth)
            .field("sq_head", &self.sq_head.load(Ordering::Relaxed))
            .field("sq_tail", &self.sq_tail.load(Ordering::Relaxed))
            .field("cq_head", &self.cq_head.load(Ordering::Relaxed))
            .finish()
    }
}