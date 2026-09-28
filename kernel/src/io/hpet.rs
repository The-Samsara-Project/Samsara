// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! High Precision Event Timer (HPET) driver.
//!
//! The HPET is probed at its architecturally fixed address, `0xFED0_0000`
//! (chipsets may relocate it via the ACPI HPET table; the fixed address is
//! universal in practice and is what QEMU and every post-2006 chipset ship).
//! Only the main counter is used here: it is a free-running monotonic clock
//! fed at a fixed firmware-programmed frequency that Samsara reads to
//! calibrate the TSC and (indirectly) the LAPIC timer.
//!
//! The register block is mapped uncached through the device MMIO window so
//! the counter reads are never absorbed by a data cache.

use crate::memory;
use crate::sync::OnceCell;
use core::ptr;

/// Physical base of the HPET register block.
pub const HPET_PHYS_BASE: usize = 0xFED0_0000;
/// Bytes of register space used.
const REGION_SIZE: usize = 0x400;

/// Capability/ID register low dword: zero means no HPET is present.
const CAP_ID: usize = 0x00;
/// Capability/ID register high dword: main counter tick period in fs.
const CAP_PERIOD_FS: usize = 0x04;
/// General configuration register.
const CFG: usize = 0x10;
/// Configuration bit: enable the main counter.
const CFG_ENABLE: u32 = 1 << 0;
/// Configuration bit: legacy-replacement routing (never enabled).
const CFG_LEGACY_ROUTE: u32 = 1 << 1;
/// Main counter register (low dword).
const COUNTER_LO: usize = 0x20;
/// Main counter register (high dword).
const COUNTER_HI: usize = 0x24;

struct Hpet {
    /// Mapped virtual address of the register block (kept as `usize` so the
    /// struct stays `Send + Sync` for `OnceCell`).
    base: usize,
    /// Main counter tick period in femtoseconds.
    period_fs: u128,
}

impl Hpet {
    /// Read one 32-bit register through a volatile alias.
    fn read32(&self, offset: usize) -> u32 {
        // SAFETY: `base` aliases a mapped, uncached device window covering
        // the register block.
        unsafe { ptr::read_volatile((self.base as *mut u32).add(offset / 4)) }
    }

    /// Write one 32-bit register through a volatile alias.
    fn write32(&self, offset: usize, value: u32) {
        // SAFETY: see `read32`.
        unsafe { ptr::write_volatile((self.base as *mut u32).add(offset / 4), value) }
    }

    /// Snapshot the 64-bit main counter, compensating for rollover of the
    /// low dword between the two reads.
    fn counter(&self) -> u64 {
        loop {
            let hi = self.read32(COUNTER_HI) as u64;
            let lo = self.read32(COUNTER_LO) as u64;
            // If the low half overflowed between the reads, the high half we
            // captured may be one short; re-read until both halves agree.
            if hi == self.read32(COUNTER_HI) as u64 {
                return (hi << 32) | lo;
            }
        }
    }

    /// Monotonic nanoseconds since the counter was enabled.
    fn nanos(&self) -> u64 {
        let ticks = self.counter() as u128;
        (ticks * self.period_fs / 1_000_000) as u64
    }
}

static HPET: OnceCell<Hpet> = OnceCell::new();

/// True once an HPET is initialized and usable.
pub fn available() -> bool {
    HPET.get().is_some()
}

/// Probe for, map and enable the HPET main counter.
pub fn init() {
    if HPET.get().is_some() {
        return;
    }
    let virt = memory::map_device_mmio(HPET_PHYS_BASE, REGION_SIZE);
    let base = virt as usize;

    // SAFETY: the window is mapped (see `memory::map_device_mmio`).
    let id = unsafe { ptr::read_volatile(base as *mut u32) };
    let period_fs = unsafe { ptr::read_volatile((base as *mut u32).add(CAP_PERIOD_FS / 4)) } as u64 as u128;
    if id == 0 || id == 0xFFFF_FFFF || period_fs == 0 {
        crate::log::kdebug!("hpet: no timer at {:#x} (id {:#x})", HPET_PHYS_BASE, id);
        return;
    }

    let hpet = Hpet { base, period_fs };
    // Clear legacy-replacement routing, enable the free-running counter.
    let cfg = (hpet.read32(CFG) & !CFG_LEGACY_ROUTE) | CFG_ENABLE;
    hpet.write32(CFG, cfg);

    if HPET.set(hpet).is_ok() {
        let period_ns = period_fs as u64 / 1_000_000;
        crate::log::kinfo!(
            "hpet: id {:#x}, {} ns/ticks, revision {:#x}",
            id,
            period_ns,
            id & 0xFF
        );
    }
}

/// Main counter tick period in femtoseconds.
pub fn period_fs() -> u128 {
    HPET.get().map(|h| h.period_fs).unwrap_or(0)
}

/// Current free-running main counter value.
pub fn counter() -> u64 {
    HPET.get().map(|h| h.counter()).unwrap_or(0)
}

/// Monotonic nanoseconds since the HPET counter was enabled.
pub fn nanos() -> u64 {
    HPET.get().map(|h| h.nanos()).unwrap_or(0)
}