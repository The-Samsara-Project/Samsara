// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! SATA Port implementation - handles ATA command execution.

#![no_std]
#![allow(missing_docs)]

extern crate alloc;

use core::ptr::NonNull;
use core::sync::atomic::{AtomicU32, Ordering};
use alloc::vec::Vec;
use alloc::boxed::Box;
use alloc::sync::Arc;

use driver_common::{DriverError, DriverResult, alloc_dma_pages, free_dma_pages, phys_to_virt, virt_to_phys};
use crate::ahci::{AhciRegisters, AhciPortRegisters, AhciCommandHeader};
use crate::ata::{IdentifyData, CommandTable, PrdtEntry, build_read_dma_ext_fis, build_write_dma_ext_fis, build_flush_cache_ext_fis, build_identify_fis, build_set_features_fis, status};

/// SATA Port - wraps an AHCI port and provides ATA command interface
pub struct SataPort {
    controller: NonNull<AhciRegisters>,
    port_num: u8,
    port_regs: NonNull<AhciPortRegisters>,
    cmd_slot: u32,
}

impl SataPort {
    /// Create a new SATA port
    pub fn new(port_num: u8) -> DriverResult<Self> {
        // This is a placeholder - the actual AHCI port is created in ahci.rs
        // This constructor is used for the block device interface
        Err(DriverError::NotSupported)
    }
    
    /// Create from AHCI port registers
    pub(crate) fn from_ahci_port(controller: NonNull<AhciRegisters>, port_num: u8) -> Self {
        let port_regs = unsafe { 
            NonNull::new_unchecked(
                &(*controller.as_ptr()).ports[port_num as usize] as *const AhciPortRegisters as *mut AhciPortRegisters
            )
        };
        
        Self {
            controller,
            port_num,
            port_regs,
            cmd_slot: 0,
        }
    }
    
    /// Get port registers
    fn port_regs(&self) -> &AhciPortRegisters {
        unsafe { self.port_regs.as_ref() }
    }
    
    /// Wait for port to be ready (not busy, DRQ clear)
    fn wait_ready(&self, timeout_ms: u32) -> DriverResult<()> {
        let port_regs = self.port_regs();
        let mut timeout = timeout_ms * 1000;
        
        loop {
            let ssts = port_regs.ssts.load(Ordering::Acquire);
            let det = (ssts >> 0) & 0xF;
            if det != 3 {
                return Err(DriverError::NotFound);
            }
            
            let tfd = port_regs.tfd.load(Ordering::Acquire);
            if (tfd & (status::BSY | status::DRQ)) == 0 {
                return Ok(());
            }
            
            if timeout == 0 {
                return Err(DriverError::Timeout);
            }
            timeout -= 1;
            core::hint::spin_loop();
        }
    }
    
    /// Find a free command slot
    fn find_free_slot(&self) -> Option<u32> {
        let port_regs = self.port_regs();
        let ci = port_regs.ci.load(Ordering::Acquire);
        let sact = port_regs.sact.load(Ordering::Acquire);
        let used = ci | sact;
        
        for i in 0..32 {
            if (used >> i) & 1 == 0 {
                return Some(i);
            }
        }
        None
    }
    
    /// Execute a command with PRDT
    fn execute_command(&mut self, fis: [u8; 64], prdt: &[PrdtEntry], is_write: bool) -> DriverResult<()> {
        let slot = self.find_free_slot().ok_or(DriverError::Busy)?;
        self.cmd_slot = slot;
        
        let port_regs = self.port_regs();
        
        // Get command header
        let clb = port_regs.clb.load(Ordering::Acquire) as u64 | 
                  ((port_regs.clbu.load(Ordering::Acquire) as u64) << 32);
        let cmd_header_ptr = unsafe { phys_to_virt(clb) } as *mut AhciCommandHeader;
        let cmd_header = unsafe { &mut *cmd_header_ptr.add(slot as usize) };
        
        // Get command table
        let ctba = cmd_header.ctba as u64 | ((cmd_header.ctbau as u64) << 32);
        let cmd_table_ptr = unsafe { phys_to_virt(ctba) } as *mut CommandTable;
        let cmd_table = unsafe { &mut *cmd_table_ptr };
        
        // Clear command table
        unsafe { core::ptr::write_bytes(cmd_table as *mut _ as *mut u8, 0, core::mem::size_of::<CommandTable>()) };
        
        // Copy FIS
        cmd_table.cfis.copy_from_slice(&fis);
        
        // Setup PRDT
        cmd_header.prdtl = prdt.len() as u16;
        for (i, entry) in prdt.iter().enumerate() {
            if i >= 64 {
                return Err(DriverError::InvalidArgument);
            }
            cmd_table.prdt[i] = *entry;
        }
        
        // Clear interrupt status
        port_regs.is.store(0xFFFFFFFF, Ordering::Release);
        
        // Issue command
        port_regs.ci.fetch_or(1u32 << slot, Ordering::Release);
        
        // Wait for completion
        self.wait_for_completion(slot, is_write)
    }
    
