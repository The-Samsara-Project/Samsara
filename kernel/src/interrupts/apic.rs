// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! Local APIC driver.
//!
//! Brings the local APIC up out of its power-on "inactive" state, software
//! enables it, masks every LVT source, calibrates the timer bus frequency
//! against the (already calibrated) TSC, and then programs the LAPIC timer as
//! the kernel's periodic tick source at [`crate::time::TIMER_HZ`].
//!
//! Once `online()`, all interrupt acknowledgement flows through the LAPIC's
//! EOI register: fixed-delivery IOAPIC redirections are releveled by the
//! LAPIC EOI broadcast, so device EOI is a single register write regardless
//! of which line fired.

use crate::memory;
use crate::time;
use crate::io::tsc;
use core::arch::asm;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

// --- IA32_APIC_BASE MSR ---------------------------------------------------

const MSR_APIC_BASE: u32 = 0x1B;
const APIC_ENABLE: u64 = 1 << 11;
const APIC_X2APIC: u64 = 1 << 10;
const APIC_BASE_MASK: u64 = 0x0000_000F_FFFF_F000;

// --- Local APIC register offsets ------------------------------------------

/// Local APIC ID register.
const REG_ID: usize = 0x20;
/// Local APIC version register (`VER`).
const REG_VER: usize = 0x30;
/// Task priority register.
const REG_TPR: usize = 0x80;
/// End-of-interrupt register.
const REG_EOI: usize = 0xB0;
/// Logical destination register.
const REG_LDR: usize = 0xD0;
/// Destination format register.
const REG_DFR: usize = 0xE0;
/// Spurious interrupt vector register (`SVR`).
const REG_SVR: usize = 0xF0;
/// Error status register.
const REG_ESR: usize = 0x280;
/// Interrupt command register (low/high).
const REG_ICR_LO: usize = 0x300;
const REG_ICR_HI: usize = 0x310;
/// LVT entries.
const REG_LVT_CMCI: usize = 0x2F0;
const REG_LVT_TIMER: usize = 0x320;
const REG_LVT_THERMAL: usize = 0x330;
const REG_LVT_PERF: usize = 0x340;
const REG_LVT_LINT0: usize = 0x350;
const REG_LVT_LINT1: usize = 0x360;
const REG_LVT_ERROR: usize = 0x370;
/// Timer registers.
const REG_TIMER_ICR: usize = 0x380;
const REG_TIMER_CCR: usize = 0x390;
const REG_TIMER_DCR: usize = 0x3E0;

/// Spurious-interrupt vector installed in `SVR`; the handler at IDT `0xFF`.
const SPURIOUS_VECTOR: u32 = 0xFF;
/// LAPIC error vector; the handler at IDT `0xFE`.
const ERROR_VECTOR: u32 = 0xFE;
/// Software-enable plus focus-processor-check disable bits for `SVR`.
const SVR_ENABLE: u32 = 1 << 8;

/// LVT mask bit.
const LVT_MASKED: u32 = 1 << 16;
/// LVT timer mode: periodic.
const LVT_TIMER_PERIODIC: u32 = 1 << 17;

/// Timer divide-configurations: value -> actual divider.
const DCR_DIV1: u32 = 0xB; //  1
const DCR_DIV16: u32 = 0x3; // 16

/// True once the LAPIC is enabled and driving EOI + the scheduler tick.
static ONLINE: AtomicBool = AtomicBool::new(false);

/// Mapped virtual base of the LAPIC register window.
static APIC_VIRT: AtomicUsize = AtomicUsize::new(0);

/// Calibrated LAPIC timer bus frequency in Hz.
static TIMER_BUS_HZ: AtomicUsize = AtomicUsize::new(0);

/// True once the LAPIC is driving interrupts.
pub fn online() -> bool {
    ONLINE.load(Ordering::Relaxed)
}

fn base() -> *mut u32 {
    APIC_VIRT.load(Ordering::Relaxed) as *mut u32
}

fn read(offset: usize) -> u32 {
    // SAFETY: `base` aliases the mapped, uncached LAPIC window.
    unsafe { ptr::read_volatile(base().add(offset / 4)) }
}

fn write(offset: usize, value: u32) {
    // SAFETY: see `read`.
    unsafe { ptr::write_volatile(base().add(offset / 4), value) }
}

