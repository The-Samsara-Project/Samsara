// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa
//
// The native PTY shell. It runs on the slave side (`/dev/pts0`) and re-arms a
// sane termios at startup: the installer left the PTY raw, and this shell
// wants the kernel's line discipline (drivers/pty, modelled on zinnia's
// device/tty) to do echo, VERASE/VKILL editing and line completion, so it
// reads whole lines and runs them. Incoming escape sequences (arrow keys) are
// dropped so they never end up in a command line.
//
// Commands live OUTSIDE the shell: the fork/exec/waitpid trio runs the
// `samutils` multi-call binary (PROG_SAMUTILS) with an argv whose argv[0]
// names the command and whose remaining entries are its arguments. Only `cd`
// (which must change the shell's own working directory) and `exit` are
// builtins. The prompt is bash/zsh-flavoured: a green `user@host`-style label
// and a blue working directory.
//
// Everything it displays is written *back to the slave*; from there it travels
// through the discipline (OPOST) to the master, where fbterm reads it
// emulator renders it. Nothing here talks to the console directly.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::{self, Write};

use nutcracker_rt::println;
use nutcracker_rt::syscall::{self, poll_events, PollFd};

const MAX_LINE: usize = 512;
/// Index of the samutils multi-call binary (matches kernel `PROG_SAMUTILS`).
const PROG_SAMUTILS: u64 = 12;

/// Write the whole slice to `fd`, tolerating short writes.
fn write_all(fd: usize, bytes: &[u8]) {
    let mut off = 0;
    while off < bytes.len() {
        match syscall::write(fd, &bytes[off..]) {
            Ok(0) => break,
            Ok(n) => off += n,
            Err(_) => break,
        }
    }
}

/// `core::fmt::Write` sink that pushes formatted text to the slave.
struct SlaveWriter {
    fd: usize,
}

impl Write for SlaveWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        write_all(self.fd, s.as_bytes());
        Ok(())
    }
}

fn errstr(e: i64) -> &'static str {
    match e {
        -1 => "permission denied",
        -2 => "no such file or directory",
        -13 => "permission denied",
        -20 => "not a directory",
        -21 => "is a directory",
        -22 => "invalid argument",
        _ => "error",
    }
}

/// Print a `user`-style prompt with the current working directory, bash/zsh
/// fashion: `samsara:/cwd$ ` with the label green and the path blue.
fn prompt(fd: usize) {
    let cwd = syscall::getcwd().unwrap_or_else(|_| String::from("/"));
    let mut w = SlaveWriter { fd };
    let _ = write!(w, "\x1b[1;32msamsara\x1b[0m:");
    let _ = write!(w, "\x1b[1;34m{}\x1b[0m", cwd);
    if syscall::getuid() == 0 {
        let _ = write!(w, "# ");
    } else {
        let _ = write!(w, "$ ");
    }
}

/// Restore a normal interactive tty on the slave: canonical editing with echo
/// and signal keys (ISIG | ICANON | ECHO | ECHOE | ECHOK), CR->NL input
/// translation (ICRNL) and NL->CRNL output translation (OPOST | ONLCR), so the
/// kernel discipline does the echoing/editing and lines arrive whole.
fn set_tty(fd: usize) {
    let mut t = core::mem::MaybeUninit::<syscall::Termios>::uninit();
    let mut t = unsafe { t.assume_init() };
    if syscall::ioctl(fd, syscall::TCGETS, &mut t).is_err() {
        return;
    }
    t.c_lflag |= syscall::ISIG | syscall::ICANON | syscall::ECHO | syscall::ECHOE | syscall::ECHOK;
    t.c_iflag |= syscall::ICRNL;
    t.c_oflag |= syscall::OPOST | syscall::ONLCR;
    let _ = syscall::ioctl(fd, syscall::TCSETS, &mut t);
}

/// Split a command line into whitespace-separated words.
fn tokenize(line: &str) -> Vec<String> {
    line.split_whitespace().map(String::from).collect()
}

