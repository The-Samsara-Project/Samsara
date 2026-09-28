// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! `pwd` — print the working directory.

use core::fmt::Write;
use nutcracker_rt::syscall;

use super::{errstr, Tty};

pub fn main(_args: &[&str], out: &mut Tty) -> i32 {
    match syscall::getcwd() {
        Ok(cwd) => {
            let _ = writeln!(out, "{}", cwd);
            0
        }
        Err(e) => {
            let _ = writeln!(out, "pwd: {}", errstr(e));
            1
        }
    }
}