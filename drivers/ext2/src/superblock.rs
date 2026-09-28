// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! ext2 superblock structures

#![no_std]
#![allow(missing_docs)]

use core::mem;

/// ext2 superblock (1024 bytes at offset 1024)
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct Superblock {
    pub inodes_count: u32,
    pub blocks_count: u32,
    pub r_blocks_count: u32,
    pub free_blocks_count: u32,
    pub free_inodes_count: u32,
    pub first_data_block: u32,
    pub log_block_size: u32,
    pub log_frag_size: u32,
    pub blocks_per_group: u32,
    pub frags_per_group: u32,
    pub inodes_per_group: u32,
    pub mtime: u32,
    pub wtime: u32,
    pub mnt_count: u16,
    pub max_mnt_count: u16,
    pub magic: u16,
    pub state: u16,
    pub errors: u16,
    pub minor_rev_level: u16,
    pub lastcheck: u32,
    pub checkinterval: u32,
    pub creator_os: u32,
    pub rev_level: u32,
    pub def_resuid: u16,
    pub def_resgid: u16,
    // ext2 dynamic revision fields
    pub first_ino: u32,
    pub inode_size: u16,
    pub block_group_nr: u16,
    pub feature_compat: u32,
    pub feature_incompat: u32,
    pub feature_ro_compat: u32,
    pub uuid: [u8; 16],
    pub volume_name: [u8; 16],
    pub last_mounted: [u8; 64],
    pub algorithm_usage_bitmap: u32,
    // Performance hints
    pub s_prealloc_blocks: u8,
    pub s_prealloc_dir_blocks: u8,
    _reserved1: [u8; 2],
    // Journaling support (ext3)
    pub journal_uuid: [u8; 16],
    pub journal_inum: u32,
    pub journal_dev: u32,
    pub last_orphan: u32,
    _reserved2: [u32; 237],
}

impl Superblock {
    /// Parse superblock from raw bytes
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < mem::size_of::<Self>() {
            return None;
        }
        let sb = unsafe { &*(bytes.as_ptr() as *const Self) };
        Some(*sb)
    }
    
    /// Get block size in bytes
    pub fn block_size(&self) -> u32 {
        1024 << self.log_block_size
    }
    
    /// Get fragment size in bytes
    pub fn frag_size(&self) -> u32 {
        if self.log_frag_size > 0 {
            1024 << self.log_frag_size
        } else {
            self.block_size()
        }
    }
}

/// Block Group Descriptor
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct BlockGroupDescriptor {
    pub block_bitmap: u32,
    pub inode_bitmap: u32,
    pub inode_table: u32,
    pub free_blocks_count: u16,
    pub free_inodes_count: u16,
    pub used_dirs_count: u16,
    _pad: [u8; 14],
}

impl BlockGroupDescriptor {
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < mem::size_of::<Self>() {
            return None;
        }
        let bgd = unsafe { &*(bytes.as_ptr() as *const Self) };
        Some(*bgd)
    }
}

/// Block Group information
#[derive(Debug, Clone)]
pub struct BlockGroup {
    pub block_bitmap: u64,
    pub inode_bitmap: u64,
    pub inode_table: u64,
    pub free_blocks: u16,
    pub free_inodes: u16,
    pub used_dirs: u16,
}

/// Parse all block group descriptors
pub fn parse_block_groups(data: &[u8], sb: &Superblock) -> crate::DriverResult<alloc::vec::Vec<BlockGroup>> {
    let desc_size = mem::size_of::<BlockGroupDescriptor>();
    let num_groups = (sb.blocks_count + sb.blocks_per_group - 1) / sb.blocks_per_group;
    let mut groups = alloc::vec::Vec::with_capacity(num_groups as usize);
    
    for i in 0..num_groups as usize {
        let offset = i * desc_size;
        if offset + desc_size > data.len() {
            break;
        }
        if let Some(desc) = BlockGroupDescriptor::from_bytes(&data[offset..offset + desc_size]) {
            groups.push(BlockGroup {
                block_bitmap: desc.block_bitmap as u64,
                inode_bitmap: desc.inode_bitmap as u64,
                inode_table: desc.inode_table as u64,
                free_blocks: desc.free_blocks_count,
                free_inodes: desc.free_inodes_count,
                used_dirs: desc.used_dirs_count,
            });
        }
    }
    
    Ok(groups)
}

/// Feature flags
pub mod features {
    pub mod compat {
        pub const DIR_PREALLOC: u32 = 0x0001;
        pub const IMAGIC_INODES: u32 = 0x0002;
        pub const HAS_JOURNAL: u32 = 0x0004;
        pub const EXT_ATTR: u32 = 0x0008;
        pub const RESIZE_INODE: u32 = 0x0010;
        pub const DIR_INDEX: u32 = 0x0020;
    }
    
    pub mod incompat {
        pub const COMPRESSION: u32 = 0x0001;
        pub const FILETYPE: u32 = 0x0002;
        pub const RECOVER: u32 = 0x0004;
        pub const JOURNAL_DEV: u32 = 0x0008;
        pub const META_BG: u32 = 0x0010;
    }
    
    pub mod ro_compat {
        pub const SPARSE_SUPER: u32 = 0x0001;
        pub const LARGE_FILE: u32 = 0x0002;
        pub const BTREE_DIR: u32 = 0x0004;
    }
}