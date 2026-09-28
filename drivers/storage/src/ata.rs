// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! ATA/ATAPI command definitions and structures.

#![no_std]
#![allow(missing_docs)]

use core::mem;

/// ATA Commands
pub mod commands {
    pub const READ_SECTORS: u8 = 0x20;
    pub const READ_SECTORS_EXT: u8 = 0x24;
    pub const READ_DMA_EXT: u8 = 0x25;
    pub const READ_NATIVE_MAX_ADDRESS_EXT: u8 = 0x27;
    pub const READ_MULTIPLE_EXT: u8 = 0x29;
    pub const READ_LOG_EXT: u8 = 0x2F;
    pub const WRITE_SECTORS: u8 = 0x30;
    pub const WRITE_SECTORS_EXT: u8 = 0x34;
    pub const WRITE_DMA_EXT: u8 = 0x35;
    pub const WRITE_MULTIPLE_EXT: u8 = 0x39;
    pub const WRITE_LOG_EXT: u8 = 0x3F;
    pub const WRITE_DMA_FUA_EXT: u8 = 0x3D;
    pub const FLUSH_CACHE: u8 = 0xE7;
    pub const FLUSH_CACHE_EXT: u8 = 0xEA;
    pub const IDENTIFY_DEVICE: u8 = 0xEC;
    pub const SET_FEATURES: u8 = 0xEF;
    pub const READ_LOG_DMA_EXT: u8 = 0x47;
    pub const WRITE_LOG_DMA_EXT: u8 = 0x57;
    pub const READ_FPDMA_QUEUED: u8 = 0x60;
    pub const WRITE_FPDMA_QUEUED: u8 = 0x61;
    pub const NCQ_NON_DATA: u8 = 0x63;
    pub const SEND_FPDMA_QUEUED: u8 = 0x64;
    pub const DATA_SET_MANAGEMENT: u8 = 0x06;
}

/// ATA Feature subcommands
pub mod features {
    pub const ENABLE_WRITE_CACHE: u8 = 0x02;
    pub const ENABLE_SATA_FEATURE: u8 = 0x10;
    pub const DISABLE_MEDIA_STATUS: u8 = 0x31;
    pub const ENABLE_APM: u8 = 0x05;
    pub const DISABLE_APM: u8 = 0x85;
    pub const ENABLE_PUIS: u8 = 0x06;
    pub const DISABLE_PUIS: u8 = 0x86;
    pub const SET_TRANSFER_MODE: u8 = 0x03;
}

/// ATA Identify Device data structure (512 bytes)
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct IdentifyData {
    pub general_config: u16,
    pub num_cylinders: u16,
    pub specific_config: u16,
    pub num_heads: u16,
    pub unformatted_bytes_per_track: u16,
    pub unformatted_bytes_per_sector: u16,
    pub sectors_per_track: u16,
    pub vendor_unique_1: [u16; 3],
    pub serial: [u8; 20],
    pub buffer_type: u16,
    pub buffer_size: u16,
    pub ecc_bytes: u16,
    pub firmware_rev: [u8; 8],
    pub model: [u8; 40],
    pub max_multiple_sectors: u16,
    pub vendor_unique_2: u16,
    pub double_word_io: u16,
    pub capabilities_1: u16,
    pub capabilities_2: u16,
    pub vendor_unique_3: u16,
    pub pio_timing: u16,
    pub dma_timing: u16,
    pub current_field_valid: u16,
    pub current_cylinders: u16,
    pub current_heads: u16,
    pub current_sectors: u16,
    pub current_capacity: u32,
    pub multiple_sectors_setting: u16,
    pub max_lba_28: u32,
    pub single_word_dma: u16,
    pub multi_word_dma: u16,
    pub advanced_pio_modes: u16,
    pub min_multiword_dma_cycle: u16,
    pub recommended_multiword_dma_cycle: u16,
    pub min_pio_cycle_no_flow: u16,
    pub min_pio_cycle_flow: u16,
    _reserved_1: [u16; 2],
    pub queue_depth: u16,
    _reserved_2: [u16; 4],
    pub major_version: u16,
    pub minor_version: u16,
    pub command_set_1: u16,
    pub command_set_2: u16,
    pub cfsse: u16,
    pub cfs_enable_1: u16,
    pub cfs_enable_2: u16,
    pub csf_default: u16,
    pub ultra_dma: u16,
    pub erase_time: u16,
    pub erase_time_enhanced: u16,
    pub apm_level: u16,
    pub master_pwd_rev: u16,
    pub hardware_reset: u16,
    pub acoustic_mgmt: u16,
    _reserved_3: [u16; 2],
    pub max_lba_48: u64,
    _reserved_4: [u16; 11],
    pub dma_setup: u16,
    _reserved_5: [u16; 3],
    pub max_48bit_lba: u64,
    _reserved_6: [u16; 28],
    pub transport_major: u16,
    pub transport_minor: u16,
    _reserved_7: [u16; 21],
    pub sector_size: u16,
    _reserved_8: [u16; 6],
    pub additional_supported: u16,
    pub additional_enabled: u16,
    _reserved_9: [u16; 25],
    pub integrity_word: u16,
}

