// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! AHCI (Advanced Host Controller Interface) implementation.

#![no_std]
#![allow(missing_docs)]

extern crate alloc;

use core::ptr::NonNull;
use core::sync::atomic::{AtomicU32, Ordering};
use alloc::vec::Vec;
use alloc::boxed::Box;
use alloc::sync::Arc;
use spin::Mutex;

use driver_common::{DriverError, DriverResult, alloc_dma_pages, free_dma_pages, phys_to_virt, virt_to_phys, PciDeviceInfo};

// The controller/port structs hold MMIO pointers (`NonNull`) and are shared
// across threads via `Arc`/`Mutex`; accessing them is always done through
// volatile atomic register accesses, so marking them `Send`/`Sync` is sound.
unsafe impl Send for AhciController {}
unsafe impl Sync for AhciController {}
unsafe impl Send for AhciPort {}
unsafe impl Sync for AhciPort {}
unsafe impl Send for SataPort {}
unsafe impl Sync for SataPort {}
use crate::port::SataPort;

/// AHCI Generic Host Control registers
#[repr(C)]
pub struct AhciRegisters {
    pub cap: AtomicU32,        // 0x00: Host Capabilities
    pub ghc: AtomicU32,        // 0x04: Global Host Control
    pub is: AtomicU32,         // 0x08: Interrupt Status
    pub pi: AtomicU32,         // 0x0C: Ports Implemented
    pub vs: AtomicU32,         // 0x10: Version
    pub ccc_ctl: AtomicU32,    // 0x14: Command Completion Coalescing Control
    pub ccc_pts: AtomicU32,    // 0x18: Command Completion Coalescing Ports
    pub em_loc: AtomicU32,     // 0x1C: Enclosure Management Location
    pub em_ctl: AtomicU32,     // 0x20: Enclosure Management Control
    pub cap2: AtomicU32,       // 0x24: Host Capabilities Extended
    pub bohc: AtomicU32,       // 0x28: BIOS/OS Handoff Control and Status
    _reserved: [u8; 0xA0 - 0x2C],
    pub ports: [AhciPortRegisters; 32], // 0x100 - 0x10FF
}

/// AHCI Port Registers
#[repr(C)]
pub struct AhciPortRegisters {
    pub clb: AtomicU32,        // 0x00: Command List Base Address
    pub clbu: AtomicU32,       // 0x04: Command List Base Address Upper 32 bits
    pub fb: AtomicU32,         // 0x08: FIS Base Address
    pub fbu: AtomicU32,        // 0x0C: FIS Base Address Upper 32 bits
    pub is: AtomicU32,         // 0x10: Interrupt Status
    pub ie: AtomicU32,         // 0x14: Interrupt Enable
    pub cmd: AtomicU32,        // 0x18: Command and Status
    _reserved0: AtomicU32,     // 0x1C: Reserved
    pub tfd: AtomicU32,        // 0x20: Task File Data
    pub sig: AtomicU32,        // 0x24: Signature
    pub ssts: AtomicU32,       // 0x28: Serial ATA Status
    pub sctl: AtomicU32,       // 0x2C: Serial ATA Control
    pub serr: AtomicU32,       // 0x30: Serial ATA Error
    pub sact: AtomicU32,       // 0x34: Serial ATA Active
    pub ci: AtomicU32,         // 0x38: Command Issue
    pub sntf: AtomicU32,       // 0x3C: SNotification Register
    pub fbs: AtomicU32,        // 0x40: FIS-based Switching Control
    pub devslp: AtomicU32,     // 0x44: Device Sleep
    _reserved1: [AtomicU32; 11], // 0x48 - 0x70
    pub vs: [AtomicU32; 4],    // 0x74 - 0x80: Vendor Specific
}

/// AHCI Controller
pub struct AhciController {
    registers: NonNull<AhciRegisters>,
    ports: Vec<Option<Arc<AhciPort>>>,
    num_ports: u32,
    cap: u32,
    cap2: u32,
}