/// `cpuid(leaf)` -> (eax, ebx, ecx, edx).
///
/// `rbx` is reserved by LLVM, so the `ebx` result is shuttled through a
/// scratch register around the call.
fn cpuid(leaf: u32) -> (u32, u32, u32, u32) {
    let (mut eax, mut ebx, mut ecx, mut edx): (u32, u32, u32, u32) = (0, 0, 0, 0);
    // SAFETY: cpuid is side-effect free apart from its outputs.
    unsafe {
        asm!(
            "xchg rbx, {tmp}",
            "cpuid",
            "xchg rbx, {tmp}",
            tmp = out(reg) ebx,
            in("eax") leaf,
            lateout("eax") eax,
            lateout("ecx") ecx,
            lateout("edx") edx,
            options(nostack)
        );
    }
    (eax, ebx, ecx, edx)
}

fn rdmsr(msr: u32) -> u64 {
    let lo: u32;
    let hi: u32;
    // SAFETY: rdmsr reads the requested model-specific register.
    unsafe {
        asm!("rdmsr", in("ecx") msr, out("eax") lo, out("edx") hi, options(nostack));
    }
    ((hi as u64) << 32) | lo as u64
}

fn wrmsr(msr: u32, value: u64) {
    // SAFETY: wrmsr writes the requested model-specific register.
    unsafe {
        asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") value as u32,
            in("edx") (value >> 32) as u32,
            options(nostack)
        );
    }
}

/// Detect and enable the local APIC, returning `false` when none exists.
///
/// Performs no calibration; call [`calibrate_and_start_timer`] afterwards to
/// arm the scheduler tick.
fn enable_lapic() -> bool {
    // CPUID.1:EDX bit 9 reports an on-chip APIC.
    let (_, _, _, edx) = cpuid(1);
    if edx & (1 << 9) == 0 {
        crate::log::kdebug!("apic: no local APIC reported by CPUID");
        return false;
    }

    let mut msr = rdmsr(MSR_APIC_BASE);
    if msr & APIC_ENABLE == 0 {
        wrmsr(MSR_APIC_BASE, msr | APIC_ENABLE);
        msr = rdmsr(MSR_APIC_BASE);
        if msr & APIC_ENABLE == 0 {
            crate::log::kdebug!("apic: failed to enable local APIC");
            return false;
        }
    }
    // Leave x2APIC mode disabled: Samsara uses the MMIO xAPIC interface.
    if msr & APIC_X2APIC != 0 {
        wrmsr(MSR_APIC_BASE, msr & !APIC_X2APIC);
        if rdmsr(MSR_APIC_BASE) & APIC_X2APIC != 0 {
            crate::log::kdebug!("apic: x2APIC locked on; MMIO path unavailable");
            return false;
        }
    }

    let phys = (msr & APIC_BASE_MASK) as usize;
    if phys == 0 {
        crate::log::kdebug!("apic: bogus APIC base {:#x}", phys);
        return false;
    }
    let virt = memory::map_device_mmio(phys, 0x1000);
    APIC_VIRT.store(virt, Ordering::Release);

    // A real APIC must answer with a sane version register.
    let ver = read(REG_VER);
    if ver == 0 || ver == 0xFFFF_FFFF {
        crate::log::kdebug!("apic: bad version register {:#x}", ver);
        return false;
    }

    // Software-enable with the spurious vector installed.
    write(REG_SVR, SPURIOUS_VECTOR | SVR_ENABLE);
    // Mask every LVT source until the kernel explicitly arms them.
    write(REG_LVT_CMCI, LVT_MASKED);
    write(REG_LVT_TIMER, LVT_MASKED);
    write(REG_LVT_THERMAL, LVT_MASKED);
    write(REG_LVT_PERF, LVT_MASKED);
    write(REG_LVT_LINT0, LVT_MASKED);
    write(REG_LVT_LINT1, LVT_MASKED);
    write(REG_LVT_ERROR, LVT_MASKED | ERROR_VECTOR);
    // Flat logical-destination model (not used by the BSP, kept canonical).
    write(REG_DFR, 0xFFFF_FFFF);
    write(REG_LDR, 0);

    // Clear any stale error status.
    write(REG_ESR, 0);
    let _ = read(REG_ESR);

    crate::log::kinfo!(
        "apic: local APIC at phys {:#x}, id {}, version {:#x}",
        phys,
        read(REG_ID) >> 24,
        ver & 0xFF
    );
    true
}

