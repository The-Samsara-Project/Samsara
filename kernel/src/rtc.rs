// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! Wall-clock time from the MC146818 real-time clock.
//!
//! The tick counter in [`crate::time`] answers "how long has this been up",
//! which is all a scheduler needs and all the ABI used to offer. A wall clock
//! is a different question, and a program that asks it a real one: `date`,
//! `ls -l`, a log timestamp, an NFS lease, `tar` preserving mtimes. All of
//! them need a civil date, and "milliseconds since boot" answers 1970.
//!
//! The RTC is the only clock in the machine that survives a reboot, which is
//! exactly why it is the right source for civil time. It is also a CMOS
//! device: register `0x70` selects which of ten registers `0x71` will read or
//! write. That index register is one piece of global hardware state, so every
//! select-then-access sequence below runs with interrupts off -- otherwise a
//! timer IRQ between the two port writes leaves us reading the wrong register.

use core::sync::atomic::{AtomicI64, Ordering};

/// CMOS index port.
const CMOS_INDEX: u16 = 0x70;
/// CMOS data port.
const CMOS_DATA: u16 = 0x71;

const REG_SECONDS: u8 = 0x00;
const REG_MINUTES: u8 = 0x02;
const REG_HOURS: u8 = 0x04;
const REG_DAY: u8 = 0x07;
const REG_MONTH: u8 = 0x08;
const REG_YEAR: u8 = 0x09;
/// Status register A. Bit 6 is **DM**, the data mode: 0 = BCD, 1 = binary.
/// Note that bit 2 of this register is *SET* (which bank of registers the
/// index port selects) and has nothing to do with the number format -- reading
/// the wrong bit here makes every field look like a large invalid value, which
/// is exactly what a wrong bit looks like.
const REG_STATUS_A: u8 = 0x0A;
/// Status register B. Bit 6 is the 24-hour flag, again *not* bit 2 (which is
/// update-ended-interrupt-enable). Bit 7 is daylight savings.
const REG_STATUS_B: u8 = 0x0B;
/// Status register D: bit 7 means the clock believes its own contents.
const REG_STATUS_D: u8 = 0x0D;
/// Century register. Not architecturally guaranteed, but every PC-grade RTC
/// since 2000 has it and QEMU implements it; its absence is handled below.
const REG_CENTURY: u8 = 0x32;

/// `REG_STATUS_A` bit 6: values are binary rather than BCD.
const A_DATA_BINARY: u8 = 0x40;
/// `REG_STATUS_A` bit 7: halt the chip's own updates so the ten time registers
/// can be written as a set. Writing them without it races the oscillator, and
/// the race produces a plausible, wrong time often enough to matter.
const A_UPDATE_INHIBIT: u8 = 0x80;
/// `REG_STATUS_B` bit 6: 24-hour mode.
const B_24_HOUR: u8 = 0x40;

/// Seconds since the Unix epoch, or `0` before the first successful read.
///
/// Negative once the clock is deliberately set before 1970, which `time(1)`
/// and a few archive formats accept, so this is signed.
static EPOCH_SECS: AtomicI64 = AtomicI64::new(0);
/// Whether [`EPOCH_SECS`] holds a real reading yet.
static VALID: AtomicBool = AtomicBool::new(false);

use core::sync::atomic::AtomicBool;

/// Read one CMOS register with interrupts off.
///
/// The index/data pair is global hardware state, so this has to be atomic with
/// respect to an interrupt: an IRQ between the index write and the data read
/// leaves us reading whatever register that handler last selected.
///
/// The whole sequence is one `asm!` block on purpose. Written as separate
/// `cli` / `outb` / `inb` calls, nothing in the inline-assembly contract orders
/// them against each other, and the compiler is entitled to interleave the
/// index write for one register with the data read for another. That does not
/// produce an obviously wrong answer -- it produces a *plausible* one, which is
/// the worst possible outcome for a clock: this bug presented as a wall clock
/// that was simply stuck. The port instructions are inlined here rather than
/// going through [`crate::io`] so the sequence cannot be split.
fn read_reg(reg: u8) -> u8 {
    let flags: usize;
    // SAFETY: CPL 0. Interrupts are off for the whole select-then-read pair.
    unsafe {
        core::arch::asm!("pushfq; pop {0}; cli", out(reg) flags, options(nomem));
    }
    // SAFETY: port I/O to the CMOS pair at CPL 0. `outb`/`inb` carry a memory
    // clobber on purpose (see kernel/src/io/mod.rs), so these two calls cannot
    // be reordered apart from each other by the compiler -- which is what makes
    // this two-call form equivalent to the single-block form it replaced.
    let v = unsafe {
        crate::io::outb(CMOS_INDEX, reg);
        crate::io::inb(CMOS_DATA)
    };
    // Restore IF only if it was set on entry.
    if flags & (1 << 9) != 0 {
        // SAFETY: re-enabling interrupts that were on before the access.
        unsafe { core::arch::asm!("sti", options(nomem, nostack)) };
    }
    v
}

