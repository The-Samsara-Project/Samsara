// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! `echo [-n] [TEXT...]` — print its arguments.

use core::fmt::Write;

use super::Tty;

pub fn main(args: &[&str], out: &mut Tty) -> i32 {
    let mut newline = true;
    let mut rest = args;
    if let Some(first) = rest.first() {
        if *first == "-n" {
            newline = false;
            rest = &rest[1..];
        }
    }
    for (i, a) in rest.iter().enumerate() {
        if i > 0 {
            let _ = out.write_str(" ");
        }
        let _ = out.write_str(a);
    }
    if newline {
        let _ = out.write_str("\n");
    }
    0
}