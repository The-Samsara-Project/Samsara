// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa
//
// Tiny exec target: reached only via `exec()` from another image. Prints a
// line proving the address space was rebuilt and control transferred, then
// exits 0 so a reaper (e.g. the installer's self-test runner) can reap it.

#![no_std]
#![no_main]

use nutcracker_rt::println;
use nutcracker_rt::syscall;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    println!(
        "[exectst] exec'ed image alive, endpoint {}, exiting 0",
        syscall::get_epid()
    );
    syscall::proc_exit_code(0);
}