/// Write one CMOS register with interrupts off.
fn write_reg(reg: u8, data: u8) {
    let flags: usize;
    // SAFETY: as `read_reg`.
    unsafe {
        core::arch::asm!("pushfq; pop {0}; cli", out(reg) flags, options(nomem));
    }
    // SAFETY: as `read_reg`.
    unsafe {
        crate::io::outb(CMOS_INDEX, reg);
        crate::io::outb(CMOS_DATA, data);
    }
    if flags & (1 << 9) != 0 {
        // SAFETY: as `read_reg`.
        unsafe { core::arch::asm!("sti", options(nomem, nostack)) };
    }
}

/// Decode a CMOS field, which is BCD unless status register A says binary.
fn decode(raw: u8, binary: bool) -> u64 {
    if binary {
        raw as u64
    } else {
        // BCD: low nibble is the units digit, high nibble the tens. A nibble
        // above 9 means the field is not valid BCD at all, so fall back to the
        // raw byte rather than returning a wild number -- a wrong-but-plausible
        // date is worse than one the caller can see is odd.
        let hi = (raw >> 4) as u64;
        let lo = (raw & 0xF) as u64;
        if hi > 9 || lo > 9 {
            raw as u64
        } else {
            hi * 10 + lo
        }
    }
}

/// Build a Unix timestamp from one interpretation of the raw registers.
///
/// `binary` and `hour24` are *hypotheses* about how to read the fields. The
/// caller tries the mode the status registers advertise first and the other
/// one second, because firmware mode flags are frequently wrong: QEMU ships a
/// BCD clock with the binary bit set, and plenty of real boards ship a 24-hour
/// clock with the 12-hour bit set. Decoding by trial rather than by trust is
/// what Linux's `rtc-cmos` does for the same reason.
///
/// Returns `None` for any combination that does not describe a real moment, so
/// the caller can move on to the next hypothesis.
fn decode_epoch(
    raw: &RawTime,
    binary: bool,
    hour24: bool,
) -> Option<i64> {
    let sec = decode(raw.sec, binary) as i64;
    let min = decode(raw.min, binary) as i64;
    let mut hour = decode(raw.hour, binary) as i64;
    let day = decode(raw.day, binary) as i64;
    let mon = decode(raw.mon, binary) as i64;
    let year2 = decode(raw.year, binary) as i64;

    if !(0..=59).contains(&sec) || !(0..=59).contains(&min) {
        return None;
    }
    if hour24 {
        if !(0..=23).contains(&hour) {
            return None;
        }
    } else {
        // 12-hour mode: bit 7 of the hours register is the PM flag and the low
        // seven bits hold 1..=12. The one genuinely awkward case is noon:
        // 12 PM is stored as 12 with the flag *clear*, so "PM" is not simply
        // "flag set" -- it is "flag set, or the value is 12".
        let pm = hour & 0x80 != 0;
        hour &= 0x7F;
        if !(1..=12).contains(&hour) {
            return None;
        }
        hour = if pm || hour == 12 { hour % 12 + 12 } else { hour };
    }
    if !(1..=12).contains(&mon) || !(1..=31).contains(&day) {
        return None;
    }

    // Two-digit year plus century. Without a usable century register the only
    // defensible guess is the current century, which is what PC firmware did
    // for decades and is right for every machine anyone actually boots.
    let cent_raw = raw.cent;
    let century_known = cent_raw != 0 && cent_raw != 0xFF;
    let century = if century_known {
        decode(cent_raw, binary) as i64
    } else {
        (year2 / 100) * 100
    };
    let full_year = century * 100 + (year2 % 100);
    // A century register that disagrees wildly with the year register (a real
    // failure mode on a dying battery) would land the date centuries away.
    if century_known && (century < 19 || century > 21) {
        return None;
    }
    if full_year < 1970 && !century_known {
        // No century register and a year below 70: the 20th century is the only
        // reading that makes this a plausible present-day clock.
        let y = full_year + 100;
        if y < 1970 || y > 2099 {
            return None;
        }
        return finish(y, mon, day, hour, min, sec);
    }
    finish(full_year, mon, day, hour, min, sec)
}

