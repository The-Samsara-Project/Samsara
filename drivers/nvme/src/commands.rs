// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! NVMe command definitions and construction.

#![no_std]
#![allow(missing_docs)]

use core::fmt;

/// Admin Command Opcodes
pub mod admin_opcodes {
    pub const DELETE_IO_SQ: u8 = 0x00;
    pub const CREATE_IO_SQ: u8 = 0x01;
    pub const GET_LOG_PAGE: u8 = 0x02;
    pub const DELETE_IO_CQ: u8 = 0x04;
    pub const CREATE_IO_CQ: u8 = 0x05;
    pub const IDENTIFY: u8 = 0x06;
    pub const ABORT: u8 = 0x08;
    pub const SET_FEATURES: u8 = 0x09;
    pub const GET_FEATURES: u8 = 0x0A;
    pub const ASYNC_EVENT_REQUEST: u8 = 0x0C;
    pub const NAMESPACE_MANAGEMENT: u8 = 0x0D;
    pub const FIRMWARE_COMMIT: u8 = 0x10;
    pub const FIRMWARE_IMAGE_DOWNLOAD: u8 = 0x11;
    pub const DEVICE_SELF_TEST: u8 = 0x14;
    pub const NAMESPACE_ATTACHMENT: u8 = 0x15;
    pub const FORMAT_NVM: u8 = 0x80;
    pub const SECURITY_SEND: u8 = 0x81;
    pub const SECURITY_RECEIVE: u8 = 0x82;
}

/// NVM Command Opcodes
pub mod nvm_opcodes {
    pub const FLUSH: u8 = 0x00;
    pub const WRITE: u8 = 0x01;
    pub const READ: u8 = 0x02;
    pub const WRITE_UNCORRECTABLE: u8 = 0x04;
    pub const COMPARE: u8 = 0x05;
    pub const DATASET_MANAGEMENT: u8 = 0x09;
    pub const WRITE_ZEROES: u8 = 0x08;
}

/// Feature Identifiers
pub mod feature_ids {
    pub const ARBITRATION: u8 = 0x01;
    pub const POWER_MANAGEMENT: u8 = 0x02;
    pub const TEMPERATURE_THRESHOLD: u8 = 0x03;
    pub const ERROR_RECOVERY: u8 = 0x04;
    pub const VOLATILE_WRITE_CACHE: u8 = 0x06;
    pub const NUMBER_OF_QUEUES: u8 = 0x07;
    pub const INTERRUPT_COALESCING: u8 = 0x08;
    pub const INTERRUPT_VECTOR_CONFIG: u8 = 0x09;
    pub const WRITE_ATOMICITY: u8 = 0x0A;
    pub const ASYNC_EVENT_CONFIG: u8 = 0x0B;
    pub const AUTONOMOUS_POWER_STATE_TRANSITION: u8 = 0x0C;
    pub const HOST_MEMORY_BUFFER: u8 = 0x0D;
    pub const TIMESTAMP: u8 = 0x0E;
    pub const KEEP_ALIVE_TIMER: u8 = 0x0F;
    pub const HOST_CONTROLLED_THERMAL_MGMT: u8 = 0x10;
    pub const NON_OPERATIONAL_POWER_STATE_CONFIG: u8 = 0x11;
    pub const READ_RECOVERY_LEVEL_CONFIG: u8 = 0x12;
    pub const PREDICTABLE_LATENCY_MODE_CONFIG: u8 = 0x13;
    pub const PREDICTABLE_LATENCY_MODE_WINDOW: u8 = 0x14;
}

/// Identify Controller CNS values
pub mod identify_cns {
    pub const NAMESPACE: u32 = 0x00;
    pub const CONTROLLER: u32 = 0x01;
    pub const ACTIVE_NS_LIST: u32 = 0x02;
    pub const NS_DESCRIPTOR_LIST: u32 = 0x03;
    pub const UUID_LIST: u32 = 0x04;
    pub const NAMESPACE_PRESENT: u32 = 0x10;
    pub const CONTROLLER_NS_LIST: u32 = 0x11;
    pub const NVME_SUBSYSTEM: u32 = 0x12;
}