/// Measure the LAPIC timer bus frequency in Hz using the calibrated TSC.
///
/// Runs the timer in a masked one-shot from a full initial count and times an
/// observed count decrement against TSC cycles.
fn calibrate_bus_hz() -> Option<u64> {
    let tsc_hz = tsc::hz();
    if tsc_hz == 0 {
        return None;
    }
    write(REG_TIMER_DCR, DCR_DIV1);
    write(REG_LVT_TIMER, LVT_MASKED); // one-shot, silent

    let mut best: u128 = 0;
    for _ in 0..2 {
        write(REG_TIMER_ICR, 0xFFFF_FFFF);
        let t0 = tsc::rdtsc();
        let target = tsc_hz / 500; // ~2 ms window
        loop {
            if tsc::rdtsc() - t0 >= target {
                break;
            }
        }
        let t1 = tsc::rdtsc();
        let cc = read(REG_TIMER_CCR) as u64;
        let elapsed = (0xFFFF_FFFFu64 - cc) & 0xFFFF_FFFF;
        let delta = t1 - t0;
        if elapsed == 0 || delta == 0 {
            continue;
        }
        let hz = elapsed as u128 * tsc_hz as u128 / delta as u128;
        if hz > best {
            best = hz;
        }
    }
    if best == 0 {
        return None;
    }
    crate::log::kinfo!("apic: timer bus calibrated at {} Hz", best);
    Some(best as u64)
}

/// Full bring-up. Returns `true` when the LAPIC is software-enabled, its
/// timer is calibrated and the periodic scheduler tick is armed.
pub fn init() -> bool {
    if ONLINE.load(Ordering::Relaxed) {
        return true;
    }
    if !enable_lapic() {
        return false;
    }

    let bus_hz = match calibrate_bus_hz() {
        Some(h) => h,
        None => {
            crate::log::kwarn!("apic: timer calibration failed; disabling LAPIC");
            write(REG_SVR, 0); // return to inactive state
            ONLINE.store(false, Ordering::Relaxed);
            return false;
        }
    };
    TIMER_BUS_HZ.store(bus_hz as usize, Ordering::Relaxed);

    // Arm the periodic scheduler tick. Keep the initial count within 32 bits
    // by picking a larger divider if a 1 count/second divisor would overflow.
    let mut dcr = DCR_DIV1;
    let mut count = bus_hz / time::TIMER_HZ;
    if count > 0xFFFF_FFFF {
        dcr = DCR_DIV16;
        count /= 16;
    }
    write(REG_TIMER_DCR, dcr);
    // Periodic, vector 0x20 (the kernel's timer IRQ), unmasked.
    write(REG_LVT_TIMER, 0x20 | LVT_TIMER_PERIODIC);
    write(REG_TIMER_ICR, count as u32);

    ONLINE.store(true, Ordering::Release);
    crate::log::kinfo!(
        "apic: periodic timer armed ({} Hz, {}/tick)",
        time::TIMER_HZ,
        count
    );
    true
}

/// Send end-of-interrupt to the local APIC.
pub fn end_of_interrupt() {
    write(REG_EOI, 0);
}

/// Clear + log the LAPIC error status (called from the error vector handler).
pub fn clear_error() {
    write(REG_ESR, 0);
    let _ = read(REG_ESR);
    // The error status may have masked the LVT entry; re-arm it.
    write(REG_LVT_ERROR, LVT_MASKED | ERROR_VECTOR);
}

/// Local APIC ID of the boot processor, the physical delivery target for the
/// IO-APIC's redirection entries.
pub fn local_id() -> u8 {
    (read(REG_ID) >> 24) as u8
}

/// Diagnostics for the `#GP` panic handler: the LAPIC IRR/ISR bitmaps
/// (pending / in-service vectors), plus SVR, TPR and the error register.
/// A stray interrupt with no IDT gate shows up here as a pending vector
/// bit the moment the CPU tried to deliver it.
pub(crate) fn diag_dump_vectors() {
    if !ONLINE.load(Ordering::Relaxed) {
        crate::log::kwarn!("lapic: not online, skipping vector dump");
        return;
    }
    for n in 0..8usize {
        let irr = read(0x200 + n * 0x10);
        let isr = read(0x100 + n * 0x10);
        if irr != 0 || isr != 0 {
            crate::log::kwarn!("lapic[{}]: IRR={:#010x} ISR={:#010x}", n, irr, isr);
        }
    }
    crate::log::kwarn!(
        "lapic SVR={:#010x} TPR={:#010x} ESR={:#010x}",
        read(REG_SVR),
        read(REG_TPR),
        read(REG_ESR)
    );
}

/// Calibrated LAPIC timer bus frequency, for diagnostics.
pub fn timer_bus_hz() -> u64 {
    TIMER_BUS_HZ.load(Ordering::Relaxed) as u64
}