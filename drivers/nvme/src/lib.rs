// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! NVMe (Non-Volatile Memory Express) driver for Samsara.
//!
//! This driver implements the NVMe 1.4 specification for communicating with
//! NVMe SSDs over PCIe. It supports:
//! - Multiple I/O queues with interrupt-driven completion
//! - PRP (Physical Region Page) lists for scatter/gather I/O
//! - Namespace management and identification
//! - Read, Write, Flush, and Dataset Management (TRIM) commands
//! - MSI/MSI-X interrupt handling
//! - Admin and I/O command submission/completion

#![no_std]
#![allow(missing_docs)]

extern crate alloc;

use core::ptr::NonNull;
use core::sync::atomic::{AtomicU16, AtomicU32, AtomicBool, Ordering};
use core::time::Duration;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

use driver_common::{
    DeviceCapabilities, DeviceClass, DeviceId, DeviceInfo, DriverError, DriverRegistration,
    DriverResult, BlockDevice, alloc_dma_pages, free_dma_pages, phys_to_virt,
    register_block_device,
    ticks, timer_hz, DriverEntry,
};

use crate::pci::PciDevice;
use crate::queue::Queue;
use crate::registers::{
    CompletionQueueEntry, ControllerCapabilities, ControllerConfig, ControllerRegisters,
    ControllerStatus, NvmeCommand, PrpEntry,
};
use crate::commands::*;

pub mod registers;
pub mod commands;
pub mod pci;
pub mod queue;
pub mod namespace;
pub mod dma;

use dma::DmaBuf;
use namespace::Namespace;

/// NVMe driver state
pub struct NvmeController {
    pci_device: PciDevice,
    registers: NonNull<ControllerRegisters>,
    admin_queue: Queue,
    io_queues: BTreeMap<u16, Queue>,
    namespaces: BTreeMap<u32, Namespace>,
    doorbell_stride: usize,
    max_queue_entries: u16,
    page_size: u32,
    interrupt_vector: u8,
    controller_ready: AtomicBool,
}

// The controller is only ever accessed while holding the driver's `Arc<Mutex>`
// (or its own internal synchronization), so sharing it across tasks is safe.
unsafe impl Send for NvmeController {}
unsafe impl Sync for NvmeController {}

impl NvmeController {
    /// Create a new NVMe controller driver
    pub fn new(pci_device: PciDevice) -> DriverResult<Self> {
        let bar0 = pci_device.bar0_64();
        let bar0_size = pci_device.bar0_size();
        
        // Map the controller registers
        let registers_ptr = unsafe { phys_to_virt(bar0) } as *mut ControllerRegisters;
        let registers = NonNull::new(registers_ptr).ok_or(DriverError::HardwareFault)?;

        // Read controller capabilities
        let caps = unsafe { registers.as_ref().read_cap() };
        let max_queue_entries = caps.max_queue_entries.min(1024);
        let page_size = caps.page_size;

        let mut controller = Self {
            pci_device,
            registers,
            admin_queue: Queue::new(32)?,
            io_queues: BTreeMap::new(),
            namespaces: BTreeMap::new(),
            doorbell_stride: 1,
            max_queue_entries,
            page_size,
            interrupt_vector: 0,
            controller_ready: AtomicBool::new(false),
        };

        controller.init_controller()?;
        Ok(controller)
    }

    /// Initialize the NVMe controller
    fn init_controller(&mut self) -> DriverResult<()> {
        // NVMe DMA requires bus mastering and memory-space decode enabled.
        self.pci_device.enable_bus_master();
        self.pci_device.enable_memory_space();

        self.reset_controller()?;
        self.identify_controller()?;
        self.configure_admin_queue()?;
        self.configure_io_queues()?;
        self.discover_namespaces()?;
        self.setup_interrupts()?;
        
        self.controller_ready.store(true, Ordering::Release);
        Ok(())
    }

