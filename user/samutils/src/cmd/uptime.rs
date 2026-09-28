// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! `uptime` — how long the kernel has been running.

use core::fmt::Write;
use nutcracker_rt::syscall;

use super::Tty;

pub fn main(_args: &[&str], out: &mut Tty) -> i32 {
    let ms = syscall::uptime_ms();
    let _ = writeln!(out, "up {} ms", ms);
    0
}