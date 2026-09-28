// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! `true` / `false` — always succeed / always fail.

use super::Tty;

pub fn cmd_true(_args: &[&str], _out: &mut Tty) -> i32 {
    0
}

pub fn cmd_false(_args: &[&str], _out: &mut Tty) -> i32 {
    1
}