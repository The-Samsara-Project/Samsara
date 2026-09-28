// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! `help` — list the commands available in samutils.

use core::fmt::Write;

use super::Tty;

pub fn main(_args: &[&str], out: &mut Tty) -> i32 {
    let _ = writeln!(out, "samutils commands:");
    let _ = writeln!(
        out,
        "  ls      list a directory          cat    concatenate files"
    );
    let _ = writeln!(
        out,
        "  cd      change directory          pwd    print working directory"
    );
    let _ = writeln!(
        out,
        "  echo    print text                stat   file metadata"
    );
    let _ = writeln!(
        out,
        "  hexdump hex dump a file (od)      wc     line/word/char counts"
    );
    let _ = writeln!(
        out,
        "  head    first lines of a file     tail   last lines of a file"
    );
    let _ = writeln!(
        out,
        "  grep    filter lines              touch  create empty files"
    );
    let _ = writeln!(
        out,
        "  mkdir   create directories        clear  clear the screen"
    );
    let _ = writeln!(
        out,
        "  uptime  kernel uptime             ver    kernel version"
    );
    let _ = writeln!(
        out,
        "  id      user identity             sleep  sleep a while"
    );
    let _ = writeln!(
        out,
        "  true/false return status           od     alias for hexdump"
    );
    0
}