    /// Reset the NVMe controller
    fn reset_controller(&mut self) -> DriverResult<()> {
        let regs = unsafe { self.registers.as_ref() };
        
        // Disable controller
        regs.cc_update(|cc| cc & !0x1);
        
        // Wait for controller to become not ready
        let mut timeout = 100000;
        while regs.read_csts() & 0x1 != 0 {
            core::hint::spin_loop();
            timeout -= 1;
            if timeout == 0 {
                return Err(DriverError::Timeout);
            }
        }

        // Set admin queue attributes (16 entries each)
        regs.set_aqa(16, 16);
        
        // Set admin queue base addresses
        regs.set_asq(self.admin_queue.submission_phys());
        regs.set_acq(self.admin_queue.completion_phys());
        
        // Configure CC register
        let cc = ControllerConfig {
            enable: true,
            css: 0,       // NVM command set
            mps: 0,       // 4KB page size
            ams: 0,       // Round robin arbitration
            shn: 0,       // No shutdown notification
            ios: 0,       // I/O queue size (not used for admin)
            iosc: 0,      // I/O completion queue size
            mps_max: 0,
        };
        regs.write_cc(cc.to_u32());

        // Wait for controller ready
        let mut timeout = 100000;
        while regs.read_csts() & 0x1 == 0 {
            core::hint::spin_loop();
            timeout -= 1;
            if timeout == 0 {
                return Err(DriverError::Timeout);
            }
        }

        // Calculate doorbell stride from CAP.DSTRD (doorbells begin at offset
        // 0x1000; each queue's SQ/CQ doorbell is `stride` dwords apart).
        let cap = regs.read_cap_raw();
        self.doorbell_stride = 1usize << ((cap >> 32) & 0xF) as usize;

        Ok(())
    }

/// Identify controller capabilities
    fn identify_controller(&mut self) -> DriverResult<()> {
        // Identify buffers must be 4K-aligned, physically contiguous and stay
        // put for the whole command — a heap `Vec` is none of those things.
        let buf = DmaBuf::new(4096)?;

        let cmd = identify_controller();
        let mut admin_cmd = cmd;
        admin_cmd.prp1 = buf.phys();
        admin_cmd.cid = self.admin_queue.submit(admin_cmd)?;

        self.admin_queue.ring_doorbell(unsafe { self.registers.as_ref() });

        if !self.wait_for_admin_completion(admin_cmd.cid)? {
            return Err(DriverError::IoError);
        }

        // Parse controller identify data
        let ident_data =
            unsafe { &*(buf.as_slice().as_ptr() as *const IdentifyControllerData) };

        // `mn` lives in a `#[repr(packed)]` struct, so copy it out before
        // taking a reference. The other fields are read by value as well to
        // avoid forming unaligned references into the packed struct.
        let mn = ident_data.mn;
        let vid = ident_data.vid;
        let nn = ident_data.nn;
        let maxcmd = ident_data.maxcmd;
        driver_common::kdebug!(
            "nvme: controller identify: vid={:04x} mn={:?} nn={} maxcmd={}",
            vid,
            core::str::from_utf8(&mn).unwrap_or("?"),
            nn,
            maxcmd
        );

        Ok(())
    }

    /// Configure admin queue
    fn configure_admin_queue(&mut self) -> DriverResult<()> {
        let regs = unsafe { self.registers.as_ref() };
        regs.set_sqes(6); // 64-byte entries (2^6)
        regs.set_cqes(4); // 16-byte entries (2^4)
        Ok(())
    }

