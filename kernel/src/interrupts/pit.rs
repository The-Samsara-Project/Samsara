// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! Intel 8253/8254 Programmable Interval Timer driver (channel 0).

use crate::io::{outb, io_wait};

const CHANNEL0_DATA: u16 = 0x40;
const MODE_COMMAND: u16 = 0x43;
const BASE_FREQUENCY_HZ: u32 = 1_193_182;

/// Program channel 0 to interrupt at `frequency` Hz.
pub fn init(frequency: u64) {
    let divisor = ((BASE_FREQUENCY_HZ as u64 / frequency.max(19)) as u16).max(1);
    outb(MODE_COMMAND, 0x36); // channel 0, lobyte/hibyte, rate generator
    io_wait();
    outb(CHANNEL0_DATA, (divisor & 0xFF) as u8);
    io_wait();
    outb(CHANNEL0_DATA, (divisor >> 8) as u8);
    crate::log::kdebug!("pit: {} Hz (divisor {})", frequency, divisor);
}
