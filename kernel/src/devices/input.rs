// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! `/dev/input/event0`: one evdev-style stream for keyboard and mouse.
//!
//! The PS/2 drivers already decode the hardware into events, but each queues
//! its own framing: `/dev/kbd0` holds `[modifiers, len, bytes...]` and
//! `/dev/mouse0` holds `[buttons, dx_lo, dx_hi, dy_lo, dy_hi]`. Those are right
//! for the programs written against them and wrong for everything else. A
//! ported terminal opens `/dev/input/event0` and reads Linux `input_event`
//! structures, because that is the only input interface it knows how to read.
//!
//! So this device sits in front of the two queues and translates. It is a
//! translation layer, not a second input path: the PS/2 drivers remain the only
//! code that touches the hardware. Events are converted on the way out of the
//! driver's queue rather than pushed into a second one, so there is no
//! intermediate buffer to overflow and no window in which an event can be lost
//! between the interrupt and the read that consumes it.
//!
//! Event types and codes are Linux's own values, which is the entire reason this
//! device is portable: a program that includes `linux/input-event-codes.h`
//! computes the same numbers.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use driver_common::CharDevice;
use crate::sync::Spinlock;

/// Size of one `struct input_event`: two `__kernel_ulong_t` (the timestamp the
/// kernel fills in) and two `__u16`, padded to eight-byte alignment.
const INPUT_EVENT_SIZE: usize = 24;

// --- linux/input-event-codes.h ---------------------------------------------

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_REL: u16 = 0x02;

const SYN_REPORT: u16 = 0;

const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;

/// The subset of `KEY_*` this device can emit, with Linux's values.
///
/// The codes are not contiguous and not alphabetical, so they are spelled out
/// rather than computed. Getting one wrong means a key does the wrong thing in
/// every program, which is why they are named constants and not a table of
/// numbers read off a header.
mod key {
    pub const ESC: u16 = 1;
    pub const BACKSPACE: u16 = 14;
    pub const TAB: u16 = 15;
    pub const ENTER: u16 = 28;
    pub const LEFTCTRL: u16 = 29;
    pub const LEFTSHIFT: u16 = 42;
    pub const LEFTALT: u16 = 56;
    pub const CAPSLOCK: u16 = 58;
    pub const SPACE: u16 = 57;
    pub const F1: u16 = 59;
    pub const RIGHTSHIFT: u16 = 54;
    pub const RIGHTCTRL: u16 = 97;
    pub const RIGHTALT: u16 = 100;
    pub const HOME: u16 = 102;
    pub const UP: u16 = 103;
    pub const PAGEUP: u16 = 104;
    pub const LEFT: u16 = 105;
    pub const RIGHT: u16 = 106;
    pub const DOWN: u16 = 108;
    pub const END: u16 = 107;
    pub const PAGEDOWN: u16 = 109;
    pub const INSERT: u16 = 110;
    pub const DELETE: u16 = 111;
    pub const KPENTER: u16 = 96;
    pub const BTN_LEFT: u16 = 0x110;
    pub const BTN_RIGHT: u16 = 0x111;
    pub const BTN_MIDDLE: u16 = 0x112;
}

/// The printable-ASCII block of `KEY_*`, which is where the low codes live.
///
/// evdev numbers the top row `KEY_1`..`KEY_0` and the letter block starts 29
/// keys after `KEY_1`, so both are computed from one anchor. Spelling the whole
/// block out would be 40 lines of constants that all have to agree with each
/// other anyway.
const KEY_1: u16 = 2;
const KEY_A: u16 = 30;
const KEY_MINUS: u16 = 12;
const KEY_EQUAL: u16 = 13;
const KEY_SEMICOLON: u16 = 39;
const KEY_APOSTROPHE: u16 = 40;
const KEY_GRAVE: u16 = 41;
const KEY_COMMA: u16 = 51;
const KEY_DOT: u16 = 52;
const KEY_SLASH: u16 = 53;
const KEY_LEFTBRACE: u16 = 26;
const KEY_RIGHTBRACE: u16 = 27;
const KEY_BACKSLASH: u16 = 43;

