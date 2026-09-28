// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! 16550 UART driver for the COM1 debug serial port (0x3F8).

use super::{inb, io_wait, outb};

const COM1: u16 = 0x3F8;

const DATA: u16 = 0;
const INT_ENABLE: u16 = 1;
const FIFO_CTRL: u16 = 2;
const LINE_CTRL: u16 = 3;
const MODEM_CTRL: u16 = 4;
const LINE_STATUS: u16 = 5;

const LINE_STATUS_TX_EMPTY: u8 = 0x20;

static INITIALIZED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Initialize COM1 at 115200 baud, 8N1.
pub fn init() {
    outb(COM1 + INT_ENABLE, 0x00); // disable interrupts
    outb(COM1 + LINE_CTRL, 0x80); // DLAB on
    outb(COM1 + DATA, 0x01); // divisor low: 1 => 115200 baud
    outb(COM1 + INT_ENABLE, 0x00); // divisor high
    outb(COM1 + LINE_CTRL, 0x03); // 8 bits, no parity, 1 stop
    outb(COM1 + FIFO_CTRL, 0xC7); // enable + clear FIFOs, 14-byte threshold
    outb(COM1 + MODEM_CTRL, 0x0B); // DTR | RTS | OUT2
    io_wait();
    INITIALIZED.store(true, core::sync::atomic::Ordering::Release);
}

/// Transmit a single byte, blocking until the UART can accept it.
pub fn send(byte: u8) {
    if !INITIALIZED.load(core::sync::atomic::Ordering::Acquire) {
        return;
    }
    while inb(COM1 + LINE_STATUS) & LINE_STATUS_TX_EMPTY == 0 {
        core::hint::spin_loop();
    }
    outb(COM1 + DATA, byte);
}

/// Transmit a byte slice, translating `\n` into `\r\n`.
pub fn write(bytes: &[u8]) {
    for &b in bytes {
        if b == b'\n' {
            send(b'\r');
        }
        send(b);
    }
}
