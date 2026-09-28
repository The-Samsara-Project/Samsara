// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! `mkdir [DIR...]` — create directories.

use core::fmt::Write;
use nutcracker_rt::syscall;

use super::{errstr, Tty};

pub fn main(args: &[&str], out: &mut Tty) -> i32 {
    if args.is_empty() {
        let _ = writeln!(out, "mkdir: missing operand");
        return 1;
    }
    let mut rc = 0;
    for p in args {
        match syscall::mkdir(p, 0o777) {
            Ok(()) => {}
            Err(e) => {
                let _ = writeln!(out, "mkdir: {}: {}", p, errstr(e));
                rc = 1;
            }
        }
    }
    rc
}