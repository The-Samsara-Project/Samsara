// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa
//
// samutils: the Nutcracker user-utility suite (busybox-style multi-call).
//
// One static-PIE binary embeds every command; the shell (and any other caller)
// forks, execs this image with an argv whose argv[0] names the command, then
// reaps it with waitpid. All output goes to the session's PTY slave
// (`/dev/pts/0`), from where the kernel line discipline (OPOST) forwards it to
// the terminal emulator on the master side.

#![no_std]
#![no_main]

extern crate alloc;

mod cmd;

use alloc::string::String;
use alloc::vec::Vec;
use nutcracker_rt::syscall::{self, O_RDWR};

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let fd = match syscall::open("/dev/pts/0", O_RDWR, 0) {
        Ok(fd) => fd,
        Err(e) => {
            // No tty to report to; serial is the only remaining sink.
            nutcracker_rt::println!("[samutils] open /dev/pts/0 failed: {}", e);
            syscall::proc_exit_code(1);
        }
    };

    // argv[0] names the command; the rest are its arguments.
    let argv = syscall::args();
    let prog = argv
        .first()
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .unwrap_or_default();
    let rest: Vec<String> = argv[1..]
        .iter()
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    let rest_refs: Vec<&str> = rest.iter().map(|s| s.as_str()).collect();

    let mut out = cmd::Tty { fd };
    let code = cmd::run(&prog, &rest_refs, &mut out);
    let _ = out.flush();
    syscall::proc_exit_code(code as u32);
}