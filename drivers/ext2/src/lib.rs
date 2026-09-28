// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! ext2 filesystem driver for Samsara.
//!
//! Implements the Second Extended Filesystem (ext2) for persistent storage
//! on block devices. Supports:
//! - Superblock and block group descriptor parsing
//! - Inode allocation and management
//! - Directory entry reading
//! - File data reading (direct, indirect, double-indirect, triple-indirect blocks)
//! - Block allocation bitmap management

#![no_std]
#![allow(missing_docs)]

extern crate alloc;

use core::fmt;
use alloc::vec::Vec;
use alloc::vec;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::boxed::Box;
use spin::Mutex;

use driver_common::{
    DriverError, DriverResult, BlockDevice, DeviceInfo, vfs_resolve, vfs_mount, Vnode, NodeKind,
    VnodeRef, FsError,
};
use crate::block::Ext2BlockDevice;
use crate::superblock::BlockGroup;

pub mod block;
pub mod inode;
pub mod dir;
pub mod superblock;

use superblock::Superblock;
use inode::Inode;
use dir::DirEntry;

/// ext2 filesystem instance
pub struct Ext2Filesystem {
    device: Ext2BlockDevice,
    superblock: Superblock,
    block_groups: Vec<BlockGroup>,
    inode_cache: Mutex<BTreeMap<u32, Arc<Inode>>>,
}

use alloc::collections::BTreeMap;

impl Ext2Filesystem {
    /// Create a new ext2 filesystem from a block device
    pub fn new(mut device: Box<dyn BlockDevice>) -> DriverResult<Self> {
        let block_dev = Ext2BlockDevice::new(device)?;
        
        // Read superblock (block 1, offset 1024)
        let mut sb_buf = [0u8; 1024];
        block_dev.read_blocks(2, &mut sb_buf).map_err(|_| DriverError::IoError)?;
        
        let superblock = Superblock::from_bytes(&sb_buf)
            .ok_or(DriverError::InvalidArgument)?;
        
        // Validate magic
        if superblock.magic != 0xEF53 {
            return Err(DriverError::InvalidArgument);
        }
        
        // Read block group descriptors
        let bgdt_blocks = ((superblock.blocks_per_group * core::mem::size_of::<superblock::BlockGroupDescriptor>() as u32 + superblock.block_size() - 1) / superblock.block_size()) as u64;
        let bgdt_start = superblock.first_data_block as u64 + 1;
        
        let mut bgdt_buf = alloc::vec![0u8; bgdt_blocks as usize * superblock.block_size() as usize];
        block_dev.read_blocks(bgdt_start, &mut bgdt_buf).map_err(|_| DriverError::IoError)?;
        
        let block_groups = superblock::parse_block_groups(&bgdt_buf, &superblock)?;
        
        Ok(Self {
            device: block_dev,
            superblock,
            block_groups,
            inode_cache: Mutex::new(BTreeMap::new()),
        })
    }
    
    /// Get superblock reference
    pub fn superblock(&self) -> &Superblock {
        &self.superblock
    }
    
    /// Read an inode by number
    pub fn read_inode(&self, inode_num: u32) -> DriverResult<Arc<Inode>> {
        // Check cache first
        {
            let cache = self.inode_cache.lock();
            if let Some(inode) = cache.get(&inode_num) {
                return Ok(inode.clone());
            }
        }
        
        // Calculate block group and index
        let inodes_per_group = self.superblock.inodes_per_group;
        let group_idx = (inode_num - 1) / inodes_per_group;
        let index = (inode_num - 1) % inodes_per_group;
        
        if group_idx as usize >= self.block_groups.len() {
            return Err(DriverError::NotFound);
        }
        
        let group = &self.block_groups[group_idx as usize];
        let inode_table_block = group.inode_table;
        let inode_size = self.superblock.inode_size as u64;
        let block_size = self.superblock.block_size() as u64;
        let inode_block = inode_table_block + (index as u64 * inode_size / block_size) as u64;
        let inode_offset = (index as u64 * inode_size) % block_size;
        
        let mut buf = vec![0u8; self.superblock.block_size() as usize];
        self.device.read_blocks(inode_block, &mut buf).map_err(|_| DriverError::IoError)?;
        
        let inode = Inode::from_bytes(&buf[inode_offset as usize..], inode_num)
            .ok_or(DriverError::InvalidArgument)?;
        
        let inode_arc = Arc::new(inode);
        self.inode_cache.lock().insert(inode_num, inode_arc.clone());
        
        Ok(inode_arc)
    }
    
