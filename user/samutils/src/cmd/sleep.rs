// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! `sleep SECONDS` — pause for a whole number of seconds. Interrupted by any
//! caught signal (the kernel returns `EINTR`).

use core::fmt::Write;
use nutcracker_rt::syscall;

use super::errstr;
use super::Tty;

pub fn main(args: &[&str], out: &mut Tty) -> i32 {
    let secs: u64 = match args.first().and_then(|a| a.parse().ok()) {
        Some(s) => s,
        None => {
            let _ = writeln!(out, "sleep: usage: sleep SECONDS");
            return 1;
        }
    };
    match syscall::nanosleep(secs.saturating_mul(1000)) {
        Ok(()) => 0,
        Err(e) => {
            let _ = writeln!(out, "sleep: {}", errstr(e));
            1
        }
    }
}