    /// Configure I/O queues
    fn configure_io_queues(&mut self) -> DriverResult<()> {
        let num_queues = self.max_queue_entries.min(8) as u16; // Start with up to 8 queues
        
        for qid in 1..=num_queues {
            let mut queue = Queue::new(64)?;
            queue.id = qid;
            
            // Create I/O completion queue first (interrupts enabled).
            let create_cq = create_io_cq(
                qid, 64, 1, qid, // IEN=1, use queue id as interrupt vector
                queue.completion_phys()
            );
            let mut cmd = create_cq;
            cmd.cid = self.admin_queue.submit(cmd)?;
            self.admin_queue.ring_doorbell(unsafe { self.registers.as_ref() });
            self.wait_for_admin_completion(cmd.cid)?;
            
            // Create I/O submission queue (physically contiguous).
            let create_sq = create_io_sq(
                qid, 64, 1, qid,
                queue.submission_phys()
            );
            let mut cmd = create_sq;
            cmd.cid = self.admin_queue.submit(cmd)?;
            self.admin_queue.ring_doorbell(unsafe { self.registers.as_ref() });
            self.wait_for_admin_completion(cmd.cid)?;
            
            // Enable the queue in the controller
            let regs = unsafe { self.registers.as_ref() };
            regs.set_sq_base(qid, queue.submission_phys());
            regs.set_cq_base(qid, queue.completion_phys());
            
            self.io_queues.insert(qid, queue);
        }
        
            // Set number of queues feature (NCQR/NSQR in cdw11).
        let n = num_queues as u32 - 1;
        let mut cmd = NvmeCommand::admin_command(admin_opcodes::SET_FEATURES);
        cmd.cdw10 = feature_ids::NUMBER_OF_QUEUES as u32;
        cmd.cdw11 = (n << 16) | n;
        cmd.cid = self.admin_queue.submit(cmd)?;
        self.admin_queue.ring_doorbell(unsafe { self.registers.as_ref() });
        self.wait_for_admin_completion(cmd.cid)?;
        
        Ok(())
    }

/// Discover namespaces
    fn discover_namespaces(&mut self) -> DriverResult<()> {
        // Same DMA-buffer requirement as identify_controller.
        let buf = DmaBuf::new(4096)?;

        // Get active namespace list
        let cmd = identify_active_ns_list();
        let mut admin_cmd = cmd;
        admin_cmd.prp1 = buf.phys();
        admin_cmd.cid = self.admin_queue.submit(admin_cmd)?;
        self.admin_queue.ring_doorbell(unsafe { self.registers.as_ref() });
        if !self.wait_for_admin_completion(admin_cmd.cid)? {
            return Err(DriverError::IoError);
        }

        // Parse namespace list
        let ns_list = unsafe { &*(buf.as_slice().as_ptr() as *const [u32; 1024]) };
        for &nsid in ns_list.iter().take_while(|&&x| x != 0) {
            match Namespace::new(self, nsid) {
                Ok(ns) => {
                    driver_common::kdebug!("nvme: discovered namespace {} ({} blocks)", nsid, ns.size());
                    self.namespaces.insert(nsid, ns);
                }
                Err(e) => {
                    driver_common::kerror!("nvme: failed to identify namespace {}: {:?}", nsid, e);
                }
            }
        }
        
        Ok(())
    }

    /// Setup MSI interrupts
    fn setup_interrupts(&mut self) -> DriverResult<()> {
        // Assign interrupt vector (for now use a fixed one)
        self.interrupt_vector = 0x40; // Base vector for NVMe
        
        if let Err(_) = self.pci_device.enable_msi(self.interrupt_vector) {
            driver_common::kwarn!("nvme: MSI enable failed, using polling");
        }
        
        // Enable interrupts for all queues
        let regs = unsafe { self.registers.as_ref() };
        for (qid, _) in &self.io_queues {
            regs.write_cq_doorbell(*qid, 0); // Clear any pending
        }
        regs.write_cq_doorbell(0, 0); // Admin queue
        
        Ok(())
    }

    /// Wait for admin command completion
    fn wait_for_admin_completion(&mut self, cid: u16) -> DriverResult<bool> {
        let deadline = ticks() + timer_hz() * 5; // 5 second timeout
        
        loop {
            if let Some(cqe) = self.admin_queue.poll_completion() {
                if cqe.cid == cid {
                    return Ok(cqe.success());
                }
                // Other completion, requeue
            }
            
            if ticks() >= deadline {
                return Err(DriverError::Timeout);
            }
            core::hint::spin_loop();
        }
    }

    /// Submit an I/O command to a queue
    fn submit_io(&mut self, qid: u16, mut cmd: NvmeCommand) -> DriverResult<u16> {
        let queue = self.io_queues.get_mut(&qid).ok_or(DriverError::NotFound)?;
        let cid = queue.submit(cmd)?;
        queue.ring_doorbell(unsafe { self.registers.as_ref() });
        Ok(cid)
    }

