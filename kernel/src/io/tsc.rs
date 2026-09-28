// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! TSC (Time Stamp Counter) based high-resolution clock.
//!
//! `rdtsc` is a per-core, invariant (when CPUID.8000_0007.EDX[8] is set)
//! incrementing counter. Samsara calibrates its frequency against the HPET —
//! which is present and free-running on every supported target — and falls
//! back to measuring the 8254 PIT reload rate when no HPET was found.
//!
//! Once calibrated, [`nanos`] is the primary monotonic wall clock for logging,
//! `CLOCK_UPTIME_MS`, `/proc/uptime` and sleepers that need sub-tick
//! precision; the scheduler itself keeps using the coarser interrupt tick.

use crate::io::{hpet, inb, outb};
use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};

/// Calibrated TSC frequency in Hz; zero until [`init`] runs.
static TSC_HZ: AtomicU64 = AtomicU64::new(0);

/// Read the TSC.
#[inline(always)]
pub fn rdtsc() -> u64 {
    let lo: u32;
    let hi: u32;
    // SAFETY: rdtsc has no memory or register side effects beyond eax/edx.
    unsafe {
        asm!("lfence", "rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack));
    }
    ((hi as u64) << 32) | lo as u64
}

/// Calibrated TSC frequency in Hz, or 0 if not yet calibrated.
pub fn hz() -> u64 {
    TSC_HZ.load(Ordering::Relaxed)
}

/// Monotonic nanoseconds since CPU reset, once calibrated (else 0).
pub fn nanos() -> u64 {
    let h = TSC_HZ.load(Ordering::Relaxed);
    if h == 0 {
        return 0;
    }
    (rdtsc() as u128 * 1_000_000_000 / h as u128) as u64
}

/// PIT channel-0 base frequency (8254 master clock).
const PIT_BASE_HZ: u128 = 1_193_182;

/// Channel-0 reload value used for TSC calibration; even (mode 2 requires an
/// even count) and fast enough that a full period is ~0.86 ms.
const PIT_CAL_RELOAD: u16 = 1024;

/// Sanity bounds for a believable TSC frequency.
const MIN_HZ: u128 = 10_000_000;
const MAX_HZ: u128 = 10_000_000_000;

fn plausible(hz: u128) -> bool {
    (MIN_HZ..=MAX_HZ).contains(&hz)
}

/// Calibrate TSC against the (already initialized) HPET: spin for a fixed
/// HPET elapsed interval, derive the TSC rate from the cycle delta.
///
/// Some emulated HPETs expose valid capability registers but never advance
/// their main counter; a short liveness probe plus a bounded window keep the
/// fallback path live instead of hanging the boot.
fn calibrate_via_hpet() -> Option<u64> {
    if !hpet::available() {
        return None;
    }
    // The counter must be free-running: any real HPET moves within a few
    // hundred microseconds of enabling.
    let probe_a = hpet::counter();
    let spin = rdtsc();
    while rdtsc() - spin < 200_000 {}
    let probe_b = hpet::counter();
    if probe_b == probe_a {
        crate::log::kdebug!("tsc: hpet counter stalled; calibrating without it");
        return None;
    }

    let period_fs = hpet::period_fs();
    // Measure a ~10 ms window.
    let target_ticks = (10_000_000_000u128 / period_fs).max(1) as u64;
    let mut best: u128 = 0;
    for _ in 0..2 {
        let h0 = hpet::counter();
        let t0 = rdtsc();
        let mut hend = hpet::counter();
        // Safety net: never spin more than ~100M counter reads.
        let mut guard = 0u32;
        while hend - h0 < target_ticks && guard < 100_000_000 {
            hend = hpet::counter();
            guard += 1;
        }
        let t1 = rdtsc();
        let actual_ns = (hend - h0) as u128 * period_fs / 1_000_000;
        if actual_ns == 0 {
            continue;
        }
        let hz = (t1 - t0) as u128 * 1_000_000_000 / actual_ns;
        if plausible(hz) && hz > best {
            best = hz;
        }
    }
    if best == 0 {
        return None;
    }
    crate::log::kdebug!("tsc: calibrated {} Hz against hpet", best);
    Some(best as u64)
}

/// PIT channel-0 counter latch helper. Latching never reprograms the channel,
/// so a running 100 Hz scheduler clock is left untouched.
fn read_pit_latch() -> u16 {
    outb(0x43, 0xC2); // channel 0, counter latch command
    let lo = inb(0x40);
    let hi = inb(0x40);
    u16::from_le_bytes([lo, hi])
}

/// Wait for a PIT counter reload: the countered value jumps from a small
/// count back up to its reload value, returning the TSC at that instant.
fn wait_reload() -> Option<u64> {
    let mut last = read_pit_latch();
    for _ in 0..2_000_000 {
        let cur = read_pit_latch();
        if cur > last && (cur as u32 - last as u32) > 512 {
            return Some(rdtsc());
        }
        last = cur;
    }
    None
}

/// Fallback when no usable HPET exists: reprogram channel 0 into a known,
/// fast mode-2 rate and derive the TSC rate from the countdown between two
/// consecutive reloads.
///
/// This runs before the scheduler PIT is armed (the PIC-or-APIC path programs
/// it afterwards), so taking over channel 0 here is safe.
fn calibrate_via_pit() -> Option<u64> {
    // Channel 0, low+high access, mode 2, binary.
    outb(0x43, 0x34);
    outb(0x40, (PIT_CAL_RELOAD & 0xFF) as u8);
    outb(0x40, (PIT_CAL_RELOAD >> 8) as u8);

    let t0 = wait_reload()?;
    let t1 = wait_reload()?;
    let delta = t1 - t0;
    if delta == 0 {
        return None;
    }
    let hz = delta as u128 * PIT_BASE_HZ / PIT_CAL_RELOAD as u128;
    if !plausible(hz) {
        return None;
    }
    crate::log::kdebug!("tsc: calibrated {} Hz against pit (reload {})", hz, PIT_CAL_RELOAD);
    Some(hz as u64)
}

/// Calibrate the TSC. Must run after the HPET is up (for the primary path)
/// and before the LAPIC timer is calibrated against this clock.
pub fn init() {
    let hz = calibrate_via_hpet().or_else(calibrate_via_pit);
    match hz {
        Some(h) => TSC_HZ.store(h, Ordering::Relaxed),
        None => crate::log::kwarn!("tsc: calibration failed; keeping tick-based time"),
    }
}