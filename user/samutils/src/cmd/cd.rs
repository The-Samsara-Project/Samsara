// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! `cd [DIR]` — change the working directory.
//!
//! The interactive shell implements `cd` itself (a directory change must apply
//! to the shell, which persists across commands); this binary exists so the
//! operation is also available to scripts and keeps the syscall exercised.

use core::fmt::Write;
use nutcracker_rt::syscall::{self};

use super::{errstr, Tty};

pub fn main(args: &[&str], out: &mut Tty) -> i32 {
    let target = args.first().copied().unwrap_or("/");
    match syscall::chdir(target) {
        Ok(()) => 0,
        Err(e) => {
            let _ = writeln!(out, "cd: {}: {}", target, errstr(e));
            1
        }
    }
}