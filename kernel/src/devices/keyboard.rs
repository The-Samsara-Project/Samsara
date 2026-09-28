// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! PS/2 keyboard driver: scancode set 1 decoding.
//!
//! Handles make/break pairs, the `E0` escape prefix for extended keys,
//! Shift/Ctrl/Alt/CapsLock state tracking (with host-controlled LED
//! feedback), and delivers framed character events to `/dev/kbd0`.
//!
//! Event framing on the device node:
//! ```text
//! [0] = modifiers bitfield at press time
//! [1] = payload length n
//! [2..2+n] = payload bytes (ASCII or an escape sequence)
//! ```

use super::ps2::{self, DATA_PORT};
use super::queue::ByteQueue;
use alloc::vec::Vec;
use crate::io::{inb, outb};
use crate::sync::Spinlock;

/// Modifier bits stored in event headers.
pub mod mods {
    /// Either Shift key held.
    pub const SHIFT: u8 = 1 << 0;
    /// CapsLock latched on.
    pub const CAPS: u8 = 1 << 1;
    /// Either Ctrl key held.
    pub const CTRL: u8 = 1 << 2;
    /// Either Alt key held.
    pub const ALT: u8 = 1 << 3;
}

struct KbdState {
    queue: ByteQueue,
    modifiers: u8,
    extended_pending: bool,
    caps_led_on: bool,
}

static STATE: Spinlock<KbdState> = Spinlock::new(KbdState {
    queue: ByteQueue::new(),
    modifiers: 0,
    extended_pending: false,
    caps_led_on: false,
});

/// Set-1 make code → `(plain byte, shifted byte)`; linear over 0x02..=0x35.
const KEYMAP: &[(u8, u8)] = &[
    (b'1', b'!'), // 0x02
    (b'2', b'@'),
    (b'3', b'#'),
    (b'4', b'$'),
    (b'5', b'%'),
    (b'6', b'^'),
    (b'7', b'&'),
    (b'8', b'*'),
    (b'9', b'('),
    (b'0', b')'), // 0x0B
    (b'-', b'_'), // 0x0C
    (b'=', b'+'), // 0x0D
    // 0x0E backspace, 0x0F tab handled specially; gap in map below via skip
    (b'q', b'Q'), // 0x10
    (b'w', b'W'),
    (b'e', b'E'),
    (b'r', b'R'),
    (b't', b'T'),
    (b'y', b'Y'),
    (b'u', b'U'),
    (b'i', b'I'),
    (b'o', b'O'),
    (b'p', b'P'), // 0x19
    (b'[', b'{'), // 0x1A
    (b']', b'}'), // 0x1B
    // 0x1C enter, 0x1D ctrl special
    (b'a', b'A'), // 0x1E
    (b's', b'S'),
    (b'd', b'D'),
    (b'f', b'F'),
    (b'g', b'G'),
    (b'h', b'H'),
    (b'j', b'J'),
    (b'k', b'K'),
    (b'l', b'L'),   // 0x26
    (b';', b':'),   // 0x27
    (b'\'', b'"'),  // 0x28
    (b'`', b'~'),   // 0x29
    // 0x2A left shift special
    (b'\\', b'|'),  // 0x2B
    (b'z', b'Z'),   // 0x2C
    (b'x', b'X'),
    (b'c', b'C'),
    (b'v', b'V'),
    (b'b', b'B'),
    (b'n', b'N'),
    (b'm', b'M'), // 0x32
    (b',', b'<'), // 0x33
    (b'.', b'>'), // 0x34
    (b'/', b'?'), // 0x35
];

fn map_key(make: u8) -> Option<(u8, u8)> {
    match make {
        0x02..=0x0D => KEYMAP.get((make - 0x02) as usize).copied(),
        0x10..=0x1B => KEYMAP.get((make - 0x10 + 12) as usize).copied(),
        0x1E..=0x29 => KEYMAP.get((make - 0x1E + 24) as usize).copied(),
        0x2B => KEYMAP.get(36).copied(),
        0x2C..=0x35 => KEYMAP.get((make - 0x2C + 37) as usize).copied(),
        _ => None,
    }
}

fn sync_caps_led(on: bool) {
    // LED command: ED <led bits>; caps is bit 2.
    if ps2::device_command_first(0xED) {
        let mut guard = 50_000u32;
        while crate::io::inb(0x64) & 0x02 != 0 && guard > 0 {
            guard -= 1;
            core::hint::spin_loop();
        }
        outb(DATA_PORT, if on { 0x04 } else { 0x00 });
    }
}