impl IdentifyData {
    /// Parse from raw bytes
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < mem::size_of::<Self>() {
            return None;
        }
        let data = unsafe { &*(bytes.as_ptr() as *const Self) };
        Some(*data)
    }
    
    /// Check if LBA48 is supported
    pub fn supports_lba48(&self) -> bool {
        (self.command_set_1 & (1 << 10)) != 0
    }
    
    /// Check if DMA is supported
    pub fn supports_dma(&self) -> bool {
        (self.capabilities_1 & (1 << 8)) != 0
    }
    
    /// Check if FLUSH CACHE EXT is supported
    pub fn supports_flush_ext(&self) -> bool {
        (self.command_set_2 & (1 << 13)) != 0
    }
    
    /// Check if FUA is supported
    pub fn supports_fua(&self) -> bool {
        (self.command_set_2 & (1 << 6)) != 0
    }
    
    /// Check if NCQ is supported
    pub fn supports_ncq(&self) -> bool {
        (self.command_set_2 & (1 << 10)) != 0
    }
    
    /// Get max LBA (28-bit or 48-bit)
    pub fn max_lba(&self) -> u64 {
        if self.supports_lba48() {
            self.max_48bit_lba
        } else {
            self.max_lba_28 as u64
        }
    }
}

/// ATA Status Register bits
pub mod status {
    pub const ERR: u32 = 1 << 0;
    pub const IDX: u32 = 1 << 1;
    pub const CORR: u32 = 1 << 2;
    pub const DRQ: u32 = 1 << 3;
    pub const DSC: u32 = 1 << 4;
    pub const DF: u32 = 1 << 5;
    pub const DRDY: u32 = 1 << 6;
    pub const BSY: u32 = 1 << 7;
}

/// ATA Error Register bits
pub mod error {
    pub const AMNF: u8 = 1 << 0;
    pub const TK0NF: u8 = 1 << 1;
    pub const ABRT: u8 = 1 << 2;
    pub const MCR: u8 = 1 << 3;
    pub const IDNF: u8 = 1 << 4;
    pub const MC: u8 = 1 << 5;
    pub const UNC: u8 = 1 << 6;
    pub const BBK: u8 = 1 << 7;
}

/// ATA Device Control Register bits
pub mod device_control {
    pub const HD: u8 = 1 << 0;
    pub const RST: u8 = 1 << 2;
    pub const SRST: u8 = 1 << 2;
    pub const NIEN: u8 = 1 << 1;
}

/// PRDT Entry (Physical Region Descriptor Table Entry)
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct PrdtEntry {
    pub dba: u32,       // Data Base Address
    pub dbau: u32,      // Data Base Address Upper
    pub rsv: u32,       // Reserved
    pub dbc: u32,       // Data Byte Count (bit 31 = interrupt on completion)
}

impl PrdtEntry {
    pub fn new(phys_addr: u64, byte_count: u32, ioc: bool) -> Self {
        let mut dbc = byte_count & 0x3FFFFF;
        if ioc {
            dbc |= 1 << 31;
        }
        Self {
            dba: phys_addr as u32,
            dbau: (phys_addr >> 32) as u32,
            rsv: 0,
            dbc,
        }
    }
}