    /// Look up a path starting from root
    pub fn lookup_path(&self, path: &str) -> DriverResult<u32> {
        let mut current_inode = self.read_inode(2)?; // Root inode is always 2
        
        for component in path.split('/').filter(|s| !s.is_empty()) {
            current_inode = self.lookup_entry(&current_inode, component)?;
        }
        
        Ok(current_inode.number)
    }
    
    /// Look up a directory entry by name
    fn lookup_entry(&self, dir_inode: &Inode, name: &str) -> DriverResult<Arc<Inode>> {
        if !dir_inode.is_directory() {
            return Err(DriverError::NotSupported);
        }
        
        let entries = self.read_directory(dir_inode)?;
        for entry in entries {
            if entry.name == name {
                return self.read_inode(entry.inode);
            }
        }
        
        Err(DriverError::NotFound)
    }
    
    /// Read directory entries
    pub fn read_directory(&self, dir_inode: &Inode) -> DriverResult<Vec<DirEntry>> {
        let mut entries = Vec::new();
        let mut offset = 0u64;
        let block_size = self.superblock.block_size() as usize;
        
        loop {
            let mut buf = vec![0u8; block_size];
            let bytes_read = self.read_inode_data(dir_inode, offset, &mut buf)?;
            if bytes_read == 0 {
                break;
            }
            
            let mut pos = 0;
            while pos < bytes_read {
                let entry = DirEntry::from_bytes(&buf[pos..]);
                if entry.inode == 0 {
                    break;
                }
                let rec_len = entry.rec_len as usize;
                if entry.name_len > 0 {
                    entries.push(entry);
                }
                pos += rec_len;
                if pos >= bytes_read {
                    break;
                }
            }
            
            offset += bytes_read as u64;
        }
        
        Ok(entries)
    }
    
    /// Read inode data (handles direct/indirect blocks)
    fn read_inode_data(&self, inode: &Inode, offset: u64, buf: &mut [u8]) -> DriverResult<usize> {
        let block_size = self.superblock.block_size() as u64;
        let file_size = inode.i_size_lower as u64;
        
        if offset >= file_size {
            return Ok(0);
        }
        
        let to_read = core::cmp::min(buf.len() as u64, file_size - offset) as usize;
        let mut total_read = 0;
        let mut current_offset = offset;
        let mut buf_offset = 0;
        
        while total_read < to_read {
            let block_index = current_offset / block_size;
            let block_offset = current_offset % block_size;
            let chunk = core::cmp::min(to_read - total_read, (block_size - block_offset) as usize);
            
            let block_num = inode.get_block(self, block_index)?;
            if block_num == 0 {
                // Sparse block - fill with zeros
                buf[buf_offset..buf_offset + chunk].fill(0);
            } else {
                let mut block_buf = vec![0u8; block_size as usize];
                self.device.read_blocks(block_num, &mut block_buf).map_err(|_| DriverError::IoError)?;
                buf[buf_offset..buf_offset + chunk].copy_from_slice(&block_buf[block_offset as usize..][..chunk]);
            }
            
            total_read += chunk;
            current_offset += chunk as u64;
            buf_offset += chunk;
        }
        
        Ok(total_read)
    }
    
    /// Get the block device size in bytes (best-effort, from the superblock).
    pub fn total_blocks(&self) -> u64 {
        self.superblock.blocks_count as u64
    }
}

/// ext2 VFS integration
pub struct Ext2Vfs {
    fs: Arc<Ext2Filesystem>,
}

impl Ext2Vfs {
    pub fn new(fs: Ext2Filesystem) -> Self {
        Self { fs: Arc::new(fs) }
    }
    
    pub fn create_vnode(&self, inode_num: u32) -> DriverResult<Arc<dyn Vnode>> {
        let inode = self.fs.read_inode(inode_num)?;
        Ok(Arc::new(Ext2Vnode { fs: self.fs.clone(), inode }))
    }
}

