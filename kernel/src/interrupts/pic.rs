// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! Legacy 8259 Programmable Interrupt Controller pair.
//!
//! Samsara remaps the PICs to vectors `0x20..0x30`. The APIC path will take
//! over as the kernel grows; until then this is the sole IRQ source.

use crate::io::{io_wait, outb};

const MASTER_CMD: u16 = 0x20;
const MASTER_DATA: u16 = 0x21;
const SLAVE_CMD: u16 = 0xA0;
const SLAVE_DATA: u16 = 0xA1;

const ICW1_INIT: u8 = 0x11;
const ICW2_MASTER_VECTOR: u8 = 0x20;
const ICW2_SLAVE_VECTOR: u8 = 0x28;
const ICW3_MASTER: u8 = 0x04;
const ICW3_SLAVE: u8 = 0x02;
const ICW4_8086: u8 = 0x01;

const EOI_SIGNAL: u8 = 0x20;

/// Remap both PICs to their new vector bases and apply an IRQ mask.
///
/// `unmasked` lists IRQ lines that stay enabled; everything else is masked.
pub fn remap_and_mask(unmasked: &[u8]) {
    outb(MASTER_CMD, ICW1_INIT);
    io_wait();
    outb(SLAVE_CMD, ICW1_INIT);
    io_wait();
    outb(MASTER_DATA, ICW2_MASTER_VECTOR);
    outb(SLAVE_DATA, ICW2_SLAVE_VECTOR);
    outb(MASTER_DATA, ICW3_MASTER);
    outb(SLAVE_DATA, ICW3_SLAVE);
    outb(MASTER_DATA, ICW4_8086);
    outb(SLAVE_DATA, ICW4_8086);

    let mut mask_master: u8 = 0xFF;
    let mut mask_slave: u8 = 0xFF;
    for &irq in unmasked {
        if irq < 8 {
            mask_master &= !(1 << irq);
        } else if irq < 15 {
            mask_slave &= !(1 << (irq - 8));
        }
    }
    outb(MASTER_DATA, mask_master);
    outb(SLAVE_DATA, mask_slave);
    crate::log::kdebug!(
        "pic: remapped to 0x20/0x28, unmasked {:?}",
        unmasked
    );
}

/// Signal end-of-interrupt for the given IRQ line.
pub fn end_of_interrupt(irq: u8) {
    outb(MASTER_CMD, EOI_SIGNAL);
    if irq >= 8 {
        outb(SLAVE_CMD, EOI_SIGNAL);
    }
}

/// Mask every 8259 input. Used once the LAPIC takes over delivery; the PICs
/// stay programmed but the IO-APIC remaps their outputs to the local APIC.
pub fn mask_all() {
    outb(MASTER_DATA, 0xFF);
    outb(SLAVE_DATA, 0xFF);
}
