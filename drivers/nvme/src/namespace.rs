// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! NVMe namespace management

#![no_std]
#![allow(missing_docs)]

extern crate alloc;

use core::ptr::NonNull;

use driver_common::{DriverError, DriverResult};
use crate::commands::*;
use crate::dma::DmaBuf;
use crate::registers::{CompletionQueueEntry, NvmeCommand, PrpEntry};

/// NVMe namespace
#[derive(Clone)]
pub struct Namespace {
    id: u32,
    size: u64,
    capacity: u64,
    block_size: u32,
    features: NamespaceFeatures,
    lba_format: LbaFormat,
}

/// Namespace features
#[derive(Debug, Clone, Copy, Default)]
pub struct NamespaceFeatures {
    pub thin_provisioning: bool,
    pub deallocate: bool,
    pub write_protected: bool,
    pub format_in_progress: bool,
}

impl Namespace {
/// Create a new namespace by sending Identify command
    pub fn new(controller: &mut crate::NvmeController, nsid: u32) -> DriverResult<Self> {
        let buf = DmaBuf::new(4096)?;

        let mut admin_cmd = identify_namespace(nsid);
        admin_cmd.prp1 = buf.phys();
        admin_cmd.cid = controller.admin_queue.submit(admin_cmd)?;
        controller.admin_queue.ring_doorbell(unsafe { controller.registers.as_ref() });

        if !controller.wait_for_admin_completion(admin_cmd.cid)? {
            return Err(DriverError::IoError);
        }

        let ident_data =
            unsafe { &*(buf.as_slice().as_ptr() as *const IdentifyNamespaceData) };
        
        let flbas = ident_data.flbas;
        // Copy the LBA format array out of the packed struct before indexing it.
        let lbaf_arr = ident_data.lbaf;
        let lbaf = lbaf_arr[flbas as usize & 0xF];
        let block_size = 1u32 << lbaf.lbads;
        
        let features = NamespaceFeatures {
            thin_provisioning: ident_data.nsfeat & 0x01 != 0,
            deallocate: ident_data.nsfeat & 0x02 != 0,
            write_protected: ident_data.nsfeat & 0x08 != 0,
            format_in_progress: ident_data.nsfeat & 0x10 != 0,
        };
        
        Ok(Self {
            id: nsid,
            size: ident_data.nsze,
            capacity: ident_data.ncap,
            block_size,
            features,
            lba_format: lbaf,
        })
    }

    /// Get namespace ID
    pub fn id(&self) -> u32 {
        self.id
    }

    /// Get namespace size in blocks
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Get namespace capacity
    pub fn capacity(&self) -> u64 {
        self.capacity
    }

    /// Get block size
    pub fn block_size(&self) -> u32 {
        self.block_size
    }

    /// Get namespace features
    pub fn features(&self) -> &NamespaceFeatures {
        &self.features
    }

    /// Read from namespace using PRP lists
    pub fn read(&self, controller: &mut crate::NvmeController, lba: u64, buf: &mut [u8]) -> DriverResult<usize> {
        if buf.len() % self.block_size as usize != 0 {
            return Err(DriverError::AlignmentError);
        }

        let num_blocks = buf.len() / self.block_size as usize;
        if num_blocks == 0 {
            return Ok(0);
        }

        if lba + num_blocks as u64 > self.size {
            return Err(DriverError::InvalidArgument);
        }

// Use first available I/O queue
        let qid = controller.io_queues.keys().next().copied().unwrap_or(1);
        let queue = controller.io_queues.get_mut(&qid).ok_or(DriverError::NotReady)?;

        // Bounce through page-aligned, physically contiguous DMA memory: the
        // caller's slice lives in the kernel heap, whose pages are neither.
        let mut dma = DmaBuf::new(buf.len())?;
        let (prp1, prp2) =
            queue.build_prp(dma.phys(), buf.len(), controller.page_size as usize)?;

        let cmd = read(self.id, lba, num_blocks as u16, prp1, prp2);
        let mut io_cmd = cmd;
        io_cmd.cid = queue.submit(io_cmd)?;
        queue.ring_doorbell(unsafe { controller.registers.as_ref() });

        // Wait for completion (polling for now)
        let n = self.wait_for_completion(controller, qid, io_cmd.cid, buf.len())?;
        buf.copy_from_slice(dma.as_slice());
        Ok(n)
    }

