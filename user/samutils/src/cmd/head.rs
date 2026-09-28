// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! `head [-n N] [FILE...]` — print the first N lines (default 10) of each
//! file, or of stdin when no file is given.

use core::fmt::Write;
use nutcracker_rt::syscall;

use super::{open_read, read_all, write_all, Tty};

fn print_n(fd: usize, n: usize, out: &mut Tty) {
    let data = read_all(fd);
    let mut lines = 0usize;
    let mut end = data.len();
    // Find where the Nth newline falls.
    for (i, &b) in data.iter().enumerate() {
        if b == b'\n' {
            lines += 1;
            if lines >= n {
                end = i + 1;
                break;
            }
        }
    }
    write_all(out.fd, &data[..end]);
}

pub fn main(args: &[&str], out: &mut Tty) -> i32 {
    let mut n = 10usize;
    let mut files: alloc::vec::Vec<&str> = alloc::vec::Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        let a = args[i];
        if a == "-n" {
            i += 1;
            if i >= args.len() {
                let _ = writeln!(out, "head: option '-n' requires an argument");
                return 1;
            }
            n = match args[i].parse() {
                Ok(v) => v,
                Err(_) => {
                    let _ = writeln!(out, "head: invalid number '{}'", args[i]);
                    return 1;
                }
            };
        } else if a.starts_with('-') && a.len() > 1 && a[1..].chars().all(|c| c.is_ascii_digit()) {
            n = a[1..].parse().unwrap_or(10);
        } else {
            files.push(a);
        }
        i += 1;
    }

    if files.is_empty() {
        if let Some(fd) = super::open_tty_in() {
            print_n(fd, n, out);
            let _ = syscall::close(fd);
        }
        return 0;
    }

    let mut rc = 0;
    let multi = files.len() > 1;
    for (idx, p) in files.iter().enumerate() {
        if multi {
            let _ = writeln!(out, "==> {} <==", p);
        }
        match open_read("head", p, out) {
            Some(fd) => {
                print_n(fd, n, out);
                let _ = syscall::close(fd);
            }
            None => rc = 1,
        }
        if multi && idx + 1 < files.len() {
            let _ = writeln!(out);
        }
    }
    rc
}