/// Log Page Identifiers
pub mod log_page_ids {
    pub const ERROR_INFO: u8 = 0x01;
    pub const SMART: u8 = 0x02;
    pub const FIRMWARE_SLOT: u8 = 0x03;
    pub const CHANGED_NS_LIST: u8 = 0x04;
    pub const COMMAND_EFFECTS: u8 = 0x05;
    pub const DEVICE_SELF_TEST: u8 = 0x06;
    pub const TELEMETRY_HOST_INIT: u8 = 0x07;
    pub const TELEMETRY_CTRL_INIT: u8 = 0x08;
    pub const ENDURANCE_GROUP: u8 = 0x09;
}

/// Identify Controller Data Structure (4096 bytes)
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct IdentifyControllerData {
    pub vid: u16,
    pub ssvid: u16,
    pub sn: [u8; 20],
    pub mn: [u8; 40],
    pub fr: [u8; 8],
    pub rab: u8,
    pub ieee: [u8; 3],
    pub cmic: u8,
    pub mdts: u16,
    pub cntlid: u16,
    pub ver: u32,
    pub rtd3r: u32,
    pub rtd3e: u32,
    pub oaes: u32,
    pub ctratt: u32,
    pub rrls: u16,
    _reserved0: [u8; 9],
    pub cntrld: u8,
    pub crdt1: u16,
    pub crdt2: u16,
    pub crdt3: u16,
    _reserved1: [u8; 248],
    pub oacs: u16,
    pub acl: u8,
    pub aerl: u8,
    pub frmw: u8,
    pub lpa: u8,
    pub elpe: u8,
    pub npss: u8,
    pub avscc: u8,
    pub apsta: u8,
    pub wctemp: u16,
    pub cctemp: u16,
    pub mtfa: u16,
    pub hmpre: u32,
    pub hmmin: u32,
    pub tnvmcap: [u8; 16],
    pub unvmcap: [u8; 16],
    pub rpmbs: u32,
    pub edstt: u16,
    pub dsto: u16,
    pub fwug: u16,
    pub kas: u16,
    pub hctemp: u16,
    pub mntmt: u16,
    pub extost: u16,
    _reserved2: [u8; 296],
    pub anagrpid: u32,
    pub nanagrpid: u32,
    _reserved3: [u8; 248],
    pub sqes: u8,
    pub cqes: u8,
    pub maxcmd: u16,
    pub nn: u32,
    pub oncs: u16,
    pub fuses: u16,
    pub fna: u8,
    pub vwc: u8,
    pub awun: u16,
    pub awupf: u16,
    pub nvscc: u8,
    pub acwu: u16,
    _reserved4: [u8; 246],
    pub anagrpid2: u32,
    pub nanagrpid2: u32,
    _reserved5: [u8; 248],
    pub psd: [PowerStateDescriptor; 32],
    _reserved6: [u8; 1024],
    pub vs: [u8; 1024],
}

/// Identify Namespace Data Structure (4096 bytes)
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct IdentifyNamespaceData {
    pub nsze: u64,
    pub ncap: u64,
    pub nuse: u64,
    pub nsfeat: u8,
    pub nlbaf: u8,
    pub flbas: u8,
    pub mc: u8,
    pub dps: u8,
    pub nmic: u8,
    pub rescap: u8,
    pub fpi: u8,
    pub dlfeat: u8,
    pub nawun: u16,
    pub nawupf: u16,
    pub nacwu: u16,
    pub nabsn: u16,
    pub nabo: u16,
    pub nabspf: u16,
    pub noiob: u16,
    pub nvmcap: [u8; 16],
    _reserved0: [u8; 40],
    pub nguid: [u8; 16],
    pub eui64: u64,
    pub lbaf: [LbaFormat; 16],
    _reserved1: [u8; 192],
    pub vs: [u8; 3712],
}

/// LBA Format
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct LbaFormat {
    pub ms: u16,
    pub lbads: u8,
    pub rp: u8,
}

/// Power State Descriptor
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct PowerStateDescriptor {
    pub mp: u16,
    pub res1: u8,
    pub mps: u8,
    pub enlat: u32,
    pub exlat: u32,
    pub rrt: u8,
    pub rrl: u8,
    pub rwt: u8,
    pub rwl: u8,
    _reserved0: [u8; 16],
    pub ipl: u32,
    pub ips: u32,
    pub icc: u32,
    pub ccd: u32,
    _reserved1: [u8; 196],
}

