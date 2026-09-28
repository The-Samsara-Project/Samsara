// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! `grep PATTERN [FILE...]` — print lines containing the literal substring
//! `PATTERN`. Reads stdin when no file is given; prefixes `name:` when more
//! than one file is searched.

use core::fmt::Write;
use nutcracker_rt::syscall;

use super::{open_read, read_all, write_all, Tty};

fn lc(b: u8) -> u8 {
    if b.is_ascii_uppercase() {
        b + 32
    } else {
        b
    }
}

fn contains(line: &[u8], pat: &[u8], insensitive: bool) -> bool {
    if pat.is_empty() {
        return true;
    }
    if insensitive {
        line.windows(pat.len()).any(|w| {
            w.iter()
                .zip(pat.iter())
                .all(|(a, b)| lc(*a) == lc(*b))
        })
    } else {
        line.windows(pat.len()).any(|w| w == pat)
    }
}

fn search(data: &[u8], pat: &[u8], insensitive: bool, prefix: Option<&str>, out: &mut Tty) {
    let mut start = 0usize;
    for (i, &b) in data.iter().enumerate() {
        if b == b'\n' {
            if contains(&data[start..i], pat, insensitive) {
                if let Some(p) = prefix {
                    let _ = write!(out, "{}:", p);
                }
                write_all(out.fd, &data[start..i]);
                let _ = out.write_str("\n");
            }
            start = i + 1;
        }
    }
    // Trailing partial line without a newline.
    if start < data.len() && contains(&data[start..], pat, insensitive) {
        if let Some(p) = prefix {
            let _ = write!(out, "{}:", p);
        }
        write_all(out.fd, &data[start..]);
        let _ = out.write_str("\n");
    }
}

pub fn main(args: &[&str], out: &mut Tty) -> i32 {
    if args.is_empty() {
        let _ = writeln!(out, "grep: usage: grep [-i] PATTERN [FILE...]");
        return 1;
    }
    let mut insensitive = false;
    let mut pat_idx = 0usize;
    if args[0] == "-i" {
        if args.len() < 2 {
            let _ = writeln!(out, "grep: usage: grep [-i] PATTERN [FILE...]");
            return 1;
        }
        insensitive = true;
        pat_idx = 1;
    }
    let pat = args[pat_idx];
    let files = &args[pat_idx + 1..];
    let multi = files.len() > 1;

    if files.is_empty() {
        match super::open_tty_in() {
            Some(fd) => {
                let data = read_all(fd);
                search(&data, pat.as_bytes(), insensitive, None, out);
                let _ = syscall::close(fd);
            }
            None => return 1,
        }
        return 0;
    }

    let mut rc = 0;
    for p in files {
        match open_read("grep", p, out) {
            Some(fd) => {
                let data = read_all(fd);
                search(
                    &data,
                    pat.as_bytes(),
                    insensitive,
                    if multi { Some(p) } else { None },
                    out,
                );
                let _ = syscall::close(fd);
            }
            None => rc = 1,
        }
    }
    rc
}