fn handle_scancode(code: u8) {
    let mut s = STATE.lock();

    if code == 0xE0 {
        s.extended_pending = true;
        return;
    }
    let extended = core::mem::replace(&mut s.extended_pending, false);

    let is_break = code & 0x80 != 0;
    let make = code & 0x7F;

    // Modifier transitions ------------------------------------------------
    match make {
        0x2A | 0x36 if !extended => {
            if is_break {
                s.modifiers &= !mods::SHIFT;
            } else {
                s.modifiers |= mods::SHIFT;
            }
            return;
        }
        0x1D => {
            // Left Ctrl plain, right Ctrl as E0 1D — both toggle CTRL.
            if is_break {
                s.modifiers &= !mods::CTRL;
            } else {
                s.modifiers |= mods::CTRL;
            }
            return;
        }
        0x38 => {
            // Left Alt plain, right Alt (AltGr) as E0 38.
            if is_break {
                s.modifiers &= !mods::ALT;
            } else {
                s.modifiers |= mods::ALT;
            }
            return;
        }
        0x3A if !extended && !is_break => {
            // CapsLock latches on make only.
            s.caps_led_on = !s.caps_led_on;
            let led_on = s.caps_led_on;
            if led_on {
                s.modifiers |= mods::CAPS;
            } else {
                s.modifiers &= !mods::CAPS;
            }
            drop(s);
            sync_caps_led(led_on);
            return;
        }
        _ => {}
    }

    if is_break {
        return;
    }

    let mods_now = s.modifiers;
    let shift = mods_now & mods::SHIFT != 0;
    let caps = mods_now & mods::CAPS != 0;

    let payload: alloc::vec::Vec<u8> = match (extended, make) {
        (true, 0x48) => b"\x1b[A".to_vec(),  // Up
        (true, 0x50) => b"\x1b[B".to_vec(),  // Down
        (true, 0x4B) => b"\x1b[D".to_vec(),  // Left
        (true, 0x4D) => b"\x1b[C".to_vec(),  // Right
        (true, 0x53) => b"\x1b[3~".to_vec(), // Delete
        (true, 0x47) => b"\x1b[H".to_vec(),  // Home
        (true, 0x4F) => b"\x1b[F".to_vec(),  // End
        (true, 0x49) => b"\x1b[5~".to_vec(), // PageUp
        (true, 0x51) => b"\x1b[6~".to_vec(), // PageDown
        (true, 0x1C) => b"\n".to_vec(),      // Keypad Enter
        (false, 0x39) => b" ".to_vec(),      // Space
        (false, 0x1C) => b"\n".to_vec(),     // Enter
        (false, 0x0E) => b"\x08".to_vec(),   // Backspace
        (false, 0x0F) => b"\t".to_vec(),     // Tab
        (false, _) => match map_key(make) {
            Some((plain, shifted)) => {
                let mut c = if shift { shifted } else { plain };
                if caps && c.is_ascii_alphabetic() {
                    c = if shift {
                        c.to_ascii_lowercase()
                    } else {
                        c.to_ascii_uppercase()
                    };
                }
                alloc::vec![c]
            }
            None => return,
        },
        _ => return,
    };

    let mut event = Vec::with_capacity(payload.len() + 2);
    event.push(mods_now);
    event.push(payload.len() as u8);
    event.extend_from_slice(&payload);
    s.queue.push(&event);
}

/// IRQ1 handler invoked by the common interrupt router.
pub(crate) fn irq_handler() {
    // Only consume bytes that came from the first port.
    if ps2::status_has_output() && !ps2::status_is_aux() {
        let code = inb(DATA_PORT);
        handle_scancode(code);
    }
}

/// Polling fallback: drain any pending scancodes without an IRQ.
/// Called from the scheduler idle path so input works even if the
/// interrupt line is not connected (e.g., some VM configurations).
pub fn poll() {
    while ps2::status_has_output() && !ps2::status_is_aux() {
        let code = inb(DATA_PORT);
        handle_scancode(code);
    }
}

/// Hook IRQ1 and synchronize the CapsLock LED with initial state.
pub fn init(available: bool) {
    if !available {
        crate::log::kwarn!("kbd: no keyboard detected; driver idle");
        return;
    }
    crate::interrupts::idt::route_irq(1, irq_handler);
}

/// Remove up to `buf.len()` bytes of queued events, returning how many were
/// taken.
///
/// Used by `/dev/input/event0`, which converts the framed events into evdev
/// form. Draining under the queue's own lock is what makes the conversion
/// lossless: an event removed here has been accounted for, so there is no window
/// in which two readers could both claim it or neither could.
pub fn take_events(buf: &mut [u8]) -> usize {
    STATE.lock().queue.try_read(buf)
}

/// Whether any input is waiting to be converted.
pub fn has_pending() -> bool {
    STATE.lock().queue.has_data()
}

/// Device node implementation backing `/dev/kbd0`.
pub struct KbdDevice;

impl driver_common::CharDevice for KbdDevice {
    fn read(&self, buf: &mut [u8]) -> usize {
        STATE.lock().queue.try_read(buf)
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
        "ps2-set1-keyboard"
    }
}
