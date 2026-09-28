// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa
//
// Demo ring-3 program: proves process execution, syscalls, the heap, and
// IPC against the console server.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;

use nutcracker_rt::ipc::{self, MsgFrame};
use nutcracker_rt::println;
use nutcracker_rt::syscall::{self, EP_CONSOLED};

#[no_mangle]
pub extern "C" fn _start() -> ! {
    println!("[hello] greeting from ring 3");

    let ep = syscall::get_epid();
    println!(
        "[hello] my endpoint is {}, kernel uptime {} ms",
        ep,
        syscall::uptime_ms()
    );

    let mut buf = [0u8; 64];
    let n = syscall::kernel_version(&mut buf);
    let ver = core::str::from_utf8(&buf[..n]).unwrap_or("<invalid>");
    println!("[hello] kernel version = {}", ver);

    // argv and envp are staged on the initial stack for a libc-based program;
    // these two accessors read the same data back over a syscall, which is how
    // a Rust caller without a libc startup gets at them.
    let args = syscall::args();
    println!("[hello] argc = {}", args.len());
    for (i, a) in args.iter().enumerate() {
        let s = core::str::from_utf8(a).unwrap_or("<non-utf8>");
        println!("[hello]   argv[{}] = {:?}", i, s);
    }
    let env = syscall::env();
    println!("[hello] envp has {} entries", env.len());
    for e in env.iter() {
        println!("[hello]   {}", e);
    }

    let mut tick = 0u64;
    let mut last = 0u64;
    loop {
        let now = syscall::uptime_ms();
        if now.saturating_sub(last) >= 500 {
            let text = format!("tick {} @ {} ms", tick, now);
            let mut f = MsgFrame::new();
            f.set_payload(text.as_bytes());
            match ipc::send(EP_CONSOLED, &f) {
                Ok(()) => {}
                Err(e) => println!("[hello] send failed: {}", e),
            }
            last = now;
            tick += 1;
        }
        syscall::yield_now();
    }
}