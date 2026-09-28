// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! PS/2 mouse driver (auxiliary channel).
//!
//! Configures the device for streaming mode and decodes standard 3-byte
//! packets into framed events on `/dev/mouse0`:
//!
//! ```text
//! [0] buttons bitfield (bit0 left, bit1 right, bit2 middle)
//! [1..3] signed X delta (little-endian i16)
//! [3..5] signed Y delta (little-endian i16)
//! ```
//!
//! Packets whose overflow bits are set are dropped rather than propagated
//! with corrupted deltas.

use super::ps2::{self, DATA_PORT};
use super::queue::ByteQueue;
use crate::io::{inb, outb};
use crate::sync::Spinlock;

struct MouseState {
    queue: ByteQueue,
    /// Packet assembly buffer.
    pkt: [u8; 3],
    /// Bytes collected so far for the current packet.
    pkt_len: usize,
    /// Current button state mirrored for absolute queries.
    buttons: u8,
}

static STATE: Spinlock<MouseState> = Spinlock::new(MouseState {
    queue: ByteQueue::new(),
    pkt: [0; 3],
    pkt_len: 0,
    buttons: 0,
});

fn wait_write() {
    let mut guard = 50_000u32;
    while crate::io::inb(0x64) & 0x02 != 0 && guard > 0 {
        guard -= 1;
        core::hint::spin_loop();
    }
}

/// Enable the aux channel, reset to defaults and start data reporting.
pub fn init() -> bool {
    // Enable second port clock line.
    crate::devices::ps2::enable_second_port();

    // Reset the device and wait for completion (0xAA) + id (0x00).
    if !ps2::device_command_second(0xFF) {
        crate::log::kdebug!("mouse: no ACK to reset");
        return false;
    }
    // Consume BAT result + device ID.
    let _ = ps2::read_data();
    let _ = ps2::read_data();

    if !ps2::device_command_second(0xF6) {
        // Set defaults.
        return false;
    }
    // Sample rate 40 Hz for predictable stream pacing.
    if ps2::device_command_second(0xF3) {
        wait_write();
        outb(DATA_PORT, 40);
    }

    // Start streaming packets.
    if !ps2::device_command_second(0xF4) {
        return false;
    }

    crate::interrupts::idt::route_irq(12, irq_handler);
    crate::log::kdebug!("mouse: streaming enabled, IRQ12 routed");
    true
}

fn push_event(buttons: u8, dx: i16, dy: i16) {
    let mut ev = [0u8; 5];
    ev[0] = buttons;
    ev[1..3].copy_from_slice(&dx.to_le_bytes());
    ev[3..5].copy_from_slice(&dy.to_le_bytes());
    STATE.lock().queue.push(&ev);
}

/// Polling fallback draining any pending aux packets.
pub fn poll() {
    while ps2::status_has_output() && ps2::status_is_aux() {
        let b = inb(DATA_PORT);
        feed(b);
    }
}

/// IRQ12 handler invoked by the common interrupt router.
pub(crate) fn irq_handler() {
    while ps2::status_has_output() && ps2::status_is_aux() {
        let b = inb(DATA_PORT);
        feed(b);
    }
}

fn feed(b: u8) {
    let mut s = STATE.lock();

    // Byte 0 must have bit 3 set; resynchronize otherwise.
    if s.pkt_len == 0 && b & 0x08 == 0 {
        return;
    }
    let idx = s.pkt_len;
    s.pkt[idx] = b;
    s.pkt_len = idx + 1;

    if s.pkt_len < 3 {
        return;
    }
    let pkt = s.pkt;
    s.pkt_len = 0;

    let overflow = pkt[0] & 0xC0 != 0;
    s.buttons = pkt[0] & 0x07;
    let buttons = s.buttons;
    drop(s);

    if overflow {
        return;
    }
    // Sign-extend 9-bit two's complement values.
    let dx = sign_extend(pkt[1], pkt[0] & 0x10 != 0);
    let dy = sign_extend(pkt[2], pkt[0] & 0x20 != 0);
    push_event(buttons, dx, -dy);
}

fn sign_extend(value: u8, negative: bool) -> i16 {
    if negative {
        (value as u16 | 0xFF00) as i16
    } else {
        value as i16
    }
}

/// Device node implementation backing `/dev/mouse0`.
pub struct MouseDevice;

/// Remove up to `buf.len()` bytes of queued events, returning how many were
/// taken. See [`crate::devices::keyboard::take_events`] for why draining under
/// the queue's lock is what makes the conversion lossless.
pub fn take_events(buf: &mut [u8]) -> usize {
    STATE.lock().queue.try_read(buf)
}

/// Whether any input is waiting to be converted.
pub fn has_pending() -> bool {
    STATE.lock().queue.has_data()
}

impl driver_common::CharDevice for MouseDevice {
    fn read(&self, buf: &mut [u8]) -> usize {
        // Return whole events only.
        let whole = buf.len() - (buf.len() % 5);
        if whole == 0 {
            return 0;
        }
        STATE.lock().queue.try_read(&mut buf[..whole])
    }

    fn write(&self, _buf: &[u8]) -> usize {
        0
    }

    fn has_data(&self) -> bool {
        STATE.lock().queue.has_data()
    }

    fn park(&self, task: usize, interest: u16) -> bool {
        let _ = task;
        STATE.lock().queue.poll_park()
    }

    fn unpark(&self, task: usize) {
        let _ = task;
        STATE.lock().queue.poll_cancel();
    }

    fn name(&self) -> &'static str {
        "ps2-mouse"
    }
}
