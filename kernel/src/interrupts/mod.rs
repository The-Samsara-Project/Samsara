// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! CPU structure bring-up: GDT, TSS, IDT, interrupt controllers and timer.

pub mod gdt;
pub mod idt;
pub mod pic;
pub mod pit;
pub mod apic;
pub mod ioapic;

use crate::time;

/// Bring up the complete interrupt and descriptor-table infrastructure.
///
/// Order matters: the HPET must be up before the TSC is calibrated, and the
/// TSC before the LAPIC timer can measure its bus frequency. With the local
/// APIC online the LAPIC timer drives the scheduler tick and the IO-APIC
/// forwards device edges; otherwise the kernel falls back to the legacy PIC
/// + PIT path.
pub fn init() {
    gdt::init();
    idt::init();
    crate::io::hpet::init();
    crate::log::kdebug!("int: hpet done");
    crate::io::tsc::init();
    crate::log::kdebug!("int: tsc done");
    let ok = apic::init();
    crate::log::kdebug!("int: apic done ok={}", ok);
    ioapic::init(ok);
    if ok {
        pic::mask_all();
    } else {
        // Timer, keyboard and mouse are the phase-current IRQ consumers.
        pic::remap_and_mask(&[0, 1, 12]);
        pit::init(time::TIMER_HZ);
    }
    crate::log::kdebug!(
        "int: tss@{:#x} ist0={:#x}",
        gdt::tss_addr() as *const _ as usize,
        gdt::current_ist0()
    );
    unsafe {
        core::arch::asm!("sti", options(nomem, nostack));
    }
    crate::log::kdebug!("interrupts: online (apic={})", apic::online());
}

/// Invoke the periodic timer bookkeeping from the IRQ0 handler.
pub(crate) fn on_timer_irq() {
    time::tick();
    crate::task::sched::on_timer_tick();
}

/// Acknowledge an interrupt. With the LAPIC online, its EOI broadcast
/// relevels every fixed-delivery IO-APIC line; otherwise the 8259 pair is
/// signalled directly for the offending IRQ.
pub(crate) fn eoi(irq: u8) {
    if apic::online() {
        apic::end_of_interrupt();
    } else {
        pic::end_of_interrupt(irq);
    }
}