impl AhciController {
    /// Create a new AHCI controller from PCI device
    pub fn new(pci_dev: PciDeviceInfo) -> DriverResult<Self> {
        let bar5 = pci_dev.bar.get(5).copied().unwrap_or(0);
        if bar5 == 0 {
            return Err(DriverError::HardwareFault);
        }
        
        let regs_ptr = unsafe { phys_to_virt(bar5) } as *mut AhciRegisters;
        let registers = NonNull::new(regs_ptr).ok_or(DriverError::HardwareFault)?;
        
        let cap = unsafe { registers.as_ref().cap.load(Ordering::Acquire) };
        let cap2 = unsafe { registers.as_ref().cap2.load(Ordering::Acquire) };
        let pi = unsafe { registers.as_ref().pi.load(Ordering::Acquire) };
        
        let num_ports = (0..32).filter(|i| (pi >> i) & 1 == 1).count() as u32;
        
        let mut controller = Self {
            registers,
            ports: Vec::with_capacity(num_ports as usize),
            num_ports,
            cap,
            cap2,
        };
        
        controller.init()?;
        Ok(controller)
    }
    
    /// Initialize the controller
    fn init(&mut self) -> DriverResult<()> {
        let regs = unsafe { self.registers.as_mut() };
        
        // Reset controller
        regs.ghc.fetch_or(1u32 << 31, Ordering::Release); // HBA Reset
        let mut timeout = 10000;
        while regs.ghc.load(Ordering::Acquire) & (1u32 << 31) != 0 {
            core::hint::spin_loop();
            timeout -= 1;
            if timeout == 0 {
                return Err(DriverError::Timeout);
            }
        }
        
        // Enable AHCI mode
        regs.ghc.fetch_or(1u32 << 31, Ordering::Release); // AHCI Enable
        
        // Initialize each implemented port
        let pi = regs.pi.load(Ordering::Acquire);
        for i in 0..32 {
            if (pi >> i) & 1 == 1 {
                let port = AhciPort::new(self.registers, i as u8)?;
                self.ports.push(Some(Arc::new(port)));
            } else {
                self.ports.push(None);
            }
        }
        
        Ok(())
    }
    
    /// Get ports
    pub fn ports(&self) -> &[Option<Arc<AhciPort>>] {
        &self.ports
    }
}

/// AHCI Port
pub struct AhciPort {
    controller: NonNull<AhciRegisters>,
    port_num: u8,
    cmd_list_phys: u64,
    fis_phys: u64,
    cmd_tables: Vec<u64>,
    sata_port: Arc<Mutex<SataPort>>,
}

