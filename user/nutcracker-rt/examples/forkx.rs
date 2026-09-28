// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa
//
// Ring-3 demonstration of the fork / exec / waitpid trio:
//
//   1. the parent forks; the child proves it runs by exiting with status 7,
//   2. the parent blocks in waitpid and reaps that exact status,
//   3. the parent exec's a *different* embedded image ("exectst") in place,
//      proving the address space is rebuilt and control transfers to it. The
//      exec target is bounded (it exits 0), so a reaper such as the
//      installer's self-test runner can reap the whole chain successfully.

#![no_std]
#![no_main]

use nutcracker_rt::println;
use nutcracker_rt::syscall;

/// Embedded program table index of the bounded `exectst` image (matches
/// `kernel/src/user.rs`), the replacement for the looping `hello` demo.
const PROG_EXECTST: u64 = 11;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    println!("[forkx] parent alive, pid={}", syscall::get_epid());

    let child = syscall::fork();
    if child < 0 {
        println!("[forkx] fork failed: {}", child);
        syscall::proc_exit_code(1);
    }
    if child == 0 {
        println!(
            "[forkx] child alive, pid={}; returning to fork point with result 0",
            syscall::get_epid()
        );
        syscall::proc_exit_code(7);
    }

    println!("[forkx] parent forked pid={}, waiting", child);
    let mut status = -1;
    match syscall::waitpid(child as u64, &mut status) {
        Ok(st) => println!("[forkx] waitpid reaped status={} (expected 7)", st),
        Err(e) => println!("[forkx] waitpid failed: {}", e),
    }

    // Exec *with* arguments, not without. This is the only exec in the tree that
    // hands a program a non-empty argv, and it is what proves the kernel stages
    // them in order: `exectst` compares what arrived against what was sent and
    // exits non-zero on a mismatch. The three values are chosen to be
    // distinguishable in a log -- one short, one short, one long enough that a
    // reversal is obvious rather than something to squint at.
    let argv: [&str; 4] = ["/exectst", "alpha", "beta", "gamma-with-a-long-name"];
    println!(
        "[forkx] parent exec'ing \"exectst\" image with {} args",
        argv.len() - 1
    );
    match syscall::exec(PROG_EXECTST, Some(&argv)) {
        Ok(()) => unreachable!("exec returned without error"),
        Err(e) => {
            println!("[forkx] exec failed: {}", e);
            syscall::proc_exit_code(1);
        }
    }
}