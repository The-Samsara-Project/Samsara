// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! PCI bus enumeration and configuration space access.

use crate::sync::{OnceCell, Spinlock};
use crate::io::{inb, inl, inw, outb, outl, outw};
use alloc::vec::Vec;
use driver_common::{PciAddress, PciDeviceInfo};

const CONFIG_ADDRESS: u16 = 0xCF8;
const CONFIG_DATA: u16 = 0xCFC;

const PCI_CAP_ID_MSI: u8 = 0x05;
const PCI_CAP_ID_MSIX: u8 = 0x11;
const PCI_CAP_ID_PCIE: u8 = 0x10;

/// PCI class codes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum PciClass {
    /// Non-volatile memory controller (NVMe) mass-storage class.
    Nvme = 0x0108,
    /// Device whose class code is not classified by [`PciClass`].
    Unknown = 0xFFFF,
}

/// A PCI device discovered during bus enumeration.
#[derive(Debug, Clone)]
pub struct PciDevice {
    /// Bus/device/function address of this device.
    pub addr: PciAddress,
    /// Device vendor identifier from config space.
    pub vendor_id: u16,
    /// Device identifier from config space.
    pub device_id: u16,
    /// PCI base class/subclass code.
    pub class_code: u16,
    /// PCI subclass code.
    pub subclass: u8,
    /// Programming interface byte from config space.
    pub prog_if: u8,
    /// Revision ID byte from config space.
    pub revision: u8,
    /// Base address registers `BAR0`..`BAR5`.
    pub bar: [u64; 6],
    /// Probed size (bytes) of each base address register.
    pub bar_size: [u64; 6],
    /// Legacy interrupt line read from config space.
    pub irq_line: u8,
    /// Legacy interrupt pin read from config space.
    pub irq_pin: u8,
    /// Raw capability list discovered during enumeration.
    pub capabilities: Vec<PciCapability>,
    /// Config-space offset of the MSI capability, if present.
    pub msi_cap_offset: Option<u8>,
    /// Config-space offset of the MSI-X capability, if present.
    pub msix_cap_offset: Option<u8>,
    /// Config-space offset of the PCIe capability, if present.
    pub pcie_cap_offset: Option<u8>,
}

/// A single entry of a PCI capability list.
#[derive(Debug, Clone, Copy)]
pub struct PciCapability {
    /// Capability identifier (see `PCI_CAP_ID_*`).
    pub cap_id: u8,
    /// Config-space offset of this capability.
    pub offset: u8,
}

impl PciDevice {
    fn read_config8(&self, offset: u8) -> u8 {
        outl(CONFIG_ADDRESS, self.addr.config_addr(offset));
        inb(CONFIG_DATA)
    }

    fn read_config16(&self, offset: u8) -> u16 {
        outl(CONFIG_ADDRESS, self.addr.config_addr(offset));
        inw(CONFIG_DATA)
    }

    fn read_config32(&self, offset: u8) -> u32 {
        outl(CONFIG_ADDRESS, self.addr.config_addr(offset));
        inl(CONFIG_DATA)
    }

    fn write_config8(&self, offset: u8, value: u8) {
        outl(CONFIG_ADDRESS, self.addr.config_addr(offset));
        outb(CONFIG_DATA, value);
    }

    fn write_config16(&self, offset: u8, value: u16) {
        outl(CONFIG_ADDRESS, self.addr.config_addr(offset));
        outw(CONFIG_DATA, value);
    }

    fn write_config32(&self, offset: u8, value: u32) {
        outl(CONFIG_ADDRESS, self.addr.config_addr(offset));
        outl(CONFIG_DATA, value);
    }

    fn read_bar(&self, index: usize) -> u64 {
        let offset = 0x10 + index * 4;
        let lo = self.read_config32(offset as u8);
        let bar_type = lo & 0x1;
        if bar_type == 0 {
            lo as u64
        } else {
            let hi = self.read_config32(offset as u8 + 4);
            ((hi as u64) << 32) | (lo as u64)
        }
    }

