// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa
//
// Nutcracker ring-3 runtime library. Every userspace process links this crate:
// it provides the raw `syscall` ABI wrappers, the `MsgFrame` IPC wire format,
// a pure-Rust heap backed by `MAP_ANON`, serial console logging, and the
// process-wide panic handler / global allocator.

#![no_std]

pub mod alloc;
pub mod console;
pub mod ipc;
pub mod fb;
pub mod syscall;

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    use core::fmt::Write;
    let mut s = console::Serial;
    let _ = write!(s, "\n[rt] PANIC: {}", info);
    syscall::proc_exit()
}

#[global_allocator]
pub static HEAP: alloc::Heap = alloc::Heap;