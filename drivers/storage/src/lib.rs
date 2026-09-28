// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! Storage driver for Samsara - AHCI/SATA controller driver.
//!
//! Implements the AHCI (Advanced Host Controller Interface) specification
//! for SATA controllers. Supports:
//! - AHCI 1.3.1 specification
//! - Command queuing (NCQ)
//! - Port multiplier support
//! - Hot plug detection
//! - DMA setup and data transfer
//! - Block device registration for each attached drive

#![no_std]
#![allow(missing_docs)]

extern crate alloc;

use core::ptr::NonNull;
use core::sync::atomic::{AtomicU32, AtomicBool, Ordering};
use alloc::vec::Vec;
use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::string::String;
use alloc::string::ToString;
use alloc::collections::BTreeMap;
use spin::Mutex;

use driver_common::{
    DriverError, DriverResult, BlockDevice, DeviceInfo, DeviceClass, DeviceCapabilities,
    DriverRegistration, DriverEntry, DeviceId, pci_find_class, register_block_device,
};

pub mod ahci;
pub mod ata;
pub mod port;

use ahci::{AhciController, AhciPort};
use port::SataPort;

/// Global AHCI controller instance
static AHCI_CONTROLLER: Mutex<Option<Arc<AhciController>>> = Mutex::new(None);

/// Initialize AHCI/SATA subsystem
pub fn init() {
    driver_common::kinfo!("storage: init() started");
    // Find AHCI controllers via PCI
    let controllers = unsafe { pci_find_class(0x0106) }; // AHCI class code
    driver_common::kinfo!("storage: pci_find_class(0x0106) -> {} controller(s)", controllers.len());
    
    for pci_dev in controllers {
        driver_common::kinfo!(
            "storage: PCI {:02x}:{:02x}.{:x} [{:04x}:{:04x}] class={:04x} subclass={:02x} prog_if={:02x} bar5={:?}",
            pci_dev.addr.0, pci_dev.addr.1, pci_dev.addr.2,
            pci_dev.vendor_id, pci_dev.device_id,
            pci_dev.class_code, pci_dev.subclass, pci_dev.prog_if,
            pci_dev.bar.get(5)
        );
        if pci_dev.prog_if == 0x01 { // AHCI 1.0+
            match AhciController::new(pci_dev) {
                Ok(controller) => {
                    driver_common::kinfo!("storage: AhciController::new OK");
                    let controller_arc = Arc::new(controller);
                    *AHCI_CONTROLLER.lock() = Some(controller_arc.clone());
                    
                    // Initialize ports and register block devices
                    if let Err(e) = register_ports(&controller_arc) {
                        driver_common::kerror!("storage: register_ports failed: {:?}", e);
                        driver_common::kerror!("ahci: failed to register ports: {:?}", e);
                    }
                }
                Err(e) => {
                    driver_common::kerror!("ahci: failed to initialize controller: {:?}", e);
                }
            }
        }
    }
}

/// Register SATA ports as block devices
fn register_ports(controller: &Arc<AhciController>) -> DriverResult<()> {
    let mut port_id = 0;
    
    for port in controller.ports() {
        if let Some(ahci_port) = port {
            if ahci_port.is_present() {
                let sata = ahci_port.sata_port();
                if let Ok(disk) = SataDisk::new(sata) {
                    let cap_mb = disk.capacity_mb();
                    let name = alloc::format!("sd{}", port_id);
                    register_block_device(&name, Arc::new(disk))?;
                    driver_common::kinfo!("ahci: registered {} ({} MB)", name, cap_mb);
                    port_id += 1;
                }
            }
        }
    }
    
    Ok(())
}

/// SATA disk block device
pub struct SataDisk {
    port: Arc<Mutex<SataPort>>,
    capacity: u64,
    block_size: u32,
    model: String,
    serial: String,
}

impl SataDisk {
    /// Create a new SATA disk from a port
    pub fn new(port: Arc<Mutex<SataPort>>) -> DriverResult<Self> {
        let mut port_guard = port.lock();
        
        // Identify the device
        let identify = port_guard.identify()?;
        
        // `model`/`serial` live in a `#[repr(packed)]` struct; copy them out
        // before taking references.
        let model_raw = identify.model;
        let serial_raw = identify.serial;
        let capacity = identify.max_48bit_lba;
        let block_size = 512; // Default, can be 4096 for advanced format
        
        let model = String::from_utf8_lossy(&model_raw).trim_end().to_string();
        let serial = String::from_utf8_lossy(&serial_raw).trim_end().to_string();

        // Drop the port guard before moving `port` into `Self`.
        drop(port_guard);

        Ok(Self {
            port,
            capacity,
            block_size,
            model,
            serial,
        })
    }
    
