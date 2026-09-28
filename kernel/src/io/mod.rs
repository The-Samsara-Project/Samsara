// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! Raw x86 I/O port access and small device drivers.

pub mod uart;
pub mod pci;
pub mod bochs;
pub mod hpet;
pub mod tsc;

use core::arch::asm;

/// Write one byte to an I/O port.
///
/// Deliberately *not* `nomem`. A port write is an observable side effect on
/// hardware, and declaring "touches no memory" tells the compiler it may be
/// reordered freely against other port accesses. For most devices that is
/// harmless, but for the CMOS the index and data ports form a two-step
/// transaction: writing the index and then reading the data is only correct if
/// nothing slips between them, and with `nomem` the compiler is entitled to
/// interleave another register's access. That failure is silent -- the clock
/// does not report nonsense, it reports a stale value forever. Leaving the
/// memory clobber in place costs nothing (these are single instructions with no
/// memory operands) and makes consecutive port accesses a correctly ordered
/// sequence.
#[inline(always)]
pub fn outb(port: u16, value: u8) {
    unsafe { asm!("out dx, al", in("dx") port, in("al") value, options(nostack)) }
}

/// Read one byte from an I/O port. See [`outb`] for why there is no `nomem`.
#[inline(always)]
pub fn inb(port: u16) -> u8 {
    unsafe {
        let value: u8;
        asm!("in al, dx", out("al") value, in("dx") port, options(nostack));
        value
    }
}

/// Write one 16-bit word to an I/O port. See [`outb`] for why there is no
/// `nomem`.
#[inline(always)]
pub fn outw(port: u16, value: u16) {
    unsafe { asm!("out dx, ax", in("dx") port, in("ax") value, options(nostack)) }
}

/// Read one 16-bit word from an I/O port. See [`outb`] for why there is no
/// `nomem`.
#[inline(always)]
pub fn inw(port: u16) -> u16 {
    unsafe {
        let value: u16;
        asm!("in ax, dx", out("ax") value, in("dx") port, options(nostack));
        value
    }
}

/// Write one 32-bit word to an I/O port.
#[inline(always)]
pub fn outl(port: u16, value: u32) {
    unsafe { asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack)) }
}

/// Read one 32-bit word from an I/O port.
#[inline(always)]
pub fn inl(port: u16) -> u32 {
    unsafe {
        let value: u32;
        asm!("in eax, dx", out("eax") value, in("dx") port, options(nomem, nostack));
        value
    }
}

/// Brief busy-wait used by legacy devices between port accesses.
#[inline(always)]
pub fn io_wait() {
    outb(0x80, 0);
}
