// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! NVMe controller registers.

#![no_std]
#![allow(missing_docs)]

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// NVMe controller registers (memory-mapped, 4K aligned)
#[repr(C, align(4096))]
pub struct ControllerRegisters {
    /// Controller Capabilities (CAP) - offset 0x0000
    pub cap: AtomicU64,
    /// Version (VS) - offset 0x0008
    pub vs: AtomicU32,
    /// Interrupt Mask Set (INTMS) - offset 0x000C
    pub intms: AtomicU32,
    /// Interrupt Mask Clear (INTMC) - offset 0x0010
    pub intmc: AtomicU32,
    /// Controller Configuration (CC) - offset 0x0014
    pub cc: AtomicU32,
    /// Reserved - offset 0x0018
    _reserved0: AtomicU32,
    /// Controller Status (CSTS) - offset 0x001C
    pub csts: AtomicU32,
    /// Reserved - offset 0x0020
    _reserved1: AtomicU32,
    /// Admin Queue Attributes (AQA) - offset 0x0024
    pub aqa: AtomicU32,
    /// Admin Submission Queue Base Address (ASQ) - offset 0x0028
    pub asq: AtomicU64,
    /// Admin Completion Queue Base Address (ACQ) - offset 0x0030
    pub acq: AtomicU64,
    /// Reserved - offset 0x0038
    _reserved2: AtomicU64,
    /// I/O Submission Queue Entry Size (SQES) - offset 0x0040
    pub sqes: AtomicU32,
    /// I/O Completion Queue Entry Size (CQES) - offset 0x0044
    pub cqes: AtomicU32,
    /// Reserved - offset 0x0048
    _reserved3: [AtomicU32; 2],
    /// I/O Submission Queue Base Address Array (SQ) - offset 0x0050
    pub sq: [AtomicU64; 32],
    /// I/O Completion Queue Base Address Array (CQ) - offset 0x0150
    pub cq: [AtomicU64; 32],
    /// Reserved - offset 0x0250
    _reserved4: [AtomicU64; 32],
    /// Doorbell Registers (DSTRD) - offset 0x0450
    pub dstrd: [AtomicU32; 256],
}

impl ControllerRegisters {
    /// Read the raw 64-bit CAP register.
    pub fn read_cap_raw(&self) -> u64 {
        self.cap.load(Ordering::Acquire)
    }

    /// Read controller capabilities
    pub fn read_cap(&self) -> ControllerCapabilities {
        let cap = self.read_cap_raw();
        let mpsmin = (cap >> 48) & 0xF;
        ControllerCapabilities {
            // MQES is a 0-based count of the maximum queue entries.
            max_queue_entries: ((cap & 0xFFFF) as u16).saturating_add(1).min(1024),
            contiguous_queues_required: (cap >> 16) & 1 != 0,
            arbitration_mechanism: ((cap >> 17) & 0x3) as u8,
            vendor_specific: ((cap >> 36) & 0x1) as u32,
            max_power_state: 0,
            admin_only_submission: false,
            supports_smbus: false,
            command_sets: ((cap >> 37) & 0xFF) as u32,
            nvm_subsystem_reset: (cap >> 36) & 1 != 0,
            page_size: 1u32 << (12 + mpsmin) as u32,
            max_data_transfer: 0,
            controller_model: 0,
            max_transfer_size: 0,
        }
    }

    /// Read controller status
    pub fn read_csts(&self) -> u32 {
        self.csts.load(Ordering::Acquire)
    }

    /// Write controller configuration
    pub fn write_cc(&self, value: u32) {
        self.cc.store(value, Ordering::Release);
    }

    /// Read controller configuration
    pub fn read_cc(&self) -> u32 {
        self.cc.load(Ordering::Acquire)
    }

    /// Update controller configuration
    pub fn cc_update<F>(&self, f: F)
    where
        F: FnOnce(u32) -> u32,
    {
        let current = self.cc.load(Ordering::Acquire);
        self.cc.store(f(current), Ordering::Release);
    }

    /// Set admin queue attributes
    pub fn set_aqa(&self, asqs: u16, acqs: u16) {
        let val = ((asqs as u32) | ((acqs as u32) << 16));
        self.aqa.store(val, Ordering::Release);
    }

    /// Set admin submission queue base address
    pub fn set_asq(&self, addr: u64) {
        self.asq.store(addr, Ordering::Release);
    }

    /// Set admin completion queue base address
    pub fn set_acq(&self, addr: u64) {
        self.acq.store(addr, Ordering::Release);
    }

    /// Set I/O submission queue base address
    pub fn set_sq_base(&self, qid: u16, addr: u64) {
        if (qid as usize) < self.sq.len() {
            self.sq[qid as usize].store(addr, Ordering::Release);
        }
    }

    /// Set I/O completion queue base address
    pub fn set_cq_base(&self, qid: u16, addr: u64) {
        if (qid as usize) < self.cq.len() {
            self.cq[qid as usize].store(addr, Ordering::Release);
        }
    }

    /// Number of dwords between adjacent doorbell registers (from CAP.DSTRD).
    fn doorbell_stride_dwords(&self) -> usize {
        1usize << ((self.read_cap_raw() >> 32) & 0xF) as usize
    }

    /// Write submission queue doorbell.
    ///
    /// Doorbell registers begin at controller offset `0x1000`. Queue `q`'s
    /// submission doorbell sits at `0x1000 + (2*q)*stride*4` bytes.
    pub fn write_sq_doorbell(&self, qid: u16, value: u16) {
        let stride = self.doorbell_stride_dwords();
        let off = 0x1000 / 4 + (2 * qid as usize) * stride;
        // SAFETY: doorbell registers live in the controller MMIO region at
        // base + 0x1000; a volatile 32-bit store is the defined access.
        unsafe {
            let ptr = (self as *const Self as *mut u32).add(off);
            core::ptr::write_volatile(ptr, value as u32);
        }
    }

