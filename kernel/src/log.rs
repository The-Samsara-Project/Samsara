// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! Structured kernel logging with level filtering, timestamps and colors.
//!
//! Output resembles the kernel ring buffer:
//!
//! ```text
//! [    0.123] INFO: Samsara initialized successfully
//! ```
//!
//! Each line is emitted as a single ANSI stream: the timestamp is tinted
//! through a red-to-orange ramp so the start of every line is immediately
//! visible, then the level tag and message are colored per level (red family
//! for errors, yellow for warnings, green for info, cyan for debug). The same
//! stream goes to the serial console and to the framebuffer console, which
//! parses the SGR escapes to color each segment — dimmed timestamps and
//! per-attribute lines are gone.

use core::fmt::{self, Write as _};
use core::sync::atomic::{AtomicU8, Ordering};

/// Log severity levels, ordered from most to least urgent.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Level {
    /// System is unusable / panic path.
    Emerg = 0,
    /// Error conditions.
    Error = 1,
    /// Warning conditions.
    Warn = 2,
    /// Informational messages (default floor at boot).
    Info = 3,
    /// Debug detail.
    Debug = 4,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Level::Emerg => "EMERG",
            Level::Error => "ERROR",
            Level::Warn => " WARN",
            Level::Info => " INFO",
            Level::Debug => "DEBUG",
        }
    }

    /// ANSI foreground/background escape that prefixes the level tag and
    /// message. Bright white on red for emergencies, bright colors otherwise.
    fn ansi(self) -> &'static str {
        match self {
            Level::Emerg => "\x1b[1;97;41m",
            Level::Error => "\x1b[91m",
            Level::Warn => "\x1b[93m",
            Level::Info => "\x1b[92m",
            Level::Debug => "\x1b[96m",
        }
    }
}

/// First hue of the timestamp ramp: a hot red.
const TS_START: (u8, u8, u8) = (0xFF, 0x30, 0x00);
/// Last hue of the timestamp ramp: orange.
const TS_END: (u8, u8, u8) = (0xFF, 0xA5, 0x00);

/// Color of the timestamp character at position `i` of `len`.
///
/// The ramp goes from [`TS_START`] (red) to [`TS_END`] (orange) across the
/// line's opening `[    0.000]`, so the reader's eye always lands on the start
/// of each log line.
fn timestamp_color(i: usize, len: usize) -> (u8, u8, u8) {
    if len <= 1 {
        return TS_START;
    }
    let n = len - 1;
    let lerp = |a: u8, b: u8| {
        let d = ((b as i32 - a as i32) * i as i32) / n as i32;
        (a as i32 + d) as u8
    };
    (
        lerp(TS_START.0, TS_END.0),
        lerp(TS_START.1, TS_END.1),
        lerp(TS_START.2, TS_END.2),
    )
}

static MAX_LEVEL: AtomicU8 = AtomicU8::new(Level::Info as u8);

/// Set the maximum level that will be emitted.
pub fn init(max: Level) {
    MAX_LEVEL.store(max as u8, Ordering::Relaxed);
}

fn enabled(level: Level) -> bool {
    (level as u8) <= MAX_LEVEL.load(Ordering::Relaxed)
}

struct BufWriter<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl fmt::Write for BufWriter<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let b = s.as_bytes();
        let room = self.buf.len().saturating_sub(self.pos);
        let k = b.len().min(room);
        self.buf[self.pos..self.pos + k].copy_from_slice(&b[..k]);
        self.pos += k;
        Ok(())
    }
}

/// Core logging routine: one formatted, timestamped, colorized line to both
/// consoles. Bounded buffers only — safe from exception contexts.
pub fn log(level: Level, args: fmt::Arguments<'_>) {
    if !enabled(level) {
        return;
    }
    let ms = crate::time::millis();
    let sec = ms / 1000;
    let frac = ms % 1000;

    // Format the caller's message into a bounded buffer.
    let mut msg = [0u8; 384];
    let mut w = BufWriter { buf: &mut msg, pos: 0 };
    let _ = fmt::write(&mut w, args);
    let msg_len = w.pos;

    let mut line = [0u8; 1024];
    let mut lw = BufWriter { buf: &mut line, pos: 0 };

    // Timestamp: one truecolor escape per character, red fading to orange.
    let mut ts = [0u8; 24];
    let mut tw = BufWriter { buf: &mut ts, pos: 0 };
    let _ = fmt::write(&mut tw, format_args!("[{:>7}.{:03}]", sec, frac));
    let ts_len = tw.pos;
    for (i, &c) in ts[..ts_len].iter().enumerate() {
        let (r, g, b) = timestamp_color(i, ts_len);
        let _ = fmt::write(
            &mut lw,
            format_args!("\x1b[38;2;{};{};{}m{}", r, g, b, c as char),
        );
    }

    // Level tag and message in the level color, then a full reset.
    let _ = lw.write_str("\x1b[0m ");
    let _ = lw.write_str(level.ansi());
    let _ = lw.write_str(level.as_str());
    let _ = lw.write_str(": ");
    match core::str::from_utf8(&msg[..msg_len]) {
        Ok(s) => {
            let _ = lw.write_str(s);
        }
        Err(_) => {
            let _ = lw.write_str("<binary>");
        }
    }
    let _ = lw.write_str("\x1b[0m\n");
    let line_len = lw.pos;

    crate::io::uart::write(&line[..line_len]);
    crate::console::write(&line[..line_len]);
}

macro_rules! kemerg {
    ($($arg:tt)*) => {
        $crate::log::log($crate::log::Level::Emerg, format_args!($($arg)*))
    };
}

macro_rules! kerror {
    ($($arg:tt)*) => {
        $crate::log::log($crate::log::Level::Error, format_args!($($arg)*))
    };
}

macro_rules! kwarn {
    ($($arg:tt)*) => {
        $crate::log::log($crate::log::Level::Warn, format_args!($($arg)*))
    };
}

macro_rules! kinfo {
    ($($arg:tt)*) => {
        $crate::log::log($crate::log::Level::Info, format_args!($($arg)*))
    };
}

macro_rules! kdebug {
    ($($arg:tt)*) => {
        $crate::log::log($crate::log::Level::Debug, format_args!($($arg)*))
    };
}

pub(crate) use {kdebug, kemerg, kerror, kinfo, kwarn};