/// Modifier bits, matching the keyboard driver's own encoding so its framed
/// events need no second translation table.
const MOD_SHIFT: u8 = 1 << 0;
const MOD_CTRL: u8 = 1 << 2;
const MOD_ALT: u8 = 1 << 3;

struct State {
    /// Converted events waiting to be read, each already a full
    /// `INPUT_EVENT_SIZE` bytes.
    outbox: VecDeque<[u8; INPUT_EVENT_SIZE]>,
    /// Task parked in `read`, woken when the outbox becomes non-empty.
    waiter: Option<crate::task::TaskId>,
    /// Last modifier state reported, so edges are only sent on a change.
    mods: u8,
}

static STATE: Spinlock<State> = Spinlock::new(State {
    outbox: VecDeque::new(),
    waiter: None,
    mods: 0,
});

/// Queue one converted event, waking a parked reader.
fn emit(kind: u16, code: u16, value: i32) {
    let mut s = STATE.lock();
    let mut ev = [0u8; INPUT_EVENT_SIZE];
    // The timestamp stays zero. On a real evdev stream the kernel stamps each
    // event, but filling it in here would mean a CMOS read per event in the
    // PS/2 path, and a program that wants timing has CLOCK_MONOTONIC. Leaving
    // it zero is what a device that cannot time its own events reports.
    ev[8..10].copy_from_slice(&kind.to_ne_bytes());
    ev[10..12].copy_from_slice(&code.to_ne_bytes());
    ev[12..16].copy_from_slice(&value.to_ne_bytes());
    s.outbox.push_back(ev);
    if let Some(t) = s.waiter.take() {
        drop(s);
        crate::task::sched::wake(t);
    }
}

/// Emit `SYN_REPORT`, which evdev uses to mark the end of one input frame.
///
/// A terminal that redraws per report and receives none will either redraw per
/// key or never redraw. One report per driver event is the contract.
fn sync() {
    emit(EV_SYN, SYN_REPORT, 0);
}

/// Map one byte from the keyboard driver to a Linux keycode.
///
/// The driver hands over *characters*; a terminal needs to know *which key* and
/// to derive the character itself. That inversion is the whole reason evdev
/// exists, and getting it wrong here makes the terminal unusable rather than
/// merely inaccurate.
fn keycode_for(ch: u8, mods: u8) -> Option<u16> {
    let shift = mods & MOD_SHIFT != 0;
    Some(match ch {
        0x1b => key::ESC,
        0x08 => key::BACKSPACE,
        0x09 => key::TAB,
        b'\r' | b'\n' => key::ENTER,
        b' ' => key::SPACE,
        b'0'..=b'9' => KEY_1 + 10 + (ch - b'0') as u16,
        b'a'..=b'z' => KEY_A + (ch - b'a') as u16,
        b'A'..=b'Z' => KEY_A + (ch - b'A') as u16,
        _ => punctuation(ch, shift)?,
    })
}

