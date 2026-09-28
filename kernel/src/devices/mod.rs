// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! In-kernel device drivers.
//!
//! Since the microkernel rework the PS/2 controller is owned by the ring-3
//! `inputd` server (IRQ 1 is delivered to it as an IPC message); the kernel
//! only performs the one-time controller bring-up and leaves the lines
//! unbound so the driver can claim them.

pub mod input;
pub mod keyboard;
pub mod mouse;
pub mod ps2;
pub mod queue;

use alloc::sync::Arc;

/// Bring up the PS/2 subsystem and expose input devices on `/dev`.
pub fn init_input() {
    let detected = ps2::init();

    // NB: legacy driver hooks are intentionally NOT attached. IRQ 1 will be
    // bound to the `inputd` endpoint by its ring-3 driver. `/dev/kbd0` stays
    // registered so kernel-side tooling can still poll, but nothing routes
    // the interrupt into it anymore.
    if !detected.mouse {
        mouse::init();
    }

    // Expose the devices through devfs.
    let _ = crate::vfs::devfs::register("kbd0", Arc::new(keyboard::KbdDevice));
    if detected.mouse {
        let _ = crate::vfs::devfs::register("mouse0", Arc::new(mouse::MouseDevice));
    }

    // `/dev/input/event0`, the evdev-style stream a ported terminal reads. It
    // is layered over the two devices above rather than replacing them, so
    // anything already using the native framing keeps working.
    let input = input::register();
    let _ = crate::vfs::devfs::register_subdir("input", input);

    // Everything is routed; allow the controller to raise IRQs.
    ps2::enable_interrupts();

    crate::log::kinfo!("input: keyboard online{}", if detected.mouse { ", mouse online" } else { "" });
}