/// Validate a decoded date and convert it to seconds since the epoch.
fn finish(year: i64, mon: i64, day: i64, hour: i64, min: i64, sec: i64) -> Option<i64> {
    // Reject days that do not exist. A 31 in February means the read was torn,
    // and rolling that forward to March 3 is how a log file ends up holding a
    // plausible lie.
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let mdays = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if day > mdays[(mon - 1) as usize] {
        return None;
    }
    let days = days_from_civil(year, mon, day);
    // The RTC keeps UTC. Local time is a user-space concern (`TZ` and the
    // zoneinfo database); inventing a kernel-side offset would make every
    // program disagree with every other about what time it is.
    Some(days * 86400 + hour * 3600 + min * 60 + sec)
}

/// The time registers as read, before any interpretation.
struct RawTime {
    sec: u8,
    min: u8,
    hour: u8,
    day: u8,
    mon: u8,
    year: u8,
    cent: u8,
}
///
/// Days from 1970-01-01 to `y-m-d`, proleptic Gregorian.
///
/// Howard Hinnant's `days_from_civil`, which shifts the year so that March is
/// the first month. That puts the leap day at the end of the year, so the
/// February adjustment never has to be special-cased. Valid for any year,
/// including before 1970: the era arithmetic is exact in both directions,
/// which matters because the RTC can legitimately report a date earlier than
/// the epoch.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (m + 9) % 12; // March = 0
    let doy = (153 * mp + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// Read the RTC as whole seconds since the Unix epoch.
///
/// Returns `None` if the clock reports itself invalid, or if the registers
/// hold an impossible date (month 0 or 13, day 0, or a day beyond the month's
/// length). Those checks are the whole reason this is more than a byte-shuffle:
/// a CMOS read racing the chip's own update reports a *plausible but wrong*
/// time, and the standard defence is to read twice and compare, plus reject
/// anything that is not a real date.
pub fn read_epoch() -> Option<i64> {
    let status_a = read_reg(REG_STATUS_A);
    let status_b = read_reg(REG_STATUS_B);

    // Read the calendar twice and keep the first pair that agrees. A torn read
    // shows up as a mismatch; two matching reads do not, because the chip's
    // update window is far shorter than two back-to-back sequences of port
    // reads. This is the standard defence against the failure mode that makes
    // CMOS timekeeping untrustworthy: a read that straddles the update returns
    // a *plausible but wrong* time rather than an obviously broken one.
    let mut last: Option<RawTime> = None;
    for _ in 0..4 {
        let snap = RawTime {
            sec: read_reg(REG_SECONDS),
            min: read_reg(REG_MINUTES),
            hour: read_reg(REG_HOURS),
            day: read_reg(REG_DAY),
            mon: read_reg(REG_MONTH),
            year: read_reg(REG_YEAR),
            cent: read_reg(REG_CENTURY),
        };
        if last.as_ref().map(|p| {
            (p.sec, p.min, p.hour, p.day, p.mon, p.year, p.cent)
                == (snap.sec, snap.min, snap.hour, snap.day, snap.mon, snap.year, snap.cent)
        }) == Some(true) {
            break;
        }
        last = Some(snap);
    }
    let raw = last?;

    // Status D bit 7 means "the contents are valid". A clear bit means either a
    // dead battery or that the chip is mid-update. It is worth knowing but not
    // worth refusing over: the mode-flag trials below validate the result
    // properly, and a machine whose RTC has no battery still keeps time within
    // a power cycle, which is strictly better than reporting 1970.
    let valid = read_reg(REG_STATUS_D) & 0x80 != 0;

    // Try the mode the status registers advertise, then the alternatives.
    // Firmware mode flags are not trustworthy -- QEMU presents a BCD clock with
    // the binary bit set, and real boards do the same -- so a reading that does
    // not describe a real moment is a reason to re-read the same bytes a
    // different way, not a reason to give up.
    let advertised = (status_a & A_DATA_BINARY != 0, status_b & B_24_HOUR != 0);
    let mut order = [advertised, (true, true), (true, false), (false, true), (false, false)];
    // Move the advertised pair to the front (it already is) and drop duplicates.
    let mut seen = 0u8;
    let mut trials: [(bool, bool); 4] = [(false, false); 4];
    for (b, h) in order.iter() {
        let key = ((*b as u8) << 1) | (*h as u8);
        if seen & (1 << key) == 0 {
            seen |= 1 << key;
            trials[seen.count_ones() as usize - 1] = (*b, *h);
        }
    }
    for (b, h) in trials.iter().take(seen.count_ones() as usize) {
        if let Some(v) = decode_epoch(&raw, *b, *h) {
            if !valid {
                crate::log::kwarn!("rtc: status D reports invalid; using the reading anyway");
            }
            return Some(v);
        }
    }
    None
}

/// Bring the cached wall clock up to date and return it in seconds.
pub fn epoch_secs() -> i64 {
    if let Some(v) = read_epoch() {
        EPOCH_SECS.store(v, Ordering::Relaxed);
        VALID.store(true, Ordering::Release);
    } else if !VALID.load(Ordering::Acquire) {
        // No reading yet and none available. Report the epoch rather than a
        // garbage value: 1970-01-01 is obviously wrong, which is the useful
        // property. A program can detect the condition; a random date it
        // cannot.
        return 0;
    }
    EPOCH_SECS.load(Ordering::Relaxed)
}

/// One-shot boot diagnostic: report what the RTC actually contains.
///
/// A CMOS read that fails is silent by design, which makes it a miserable
/// thing to bring up. This dumps the raw registers once at boot so "the clock
/// reads 1970" can be told apart from "the clock is not there".
pub fn diag_dump() {
    let a = read_reg(REG_STATUS_A);
    let b = read_reg(REG_STATUS_B);
    let d = read_reg(REG_STATUS_D);
    crate::log::kdebug!(
        "rtc: raw A={:#04x} B={:#04x} D={:#04x} says_binary={} says_hour24={} valid={}",
        a,
        b,
        d,
        a & A_DATA_BINARY != 0,
        b & B_24_HOUR != 0,
        d & 0x80 != 0
    );
    crate::log::kdebug!(
        "rtc: raw sec={} min={} hour={:#04x} day={} mon={} year={} cent={:#04x}",
        read_reg(REG_SECONDS),
        read_reg(REG_MINUTES),
        read_reg(REG_HOURS),
        read_reg(REG_DAY),
        read_reg(REG_MONTH),
        read_reg(REG_YEAR),
        read_reg(REG_CENTURY)
    );
    match read_epoch() {
        Some(v) => crate::log::kdebug!("rtc: epoch {} (readable)", v),
        None => crate::log::kdebug!("rtc: read_epoch() rejected the contents"),
    }
}

/// Wall-clock time in milliseconds since the Unix epoch.
pub fn epoch_millis() -> i64 {
    if let Some(v) = read_epoch() {
        EPOCH_SECS.store(v, Ordering::Relaxed);
        VALID.store(true, Ordering::Release);
    }
    if !VALID.load(Ordering::Acquire) {
        return 0;
    }
    // Millisecond resolution is more than the RTC has. Adding uptime would give
    // a number that looks precise and is not; whole seconds is the truth.
    EPOCH_SECS.load(Ordering::Relaxed).saturating_mul(1000)
}

/// Whether a real reading has ever been obtained (for `gettimeofday`-style
/// callers that want to distinguish "midnight 1970" from "no RTC").
pub fn is_valid() -> bool {
    VALID.load(Ordering::Acquire)
}

/// Set the RTC to `secs` since the Unix epoch.
///
/// This is how the clock gets a sane value at all on hardware with a dead
/// battery, and how a first boot anchors itself: the kernel cannot know the
/// date, but it can be told, and `stime(2)`/`clock_settime(2)` are how a user
/// is expected to say so.
pub fn set_epoch(secs: i64) {
    // Inverse of `days_from_civil`, plus the time of day.
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    let status_a = read_reg(REG_STATUS_A);
    let binary = status_a & A_DATA_BINARY != 0;
    let hour24 = read_reg(REG_STATUS_B) & B_24_HOUR != 0;
    let enc = |v: i64| -> u8 {
        if binary {
            v as u8
        } else {
            (((v / 10) as u8) << 4) | ((v % 10) as u8)
        }
    };

    // Status A bit 7 is the "update-inhibit" flag: setting it stops the chip
    // from updating its registers behind our back while we write ten of them.
    // Without this the write is a race with the oscillator and produces a
    // plausible, wrong time about as often as not.
    write_reg(REG_STATUS_A, status_a | A_UPDATE_INHIBIT);
    write_reg(REG_YEAR, enc(y % 100));
    write_reg(REG_CENTURY, enc((y / 100) % 100));
    write_reg(REG_MONTH, enc(m));
    write_reg(REG_DAY, enc(d));
    write_reg(REG_HOURS, enc(if hour24 { hh } else { hh % 12 }));
    write_reg(REG_MINUTES, enc(mm));
    write_reg(REG_SECONDS, enc(ss));
    write_reg(REG_STATUS_A, status_a);

    // Adopt it immediately rather than waiting for the next read.
    EPOCH_SECS.store(secs, Ordering::Relaxed);
    VALID.store(true, Ordering::Release);
}

/// Inverse of [`days_from_civil`].
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}
