// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa
//
// Command dispatcher and shared plumbing for samutils: the tty sink every
// command writes through, whole-file helpers, and the name -> function table.

pub mod cat;
pub mod cd;
pub mod clear;
pub mod echo;
pub mod grep;
pub mod head;
pub mod hexdump;
pub mod help;
pub mod id;
pub mod ls;
pub mod mkdir;
pub mod pwd;
pub mod stat;
pub mod sleep;
pub mod tail;
pub mod touch;
pub mod truefalse;
pub mod uptime;
pub mod ver;
pub mod wc;

use core::fmt::{self, Write};
use nutcracker_rt::syscall::{self, O_RDONLY};

/// `core::fmt::Write` sink that pushes formatted text to the PTY slave.
pub struct Tty {
    pub fd: usize,
}

impl Tty {
    /// No-op: writes are unbuffered and flush as they go.
    pub fn flush(&mut self) -> fmt::Result {
        Ok(())
    }
}

impl Write for Tty {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        write_all(self.fd, s.as_bytes());
        Ok(())
    }
}

/// Write the whole slice to `fd`, tolerating short writes.
pub fn write_all(fd: usize, bytes: &[u8]) {
    let mut off = 0;
    while off < bytes.len() {
        match syscall::write(fd, &bytes[off..]) {
            Ok(0) => break,
            Ok(n) => off += n,
            Err(_) => break,
        }
    }
}

/// Read an entire file (or byte stream) into memory.
pub fn read_all(fd: usize) -> alloc::vec::Vec<u8> {
    let mut out = alloc::vec::Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        match syscall::read(fd, &mut tmp) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&tmp[..n]),
            Err(_) => break,
        }
    }
    out
}

/// Open a path for reading, printing `who: path: <reason>` on failure.
pub fn open_read(who: &str, path: &str, out: &mut Tty) -> Option<usize> {
    match syscall::open(path, O_RDONLY, 0) {
        Ok(fd) => Some(fd),
        Err(e) => {
            let _ = writeln!(out, "{}: {}: {}", who, path, errstr(e));
            None
        }
    }
}

/// Open the tty for reading (used by `cat`/`grep`/`wc` with no file args).
pub fn open_tty_in() -> Option<usize> {
    syscall::open("/dev/pts0", O_RDONLY, 0).ok()
}

/// Human-readable rendition of a (negative) kernel errno.
pub fn errstr(e: i64) -> &'static str {
    match e {
        -1 => "permission denied",
        -2 => "no such file or directory",
        -5 => "i/o error",
        -9 => "bad file descriptor",
        -11 => "resource temporarily unavailable",
        -12 => "out of memory",
        -13 => "permission denied",
        -17 => "file exists",
        -20 => "not a directory",
        -21 => "is a directory",
        -22 => "invalid argument",
        -28 => "no space left",
        -32 => "broken pipe",
        -38 => "operation not supported",
        _ => "error",
    }
}

/// One command: `fn(&args, &mut tty) -> exit code`.
type Run = fn(&[&str], &mut Tty) -> i32;

const COMMANDS: &[(&str, Run)] = &[
    ("ls", ls::main),
    ("cat", cat::main),
    ("cd", cd::main),
    ("pwd", pwd::main),
    ("echo", echo::main),
    ("clear", clear::main),
    ("ver", ver::main),
    ("uptime", uptime::main),
    ("id", id::main),
    ("stat", stat::main),
    ("hexdump", hexdump::main),
    ("od", hexdump::main),
    ("wc", wc::main),
    ("head", head::main),
    ("tail", tail::main),
    ("touch", touch::main),
    ("mkdir", mkdir::main),
    ("grep", grep::main),
    ("sleep", sleep::main),
    ("true", truefalse::cmd_true),
    ("false", truefalse::cmd_false),
    ("help", help::main),
];

/// Dispatch `argv[0]` to its implementation.
pub fn run(prog: &str, args: &[&str], out: &mut Tty) -> i32 {
    for (name, f) in COMMANDS {
        if *name == prog {
            return f(args, out);
        }
    }
    let _ = writeln!(out, "samutils: {}: command not found", prog);
    127
}