// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! ext2 directory entry parsing.

#![no_std]
#![allow(missing_docs)]

use alloc::string::String;

/// An ext2 directory entry.
///
/// `rec_len` is the on-disk record length (used to advance to the next entry);
/// `name`/`name_len`/`file_type` are the decoded fields. `inode == 0` marks
/// an empty/unused slot.
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub inode: u32,
    pub rec_len: u32,
    pub name_len: u32,
    pub file_type: u8,
    pub name: String,
}

impl DirEntry {
    /// Parse a directory entry from `bytes` (starting at the entry start).
    pub fn from_bytes(bytes: &[u8]) -> DirEntry {
        if bytes.len() < 8 {
            return DirEntry {
                inode: 0,
                rec_len: 0,
                name_len: 0,
                file_type: 0,
                name: String::new(),
            };
        }
        let inode = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let rec_len = u16::from_le_bytes([bytes[4], bytes[5]]) as u32;
        let name_len = bytes[6] as u32;
        let file_type = bytes[7];

        let name = if name_len > 0 && (8 + name_len as usize) <= bytes.len() {
            String::from_utf8_lossy(&bytes[8..8 + name_len as usize]).into_owned()
        } else {
            String::new()
        };

        DirEntry {
            inode,
            rec_len,
            name_len,
            file_type,
            name,
        }
    }
}
