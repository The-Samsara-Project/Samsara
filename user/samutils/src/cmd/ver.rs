// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! `ver` — print the kernel version string.

use core::fmt::Write;
use nutcracker_rt::syscall;

use super::Tty;

pub fn main(_args: &[&str], out: &mut Tty) -> i32 {
    let mut buf = [0u8; 128];
    let n = syscall::kernel_version(&mut buf);
    let _ = writeln!(out, "Samsara {}", core::str::from_utf8(&buf[..n]).unwrap_or("?"));
    0
}