    /// Write completion queue doorbell.
    ///
    /// Queue `q`'s completion doorbell sits at `0x1000 + (2*q+1)*stride*4`.
    pub fn write_cq_doorbell(&self, qid: u16, value: u16) {
        let stride = self.doorbell_stride_dwords();
        let off = 0x1000 / 4 + (2 * qid as usize + 1) * stride;
        unsafe {
            let ptr = (self as *const Self as *mut u32).add(off);
            core::ptr::write_volatile(ptr, value as u32);
        }
    }

    /// Set SQES
    pub fn set_sqes(&self, size: u8) {
        self.sqes.store(size as u32, Ordering::Release);
    }

    /// Set CQES
    pub fn set_cqes(&self, size: u8) {
        self.cqes.store(size as u32, Ordering::Release);
    }
}

/// Parsed Controller Capabilities
#[derive(Debug, Clone, Copy)]
pub struct ControllerCapabilities {
    pub max_queue_entries: u16,
    pub contiguous_queues_required: bool,
    pub arbitration_mechanism: u8,
    pub vendor_specific: u32,
    pub max_power_state: u8,
    pub admin_only_submission: bool,
    pub supports_smbus: bool,
    pub command_sets: u32,
    pub nvm_subsystem_reset: bool,
    pub page_size: u32,
    pub max_data_transfer: u32,
    pub controller_model: u8,
    pub max_transfer_size: u32,
}

/// Controller Configuration (CC) bits
#[derive(Debug, Clone, Copy)]
pub struct ControllerConfig {
    pub enable: bool,
    pub css: u8,        // Command Set Selected
    pub mps: u8,        // Memory Page Size
    pub ams: u8,        // Arbitration Mechanism Selected
    pub shn: u8,        // Shutdown Notification
    pub ios: u8,        // I/O Queue Size
    pub iosc: u8,       // I/O Completion Queue Size
    pub mps_max: u8,    // Memory Page Size Maximum
}

impl ControllerConfig {
    pub fn to_u32(self) -> u32 {
        let mut val: u32 = 0;
        // CC.EN at bit 0.
        val |= self.enable as u32;
        // CC.CSS at bits 4-7.
        val |= (self.css as u32 & 0xF) << 4;
        // CC.MPS at bits 8-11 (page size = 2^(12 + MPS)).
        val |= (self.mps as u32 & 0xF) << 8;
        // CC.AMS at bits 11-13.
        val |= (self.ams as u32 & 0x7) << 11;
        // CC.SHN at bits 14-15.
        val |= (self.shn as u32 & 0x3) << 14;
        val
    }
}

/// Controller Status (CSTS)
#[derive(Debug, Clone, Copy)]
pub struct ControllerStatus {
    pub ready: bool,
    pub controller_fatal_status: bool,
    pub shutdown_status: u8,
    pub nvm_subsystem_reset_occurred: bool,
    pub processing_paused: bool,
}

impl ControllerRegisters {
    pub fn read_csts_parsed(&self) -> ControllerStatus {
        let csts = self.csts.load(Ordering::Acquire);
        ControllerStatus {
            ready: csts & 0x1 != 0,
            controller_fatal_status: csts & 0x2 != 0,
            shutdown_status: ((csts >> 2) & 0x3) as u8,
            nvm_subsystem_reset_occurred: (csts & 0x8) != 0,
            processing_paused: (csts & 0x10) != 0,
        }
    }
}

/// Admin Queue Attributes (AQA)
#[derive(Debug, Clone, Copy)]
pub struct AdminQueueAttributes {
    pub asqs: u16,
    pub acqs: u16,
}

/// Completion Queue Entry
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct CompletionQueueEntry {
    pub dw0: u32,
    pub dw1: u32,
    pub sqhd: u16,
    pub sqid: u16,
    pub cid: u16,
    pub status: u16,
}

impl CompletionQueueEntry {
    pub fn status_code(&self) -> u16 {
        self.status >> 1
    }

    pub fn phase(&self) -> bool {
        self.status & 1 != 0
    }

    pub fn success(&self) -> bool {
        self.status == 0
    }
}

/// PRP Entry
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct PrpEntry {
    pub prp: u64,
}

/// NVMe Command Structure
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct NvmeCommand {
    pub opcode: u8,
    pub flags: u8,
    pub cid: u16,
    pub nsid: u32,
    pub cdw2: u32,
    pub cdw3: u32,
    pub metadata_ptr: u64,
    pub prp1: u64,
    pub prp2: u64,
    pub cdw10: u32,
    pub cdw11: u32,
    pub cdw12: u32,
    pub cdw13: u32,
    pub cdw14: u32,
    pub cdw15: u32,
}

impl NvmeCommand {
    pub fn new() -> Self {
        Self {
            opcode: 0,
            flags: 0,
            cid: 0,
            nsid: 0,
            cdw2: 0,
            cdw3: 0,
            metadata_ptr: 0,
            prp1: 0,
            prp2: 0,
            cdw10: 0,
            cdw11: 0,
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        }
    }

    pub fn admin_command(opcode: u8) -> Self {
        let mut cmd = Self::new();
        cmd.opcode = opcode;
        cmd.flags = 0x01; // Admin command
        cmd
    }

    pub fn nvm_command(opcode: u8) -> Self {
        let mut cmd = Self::new();
        cmd.opcode = opcode;
        cmd.flags = 0x00; // NVM command
        cmd
    }
}