    fn probe_bar_size(&mut self, index: usize) -> u64 {
        let offset = 0x10 + index * 4;
        let original = self.read_config32(offset as u8);
        let bar_type = original & 0x1;
        
        if bar_type == 0 {
            self.write_config32(offset as u8, 0xFFFFFFFF);
            let size = (!self.read_config32(offset as u8)).wrapping_add(1) as u64;
            self.write_config32(offset as u8, original);
            size
        } else {
            let original_hi = self.read_config32(offset as u8 + 4);
            self.write_config32(offset as u8, 0xFFFFFFFF);
            self.write_config32(offset as u8 + 4, 0xFFFFFFFF);
            let size = (!self.read_config32(offset as u8)).wrapping_add(1) as u64;
            self.write_config32(offset as u8, original);
            self.write_config32(offset as u8 + 4, original_hi);
            size
        }
    }

    fn find_capability(&mut self, cap_id: u8) -> Option<u8> {
        let mut cap_ptr = self.read_config8(0x34);
        while cap_ptr != 0 && cap_ptr != 0xFF {
            let cap = self.read_config8(cap_ptr);
            if cap == cap_id {
                return Some(cap_ptr);
            }
            cap_ptr = self.read_config8(cap_ptr + 1);
        }
        None
    }

    fn enable_bus_master(&mut self) {
        let cmd = self.read_config16(0x04);
        self.write_config16(0x04, cmd | 0x0004);
    }

    fn enable_memory_space(&mut self) {
        let cmd = self.read_config16(0x04);
        self.write_config16(0x04, cmd | 0x0002);
    }

    fn enable_msi(&mut self) -> Result<(), ()> {
        let cap_ptr = self.find_capability(PCI_CAP_ID_MSI).ok_or(())?;
        self.msi_cap_offset = Some(cap_ptr);

        let mut msi_ctrl = self.read_config16(cap_ptr);
        msi_ctrl |= 1;
        self.write_config16(cap_ptr, msi_ctrl);

        Ok(())
    }
}

static PCI_DEVICES: OnceCell<Spinlock<Vec<PciDevice>>> = OnceCell::new();

/// Enumerate all PCI devices on the bus.
pub fn enumerate() -> Vec<PciDevice> {
    let mut devices = Vec::new();
    
    for bus in 0..=255 {
        for device in 0..32 {
            for function in 0..8 {
                let addr = PciAddress::new(bus, device, function);
                outl(CONFIG_ADDRESS, addr.config_addr(0x00));
                let vendor_device = inl(CONFIG_DATA);
                
                let vendor_id = vendor_device as u16;
                if vendor_id == 0xFFFF || vendor_id == 0x0000 {
                    if function == 0 {
                        break;
                    }
                    continue;
                }
                
                let device_id = (vendor_device >> 16) as u16;
                let class_rev = addr.config_addr(0x08);
                outl(CONFIG_ADDRESS, class_rev);
                let class_info = inl(CONFIG_DATA);
                
                let class_code = (class_info >> 16) as u16;
                let subclass = (class_info >> 8) as u8;
                let prog_if = class_info as u8;
                let revision = (class_info >> 24) as u8;
                
                let mut pci_dev = PciDevice {
                    addr,
                    vendor_id,
                    device_id,
                    class_code,
                    subclass,
                    prog_if,
                    revision,
                    bar: [0; 6],
                    bar_size: [0; 6],
                    irq_line: 0,
                    irq_pin: 0,
                    capabilities: Vec::new(),
                    msi_cap_offset: None,
                    msix_cap_offset: None,
                    pcie_cap_offset: None,
                };
                
                for i in 0..6 {
                    pci_dev.bar[i] = pci_dev.read_bar(i);
                    pci_dev.bar_size[i] = pci_dev.probe_bar_size(i);
                }
                
                pci_dev.irq_line = pci_dev.read_config8(0x3C);
                pci_dev.irq_pin = pci_dev.read_config8(0x3D);
                
                let mut cap_ptr = pci_dev.read_config8(0x34);
                while cap_ptr != 0 && cap_ptr != 0xFF {
                    let cap_id = pci_dev.read_config8(cap_ptr);
                    pci_dev.capabilities.push(PciCapability {
                        cap_id,
                        offset: cap_ptr,
                    });
                    
                    match cap_id {
                        PCI_CAP_ID_MSI => pci_dev.msi_cap_offset = Some(cap_ptr),
                        PCI_CAP_ID_MSIX => pci_dev.msix_cap_offset = Some(cap_ptr),
                        PCI_CAP_ID_PCIE => pci_dev.pcie_cap_offset = Some(cap_ptr),
                        _ => {}
                    }
                    
                    cap_ptr = pci_dev.read_config8(cap_ptr + 1);
                }
                
                pci_dev.enable_bus_master();
                pci_dev.enable_memory_space();
                
                devices.push(pci_dev);
            }
        }
    }
    
    devices
}