/// Try to mount the first available block device as ext2
pub fn try_mount_first() -> DriverResult<()> {
    // Get all block devices from devfs
    let devfs_root = unsafe { vfs_resolve("/dev") }.map_err(|_| DriverError::IoError)?;
    let entries = devfs_root.list().map_err(|_| DriverError::IoError)?;
    
    for (name, kind) in entries {
        if kind == NodeKind::File && name.starts_with("sd") {
            // Try to open as block device
            if let Ok(node) = unsafe { vfs_resolve(&alloc::format!("/dev/{}", name)) } {
                // Check if it's a block device by trying to read superblock
                let mut sb_buf = [0u8; 1024];
                if let Ok(_) = node.read_at(1024, &mut sb_buf) {
                    if u16::from_le_bytes([sb_buf[0x38], sb_buf[0x39]]) == 0xEF53 {
                        // Found ext2 filesystem
                        driver_common::kinfo!("ext2: mounting {} as /", name);
                        
                        // Create block device wrapper
                        let block_dev = DevfsBlockDevice { node: node.clone() };
                        let fs = Ext2Filesystem::new(Box::new(block_dev))?;
                        let vfs = Ext2Vfs::new(fs);
                        
                        // Mount at root (replace ramfs)
                        let root_vnode = vfs.create_vnode(2)?; // root inode
                        unsafe { vfs_mount("/", root_vnode) }.map_err(|_| DriverError::InvalidArgument)?;
                        
                        driver_common::kinfo!("ext2: mounted {} successfully", name);
                        return Ok(());
                    }
                }
            }
        }
    }
    
    Err(DriverError::NotFound)
}

/// Block device wrapper for devfs nodes
struct DevfsBlockDevice {
    node: VnodeRef,
}

impl driver_common::BlockDevice for DevfsBlockDevice {
    fn read_blocks(&self, lba: u64, buf: &mut [u8]) -> DriverResult<usize> {
        let block_size = 512; // Default
        let offset = lba * block_size as u64;
        self.node.read_at(offset, buf).map_err(|_| DriverError::IoError)
    }
    
    fn write_blocks(&self, lba: u64, buf: &[u8]) -> DriverResult<usize> {
        let block_size = 512;
        let offset = lba * block_size as u64;
        self.node.write_at(offset, buf).map_err(|_| DriverError::IoError)
    }
    
    fn flush(&self) -> DriverResult<()> {
        Ok(())
    }
    
    fn block_size(&self) -> u32 {
        512
    }
    
    fn num_blocks(&self) -> u64 {
        self.node.size_hint() / 512
    }
    
    fn device_info(&self) -> driver_common::DeviceInfo {
        driver_common::DeviceInfo {
            id: driver_common::DeviceId::new(0, 0, 0),
            class: driver_common::DeviceClass::Block,
            vendor_id: 0,
            device_id: 0,
            revision: 0,
            capabilities: driver_common::DeviceCapabilities::READ | driver_common::DeviceCapabilities::WRITE,
            name: alloc::string::String::from("devfs-block"),
            driver_name: Some(alloc::string::String::from("devfs")),
        }
    }
}

struct Ext2Vnode {
    fs: Arc<Ext2Filesystem>,
    inode: Arc<Inode>,
}

impl Vnode for Ext2Vnode {
    fn kind(&self) -> NodeKind {
        if self.inode.is_directory() {
            NodeKind::Dir
        } else {
            NodeKind::File
        }
    }
    
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        self.fs.read_inode_data(&self.inode, offset, buf).map_err(|_| FsError::IoError)
    }
    
    fn write_at(&self, _offset: u64, _buf: &[u8]) -> Result<usize, FsError> {
        Err(FsError::NotSupported) // Read-only for now
    }
    
    fn lookup(&self, name: &str) -> Result<Arc<dyn Vnode>, FsError> {
        if !self.inode.is_directory() {
            return Err(FsError::NotADirectory);
        }
        let child_inode = self.fs.lookup_entry(&self.inode, name).map_err(|_| FsError::NotFound)?;
        Ok(Arc::new(Ext2Vnode { fs: self.fs.clone(), inode: child_inode }))
    }
    
    fn list(&self) -> Result<Vec<(String, NodeKind)>, FsError> {
        if !self.inode.is_directory() {
            return Err(FsError::NotADirectory);
        }
        let entries = self.fs.read_directory(&self.inode).map_err(|_| FsError::IoError)?;
        Ok(entries.into_iter().map(|e| {
            let kind = if e.file_type == 2 { NodeKind::Dir } else { NodeKind::File };
            (e.name, kind)
        }).collect())
    }
    
    fn size_hint(&self) -> u64 {
        self.inode.i_size_lower as u64
    }

    fn is_seekable(&self) -> bool {
        // A regular file (or a directory listed in name order) has a stable
        // byte position, so `lseek` can answer. Only a symlink would not, and
        // this driver does not follow them into a node.
        true
    }

    fn file_size(&self) -> u64 {
        self.inode.i_size_lower as u64
    }
}