/// SMART / Health Information Log (512 bytes)
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct SmartHealthLog {
    pub critical_warning: u8,
    pub temperature: u16,
    pub available_spare: u8,
    pub available_spare_threshold: u8,
    pub percentage_used: u8,
    _reserved0: [u8; 26],
    pub data_units_read: [u8; 16],
    pub data_units_written: [u8; 16],
    pub host_read_commands: [u8; 16],
    pub host_write_commands: [u8; 16],
    pub controller_busy_time: [u8; 16],
    pub power_cycles: [u8; 16],
    pub power_on_hours: [u8; 16],
    pub unsafe_shutdowns: [u8; 16],
    pub media_errors: [u8; 16],
    pub num_error_log_entries: [u8; 16],
    _reserved1: [u8; 320],
}

/// Error Information Log Entry
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct ErrorLogEntry {
    pub error_count: u64,
    pub sqid: u16,
    pub cid: u16,
    pub status: u16,
    pub param_error_loc: u16,
    pub lba: u64,
    pub nsid: u32,
    pub vs: u32,
    pub cmd_specific: [u8; 24],
    _reserved0: [u8; 2],
    pub trtype: u8,
    _reserved1: [u8; 3],
}

/// Dataset Management Range
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct DsmRange {
    pub cattr: u8,
    pub nlb: u16,
    pub slba: u64,
}

/// Create Identify Controller command
pub fn identify_controller() -> crate::registers::NvmeCommand {
    let mut cmd = crate::registers::NvmeCommand::admin_command(admin_opcodes::IDENTIFY);
    cmd.nsid = 0;
    cmd.cdw10 = identify_cns::CONTROLLER;
    cmd
}

/// Create Identify Namespace command
pub fn identify_namespace(nsid: u32) -> crate::registers::NvmeCommand {
    let mut cmd = crate::registers::NvmeCommand::admin_command(admin_opcodes::IDENTIFY);
    cmd.nsid = nsid;
    cmd.cdw10 = identify_cns::NAMESPACE;
    cmd
}

/// Create Identify Active Namespace List command
pub fn identify_active_ns_list() -> crate::registers::NvmeCommand {
    let mut cmd = crate::registers::NvmeCommand::admin_command(admin_opcodes::IDENTIFY);
    cmd.nsid = 0;
    cmd.cdw10 = identify_cns::ACTIVE_NS_LIST;
    cmd
}

/// Create Create I/O Completion Queue command
pub fn create_io_cq(qid: u16, qsize: u16, cq_flags: u16, irq_vector: u16, phys_addr: u64) -> crate::registers::NvmeCommand {
    let mut cmd = crate::registers::NvmeCommand::admin_command(admin_opcodes::CREATE_IO_CQ);
    cmd.nsid = 0;
    cmd.prp1 = phys_addr;
    cmd.cdw10 = ((qsize as u32) << 16) | (qid as u32);
    cmd.cdw11 = ((irq_vector as u32) << 16) | (cq_flags as u32);
    cmd
}

/// Create Create I/O Submission Queue command
pub fn create_io_sq(qid: u16, qsize: u16, sq_flags: u16, cqid: u16, phys_addr: u64) -> crate::registers::NvmeCommand {
    let mut cmd = crate::registers::NvmeCommand::admin_command(admin_opcodes::CREATE_IO_SQ);
    cmd.nsid = 0;
    cmd.prp1 = phys_addr;
    cmd.cdw10 = ((qsize as u32) << 16) | (qid as u32);
    cmd.cdw11 = ((cqid as u32) << 16) | (sq_flags as u32);
    cmd
}

/// Create Delete I/O Completion Queue command
pub fn delete_io_cq(qid: u16) -> crate::registers::NvmeCommand {
    let mut cmd = crate::registers::NvmeCommand::admin_command(admin_opcodes::DELETE_IO_CQ);
    cmd.nsid = 0;
    cmd.cdw10 = qid as u32;
    cmd
}

