// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! i8042 PS/2 controller driver.
//!
//! Handles controller initialization, port enabling, device identification
//! and the command/ACK protocol shared by both device ports.

use crate::io::{inb, io_wait, outb};

const STATUS: u16 = 0x64;
const COMMAND: u16 = 0x64;
/// Data port shared by both device channels (selection is via controller state).
pub const DATA_PORT: u16 = 0x60;

const STATUS_OUTPUT_FULL: u8 = 1 << 0;
const STATUS_INPUT_FULL: u8 = 1 << 1;
const STATUS_AUX_DATA: u8 = 1 << 5;

// Controller commands.
const CMD_READ_CONFIG: u8 = 0x20;
const CMD_WRITE_CONFIG: u8 = 0x60;
const CMD_DISABLE_FIRST: u8 = 0xAD;
const CMD_ENABLE_FIRST: u8 = 0xAE;
const CMD_TEST_CONTROLLER: u8 = 0xAA;
const CMD_TEST_FIRST_PORT: u8 = 0xAB;

// Device commands (written to DATA with the target port enabled).
const DEV_RESET: u8 = 0xFF;
const DEV_IDENTIFY: u8 = 0xF2;
const DEV_SCANCODE_SET: u8 = 0xF0;
const DEV_SET_DEFAULTS: u8 = 0xF6;
const DEV_ENABLE_SCANNING: u8 = 0xF4;
const DEV_DISABLE_SCANNING: u8 = 0xF5;

/// ACK response byte for device commands.
pub const RESP_ACK: u8 = 0xFA;
/// Self-test passed.
const RESP_SELF_TEST_OK: u8 = 0x55;
/// First-port interface test OK.
const RESP_PORT_OK: u8 = 0x00;
/// Keyboard BAT completed.
const RESP_BAT_OK: u8 = 0xAA;

/// Which devices the controller reports.
#[derive(Debug, Clone, Copy)]
pub struct Detected {
    /// A keyboard answered on the first port.
    pub keyboard: bool,
    /// A pointing device answered on the second port.
    pub mouse: bool,
}

static mut CONFIG_CACHE: u8 = 0;

fn wait_write() {
    let mut guard = 100_000u32;
    while inb(STATUS) & STATUS_INPUT_FULL != 0 {
        if guard == 0 {
            return;
        }
        guard -= 1;
        core::hint::spin_loop();
    }
}

fn wait_read_timeout() -> Option<u8> {
    let mut guard = 200_000u32;
    while inb(STATUS) & STATUS_OUTPUT_FULL == 0 {
        if guard == 0 {
            return None;
        }
        guard -= 1;
        core::hint::spin_loop();
    }
    Some(inb(DATA_PORT))
}

fn send_command(cmd: u8) {
    wait_write();
    outb(COMMAND, cmd);
}

fn read_config() -> u8 {
    send_command(CMD_READ_CONFIG);
    unsafe { CONFIG_CACHE = wait_read_timeout().unwrap_or(0) };
    unsafe { CONFIG_CACHE }
}

fn write_config(cfg: u8) {
    send_command(CMD_WRITE_CONFIG);
    wait_write();
    outb(DATA_PORT, cfg);
    unsafe { CONFIG_CACHE = cfg };
}

/// Send a device-level command byte to the *first* (keyboard) port and
/// wait for its ACK. Returns false on timeout.
pub fn device_command_first(cmd: u8) -> bool {
    wait_write();
    outb(DATA_PORT, cmd);
    matches!(wait_read_timeout(), Some(RESP_ACK))
}

/// Send a device-level command to the *second* (mouse) port.
pub fn device_command_second(cmd: u8) -> bool {
    send_command(0xD4); // route next data byte to aux port
    wait_write();
    outb(DATA_PORT, cmd);
    matches!(wait_read_timeout(), Some(RESP_ACK))
}

/// Read one raw data byte from the first port (blocking, bounded).
pub fn read_data() -> Option<u8> {
    wait_read_timeout()
}

/// True when the output buffer holds a byte from the auxiliary (mouse) port.
pub fn status_is_aux() -> bool {
    inb(STATUS) & STATUS_AUX_DATA != 0
}

/// True when the output buffer has data waiting.
pub fn status_has_output() -> bool {
    inb(STATUS) & STATUS_OUTPUT_FULL != 0
}