/// Run one command line: either a builtin (cd / exit) or an external command
/// via fork -> exec(samutils, argv) -> waitpid.
fn run_line(fd: usize, line: &[u8]) {
    let s = match core::str::from_utf8(line) {
        Ok(s) => s.trim(),
        Err(_) => return,
    };
    if s.is_empty() {
        return;
    }
    let words = tokenize(s);
    let cmd = words[0].as_str();

    match cmd {
        // Builtins: must change this process's own state.
        "cd" => {
            let target = words.get(1).map(|s| s.as_str()).unwrap_or("/");
            match syscall::chdir(target) {
                Ok(()) => {}
                Err(e) => {
                    let mut w = SlaveWriter { fd };
                    let _ = write!(w, "sh: cd: {}: {}\n", target, errstr(e));
                }
            }
        }
        "exit" | "quit" => {
            write_all(fd, b"bye\n");
            syscall::proc_exit_code(0);
        }
        _ => run_external(fd, cmd, &words),
    }
}

/// Fork, exec the utility binary with the given argv in the child, and reap.
fn run_external(fd: usize, cmd: &str, words: &[String]) {
    // argv[0] is the command name (samutils dispatches on it), like bash.
    let argv: Vec<&str> = words.iter().map(|w| w.as_str()).collect();

    match syscall::fork() {
        // Child: replace this image with samutils. Only exec failure returns.
        0 => {
            if let Err(e) = syscall::exec(PROG_SAMUTILS, Some(&argv)) {
                let mut w = SlaveWriter { fd };
                let _ = write!(w, "sh: {}: {}\n", cmd, errstr(e));
                syscall::proc_exit_code(127);
            }
            unreachable!()
        }
        // Parent: wait for the command to finish, then reprompt.
        pid if pid > 0 => {
            let mut status = 0i32;
            let _ = syscall::waitpid(pid as u64, &mut status);
        }
        // Fork failed.
        _ => {
            let mut w = SlaveWriter { fd };
            let _ = write!(w, "sh: fork failed\n");
        }
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    println!("[sh] shell starting");

    let fd = match syscall::open("/dev/pts0", syscall::O_RDWR, 0) {
        Ok(fd) => fd,
        Err(e) => {
            println!("[sh] open /dev/pts0 failed: {}", e);
            syscall::proc_exit_code(1);
        }
    };

    // The installer ran the slave raw; put it back into canonical mode so the
    // kernel discipline echoes, edits and delivers whole lines.
    set_tty(fd);

    write_all(fd, b"\x1b[1;36mSamsara shell\x1b[0m - try `help`, `ls /dev` or `cat /etc/init.conf`\n");
    prompt(fd);

    let mut p = [PollFd {
        fd: fd as i32,
        events: poll_events::POLLIN,
        revents: 0,
    }];

    // Line state persists across reads: canonical mode may deliver a line in
    // several chunks (long lines) and raw escape sequences can straddle reads,
    // so `line`/`len`/`esc` live outside the poll loop.
    let mut line = [0u8; MAX_LINE];
    let mut len = 0usize;
    let mut esc = 0u8;

    loop {
        match syscall::poll(&mut p, 20) {
            Ok(_) => {}
            Err(_) => {
                syscall::yield_now();
                continue;
            }
        }
        if p[0].revents & poll_events::POLLIN == 0 {
            continue;
        }

        let mut tmp = [0u8; 64];
        let n = match syscall::read(fd, &mut tmp) {
            Ok(n) => n,
            Err(_) => continue,
        };

        // Escape-sequence filter: swallow `ESC [ X` / `ESC O X` (arrows...).
        for &b in &tmp[..n] {
            match esc {
                0 if b == 0x1b => esc = 1,
                1 => esc = if b == b'[' || b == b'O' { 2 } else { 0 },
                2 => esc = 0,
                0 => match b {
                    b'\n' | b'\r' => {
                        if len > 0 {
                            run_line(fd, &line[..len]);
                        }
                        len = 0;
                        prompt(fd);
                    }
                    b if b >= 0x20 && b < 0x7f => {
                        if len < MAX_LINE {
                            line[len] = b;
                            len += 1;
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }
}