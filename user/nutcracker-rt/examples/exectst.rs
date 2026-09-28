// SPDX-License-Identifier: GPL-2.0-or-later
// Copyright (C) 2026 Harsh Nikarsa
//
// Exec target: reached only via `exec()` from another image. Two jobs, both
// about the argument vector:
//
//   1. prove the address space was rebuilt and control transferred, and
//   2. prove the *order* of the argv the kernel staged.
//
// The second is why this image is exec'd with arguments at all. Nothing else in
// the tree spawns a program with a non-empty argv, so a program that reads its
// own arguments was untested -- and the kernel lays the initial stack out by
// walking *down* it, one word per push, which makes the call order the reverse
// of the memory order. Getting that wrong stages argv back to front with the
// NULL terminator at the wrong end, and nothing crashes: mlibc reads argc off the
// top word, skips argc words, and finds an argument pointer where the
// terminator belongs. A program then sees its arguments reversed and walks off
// the end of its own argv.
//
// The symptom when that happened pointed somewhere else entirely. busybox was
// handed `[/bin/busybox --install -s /bin]`, took argv[0] as "/bin", and exited
// 127 with "bin: applet not found" -- which reads as a problem in the applet
// table and sent the search in the wrong direction for a while.
//
// So this compares against what was sent, not merely against a count. A count is
// exactly the thing that stayed right while the order was wrong.

#![no_std]
#![no_main]

use nutcracker_rt::println;
use nutcracker_rt::syscall;

/// What `forkx` execs this image with. Duplicated rather than shared: a
/// `no_std` example binary has no crate to import a constant from, and a copy
/// that can disagree with the sender is caught by the very first run of this
/// check -- which is the point of having the check.
const EXPECTED: [&str; 3] = ["alpha", "beta", "gamma-with-a-long-name"];

#[no_mangle]
pub extern "C" fn _start() -> ! {
    println!(
        "[exectst] exec'ed image alive, endpoint {}, exiting 0",
        syscall::get_epid()
    );

    let args = syscall::args();
    if args.len() != EXPECTED.len() + 1 {
        println!(
            "[exectst] argv count {} (want {})",
            args.len(),
            EXPECTED.len() + 1
        );
        syscall::proc_exit_code(1);
    }

    // argv[0] is the program name, chosen by whoever spawned this; only the
    // arguments after it have a known value.
    for (i, want) in EXPECTED.iter().enumerate() {
        let got = core::str::from_utf8(&args[i + 1]).unwrap_or("<not utf-8>");
        if got != *want {
            println!(
                "[exectst] argv[{}] = {:?}, want {:?} -- order or content wrong",
                i + 1,
                got,
                want
            );
            syscall::proc_exit_code(1);
        }
    }
    println!("[exectst] argv order and contents correct: {} args", EXPECTED.len());
    syscall::proc_exit_code(0);
}