    /// Write to namespace using PRP lists
    pub fn write(&self, controller: &mut crate::NvmeController, lba: u64, buf: &[u8]) -> DriverResult<usize> {
        if buf.len() % self.block_size as usize != 0 {
            return Err(DriverError::AlignmentError);
        }

        let num_blocks = buf.len() / self.block_size as usize;
        if num_blocks == 0 {
            return Ok(0);
        }

        if lba + num_blocks as u64 > self.size {
            return Err(DriverError::InvalidArgument);
        }

let qid = controller.io_queues.keys().next().copied().unwrap_or(1);
        let queue = controller.io_queues.get_mut(&qid).ok_or(DriverError::NotReady)?;

        // Same story as read(): the caller's data must land in DMA memory
        // before the controller is allowed anywhere near it.
        let mut dma = DmaBuf::new(buf.len())?;
        dma.as_mut_slice().copy_from_slice(buf);
        let (prp1, prp2) =
            queue.build_prp(dma.phys(), buf.len(), controller.page_size as usize)?;

        let cmd = write(self.id, lba, num_blocks as u16, prp1, prp2, true);
        let mut io_cmd = cmd;
        io_cmd.cid = queue.submit(io_cmd)?;
        queue.ring_doorbell(unsafe { controller.registers.as_ref() });

        self.wait_for_completion(controller, qid, io_cmd.cid, buf.len())
    }

    /// Flush namespace
    pub fn flush(&self, controller: &mut crate::NvmeController) -> DriverResult<()> {
        let qid = controller.io_queues.keys().next().copied().unwrap_or(1);
        let queue = controller.io_queues.get_mut(&qid).ok_or(DriverError::NotReady)?;

        let cmd = flush(self.id);
        let mut io_cmd = cmd;
        io_cmd.cid = queue.submit(io_cmd)?;
        queue.ring_doorbell(unsafe { controller.registers.as_ref() });

        self.wait_for_completion(controller, qid, io_cmd.cid, 0).map(|_| ())
    }

    /// Dataset management (TRIM)
    pub fn trim(&self, controller: &mut crate::NvmeController, ranges: &[DsmRange]) -> DriverResult<()> {
        if !self.features.deallocate {
            return Err(DriverError::UnsupportedFeature);
        }

let qid = controller.io_queues.keys().next().copied().unwrap_or(1);
        let queue = controller.io_queues.get_mut(&qid).ok_or(DriverError::NotReady)?;

        // Serialize the ranges into DMA memory; the controller reads the range
        // list straight off the bus.
        let range_size = ranges.len() * core::mem::size_of::<DsmRange>();
        let mut dma = DmaBuf::new(range_size)?;
        {
            let dst = dma.as_mut_slice();
            for (i, range) in ranges.iter().enumerate() {
                let offset = i * core::mem::size_of::<DsmRange>();
                // SAFETY: `dst[offset..offset + size_of::<DsmRange>()]` fits
                // by construction; the source is a valid `DsmRange`.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        range as *const DsmRange as *const u8,
                        dst.as_mut_ptr().add(offset),
                        core::mem::size_of::<DsmRange>(),
                    );
                }
            }
        }

        let (prp1, prp2) =
            queue.build_prp(dma.phys(), range_size, controller.page_size as usize)?;

        let cmd = dataset_management(self.id, ranges.len() as u32, prp1, prp2);
        let mut io_cmd = cmd;
        io_cmd.cid = queue.submit(io_cmd)?;
        queue.ring_doorbell(unsafe { controller.registers.as_ref() });

        self.wait_for_completion(controller, qid, io_cmd.cid, 0).map(|_| ())
    }

    /// Wait for I/O completion
    fn wait_for_completion(
        &self,
        controller: &mut crate::NvmeController,
        qid: u16,
        cid: u16,
        expected_bytes: usize,
    ) -> DriverResult<usize> {
        let deadline = driver_common::ticks() + driver_common::timer_hz() * 30; // 30 second timeout
        
        loop {
            let completions = controller.poll_io_completions(qid);
            for cqe in completions {
                if cqe.cid == cid {
                    if cqe.success() {
                        return Ok(expected_bytes);
                    } else {
                        driver_common::kerror!("nvme: command failed with status {:04x}", cqe.status_code());
                        return Err(DriverError::IoError);
                    }
                }
            }
            
            if driver_common::ticks() >= deadline {
                return Err(DriverError::Timeout);
            }
            core::hint::spin_loop();
        }
    }
}

// Identify data structures (`IdentifyControllerData`, `IdentifyNamespaceData`,
// `LbaFormat`, `PowerStateDescriptor`) are defined in `crate::commands` and
// re-used here through `use crate::commands::*`.