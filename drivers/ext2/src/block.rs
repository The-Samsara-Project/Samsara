// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! ext2 block device wrapper.

#![no_std]
#![allow(missing_docs)]

use alloc::boxed::Box;

use driver_common::{BlockDevice, DriverError, DriverResult};
use spin::Mutex;

/// Wraps a [`BlockDevice`] so the ext2 filesystem can read through a shared
/// (`&self`) reference, which the on-disk structures require while walking
/// directory trees and indirect block maps.
pub struct Ext2BlockDevice {
    device: Mutex<Box<dyn BlockDevice>>,
}

impl Ext2BlockDevice {
    /// Wrap a raw block device.
    pub fn new(device: Box<dyn BlockDevice>) -> DriverResult<Self> {
        Ok(Self {
            device: Mutex::new(device),
        })
    }

    /// Read `buf.len()` bytes starting at logical block `lba`.
    pub fn read_blocks(&self, lba: u64, buf: &mut [u8]) -> DriverResult<usize> {
        self.device.lock().read_blocks(lba, buf)
    }

    /// Write `buf.len()` bytes starting at logical block `lba`.
    pub fn write_blocks(&self, lba: u64, buf: &[u8]) -> DriverResult<usize> {
        self.device.lock().write_blocks(lba, buf)
    }
}