/// Drain any stale bytes from the output buffer.
fn flush_output() {
    while status_has_output() {
        let _ = inb(DATA_PORT);
    }
}

/// Drain any stale bytes from the output buffer (public: used before
/// unmasking the keyboard IRQ line so a byte left over from boot-time
/// probing cannot hold the line asserted and swallow every later edge).
pub fn flush_input() {
    flush_output();
}

/// Initialize the controller; returns what was detected on each port.
pub fn init() -> Detected {
    flush_output();

    // Disable both ports so configuration changes are race-free.
    send_command(CMD_DISABLE_FIRST);
    // (Second-port disable is 0xA7 on dual-channel controllers; sending it
    // harmlessly does nothing when absent.)

    // Configure: interrupts off + clocks disabled during setup, translation
    // ON so keyboards emit scancode set 1 even if they default to set 2.
    let mut cfg = read_config();
    cfg &= !(0b0000_0011); // no IRQs yet
    cfg |= 0b0100_0000; // bit6: translate to set 1
    cfg |= 0b0011_0000; // bits4-5: clock lines disabled while probing
    write_config(cfg);

    // Controller self-test.
    send_command(CMD_TEST_CONTROLLER);
    let ctrl_ok = wait_read_timeout() == Some(RESP_SELF_TEST_OK);
    crate::log::kdebug!("ps2: controller self-test {}", if ctrl_ok { "ok" } else { "FAILED" });

    // First port interface test.
    send_command(CMD_TEST_FIRST_PORT);
    let port_ok = wait_read_timeout() == Some(RESP_PORT_OK);
    crate::log::kdebug!(
        "ps2: first port interface {}",
        if port_ok { "ok" } else { "FAILED" }
    );

    // Re-enable clocks, still without IRQs until drivers are ready.
    let mut cfg = read_config();
    cfg &= !0b0011_0000;
    write_config(cfg);
    send_command(CMD_ENABLE_FIRST);

    // Probe devices.
    let keyboard = probe_keyboard();

    // Some controllers answer the final enable command only after our last
    // read drained; re-check briefly so no late ACK is left behind to wedge
    // the IRQ line once a driver binds it.
    for _ in 0..3 {
        io_wait();
        flush_output();
    }
    io_wait();
    Detected {
        keyboard,
        mouse: false, // set by mouse::init probing the aux channel
    }
}

fn probe_keyboard() -> bool {
    // Reset then identify. QEMU's i8042 answers BAT + typical IDs.
    flush_output();
    if !device_command_first(DEV_RESET) && !status_has_output() {
        crate::log::kwarn!("ps2: keyboard reset got no ACK");
        return false;
    }
    // Expect BAT pass (0xAA) possibly followed by ID bytes.
    let bat = read_data();
    if bat != Some(RESP_BAT_OK) {
        crate::log::kwarn!("ps2: keyboard BAT failed ({:?})", bat);
        return false;
    }

    // Request scancode set 1 explicitly (translation also covers it).
    if device_command_first(DEV_SCANCODE_SET)
        && {
            wait_write();
            outb(DATA_PORT, 1);
            true
        }
    {
        // Consume the ACK for the parameter byte if present.
        let _ = wait_read_timeout();
    }

    // Identification is best-effort; many devices NACK it after a reset.
    if device_command_first(DEV_IDENTIFY) {
        let id0 = read_data();
        crate::log::kdebug!("ps2: keyboard id {:?}", id0);
    }

    device_command_first(DEV_SET_DEFAULTS);
    let ok = device_command_first(DEV_ENABLE_SCANNING);
    crate::log::kdebug!("ps2: keyboard scanning {}", if ok { "enabled" } else { "not acked" });
    ok || bat == Some(RESP_BAT_OK)
}

/// Re-enable the second port clock line (used by the mouse driver).
pub fn enable_second_port() {
    send_command(0xA8); // enable second port
}

/// Enable first/second port interrupt lines in the controller config.
/// Call once driver IRQ handlers are routed.
pub fn enable_interrupts() {
    let mut cfg = read_config();
    cfg |= 0b0000_0011; // IRQ1 + IRQ12 enabled
    cfg &= !0b0011_0000; // clocks enabled
    write_config(cfg);
    crate::log::kdebug!("ps2: controller IRQ lines enabled ({:#04x})", cfg);
}