    /// Get capacity in MB
    pub fn capacity_mb(&self) -> u64 {
        (self.capacity * self.block_size as u64) / (1024 * 1024)
    }
    
    /// Get model string
    pub fn model(&self) -> &str {
        &self.model
    }
    
    /// Get serial string
    pub fn serial(&self) -> &str {
        &self.serial
    }
}

impl BlockDevice for SataDisk {
    fn read_blocks(&self, lba: u64, buf: &mut [u8]) -> DriverResult<usize> {
        let mut port = self.port.lock();
        let sectors = buf.len() / self.block_size as usize;
        port.read_sectors(lba, sectors, buf).map_err(|_| DriverError::IoError)
    }

    fn write_blocks(&self, lba: u64, buf: &[u8]) -> DriverResult<usize> {
        let mut port = self.port.lock();
        let sectors = buf.len() / self.block_size as usize;
        port.write_sectors(lba, sectors, buf).map_err(|_| DriverError::IoError)
    }

    fn flush(&self) -> DriverResult<()> {
        let mut port = self.port.lock();
        port.flush_cache().map_err(|_| DriverError::IoError)
    }
    
    fn block_size(&self) -> u32 {
        self.block_size
    }
    
    fn num_blocks(&self) -> u64 {
        self.capacity
    }
    
    fn device_info(&self) -> DeviceInfo {
        DeviceInfo {
            id: DeviceId::new(0, 0, 0),
            class: DeviceClass::Block,
            vendor_id: 0,
            device_id: 0,
            revision: 0,
            capabilities: DeviceCapabilities::READ 
                | DeviceCapabilities::WRITE 
                | DeviceCapabilities::SEEK
                | DeviceCapabilities::BLOCKING
                | DeviceCapabilities::NONBLOCKING
                | DeviceCapabilities::DMA
                | DeviceCapabilities::FLUSH
                | DeviceCapabilities::FUA,
            name: alloc::format!("SATA Disk: {}", self.model),
            driver_name: Some(alloc::string::String::from("ahci")),
        }
    }
}

/// Storage driver entry point
pub struct StorageDriver;

impl DriverEntry for StorageDriver {
    fn probe(&mut self) -> DriverResult<Vec<DeviceInfo>> {
        let controllers = unsafe { pci_find_class(0x0106) };
        let mut devices = Vec::new();
        
        for ctrl in controllers {
            if ctrl.prog_if == 0x01 {
                devices.push(DeviceInfo {
                    id: DeviceId::new(ctrl.addr.0 as u32, ctrl.addr.1 as u32, ctrl.addr.2),
                    class: DeviceClass::Block,
                    vendor_id: ctrl.vendor_id,
                    device_id: ctrl.device_id,
                    revision: ctrl.revision,
                    capabilities: DeviceCapabilities::READ 
                        | DeviceCapabilities::WRITE 
                        | DeviceCapabilities::SEEK
                        | DeviceCapabilities::BLOCKING
                        | DeviceCapabilities::NONBLOCKING
                        | DeviceCapabilities::DMA
                        | DeviceCapabilities::FLUSH
                        | DeviceCapabilities::FUA
                        | DeviceCapabilities::PARTITIONS,
                    name: alloc::format!("AHCI Controller {:04x}:{:04x}", ctrl.vendor_id, ctrl.device_id),
                    driver_name: Some(alloc::string::String::from("ahci")),
                });
            }
        }
        
        Ok(devices)
    }
    
    fn attach(&mut self, _device: DeviceInfo) -> DriverResult<()> {
        init();
        Ok(())
    }
    
    fn detach(&mut self, _device_id: DeviceId) -> DriverResult<()> {
        Ok(())
    }
}

/// Storage driver capabilities (const-evaluable via `from_bits_truncate`).
const STORAGE_CAPABILITIES: DeviceCapabilities = DeviceCapabilities::from_bits_truncate(
    DeviceCapabilities::READ.bits()
        | DeviceCapabilities::WRITE.bits()
        | DeviceCapabilities::SEEK.bits()
        | DeviceCapabilities::BLOCKING.bits()
        | DeviceCapabilities::NONBLOCKING.bits()
        | DeviceCapabilities::DMA.bits()
        | DeviceCapabilities::FLUSH.bits()
        | DeviceCapabilities::FUA.bits()
        | DeviceCapabilities::PARTITIONS.bits(),
);

pub static STORAGE_DRIVER: DriverRegistration = DriverRegistration {
    name: "storage",
    version: "1.0.0",
    classes: &[DeviceClass::Block],
    capabilities: STORAGE_CAPABILITIES,
};