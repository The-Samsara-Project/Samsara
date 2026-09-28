// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! `wc [FILE...]` — count newlines, words and bytes. Reads the whole file;
//! stdin when no file is given.

use core::fmt::Write;
use nutcracker_rt::syscall;

use super::{open_read, read_all, Tty};

fn count(data: &[u8]) -> (usize, usize, usize) {
    let mut lines = 0usize;
    let mut words = 0usize;
    let mut in_word = false;
    for &b in data {
        if b == b'\n' {
            lines += 1;
        }
        if b.is_ascii_whitespace() {
            in_word = false;
        } else if !in_word {
            in_word = true;
            words += 1;
        }
    }
    (lines, words, data.len())
}

fn fmt_counts(lines: usize, words: usize, bytes: usize, name: &str, l: bool, w: bool, b: bool) -> alloc::string::String {
    let mut line = alloc::string::String::new();
    if l {
        let _ = core::fmt::write(&mut line, format_args!("{:>7} ", lines));
    }
    if w {
        let _ = core::fmt::write(&mut line, format_args!("{:>7} ", words));
    }
    if b {
        let _ = core::fmt::write(&mut line, format_args!("{:>7} ", bytes));
    }
    line.push_str(name);
    line
}

fn report(fd: usize, name: &str, out: &mut Tty, l: bool, w: bool, b: bool) {
    let data = read_all(fd);
    let (lines, words, bytes) = count(&data);
    let _ = writeln!(out, "{}", fmt_counts(lines, words, bytes, name, l, w, b));
}

pub fn main(args: &[&str], out: &mut Tty) -> i32 {
    let mut l = false;
    let mut w = false;
    let mut b = false;
    let mut files: alloc::vec::Vec<&str> = alloc::vec::Vec::new();
    let mut flags_seen = false;
    for a in args {
        if a.len() > 1 && a.starts_with('-') {
            flags_seen = true;
            for ch in a[1..].chars() {
                match ch {
                    'l' => l = true,
                    'w' => w = true,
                    'c' => b = true,
                    _ => {
                        let _ = writeln!(out, "wc: invalid option '{}'", ch);
                        return 1;
                    }
                }
            }
        } else {
            files.push(a);
        }
    }
    if !flags_seen || (!l && !w && !b) {
        l = true;
        w = true;
        b = true;
    }
    if files.is_empty() {
        if let Some(fd) = super::open_tty_in() {
            report(fd, "", out, l, w, b);
            let _ = syscall::close(fd);
        }
        return 0;
    }
    let mut total = (0usize, 0usize, 0usize);
    let mut rc = 0;
    let mut any = false;
    for p in &files {
        match open_read("wc", p, out) {
            Some(fd) => {
                let data = read_all(fd);
                let (tl, tw, tb) = count(&data);
                total.0 += tl;
                total.1 += tw;
                total.2 += tb;
                let _ = writeln!(out, "{}", fmt_counts(tl, tw, tb, p, l, w, b));
                let _ = syscall::close(fd);
                any = true;
            }
            None => rc = 1,
        }
    }
    if any && files.len() > 1 && (l || w || b) {
        let _ = writeln!(out, "{}", fmt_counts(total.0, total.1, total.2, "total", l, w, b));
    }
    rc
}