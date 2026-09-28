// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! `clear` — clear the terminal screen (home the cursor too).

use super::{write_all, Tty};

pub fn main(_args: &[&str], out: &mut Tty) -> i32 {
    write_all(out.fd, b"\x1b[2J\x1b[H");
    0
}