impl AhciPort {
    /// Create a new AHCI port
    fn new(controller: NonNull<AhciRegisters>, port_num: u8) -> DriverResult<Self> {
        let regs = unsafe { controller.as_ref() };
        let port_regs = &regs.ports[port_num as usize];
        
        // Check if device is present
        let ssts = port_regs.ssts.load(Ordering::Acquire);
        let det = (ssts >> 0) & 0xF;
        let ipm = (ssts >> 8) & 0xF;
        
        if det != 3 || ipm != 1 {
            return Err(DriverError::NotFound);
        }
        
        // Allocate command list (32 commands, 1024 bytes each, 1K aligned)
        let cmd_list_pages = 1;
        let cmd_list_phys = unsafe { alloc_dma_pages(cmd_list_pages) }.ok_or(DriverError::OutOfMemory)?;
        let cmd_list_virt = unsafe { phys_to_virt(cmd_list_phys) } as *mut u8;
        unsafe { core::ptr::write_bytes(cmd_list_virt, 0, 1024) };
        
        // Allocate FIS receive area (256 bytes, 256-byte aligned)
        let fis_pages = 1;
        let fis_phys = unsafe { alloc_dma_pages(fis_pages) }.ok_or(DriverError::OutOfMemory)?;
        let fis_virt = unsafe { phys_to_virt(fis_phys) } as *mut u8;
        unsafe { core::ptr::write_bytes(fis_virt, 0, 256) };
        
        // Allocate command tables (32 tables, each up to 4KB)
        let mut cmd_tables = Vec::with_capacity(32);
        for _ in 0..32 {
            let table_phys = unsafe { alloc_dma_pages(1) }.ok_or(DriverError::OutOfMemory)?;
            let table_virt = unsafe { phys_to_virt(table_phys) } as *mut u8;
            unsafe { core::ptr::write_bytes(table_virt, 0, 4096) };
            cmd_tables.push(table_phys);
        }
        
        // Setup command list entries
        for i in 0..32 {
            let entry_ptr = unsafe { cmd_list_virt.add(i * 32) } as *mut AhciCommandHeader;
            unsafe {
                (*entry_ptr).prdtl = 0;
                (*entry_ptr).ctba = cmd_tables[i] as u32;
                (*entry_ptr).ctbau = (cmd_tables[i] >> 32) as u32;
            }
        }
        
        // Program port registers
        port_regs.clb.store(cmd_list_phys as u32, Ordering::Release);
        port_regs.clbu.store((cmd_list_phys >> 32) as u32, Ordering::Release);
        port_regs.fb.store(fis_phys as u32, Ordering::Release);
        port_regs.fbu.store((fis_phys >> 32) as u32, Ordering::Release);
        
        // Clear any pending interrupts
        port_regs.is.store(0xFFFFFFFF, Ordering::Release);
        
        // Enable interrupts
        port_regs.ie.store(
            (1u32 << 30) | // TFES
            (1u32 << 26) | // HBFS
            (1u32 << 25) | // HBDS
            (1u32 << 24) | // IFS
            (1u32 << 23) | // INFS
            (1u32 << 22) | // RCS
            (1u32 << 7)  | // DMPS
            (1u32 << 6)  | // PRCS
            (1u32 << 5)  | // UFS
            (1u32 << 4)  | // SDBS
            (1u32 << 3)  | // DSS
            (1u32 << 2)  | // PSS
            (1u32 << 1)  | // DHS
            (1u32 << 0),   // DPS
            Ordering::Release
        );
        
        // Start port (FRE and ST)
        port_regs.cmd.fetch_or((1u32 << 4) | (1u32 << 0), Ordering::Release);
        
        let sata_port = SataPort::from_ahci_port(controller, port_num);
        
        Ok(Self {
            controller,
            port_num,
            cmd_list_phys,
            fis_phys,
            cmd_tables,
            sata_port: Arc::new(Mutex::new(sata_port)),
        })
    }
    
    /// Check if device is present
    pub fn is_present(&self) -> bool {
        let regs = unsafe { self.controller.as_ref() };
        let port_regs = &regs.ports[self.port_num as usize];
        let ssts = port_regs.ssts.load(Ordering::Acquire);
        let det = (ssts >> 0) & 0xF;
        let ipm = (ssts >> 8) & 0xF;
        det == 3 && ipm == 1
    }
    
    /// Get SATA port
    pub fn sata_port(&self) -> Arc<Mutex<SataPort>> {
        self.sata_port.clone()
    }
}

impl Drop for AhciPort {
    fn drop(&mut self) {
        let regs = unsafe { self.controller.as_ref() };
        let port_regs = &regs.ports[self.port_num as usize];
        
        // Stop port
        port_regs.cmd.fetch_and(!((1u32 << 4) | (1u32 << 0)), Ordering::Release);
        
        // Free DMA memory
        unsafe { free_dma_pages(self.cmd_list_phys, 1); }
        unsafe { free_dma_pages(self.fis_phys, 1); }
        for &table in &self.cmd_tables {
            unsafe { free_dma_pages(table, 1); }
        }
    }
}

/// AHCI Command Header
#[repr(C)]
pub struct AhciCommandHeader {
    pub cfl: u8,        // Command FIS Length
    pub a: u8,          // ATAPI
    pub w: u8,          // Write
    pub p: u8,          // Prefetchable
    pub r: u8,          // Reset
    pub b: u8,          // BIST
    pub c: u8,          // Clear Busy
    rsv0: u8,
    pub pmp: u8,        // Port Multiplier Port
    rsv1: u8,
    pub prdtl: u16,     // Physical Region Descriptor Table Length
    pub prdbc: u32,     // Physical Region Descriptor Byte Count
    pub ctba: u32,      // Command Table Base Address
    pub ctbau: u32,     // Command Table Base Address Upper
    rsv2: [u32; 4],
}

/// FIS Types
#[repr(u8)]
enum FisType {
    RegH2D = 0x27,
    RegD2H = 0x34,
    DmaAct = 0x39,
    DmaSetup = 0x41,
    Data = 0x46,
    Bist = 0x58,
    PioSetup = 0x5F,
    DevBits = 0xA1,
}