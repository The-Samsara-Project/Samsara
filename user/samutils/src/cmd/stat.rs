// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! `stat [PATH...]` — print file metadata.

use core::fmt::Write;
use nutcracker_rt::syscall::{self, Stat};

use super::{errstr, Tty};

fn kind_name(kind: u32) -> &'static str {
    match kind {
        0 => "file",
        1 => "directory",
        2 => "character device",
        3 => "pipe",
        _ => "unknown",
    }
}

pub fn main(args: &[&str], out: &mut Tty) -> i32 {
    if args.is_empty() {
        let _ = writeln!(out, "stat: missing operand");
        return 1;
    }
    let mut rc = 0;
    for p in args {
        let mut st = Stat {
            mode: 0,
            uid: 0,
            gid: 0,
            kind: 0,
            size: 0,
        };
        match syscall::stat(p, &mut st) {
            Ok(()) => {
                let _ = writeln!(out, "  File: {}", p);
                let _ = writeln!(out, "  Size: {:<8} Kind: {}", st.size, kind_name(st.kind));
                let _ = writeln!(
                    out,
                    "  Mode: {:#06o}   Uid: {}   Gid: {}",
                    st.mode & 0o7777,
                    st.uid,
                    st.gid
                );
            }
            Err(e) => {
                let _ = writeln!(out, "stat: {}: {}", p, errstr(e));
                rc = 1;
            }
        }
    }
    rc
}