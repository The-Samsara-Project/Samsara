// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! `touch [FILE...]` — create empty files. (Samsara has no modification-time
//! metadata yet, so touch only guarantees the file exists.)

use core::fmt::Write;
use nutcracker_rt::syscall::{self, O_CREAT, O_WRONLY};

use super::{errstr, Tty};

pub fn main(args: &[&str], out: &mut Tty) -> i32 {
    if args.is_empty() {
        let _ = writeln!(out, "touch: missing operand");
        return 1;
    }
    let mut rc = 0;
    for p in args {
        match syscall::open(p, O_CREAT | O_WRONLY, 0o666) {
            Ok(fd) => {
                let _ = syscall::close(fd);
            }
            Err(e) => {
                let _ = writeln!(out, "touch: {}: {}", p, errstr(e));
                rc = 1;
            }
        }
    }
    rc
}