    /// Wait for command completion
    fn wait_for_completion(&self, slot: u32, _is_write: bool) -> DriverResult<()> {
        let port_regs = self.port_regs();
        let mut timeout = 10000000; // ~10 seconds
        
        loop {
            let is = port_regs.is.load(Ordering::Acquire);
            
            // Check for errors
            if is & ((1u32 << 30) | (1u32 << 26) | (1u32 << 25) | (1u32 << 24) | (1u32 << 23) | (1u32 << 22)) != 0 {
                let serr = port_regs.serr.load(Ordering::Acquire);
                driver_common::kerror!("sata: port {} error: is={:08x} serr={:08x}", self.port_num, is, serr);
                // Clear error
                port_regs.is.store(is, Ordering::Release);
                return Err(DriverError::IoError);
            }
            
            // Check if command completed
            let ci = port_regs.ci.load(Ordering::Acquire);
            if (ci >> slot) & 1 == 0 {
                // Command completed, check TFD for status
                let tfd = port_regs.tfd.load(Ordering::Acquire);
                if tfd & status::ERR != 0 {
                    return Err(DriverError::IoError);
                }
                return Ok(());
            }
            
            if timeout == 0 {
                return Err(DriverError::Timeout);
            }
            timeout -= 1;
            core::hint::spin_loop();
        }
    }
    
    /// Identify the device
    pub fn identify(&mut self) -> DriverResult<IdentifyData> {
        // Allocate buffer for identify data (512 bytes)
        let buf_pages = 1;
        let buf_phys = unsafe { alloc_dma_pages(buf_pages) }.ok_or(DriverError::OutOfMemory)?;
        let buf_virt = unsafe { phys_to_virt(buf_phys) } as *mut u8;
        
        // Setup PRDT
        let prdt = [PrdtEntry::new(buf_phys, 512, true)];
        
        // Build IDENTIFY command
        let fis = build_identify_fis();
        
        // Execute
        self.execute_command(fis, &prdt, false)?;
        
        // Read identify data
        let identify = IdentifyData::from_bytes(unsafe { core::slice::from_raw_parts(buf_virt, 512) })
            .ok_or(DriverError::IoError)?;
        
        unsafe { free_dma_pages(buf_phys, buf_pages); }
        
        Ok(identify)
    }
    
    /// Read sectors from device
    pub fn read_sectors(&mut self, lba: u64, count: usize, buf: &mut [u8]) -> DriverResult<usize> {
        if count == 0 || count > 65535 {
            return Err(DriverError::InvalidArgument);
        }
        
        let buf_phys = unsafe { virt_to_phys(buf.as_mut_ptr() as u64) };
        let byte_count = count * 512;
        
        // Build PRDT (assuming contiguous buffer for now)
        let prdt = [PrdtEntry::new(buf_phys, byte_count as u32, true)];
        
        // Build READ DMA EXT command
        let fis = build_read_dma_ext_fis(lba, count as u16);
        
        self.execute_command(fis, &prdt, false)?;
        
        Ok(count)
    }
    
    /// Write sectors to device
    pub fn write_sectors(&mut self, lba: u64, count: usize, buf: &[u8]) -> DriverResult<usize> {
        if count == 0 || count > 65535 {
            return Err(DriverError::InvalidArgument);
        }
        
        let buf_phys = unsafe { virt_to_phys(buf.as_ptr() as u64) };
        let byte_count = count * 512;
        
        // Build PRDT
        let prdt = [PrdtEntry::new(buf_phys, byte_count as u32, true)];
        
        // Build WRITE DMA EXT command with FUA
        let fis = build_write_dma_ext_fis(lba, count as u16, true);
        
        self.execute_command(fis, &prdt, true)?;
        
        Ok(count)
    }
    
    /// Flush cache
    pub fn flush_cache(&mut self) -> DriverResult<()> {
        let fis = build_flush_cache_ext_fis();
        let prdt: [PrdtEntry; 0] = [];
        self.execute_command(fis, &prdt, false)
    }
    
    /// Set features (enable write cache, etc.)
    pub fn set_features(&mut self, feature: u8, count: u16) -> DriverResult<()> {
        let fis = build_set_features_fis(feature, count);
        let prdt: [PrdtEntry; 0] = [];
        self.execute_command(fis, &prdt, false)
    }
}