/// Punctuation, which is where the shift handling actually lives.
///
/// The top row and the letter block above need no special cases: evdev numbers
/// them contiguously, and an upper-case letter arrives as its own byte. Only
/// punctuation has a shifted form with no unshifted partner, so only
/// punctuation is looked at twice.
fn punctuation(ch: u8, shift: bool) -> Option<u16> {
    Some(match (ch, shift) {
        // Both `-` and `_` are the *same physical key*, and the shift state is
        // what tells them apart -- that is the whole point of a shifted
        // character, and it is how the user-space keymap in inputd reads it
        // (key 0x0C is `-` unshifted and `_` shifted).
        //
        // These used to return `KEY_MINUS - 1` and `KEY_EQUAL - 1` for the
        // shifted forms, on the theory that a second character needed a second
        // key. It does not: `- 1` is 11, which is the `0` key, and `=` - 1 is
        // 12, which is the `-` key. So pressing shift and `-` reported a `0`
        // keypress, the keymap looked that up, found `0`, and typed a zero.
        // Shift-minus therefore typed a `0` and underscore typed a `-` -- which
        // is why a command line looked as though it had no dashes in it. The
        // `- 1` idiom is right for the digit row, where `!` really is the key
        // below `1`, and wrong here.
        (b'-', _) | (b'_', _) => KEY_MINUS,
        (b'=', _) | (b'+', _) => KEY_EQUAL,
        (b'[', _) | (b'{', _) => KEY_LEFTBRACE,
        (b']', _) | (b'}', _) => KEY_RIGHTBRACE,
        (b'\\', _) | (b'|', _) => KEY_BACKSLASH,
        (b';', false) | (b':', true) => KEY_SEMICOLON,
        (b'\'', false) | (b'"', true) => KEY_APOSTROPHE,
        (b'`', false) | (b'~', true) => KEY_GRAVE,
        (b',', false) | (b'<', true) => KEY_COMMA,
        (b'.', false) | (b'>', true) => KEY_DOT,
        (b'/', false) | (b'?', true) => KEY_SLASH,
        (b'!', _) => KEY_1 - 1,
        (b'@', _) => KEY_1,
        (b'#', _) => KEY_1 + 1,
        (b'$', _) => KEY_1 + 2,
        (b'%', _) => KEY_1 + 3,
        (b'^', _) => KEY_1 + 4,
        (b'&', _) => KEY_1 + 5,
        (b'*', _) => KEY_1 + 6,
        (b'(', _) => KEY_1 + 7,
        (b')', _) => KEY_1 + 8,
        _ => return None,
    })
}

/// Report a modifier only when it changed.
///
/// Both edges unconditionally would be simpler and would be wrong: a terminal
/// counts transitions, so a key event for a key that did not move reads as a
/// spurious press.
fn set_modifier(code: u16, on: bool) {
    let bit = match code {
        key::LEFTSHIFT | key::RIGHTSHIFT => MOD_SHIFT,
        key::LEFTCTRL | key::RIGHTCTRL => MOD_CTRL,
        key::LEFTALT | key::RIGHTALT => MOD_ALT,
        _ => return,
    };
    let mut s = STATE.lock();
    if (s.mods & bit != 0) == on {
        return;
    }
    if on {
        s.mods |= bit;
    } else {
        s.mods &= !bit;
    }
    drop(s);
    emit(EV_KEY, code, if on { 1 } else { 0 });
}

/// Drain the keyboard queue, converting as it goes.
///
/// Called from `read` rather than from the interrupt handler on purpose: the
/// translation allocates, and allocating in a PS/2 interrupt bottom half is a
/// far worse trade than doing it when someone asks for input.
fn pump_keyboard() {
    let mut chunk = [0u8; 32];
    loop {
        let n = crate::devices::keyboard::take_events(&mut chunk);
        if n == 0 {
            break;
        }
        translate_keyboard(&chunk[..n]);
    }
}

fn translate_keyboard(bytes: &[u8]) {
    if bytes.len() < 2 {
        return;
    }
    let mods = bytes[0];
    let len = bytes[1] as usize;
    if bytes.len() < 2 + len {
        return;
    }

    // Modifiers are key events in their own right on evdev, so a program can
    // track them without inferring them from other keys.
    set_modifier(key::LEFTSHIFT, mods & MOD_SHIFT != 0);
    set_modifier(key::LEFTCTRL, mods & MOD_CTRL != 0);
    set_modifier(key::LEFTALT, mods & MOD_ALT != 0);
    set_modifier(key::CAPSLOCK, false);

    for &ch in &bytes[2..2 + len] {
        if let Some(code) = keycode_for(ch, mods) {
            // Press and release together. The PS/2 driver reports a completed
            // keystroke, not a key transition, so the release is synthesized --
            // and it has to be, because a program that sees a press and waits
            // for a matching release will hang.
            emit(EV_KEY, code, 1);
            emit(EV_KEY, code, 0);
        }
    }
    sync();
}

