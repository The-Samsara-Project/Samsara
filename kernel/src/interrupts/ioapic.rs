// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! IO-APIC driver.
//!
//! The IO-APIC is probed at its architecturally fixed address, `0xFEC0_0000`
//! (chipsets may relocate it via the ACPI MADT; the fixed address is universal
//! in practice and is what QEMU and every post-Pentium chipset ships).
//!
//! Only the legacy 16-line identity map (GSI == ISA IRQ) is used: each
//! redirection entry delivers its vector to the boot processor's local APIC
//! in physical destination mode, so a fixed-delivery interrupt is releveled
//! by the LAPIC's EOI broadcast — no per-line IO-APIC acknowledge is needed.
//!
//! Every entry starts masked; the kernel unmasks a line when a handler binds
//! to it (see [`crate::interrupts::idt`]) so stray edges are never delivered.

use crate::memory;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Architecturally fixed physical base of the IO-APIC register block.
const IOAPIC_PHYS: usize = 0xFEC0_0000;
/// Bytes of register space used.
const REGION_SIZE: usize = 0x20;

/// Register-select indices (written to IOREGSEL before reading IOWIN).
const REG_IOAPICID: u32 = 0x00;
const REG_IOAPICVER: u32 = 0x01;

/// Byte offset of the I/O window register read/written after a select.
const IOWIN_OFFSET: usize = 0x10;

/// First redirection entry index; entry `gsi` occupies indices `0x10 + 2*gsi`
/// (low dword) and `0x10 + 2*gsi + 1` (high dword).
const REDIR_BASE: u32 = 0x10;

/// Redirection low dword: interrupt mask bit.
const REDIR_MASKED: u32 = 1 << 16;

/// First vector handed out; shares the IDT's `0x20..0x30` IRQ window.
const VECTOR_BASE: u32 = 0x20;

/// Mapped virtual base of the IO-APIC register window.
static IOAPIC_VIRT: AtomicUsize = AtomicUsize::new(0);

/// True once the IO-APIC exists, is mapped and all entries are masked.
static ONLINE: AtomicBool = AtomicBool::new(false);

/// Highest GSI that has a redirection entry (`IOAPICVER` max-redir field).
static MAX_GSI: AtomicUsize = AtomicUsize::new(15);

/// True when the IO-APIC is driving delivery.
pub fn online() -> bool {
    ONLINE.load(Ordering::Relaxed)
}

fn base() -> *mut u32 {
    IOAPIC_VIRT.load(Ordering::Relaxed) as *mut u32
}

/// Program the register-select index ahead of an IOWIN access.
fn select(index: u32) {
    // SAFETY: `base` aliases the mapped, uncached IO-APIC window.
    unsafe { ptr::write_volatile(base(), index) }
}

fn reg(index: u32) -> u32 {
    select(index);
    // SAFETY: see `select`.
    unsafe { ptr::read_volatile(base().add(IOWIN_OFFSET / 4)) }
}

fn set_reg(index: u32, value: u32) {
    select(index);
    // SAFETY: see `select`.
    unsafe { ptr::write_volatile(base().add(IOWIN_OFFSET / 4), value) }
}

fn redir_low(gsi: u32) -> u32 {
    reg(REDIR_BASE + 2 * gsi)
}

fn set_redir_low(gsi: u32, value: u32) {
    set_reg(REDIR_BASE + 2 * gsi, value)
}

fn set_redir_high(gsi: u32, value: u32) {
    set_reg(REDIR_BASE + 2 * gsi + 1, value)
}

/// Diagnostics for the `#GP` panic handler: every redirection entry, so a
/// stray vector (e.g. the 0xFA that occasionally precedes a boot-time
/// `#GP`) can be traced to a specific line and mask state.
pub(crate) fn diag_dump_redirs() {
    if !ONLINE.load(Ordering::Relaxed) {
        crate::log::kwarn!("ioapic: not online, skipping redir dump");
        return;
    }
    let max = MAX_GSI.load(Ordering::Relaxed);
    for gsi in 0..=max as u32 {
        crate::log::kwarn!("ioapic redir[{}] = {:#010x}", gsi, redir_low(gsi));
    }
}

/// Physical delivery target for every entry: the boot processor's LAPIC.
fn dest_id() -> u32 {
    (super::apic::local_id() as u32) << 24
}

/// Probe, map and mask the whole IO-APIC. Skipped entirely when the local
/// APIC never came online (delivery requires one).
pub fn init(lapic_ok: bool) {
    if !lapic_ok || ONLINE.load(Ordering::Relaxed) {
        return;
    }
    let virt = memory::map_device_mmio(IOAPIC_PHYS, REGION_SIZE);
    IOAPIC_VIRT.store(virt, Ordering::Release);

    let ver = reg(REG_IOAPICVER);
    let max_gsi = ((ver >> 16) & 0xFF) as usize;
    if ver == 0 || ver == 0xFFFF_FFFF || max_gsi == 0xFF {
        crate::log::kdebug!("ioapic: no controller at {:#x} (ver {:#x})", IOAPIC_PHYS, ver);
        return;
    }
    MAX_GSI.store(max_gsi, Ordering::Relaxed);

    let id = (reg(REG_IOAPICID) >> 24) & 0x0F;
    let dest = dest_id();
    for gsi in 0..=max_gsi as u32 {
        // Fixed delivery mode (000), edge-triggered, active-high, target the
        // BSP LAPIC in physical destination mode. Everything starts masked.
        set_redir_high(gsi, dest);
        set_redir_low(gsi, VECTOR_BASE + gsi | REDIR_MASKED);
    }

    ONLINE.store(true, Ordering::Release);
    crate::log::kinfo!(
        "ioapic: at phys {:#x}, id {:#x}, {} redirections",
        IOAPIC_PHYS,
        id,
        max_gsi + 1
    );
}

/// Mask the redirection entry for a legacy IRQ line (no-op if offline).
pub fn mask(irq: u8) {
    if !ONLINE.load(Ordering::Relaxed) {
        return;
    }
    let gsi = irq as usize;
    if gsi > MAX_GSI.load(Ordering::Relaxed) {
        return;
    }
    let low = redir_low(gsi as u32);
    set_redir_low(gsi as u32, low | REDIR_MASKED);
}

/// Unmask the redirection entry for a legacy IRQ line (no-op if offline).
pub fn unmask(irq: u8) {
    if !ONLINE.load(Ordering::Relaxed) {
        return;
    }
    let gsi = irq as usize;
    if gsi > MAX_GSI.load(Ordering::Relaxed) {
        return;
    }
    let low = redir_low(gsi as u32);
    set_redir_low(gsi as u32, low & !REDIR_MASKED);
}