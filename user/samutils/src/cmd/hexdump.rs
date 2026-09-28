// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! `hexdump [FILE...]` (alias `od`) — classic 16-byte hex dump with an ASCII
//! gutter. Reads the whole file; stdin when no file is given.

use core::fmt::Write;
use nutcracker_rt::syscall;

use super::{open_read, read_all, Tty};

fn dump(fd: usize, out: &mut Tty) {
    let data = read_all(fd);
    let mut off = 0usize;
    while off < data.len() {
        let row = &data[off..(off + 16).min(data.len())];
        let _ = write!(out, "{:08x}  ", off);
        for i in 0..16 {
            if i < row.len() {
                let _ = write!(out, "{:02x} ", row[i]);
            } else {
                let _ = out.write_str("   ");
            }
        }
        let _ = out.write_str(" |");
        for &b in row {
            let c = if (0x20..0x7f).contains(&b) { b as char } else { '.' };
            let _ = out.write_str(&alloc::string::String::from(c));
        }
        let _ = writeln!(out, "|");
        off += 16;
    }
}

pub fn main(args: &[&str], out: &mut Tty) -> i32 {
    if args.is_empty() {
        if let Some(fd) = super::open_tty_in() {
            dump(fd, out);
            let _ = syscall::close(fd);
        }
        return 0;
    }
    let mut rc = 0;
    for p in args {
        match open_read("hexdump", p, out) {
            Some(fd) => {
                dump(fd, out);
                let _ = syscall::close(fd);
            }
            None => rc = 1,
        }
    }
    rc
}