/// Drain the mouse queue, converting as it goes.
fn pump_mouse() {
    let mut chunk = [0u8; 32];
    loop {
        let n = crate::devices::mouse::take_events(&mut chunk);
        if n == 0 {
            break;
        }
        translate_mouse(&chunk[..n]);
    }
}

fn translate_mouse(bytes: &[u8]) {
    let mut off = 0;
    while off + 5 <= bytes.len() {
        let buttons = bytes[off];
        let dx = i16::from_le_bytes([bytes[off + 1], bytes[off + 2]]);
        let dy = i16::from_le_bytes([bytes[off + 3], bytes[off + 4]]);
        off += 5;
        // evdev reports motion as relative axis events and then the button
        // state, which is what a terminal's click handling expects.
        if dx != 0 {
            emit(EV_REL, REL_X, dx as i32);
        }
        if dy != 0 {
            emit(EV_REL, REL_Y, dy as i32);
        }
        emit(EV_KEY, key::BTN_LEFT, i32::from(buttons & 1 != 0));
        emit(EV_KEY, key::BTN_RIGHT, i32::from(buttons & 2 != 0));
        emit(EV_KEY, key::BTN_MIDDLE, i32::from(buttons & 4 != 0));
        sync();
    }
}

/// The device node behind `/dev/input/event0`.
pub struct InputDevice;

impl CharDevice for InputDevice {
    fn read(&self, buf: &mut [u8]) -> usize {
        // Convert before deciding the stream is empty, so a read never returns
        // nothing just because the drivers have input the pump has not seen.
        pump_keyboard();
        pump_mouse();

        let mut s = STATE.lock();
        let mut n = 0;
        // Whole events only. A partial `input_event` is worse than no event: the
        // reader would parse a truncated struct and act on a garbage keycode.
        while n + INPUT_EVENT_SIZE <= buf.len() {
            match s.outbox.pop_front() {
                Some(ev) => {
                    buf[n..n + INPUT_EVENT_SIZE].copy_from_slice(&ev);
                    n += INPUT_EVENT_SIZE;
                }
                None => break,
            }
        }
        n
    }

    fn write(&self, _buf: &[u8]) -> usize {
        0
    }

    fn has_data(&self) -> bool {
        // Check the source queues as well as the outbox, so a reader polling
        // without blocking still sees that input is pending.
        STATE.lock().outbox.is_empty()
            .then(|| {
                !crate::devices::keyboard::has_pending() && !crate::devices::mouse::has_pending()
            })
            .map(|empty_sources| !empty_sources)
            .unwrap_or(true)
    }

    fn park(&self, task: usize, interest: u16) -> bool {
        let mut s = STATE.lock();
        if !s.outbox.is_empty() {
            return true;
        }
        s.waiter = Some(crate::task::TaskId(task));
        // Re-check after registering, closing the lost-wakeup window: input
        // arriving between the outbox test and the park would otherwise wait
        // for the next event that never comes.
        !crate::devices::keyboard::has_pending() && !crate::devices::mouse::has_pending()
    }

    fn unpark(&self, _task: usize) {
        STATE.lock().waiter = None;
    }

    fn name(&self) -> &'static str {
        "samsara-input-event"
    }
}

/// Build the `/dev/input` directory and populate it.
pub fn register() -> Arc<crate::vfs::devfs::DevSubdir> {
    let dir = Arc::new(crate::vfs::devfs::DevSubdir::new("input"));
    let event0 = crate::vfs::devfs::char_node(Arc::new(InputDevice) as Arc<dyn CharDevice>);
    dir.insert("event0", event0);
    dir
}
