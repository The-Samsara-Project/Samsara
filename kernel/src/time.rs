// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! Kernel wall-clock substitute: PIT-derived tick counter.

use core::sync::atomic::{AtomicU64, Ordering};

static TICKS: AtomicU64 = AtomicU64::new(0);

/// Called by the timer interrupt handler once per tick.
pub fn tick() {
    TICKS.fetch_add(1, Ordering::Relaxed);
}

/// Current uptime in timer ticks.
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Timer frequency configured by [`crate::interrupts::pit`].
pub const TIMER_HZ: u64 = 100;

/// Uptime in milliseconds: high-resolution TSC time once calibrated, falling
/// back to the tick counter when no TSC clock is available.
pub fn millis() -> u64 {
    let ns = crate::io::tsc::nanos();
    if ns != 0 {
        ns / 1_000_000
    } else {
        ticks() * (1000 / TIMER_HZ)
    }
}

/// Periodic work performed from the idle loop on every observed tick.
pub fn on_tick_housekeeping() {
    // Reserved for scheduler preemption hooks.
}