/// Scan all 256 buses/32 devices/8 functions and cache the result in the
/// global device list.
pub fn init() {
    let devices = enumerate();
    crate::log::kinfo!("pci: enumerated {} devices", devices.len());
    
    for dev in &devices {
        crate::log::kdebug!(
            "pci: {:02x}:{:02x}.{:x} [{:04x}:{:04x}] class={:04x} rev={:02x} bars={:?}",
            dev.addr.bus, dev.addr.device, dev.addr.function,
            dev.vendor_id, dev.device_id, dev.class_code, dev.revision,
            dev.bar
        );
    }
    
    if PCI_DEVICES.set(Spinlock::new(devices)).is_err() {
        panic!("pci initialized twice");
    }
}

/// Snapshot of the enumerated PCI device list, if initialization has run.
pub fn get_devices() -> Option<alloc::vec::Vec<PciDevice>> {
    PCI_DEVICES.get().map(|l| l.lock().clone())
}

/// Find the first device matching a vendor and device ID.
pub fn find_device(vendor_id: u16, device_id: u16) -> Option<PciDevice> {
    get_devices()?.into_iter().find(|d| d.vendor_id == vendor_id && d.device_id == device_id)
}

/// Find all devices whose base class/subclass code matches `class_code`.
pub fn find_class(class_code: u16) -> Vec<PciDevice> {
    get_devices().unwrap_or_default()
        .into_iter()
        .filter(|d| d.class_code == class_code)
        .collect()
}

/// Read PCI config space (32-bit)
pub fn pci_read_config32(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    let addr = PciAddress::new(bus, device, function);
    outl(CONFIG_ADDRESS, addr.config_addr(offset));
    inl(CONFIG_DATA)
}

/// Write PCI config space (32-bit)
pub fn pci_write_config32(bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    let addr = PciAddress::new(bus, device, function);
    outl(CONFIG_ADDRESS, addr.config_addr(offset));
    outl(CONFIG_DATA, value);
}

/// Find PCI devices by class code, mapping them to [`PciDeviceInfo`].
pub fn pci_find_class(class_code: u16) -> alloc::vec::Vec<PciDeviceInfo> {
    let devices = get_devices().unwrap_or_default();
    devices
        .into_iter()
        .filter(|d| d.class_code == class_code)
        .map(|d| PciDeviceInfo {
            addr: (d.addr.bus, d.addr.device, d.addr.function),
            vendor_id: d.vendor_id,
            device_id: d.device_id,
            class_code: d.class_code,
            subclass: d.subclass,
            prog_if: d.prog_if,
            revision: d.revision,
            bar: d.bar.to_vec(),
            bar_size: d.bar_size.to_vec(),
            irq_line: d.irq_line,
            irq_pin: d.irq_pin,
            msi_cap_offset: d.msi_cap_offset,
            msix_cap_offset: d.msix_cap_offset,
            pcie_cap_offset: d.pcie_cap_offset,
        })
        .collect()
}