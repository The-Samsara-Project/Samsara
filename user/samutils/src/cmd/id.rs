// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! `id` — print the real/effective user and group ids.

use core::fmt::Write;
use nutcracker_rt::syscall;

use super::Tty;

fn name(uid: u32) -> &'static str {
    match uid {
        0 => "root",
        1000 => "user",
        _ => "",
    }
}

pub fn main(_args: &[&str], out: &mut Tty) -> i32 {
    let uid = syscall::getuid();
    let gid = syscall::getgid();
    let un = name(uid);
    let gn = name(gid);
    if !un.is_empty() {
        let _ = write!(out, "uid={}({})", uid, un);
    } else {
        let _ = write!(out, "uid={}", uid);
    }
    if !gn.is_empty() {
        let _ = writeln!(out, " gid={}({})", gid, gn);
    } else {
        let _ = writeln!(out, " gid={}", gid);
    }
    0
}