    /// Poll I/O completions
    pub fn poll_io_completions(&mut self, qid: u16) -> Vec<CompletionQueueEntry> {
        let mut completions = Vec::new();
        if let Some(queue) = self.io_queues.get_mut(&qid) {
            while let Some(cqe) = queue.poll_completion() {
                completions.push(cqe);
            }
        }
        completions
    }

    /// Get a namespace by ID
    pub fn get_namespace(&self, nsid: u32) -> Option<&Namespace> {
        self.namespaces.get(&nsid)
    }

    /// Get a mutable namespace by ID
    pub fn get_namespace_mut(&mut self, nsid: u32) -> Option<&mut Namespace> {
        self.namespaces.get_mut(&nsid)
    }

    /// Get all namespaces
    pub fn namespaces(&self) -> &BTreeMap<u32, Namespace> {
        &self.namespaces
    }

    /// Read from a namespace
    pub fn read(&mut self, nsid: u32, lba: u64, buf: &mut [u8]) -> DriverResult<usize> {
        let ns = self.namespaces.get(&nsid).ok_or(DriverError::NotFound)?.clone();
        ns.read(self, lba, buf)
    }

    /// Write to a namespace
    pub fn write(&mut self, nsid: u32, lba: u64, buf: &[u8]) -> DriverResult<usize> {
        let ns = self.namespaces.get(&nsid).ok_or(DriverError::NotFound)?.clone();
        ns.write(self, lba, buf)
    }

    /// Flush namespace
    pub fn flush(&mut self, nsid: u32) -> DriverResult<()> {
        let ns = self.get_namespace(nsid).ok_or(DriverError::NotFound)?.clone();
        ns.flush(self)
    }

    /// Dataset management (TRIM)
    pub fn trim(&mut self, nsid: u32, ranges: &[DsmRange]) -> DriverResult<()> {
        let ns = self.get_namespace(nsid).ok_or(DriverError::NotFound)?.clone();
        ns.trim(self, ranges)
    }

    /// Get controller info
    pub fn info(&self) -> DeviceInfo {
        DeviceInfo {
            id: self.device_id(),
            class: DeviceClass::Nvme,
            vendor_id: self.pci_device.vendor_id,
            device_id: self.pci_device.device_id,
            revision: self.pci_device.revision,
            capabilities: DeviceCapabilities::READ
                | DeviceCapabilities::WRITE
                | DeviceCapabilities::DMA
                | DeviceCapabilities::FLUSH
                | DeviceCapabilities::TRIM
                | DeviceCapabilities::NAMESPACE,
            name: alloc::string::String::from("NVMe Controller"),
            driver_name: Some(alloc::string::String::from("nvme")),
        }
    }

    fn device_id(&self) -> DeviceId {
        DeviceId::new(
            self.pci_device.bus as u32,
            self.pci_device.device as u32,
            self.pci_device.function,
        )
    }
}

impl Drop for NvmeController {
    fn drop(&mut self) {
        let regs = unsafe { self.registers.as_mut() };
        regs.cc_update(|cc| cc & !0x1); // Disable controller
    }
}

/// NVMe driver capabilities (const-evaluable via `from_bits_truncate`).
const NVME_CAPABILITIES: DeviceCapabilities = DeviceCapabilities::from_bits_truncate(
    DeviceCapabilities::READ.bits()
        | DeviceCapabilities::WRITE.bits()
        | DeviceCapabilities::DMA.bits()
        | DeviceCapabilities::FLUSH.bits()
        | DeviceCapabilities::TRIM.bits()
        | DeviceCapabilities::NAMESPACE.bits(),
);

/// NVMe driver registration
pub static NVME_DRIVER: DriverRegistration = DriverRegistration {
    name: "nvme",
    version: "1.0.0",
    classes: &[DeviceClass::Nvme],
    capabilities: NVME_CAPABILITIES,
};

/// NVMe driver entry point, retaining live controllers for the block devices.
pub struct NvmeDriver {
    controllers: Vec<Arc<spin::Mutex<NvmeController>>>,
}

impl NvmeDriver {
    /// Create a new driver instance.
    pub fn new() -> Self {
        Self {
            controllers: Vec::new(),
        }
    }
}

