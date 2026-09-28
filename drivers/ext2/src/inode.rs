// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! ext2 inode structures and block-map resolution.

#![no_std]
#![allow(missing_docs)]

use alloc::vec;
use alloc::vec::Vec;

use driver_common::{DriverError, DriverResult};

use crate::Ext2Filesystem;

/// On-disk ext2 inode layout (first `inode_size` bytes of an inode table
/// entry).
#[repr(C, packed)]
struct RawInode {
    i_mode: u16,
    i_uid: u16,
    i_size_lower: u32,
    i_atime: u32,
    i_ctime: u32,
    i_mtime: u32,
    i_dtime: u32,
    i_gid: u16,
    i_links_count: u16,
    i_blocks: u32,
    i_flags: u32,
    i_osd1: u32,
    i_block: [u32; 15],
    i_generation: u32,
    i_file_acl: u32,
    i_dir_acl: u32,
    i_faddr: u32,
    i_osd2: [u8; 12],
}

/// In-memory ext2 inode.
#[derive(Debug, Clone)]
pub struct Inode {
    pub i_mode: u16,
    pub i_uid: u16,
    pub i_size_lower: u32,
    pub i_atime: u32,
    pub i_ctime: u32,
    pub i_mtime: u32,
    pub i_dtime: u32,
    pub i_gid: u16,
    pub i_links_count: u16,
    pub i_blocks: u32,
    pub i_flags: u32,
    pub i_block: [u32; 15],
    pub i_generation: u32,
    pub i_file_acl: u32,
    pub i_dir_acl: u32,
    pub i_size_upper: u32,
    /// Inode number (not present on disk).
    pub number: u32,
}

impl Inode {
    /// Parse an inode from raw bytes (starting at the inode start).
    pub fn from_bytes(bytes: &[u8], number: u32) -> Option<Inode> {
        if bytes.len() < core::mem::size_of::<RawInode>() {
            return None;
        }
        let raw = unsafe { &*(bytes.as_ptr() as *const RawInode) };
        Some(Inode {
            i_mode: raw.i_mode,
            i_uid: raw.i_uid,
            i_size_lower: raw.i_size_lower,
            i_atime: raw.i_atime,
            i_ctime: raw.i_ctime,
            i_mtime: raw.i_mtime,
            i_dtime: raw.i_dtime,
            i_gid: raw.i_gid,
            i_links_count: raw.i_links_count,
            i_blocks: raw.i_blocks,
            i_flags: raw.i_flags,
            i_block: raw.i_block,
            i_generation: raw.i_generation,
            i_file_acl: raw.i_file_acl,
            i_dir_acl: raw.i_dir_acl,
            i_size_upper: 0,
            number,
        })
    }

    /// True if this inode is a directory.
    pub fn is_directory(&self) -> bool {
        (self.i_mode & 0xF000) == 0x4000
    }

    /// Total size in bytes (combining lower and upper halves).
    pub fn size(&self) -> u64 {
        (self.i_size_lower as u64) | ((self.i_size_upper as u64) << 32)
    }

    /// Resolve the on-disk block number for logical `block_index`.
    ///
    /// Handles direct blocks, single/double/triple indirect maps. Returns `0`
    /// for a sparse (unallocated) block.
    pub fn get_block(&self, fs: &Ext2Filesystem, block_index: u64) -> DriverResult<u64> {
        let block_size = fs.superblock.block_size() as u64;
        let ptrs_per_block = block_size / 4;

        if block_index < 12 {
            return Ok(self.i_block[block_index as usize] as u64);
        }

        let index = block_index - 12;
        if index < ptrs_per_block {
            let ind = self.i_block[12] as u64;
            if ind == 0 {
                return Ok(0);
            }
            let buf = read_indirect(fs, ind, block_size)?;
            return Ok(read_u32_le(&buf, (index * 4) as usize) as u64);
        }

        let index = index - ptrs_per_block;
        if index < ptrs_per_block * ptrs_per_block {
            let dind = self.i_block[13] as u64;
            if dind == 0 {
                return Ok(0);
            }
            let buf = read_indirect(fs, dind, block_size)?;
            let l1 = read_u32_le(&buf, ((index / ptrs_per_block) * 4) as usize) as u64;
            if l1 == 0 {
                return Ok(0);
            }
            let buf = read_indirect(fs, l1, block_size)?;
            return Ok(read_u32_le(&buf, ((index % ptrs_per_block) * 4) as usize) as u64);
        }

        let index = index - ptrs_per_block * ptrs_per_block;
        let tind = self.i_block[14] as u64;
        if tind == 0 {
            return Ok(0);
        }
        let buf = read_indirect(fs, tind, block_size)?;
        let l1 = read_u32_le(
            &buf,
            ((index / (ptrs_per_block * ptrs_per_block)) * 4) as usize,
        ) as u64;
        if l1 == 0 {
            return Ok(0);
        }
        let buf = read_indirect(fs, l1, block_size)?;
        let l2 = read_u32_le(
            &buf,
            (((index / ptrs_per_block) % ptrs_per_block) * 4) as usize,
        ) as u64;
        if l2 == 0 {
            return Ok(0);
        }
        let buf = read_indirect(fs, l2, block_size)?;
        Ok(read_u32_le(
            &buf,
            ((index % ptrs_per_block) * 4) as usize,
        ) as u64)
    }
}

fn read_indirect(fs: &Ext2Filesystem, block: u64, block_size: u64) -> DriverResult<Vec<u8>> {
    let mut buf = vec![0u8; block_size as usize];
    fs.device.read_blocks(block, &mut buf)?;
    Ok(buf)
}

fn read_u32_le(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}
