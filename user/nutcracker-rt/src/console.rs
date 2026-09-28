// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa
//
// Serial console logging for ring-3 programs. Everything funnels through the
// kernel's `DEBUG_WRITE` syscall so it shows up on the same serial line.

use core::fmt::{self, Write};

use crate::syscall;

/// A `fmt::Write` sink that pushes bytes straight to the debug console.
pub struct Serial;

impl Write for Serial {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        syscall::debug_write(s.as_bytes());
        Ok(())
    }
}

/// Render a `core::fmt` argument set to the console (no trailing newline).
pub fn write_fmt(args: fmt::Arguments) {
    let mut s = Serial;
    let _ = s.write_fmt(args);
}

/// Line buffer for `println!`: accumulates one whole line so it reaches the
/// console in a single `DEBUG_WRITE`. Without this, `fmt::Write` fragment
/// calls issue one syscall each and another process's output can be scheduled
/// in the middle of our line.
pub struct LineFmt<'a> {
    buf: &'a mut [u8],
    len: usize,
}

impl<'a> Write for LineFmt<'a> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let n = s.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}

/// Format `args` plus a trailing newline into a stack buffer and emit it as
/// one atomic syscall.
pub fn println_fmt(args: fmt::Arguments) {
    let mut raw = [0u8; 1024];
    let mut f = LineFmt {
        buf: &mut raw,
        len: 0,
    };
    let _ = f.write_fmt(args);
    // A truncation followed by the newline remains well-formed.
    if f.len < f.buf.len() {
        f.buf[f.len] = b'\n';
        f.len += 1;
    }
    syscall::debug_write(&f.buf[..f.len]);
}

/// Write a raw byte slice to the console followed by a newline.
pub fn println_bytes(bytes: &[u8]) {
    syscall::debug_write(bytes);
    syscall::debug_write(b"\n");
}

/// Format to the debug console (no newline).
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {{
        $crate::console::write_fmt(core::format_args!($($arg)*));
    }};
}

/// Format to the debug console with a trailing newline.
#[macro_export]
macro_rules! println {
    () => {{
        $crate::console::println_fmt(core::format_args!(""));
    }};
    ($($arg:tt)*) => {{
        $crate::console::println_fmt(core::format_args!($($arg)*));
    }};
}