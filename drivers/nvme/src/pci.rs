// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! PCI device abstraction for NVMe driver using kernel PCI subsystem.

#![no_std]
#![allow(missing_docs)]

extern crate alloc;

use core::fmt;
use alloc::vec::Vec;

use driver_common::{DriverError, DriverResult};

/// PCI device representation
#[derive(Debug, Clone)]
pub struct PciDevice {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class_code: u16,
    pub subclass: u8,
    pub prog_if: u8,
    pub revision: u8,
    pub bar: [u64; 6],
    pub bar_size: [u64; 6],
    pub irq_line: u8,
    pub irq_pin: u8,
    pub msi_cap_offset: Option<u8>,
    pub msix_cap_offset: Option<u8>,
    pub pcie_cap_offset: Option<u8>,
}

impl PciDevice {
    /// Create a new PCI device from kernel PCI device info
    pub fn from_kernel_device(info: &driver_common::PciDeviceInfo) -> Self {
        let mut bar = [0u64; 6];
        let mut bar_size = [0u64; 6];
        
        for i in 0..6 {
            if i < info.bar.len() {
                bar[i] = info.bar[i];
                bar_size[i] = info.bar_size[i];
            }
        }
        
        Self {
            bus: info.addr.0,
            device: info.addr.1,
            function: info.addr.2,
            vendor_id: info.vendor_id,
            device_id: info.device_id,
            class_code: info.class_code,
            subclass: info.subclass,
            prog_if: info.prog_if,
            revision: info.revision,
            bar,
            bar_size,
            irq_line: info.irq_line,
            irq_pin: info.irq_pin,
            msi_cap_offset: info.msi_cap_offset,
            msix_cap_offset: info.msix_cap_offset,
            pcie_cap_offset: info.pcie_cap_offset,
        }
    }

    /// Read a 32-bit value from PCI config space
    pub fn read_config32(&self, offset: u8) -> u32 {
        unsafe { driver_common::pci_read_config32(self.bus, self.device, self.function, offset) }
    }

    /// Write a 32-bit value to PCI config space
    fn write_config32(&self, offset: u8, value: u32) {
        unsafe { driver_common::pci_write_config32(self.bus, self.device, self.function, offset, value) };
    }

    /// Read a 16-bit value from PCI config space
    fn read_config16(&self, offset: u8) -> u16 {
        self.read_config32(offset) as u16
    }

    /// Write a 16-bit value to PCI config space
    fn write_config16(&self, offset: u8, value: u16) {
        self.write_config32(offset, value as u32);
    }

    /// Read an 8-bit value from PCI config space
    fn read_config8(&self, offset: u8) -> u8 {
        self.read_config32(offset) as u8
    }

    /// Write an 8-bit value to PCI config space
    fn write_config8(&self, offset: u8, value: u8) {
        self.write_config32(offset, value as u32);
    }

    /// Enable bus mastering
    pub fn enable_bus_master(&mut self) {
        let cmd = self.read_config16(0x04);
        self.write_config16(0x04, cmd | 0x0004);
    }

    /// Disable bus mastering
    fn disable_bus_master(&mut self) {
        let cmd = self.read_config16(0x04);
        self.write_config16(0x04, cmd & !0x0004);
    }

    /// Enable memory space access
    pub fn enable_memory_space(&mut self) {
        let cmd = self.read_config16(0x04);
        self.write_config16(0x04, cmd | 0x0002);
    }

    /// Enable MSI
    pub fn enable_msi(&mut self, vector: u8) -> Result<(), ()> {
        if let Some(cap_ptr) = self.msi_cap_offset {
            // Set message address (use APIC for now, simplified)
            self.write_config32(cap_ptr as u8 + 0x04, 0xFEE00000);
            self.write_config32(cap_ptr as u8 + 0x08, 0);
            
            // Set message data
            self.write_config16(cap_ptr as u8 + 0x0C, vector as u16);
            
            // Enable MSI
            let mut ctrl = self.read_config16(cap_ptr as u8);
            ctrl |= 1;
            self.write_config16(cap_ptr as u8, ctrl);
            
            Ok(())
        } else {
            Err(())
        }
    }

    /// Get BAR0 address (64-bit)
    pub fn bar0_64(&self) -> u64 {
        self.bar[0]
    }

    /// Get BAR0 size
    pub fn bar0_size(&self) -> u64 {
        self.bar_size[0]
    }
}

/// PCI device info from kernel
#[derive(Debug, Clone)]
pub struct PciDeviceInfo {
    pub addr: PciAddress,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class_code: u16,
    pub subclass: u8,
    pub prog_if: u8,
    pub revision: u8,
    pub bar: Vec<u64>,
    pub bar_size: Vec<u64>,
    pub irq_line: u8,
    pub irq_pin: u8,
    pub msi_cap_offset: Option<u8>,
    pub msix_cap_offset: Option<u8>,
    pub pcie_cap_offset: Option<u8>,
}

#[derive(Debug, Clone, Copy)]
pub struct PciAddress {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

/// Find NVMe devices
pub fn find_nvme_devices() -> Vec<PciDevice> {
    unsafe { driver_common::pci_find_class(0x0108) }
        .into_iter()
        .map(|info| PciDevice::from_kernel_device(&info))
        .collect()
}