/// Create Delete I/O Submission Queue command
pub fn delete_io_sq(qid: u16) -> crate::registers::NvmeCommand {
    let mut cmd = crate::registers::NvmeCommand::admin_command(admin_opcodes::DELETE_IO_SQ);
    cmd.nsid = 0;
    cmd.cdw10 = qid as u32;
    cmd
}

/// Create Set Features command
pub fn set_features(fid: u8, sel: u8, fvalue: u32) -> crate::registers::NvmeCommand {
    let mut cmd = crate::registers::NvmeCommand::admin_command(admin_opcodes::SET_FEATURES);
    cmd.nsid = 0;
    cmd.cdw10 = fid as u32;
    cmd.cdw11 = (sel as u32) | (fvalue << 8);
    cmd
}

/// Create Get Features command
pub fn get_features(fid: u8, sel: u8, buffer: u64) -> crate::registers::NvmeCommand {
    let mut cmd = crate::registers::NvmeCommand::admin_command(admin_opcodes::GET_FEATURES);
    cmd.nsid = 0;
    cmd.prp1 = buffer;
    cmd.cdw10 = fid as u32;
    cmd.cdw11 = sel as u32;
    cmd
}

/// Create Get Log Page command
pub fn get_log_page(lid: u8, num_entries: u16, nsid: u32, buffer: u64) -> crate::registers::NvmeCommand {
    let mut cmd = crate::registers::NvmeCommand::admin_command(admin_opcodes::GET_LOG_PAGE);
    cmd.nsid = nsid;
    cmd.prp1 = buffer;
    cmd.cdw10 = (lid as u32) | ((num_entries as u32 - 1) << 8);
    cmd.cdw11 = 0;
    cmd
}

/// Create Read command
pub fn read(nsid: u32, lba: u64, num_blocks: u16, prp1: u64, prp2: u64) -> crate::registers::NvmeCommand {
    let mut cmd = crate::registers::NvmeCommand::nvm_command(nvm_opcodes::READ);
    cmd.nsid = nsid;
    cmd.prp1 = prp1;
    cmd.prp2 = prp2;
    cmd.cdw10 = lba as u32;
    cmd.cdw11 = (lba >> 32) as u32;
    cmd.cdw12 = (num_blocks - 1) as u32;
    cmd
}

/// Create Write command
pub fn write(nsid: u32, lba: u64, num_blocks: u16, prp1: u64, prp2: u64, fua: bool) -> crate::registers::NvmeCommand {
    let mut cmd = crate::registers::NvmeCommand::nvm_command(nvm_opcodes::WRITE);
    cmd.nsid = nsid;
    cmd.prp1 = prp1;
    cmd.prp2 = prp2;
    cmd.cdw10 = lba as u32;
    cmd.cdw11 = (lba >> 32) as u32;
    cmd.cdw12 = (num_blocks - 1) as u32;
    if fua {
        cmd.cdw12 |= 1 << 31; // FUA bit
    }
    cmd
}

/// Create Flush command
pub fn flush(nsid: u32) -> crate::registers::NvmeCommand {
    let mut cmd = crate::registers::NvmeCommand::nvm_command(nvm_opcodes::FLUSH);
    cmd.nsid = nsid;
    cmd.cdw10 = 0;
    cmd
}

/// Create Dataset Management (TRIM) command
pub fn dataset_management(nsid: u32, num_ranges: u32, prp1: u64, prp2: u64) -> crate::registers::NvmeCommand {
    let mut cmd = crate::registers::NvmeCommand::nvm_command(nvm_opcodes::DATASET_MANAGEMENT);
    cmd.nsid = nsid;
    cmd.prp1 = prp1;
    cmd.prp2 = prp2;
    cmd.cdw10 = num_ranges - 1;
    cmd.cdw11 = 0x04; // Deallocate
    cmd
}

/// Create Format NVM command
pub fn format_nvm(nsid: u32, lbaf: u8, ses: u8, pi: u8, pil: u8, ms: u8) -> crate::registers::NvmeCommand {
    let mut cmd = crate::registers::NvmeCommand::admin_command(admin_opcodes::FORMAT_NVM);
    cmd.nsid = nsid;
    cmd.cdw10 = (lbaf as u32) | ((ses as u32) << 8) | ((pi as u32) << 16) | ((pil as u32) << 24) | ((ms as u32) << 26);
    cmd
}