/// Command Table (for AHCI)
#[repr(C)]
pub struct CommandTable {
    pub cfis: [u8; 64],       // Command FIS
    pub acmd: [u8; 16],       // ATAPI Command
    pub rsv: [u8; 48],        // Reserved
    pub prdt: [PrdtEntry; 64], // PRDT Entries (max 64)
}

/// Build a READ DMA EXT command FIS
pub fn build_read_dma_ext_fis(lba: u64, sector_count: u16) -> [u8; 64] {
    let mut fis = [0u8; 64];
    fis[0] = 0x27; // FIS_TYPE_REG_H2D
    fis[1] = 1 << 7; // C=1 (Command)
    fis[2] = commands::READ_DMA_EXT;
    fis[3] = 0; // Features
    
    // LBA Low (48-bit)
    fis[4] = (lba >> 0) as u8;
    fis[5] = (lba >> 8) as u8;
    fis[6] = (lba >> 16) as u8;
    fis[7] = (lba >> 24) as u8;
    fis[8] = (lba >> 32) as u8;
    fis[9] = (lba >> 40) as u8;
    
    // Device register (LBA mode)
    fis[10] = 0x40 | ((lba >> 24) & 0xF) as u8;
    
    // Sector count
    fis[12] = sector_count as u8;
    fis[13] = (sector_count >> 8) as u8;
    
    fis
}

/// Build a WRITE DMA EXT command FIS
pub fn build_write_dma_ext_fis(lba: u64, sector_count: u16, fua: bool) -> [u8; 64] {
    let mut fis = [0u8; 64];
    fis[0] = 0x27; // FIS_TYPE_REG_H2D
    fis[1] = 1 << 7; // C=1 (Command)
    fis[2] = if fua { commands::WRITE_DMA_FUA_EXT } else { commands::WRITE_DMA_EXT };
    fis[3] = 0; // Features
    
    // LBA Low (48-bit)
    fis[4] = (lba >> 0) as u8;
    fis[5] = (lba >> 8) as u8;
    fis[6] = (lba >> 16) as u8;
    fis[7] = (lba >> 24) as u8;
    fis[8] = (lba >> 32) as u8;
    fis[9] = (lba >> 40) as u8;
    
    // Device register (LBA mode)
    fis[10] = 0x40 | ((lba >> 24) & 0xF) as u8;
    
    // Sector count
    fis[12] = sector_count as u8;
    fis[13] = (sector_count >> 8) as u8;
    
    fis
}

/// Build a FLUSH CACHE EXT command FIS
pub fn build_flush_cache_ext_fis() -> [u8; 64] {
    let mut fis = [0u8; 64];
    fis[0] = 0x27; // FIS_TYPE_REG_H2D
    fis[1] = 1 << 7; // C=1 (Command)
    fis[2] = commands::FLUSH_CACHE_EXT;
    fis[10] = 0x40; // Device register (LBA mode)
    fis
}

/// Build an IDENTIFY DEVICE command FIS
pub fn build_identify_fis() -> [u8; 64] {
    let mut fis = [0u8; 64];
    fis[0] = 0x27; // FIS_TYPE_REG_H2D
    fis[1] = 1 << 7; // C=1 (Command)
    fis[2] = commands::IDENTIFY_DEVICE;
    fis[10] = 0xA0; // Device register (LBA mode, DEV=0)
    fis
}

/// Build a SET FEATURES command FIS
pub fn build_set_features_fis(feature: u8, count: u16) -> [u8; 64] {
    let mut fis = [0u8; 64];
    fis[0] = 0x27; // FIS_TYPE_REG_H2D
    fis[1] = 1 << 7; // C=1 (Command)
    fis[2] = commands::SET_FEATURES;
    fis[3] = feature; // Features
    fis[12] = count as u8;
    fis[13] = (count >> 8) as u8;
    fis[10] = 0x40; // Device register (LBA mode)
    fis
}