impl driver_common::DriverEntry for NvmeDriver {
    fn probe(&mut self) -> DriverResult<Vec<DeviceInfo>> {
        let devices = crate::pci::find_nvme_devices();
        let mut infos = Vec::new();
        
        for dev in devices {
            infos.push(DeviceInfo {
                id: DeviceId::new(dev.bus as u32, dev.device as u32, dev.function),
                class: DeviceClass::Nvme,
                vendor_id: dev.vendor_id,
                device_id: dev.device_id,
                revision: dev.revision,
                capabilities: DeviceCapabilities::READ
                    | DeviceCapabilities::WRITE
                    | DeviceCapabilities::DMA
                    | DeviceCapabilities::FLUSH
                    | DeviceCapabilities::TRIM
                    | DeviceCapabilities::NAMESPACE,
                name: alloc::format!("NVMe {:04x}:{:04x}", dev.vendor_id, dev.device_id),
                driver_name: Some(alloc::string::String::from("nvme")),
            });
        }
        
        Ok(infos)
    }

    fn attach(&mut self, device: DeviceInfo) -> DriverResult<()> {
        let pci_dev = crate::pci::find_nvme_devices()
            .into_iter()
            .find(|d| d.vendor_id == device.vendor_id && d.device_id == device.device_id)
            .ok_or(DriverError::NotFound)?;
        
        let controller = NvmeController::new(pci_dev)?;
        let controller = Arc::new(spin::Mutex::new(controller));
        register_namespaces(&controller);
        self.controllers.push(controller);
        Ok(())
    }

    fn detach(&mut self, _device_id: DeviceId) -> DriverResult<()> {
        Ok(())
    }
}

/// Initialize NVMe driver
pub fn init() {
    let mut driver = NvmeDriver::new();
    match driver.probe() {
        Ok(devices) => {
            for dev in devices {
                driver_common::kinfo!("nvme: found device {:?}", dev.name);
                if let Err(e) = driver.attach(dev) {
                    driver_common::kerror!("nvme: failed to attach: {:?}", e);
                }
            }
        }
        Err(e) => {
            driver_common::kdebug!("nvme: probe failed: {:?}", e);
        }
    }
}

/// Register each namespace of `controller` as a block device under `/dev`.
fn register_namespaces(controller: &Arc<spin::Mutex<NvmeController>>) {
    let entries: Vec<(u32, u32, u64)> = {
        let guard = controller.lock();
        guard
            .namespaces()
            .iter()
            .map(|(id, ns)| (*id, ns.block_size(), ns.size()))
            .collect()
    };

    for (nsid, block_size, num_blocks) in entries {
        let name = alloc::format!("nvme0n{}", nsid);
        let block_dev = NvmeBlockDevice {
            controller: controller.clone(),
            nsid,
            block_size,
            num_blocks,
        };

        if let Err(e) = register_block_device(&name, Arc::new(block_dev)) {
            driver_common::kerror!("nvme: failed to register {}: {:?}", name, e);
        }
    }
}

/// Block device wrapper for an NVMe namespace.
pub struct NvmeBlockDevice {
    controller: Arc<spin::Mutex<NvmeController>>,
    nsid: u32,
    block_size: u32,
    num_blocks: u64,
}

impl BlockDevice for NvmeBlockDevice {
    fn read_blocks(&self, lba: u64, blocks: &mut [u8]) -> DriverResult<usize> {
        let mut controller = self.controller.lock();
        controller.read(self.nsid, lba, blocks)
    }

    fn write_blocks(&self, lba: u64, blocks: &[u8]) -> DriverResult<usize> {
        let mut controller = self.controller.lock();
        controller.write(self.nsid, lba, blocks)
    }

    fn flush(&self) -> DriverResult<()> {
        let mut controller = self.controller.lock();
        controller.flush(self.nsid)
    }

    fn block_size(&self) -> u32 {
        self.block_size
    }

    fn num_blocks(&self) -> u64 {
        self.num_blocks
    }

    fn device_info(&self) -> DeviceInfo {
        self.controller.lock().info()
    }
}