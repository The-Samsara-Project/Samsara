// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa
//
// Raw `syscall` ABI. Numbers here MUST match `abi/nr` in the kernel and
// `docs/ABI.md`; the list is append-only.

extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::arch::asm;

/// Established (pre-microkernel) syscalls, all stable.
pub const DEBUG_WRITE: u64 = 0;
pub const YIELD: u64 = 1;
pub const CLOCK_UPTIME_MS: u64 = 2;
pub const KERNEL_VERSION: u64 = 3;
pub const EXIT: u64 = 4;
pub const OPEN: u64 = 5;
pub const CLOSE: u64 = 6;
pub const READ: u64 = 7;
pub const WRITE: u64 = 8;

/// Microkernel surface: IPC, processes, interrupts and memory grants.
pub const IPC_SEND: u64 = 9;
pub const IPC_RECV: u64 = 10;
pub const IPC_REPLY: u64 = 11;
pub const IPC_CALL: u64 = 12;
pub const PROC_SPAWN: u64 = 13;
pub const PROC_EXIT: u64 = 14;
pub const GET_EPID: u64 = 15;
pub const PORT_ALLOW: u64 = 16;
pub const IRQ_BIND: u64 = 17;
pub const IRQ_UNBIND: u64 = 18;
pub const MAP_ANON: u64 = 19;
pub const MAP_PHYS: u64 = 20;
pub const DMA_ALLOC: u64 = 21;
pub const DMA_FREE: u64 = 22;
pub const FORK: u64 = 23;
pub const EXEC: u64 = 24;
pub const WAITPID: u64 = 25;
pub const PIPE: u64 = 26;
pub const STAT: u64 = 27;
pub const CHMOD: u64 = 28;
pub const CHOWN: u64 = 29;
pub const KILL: u64 = 30;
pub const GETUID: u64 = 31;
pub const GETGID: u64 = 32;
pub const GETEUID: u64 = 33;
pub const GETEGID: u64 = 34;
pub const GETRESUID: u64 = 35;
pub const GETRESGID: u64 = 36;
pub const GETGROUPS: u64 = 37;
pub const SETGROUPS: u64 = 38;
pub const SETUID: u64 = 39;
pub const SETGID: u64 = 40;
pub const SETEUID: u64 = 41;
pub const SETEGID: u64 = 42;
pub const SETREUID: u64 = 43;
pub const SETREGID: u64 = 44;
pub const SETRESUID: u64 = 45;
pub const SETRESGID: u64 = 46;
pub const SETFSUID: u64 = 47;
pub const SETFSGID: u64 = 48;
pub const UMASK: u64 = 49;
pub const NANOSLEEP: u64 = 50;
pub const SIGACTION: u64 = 51;
pub const SIGPROCMASK: u64 = 52;
pub const SIGSUSPEND: u64 = 53;
pub const SIGALTSTACK: u64 = 54;
pub const SIGRETURN: u64 = 55;
pub const SIGPENDING: u64 = 56;
pub const GET_PID: u64 = 57;
/// Poll descriptors for readiness:
/// `(fds: ptr<PollFd>, nfds, timeout_ms)` -> number ready.
/// `timeout_ms` is signed: `-1` blocks forever, `0` polls now, `>0` waits up
/// to that many milliseconds.
pub const POLL: u64 = 58;
pub const IOCTL: u64 = 59;
/// Query the active framebuffer geometry into an `FbInfo`.
pub const FB_INFO: u64 = 60;
/// Hand the framebuffer to this process: stop the kernel text console drawing.
pub const CONSOLE_DETACH: u64 = 61;
/// `(oldfd, newfd) -> newfd`. Duplicate a descriptor onto a specific slot,
/// sharing its file offset. `oldfd == newfd` succeeds and changes nothing.
pub const DUP2: u64 = 89;
/// Hand the console back to the kernel (`CONSOLE_ATTACH`).
pub const CONSOLE_ATTACH: u64 = 62;
/// Copy this process's argument vector: an `argc` word (u64 LE) followed by
/// the NUL-terminated strings. `(buf, cap) -> bytes`; null buf / zero cap
/// returns the required size.
pub const GET_ARGS: u64 = 63;
/// Fetch the calling process's environment, encoded exactly like `GET_ARGS`: a
/// count word followed by NUL-terminated `NAME=value` strings.
pub const GET_ENV: u64 = 71;
/// Put `pid` into process group `pgid`.
pub const SETPGID: u64 = 72;
/// The calling process's process group id.
pub const GETPGRP: u64 = 73;
/// The process group of `pid`.
pub const GETPGID: u64 = 74;
/// Create a new session led by the caller.
pub const SETSID: u64 = 75;
/// The session of `pid`.
pub const GETSID: u64 = 76;
/// Change this process's working directory: `(path, path_len) -> 0`.
pub const CHDIR: u64 = 64;
/// Copy this process's working directory: `(buf, cap) -> bytes`; null buf /
/// zero cap returns the required size (excluding the trailing NUL).
pub const GETCWD: u64 = 65;
/// List a directory's children: `(path, path_len, buf, cap) -> bytes`. `buf`
/// receives the entry names as NUL-terminated strings; null buf / zero cap
/// queries the required size.
pub const READDIR: u64 = 66;
/// Create a directory: `(path, path_len, mode) -> 0`.
pub const MKDIR: u64 = 67;
/// Create a shared-memory region of `frames` pages mapped into the caller:
/// `(frames) -> shared handle`.
pub const SHM_CREATE: u64 = 68;
/// Map an existing shared-memory region by handle: `(handle) -> virtual base`.
pub const SHM_MAP: u64 = 69;
/// Drop the caller's mapping of a shared-memory region; frames are freed when
/// the last mapping goes away: `(handle) -> 0`.
pub const SHM_DESTROY: u64 = 70;

/// `MAP_PHYS` mapping flags (mirror the kernel's `vmm` bits).
pub mod map_flags {
    /// The page is writable.
    pub const WRITABLE: u64 = 1 << 1;
    /// Write-through caching (for MMIO such as a framebuffer).
    pub const WRITE_THROUGH: u64 = 1 << 3;
    /// Disable caching entirely (MMIO).
    pub const CACHE_DISABLE: u64 = 1 << 4;
    /// The page is not executable.
    pub const NO_EXECUTE: u64 = 1 << 63;
}

/// `poll` event bits reported in `PollFd::revents` (and requested in
/// `PollFd::events`); must match `driver_common` in the kernel.
pub mod poll_events {
    /// Data can be read without blocking (EOF also sets `POLLIN`).
    pub const POLLIN: u16 = 0x001;
    /// Data can be written without blocking.
    pub const POLLOUT: u16 = 0x004;
    /// Exceptional condition (e.g. a pipe write end with no reader).
    pub const POLLERR: u16 = 0x008;
    /// The stream was hung up (e.g. the last writer closed).
    pub const POLLHUP: u16 = 0x010;
    /// The descriptor is not open; always reported, never requested.
    pub const POLLNVAL: u16 = 0x020;
}

/// One `poll` descriptor, matching the kernel's `PollFd` layout exactly.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct PollFd {
    /// Descriptor to poll; negative entries are skipped.
    pub fd: i32,
    /// Requested `poll_events` bits.
    pub events: u16,
    /// Result bits filled in by the kernel.
    pub revents: u16,
}

/// Poll `fds` for readiness. `timeout_ms` is signed: `-1` blocks indefinitely,
/// `0` returns immediately, `>0` is an upper bound. On success, `revents` in
/// each entry is filled in and the number of ready descriptors is returned.
pub fn poll(fds: &mut [PollFd], timeout_ms: i64) -> Result<usize, i64> {
    // SAFETY: the kernel validates the range and writes at most `revents`
    // within each entry of our own mapped slice.
    let r = unsafe {
        raw(
            POLL,
            fds.as_mut_ptr() as u64,
            fds.len() as u64,
            timeout_ms as u64,
            0,
            0,
            0,
        )
    };
    if r < 0 {
        Err(r)
    } else {
        Ok(r as usize)
    }
}

/// `open` access modes (low flag bits).
pub const O_RDONLY: u64 = 0;
pub const O_WRONLY: u64 = 1;
pub const O_RDWR: u64 = 2;
pub const O_CREAT: u64 = 0o100;
pub const O_EXCL: u64 = 0o200;
pub const O_TRUNC: u64 = 0o1000;
/// Do not block: a read with no data available returns `EAGAIN` instead of
/// waiting for it.
pub const O_NONBLOCK: u64 = 0o4000;

/// Signals known to the kernel. `sigaction` accepts 1..=31 but rejects the
/// uncatchable pair `SIGKILL`/`SIGSTOP`.
pub mod signum {
    pub const SIGHUP: u32 = 1;
    pub const SIGINT: u32 = 2;
    pub const SIGQUIT: u32 = 3;
    pub const SIGILL: u32 = 4;
    pub const SIGTRAP: u32 = 5;
    pub const SIGABRT: u32 = 6;
    pub const SIGBUS: u32 = 7;
    pub const SIGFPE: u32 = 8;
    pub const SIGKILL: u32 = 9;
    pub const SIGUSR1: u32 = 10;
    pub const SIGSEGV: u32 = 11;
    pub const SIGUSR2: u32 = 12;
    pub const SIGPIPE: u32 = 13;
    pub const SIGALRM: u32 = 14;
    pub const SIGTERM: u32 = 15;
    pub const SIGSTKFLT: u32 = 16;
    pub const SIGCHLD: u32 = 17;
    pub const SIGCONT: u32 = 18;
    pub const SIGSTOP: u32 = 19;
    pub const SIGTSTP: u32 = 20;
    pub const SIGTTIN: u32 = 21;
    pub const SIGTTOU: u32 = 22;
    pub const SIGURG: u32 = 23;
    pub const SIGXCPU: u32 = 24;
    pub const SIGXFSZ: u32 = 25;
    pub const SIGVTALRM: u32 = 26;
    pub const SIGPROF: u32 = 27;
    pub const SIGWINCH: u32 = 28;
    pub const SIGIO: u32 = 29;
    pub const SIGPWR: u32 = 30;
    pub const SIGSYS: u32 = 31;
}

/// Default disposition: reset to the kernel's default (mostly terminate).
pub const SIG_DFL: usize = 0;
/// Ignore the signal.
pub const SIG_IGN: usize = 1;

/// `sigaction` flags (bitfield).
pub mod sa_flags {
    /// Deliver the handler on the alternate signal stack.
    pub const SA_ONSTACK: u32 = 1 << 0;
    /// Restart an interrupted blocking syscall instead of returning `EINTR`.
    pub const SA_RESTART: u32 = 1 << 1;
    /// Do not block the signal while its own handler runs.
    pub const SA_NODEFER: u32 = 1 << 2;
}

/// `sigaltstack` `ss_flags` values.
pub mod ss_flags {
    /// The thread is currently executing on this stack.
    pub const SS_ONSTACK: u32 = 1 << 0;
    /// The alternate stack is disabled.
    pub const SS_DISABLE: u32 = 1 << 1;
}

/// `sigprocmask` `how` values.
pub mod how {
    pub const SIG_BLOCK: u64 = 0;
    pub const SIG_UNBLOCK: u64 = 1;
    pub const SIG_SETMASK: u64 = 2;
}

/// Handler signature: `extern "C" fn(sig: i32)`.
pub type SigHandler = unsafe extern "C" fn(i32);

/// A signal action, matching the kernel's `SigAction` layout exactly.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SigAction {
    /// Handler address, or [`SIG_DFL`]/[`SIG_IGN`].
    pub sa_handler: usize,
    /// `sa_flags::*` bitfield.
    pub sa_flags: u32,
    /// Address of the restorer trampoline (see [`sigreturn_trampoline`]).
    pub sa_restorer: usize,
    /// Signals additionally blocked while the handler runs.
    pub sa_mask: u64,
}

/// Alternate signal stack descriptor, matching `AltStack` in the kernel.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct StackBuf {
    /// Base address of the stack.
    pub ss_base: usize,
    /// Size in bytes.
    pub ss_size: usize,
    /// `ss_flags::*`.
    pub ss_flags: u32,
}

/// Saved machine context at delivery, a verbatim mirror of the kernel's
/// 20-word syscall trap frame (words 0..20, offsets 0, 8, ... 152).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SigContext {
    pub rax: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub rbx: u64,
    pub rbp: u64,
    /// Syscall number at delivery (mostly informational).
    pub syscall_nr: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

/// Return from a signal handler. Never called directly.
#[no_mangle]
pub extern "C" fn sigreturn_trampoline() -> ! {
    unsafe {
        raw(SIGRETURN, 0, 0, 0, 0, 0, 0);
    }
    unreachable!()
}

/// Sleep at least `ms` milliseconds. Returns the remaining time, or an errno
/// (`-EINTR`) if interrupted by a signal.
pub fn nanosleep(ms: u64) -> Result<(), i64> {
    let r = unsafe { raw(NANOSLEEP, ms, 0, 0, 0, 0, 0) };
    if r < 0 {
        Err(r)
    } else {
        Ok(())
    }
}

/// Install (or query) the action for `sig`; pass [`SIG_DFL`]/[`SIG_IGN`]
/// through `act.sa_handler` to reset it.
pub fn sigaction(sig: u32, act: Option<&SigAction>, oldact: Option<&mut SigAction>) -> Result<(), i64> {
    let r = unsafe {
        raw(
            SIGACTION,
            sig as u64,
            act.map_or(0, |a| a as *const SigAction as u64),
            oldact.map_or(0, |o| o as *mut SigAction as u64),
            0,
            0,
            0,
        )
    };
    if r < 0 {
        Err(r)
    } else {
        Ok(())
    }
}

/// Change the blocked-signal mask; `set == None` queries only.
pub fn sigprocmask(how: u64, set: Option<u64>, oldset: Option<&mut u64>) -> Result<(), i64> {
    let stack = set.unwrap_or(0);
    let r = unsafe {
        raw(
            SIGPROCMASK,
            how,
            set.map_or(0, |_| &stack as *const u64 as u64),
            oldset.map_or(0, |o| o as *mut u64 as u64),
            0,
            0,
            0,
        )
    };
    if r < 0 {
        Err(r)
    } else {
        Ok(())
    }
}

/// Query the set of blocked signals (Linux `sigprocmask`-style `oldset` read).
pub fn blocked_signals() -> u64 {
    let mut old = 0u64;
    let _ = sigprocmask(how::SIG_SETMASK, None, Some(&mut old));
    old
}

/// Atomically set the mask to `mask`, then suspend until a signal arrives.
/// Returns `Err(-EINTR)` once the kernel resumes the process.
pub fn sigsuspend(mask: u64) -> Result<(), i64> {
    let r = unsafe { raw(SIGSUSPEND, mask, 0, 0, 0, 0, 0) };
    Err(r)
}

/// Set of signals currently pending for this process.
pub fn sigpending() -> u64 {
    let mut set = 0u64;
    unsafe {
        raw(SIGPENDING, &mut set as *mut u64 as u64, 0, 0, 0, 0, 0);
    }
    set
}

/// Install or query the alternate signal stack.
pub fn sigaltstack(ss: Option<&StackBuf>, old_ss: Option<&mut StackBuf>) -> Result<(), i64> {
    let r = unsafe {
        raw(
            SIGALTSTACK,
            ss.map_or(0, |s| s as *const StackBuf as u64),
            old_ss.map_or(0, |o| o as *mut StackBuf as u64),
            0,
            0,
            0,
            0,
        )
    };
    if r < 0 {
        Err(r)
    } else {
        Ok(())
    }
}

/// Page size in bytes shared with the kernel's frame allocator.
pub const PAGE_SIZE: u64 = 4096;

/// Well-known server endpoints (must match `ipc` in the kernel).
pub const EP_VFSD: u64 = 1;
pub const EP_CONSOLED: u64 = 2;
pub const EP_INPUTD: u64 = 3;
pub const EP_DEMO: u64 = 4;
/// Endpoint 5 was the window-manager compositor (`wmsrv`). The id stays
/// reserved so the well-known block is never renumbered.
pub const EP_RESERVED_WMSRV: u64 = 5;
/// First endpoint id handed to dynamically spawned processes.
pub const EP_DYNAMIC_BASE: u64 = 6;

/// Terminal ioctl request numbers (the syscall number itself lives in the
/// top-of-file `nr` block).
pub const TCGETS: u32 = 0x5401;
/// `ioctl(TCSETS)`: apply terminal attributes immediately.
pub const TCSETS: u32 = 0x5402;
/// `ioctl(TCSETSW)`: apply attributes after output drains.
pub const TCSETSW: u32 = 0x5403;
/// `ioctl(TCSETSF)`: apply attributes after flushing input.
pub const TCSETSF: u32 = 0x5404;
/// Get terminal attributes, BSD spelling.
pub const TCGETA: u32 = 0x5405;
/// Set terminal attributes immediately, BSD spelling.
pub const TCSETA: u32 = 0x5406;
pub const TCSETAW: u32 = 0x5407;
pub const TCSETAF: u32 = 0x5408;
/// Send a break.
pub const TCSBRK: u32 = 0x5409;
/// Start/stop/flush terminal output and input (`tcflow`).
pub const TCXONC: u32 = 0x540A;
/// Discard buffered input and/or output (`tcflush`).
pub const TCFLSH: u32 = 0x540B;
/// Get the foreground process group.
pub const TIOCGPGRP: u32 = 0x540F;
/// Set the foreground process group.
pub const TIOCSPGRP: u32 = 0x5410;
/// Get/set the window size.
pub const TIOCGWINSZ: u32 = 0x5413;
pub const TIOCSWINSZ: u32 = 0x5414;
/// Number of bytes available to read.
pub const FIONREAD: u32 = 0x541B;
/// Get the session id owning this terminal.
pub const TIOCGSID: u32 = 0x5429;
/// Get the pty pair index behind a master descriptor.
pub const TIOCGPTN: u32 = 0x80045430;
/// Unlock a pty slave so it can be opened.
pub const TIOCSPTLCK: u32 = 0x40045431;

/// `TCXONC` actions (Linux values).
pub const TCOOFF: u32 = 0;
pub const TCOON: u32 = 1;
pub const TCIOFF: u32 = 2;
pub const TCION: u32 = 3;
/// `TCFLSH` queues (Linux values).
pub const TCIFLUSH: u32 = 0;
pub const TCOFLUSH: u32 = 1;
pub const TCIOFLUSH: u32 = 2;

// --- process groups and sessions ------------------------------------------

/// Put `pid` into process group `pgid`; `pid == 0` means the caller.
pub fn setpgid(pid: u32, pgid: u32) -> Result<(), i64> {
    let r = unsafe { raw(SETPGID, pid as u64, pgid as u64, 0, 0, 0, 0) };
    if r < 0 {
        Err(r)
    } else {
        Ok(())
    }
}

/// The calling process's process group id.
pub fn getpgrp() -> u32 {
    (unsafe { raw(GETPGRP, 0, 0, 0, 0, 0, 0) }) as u32
}

/// The process group of `pid`; `pid == 0` means the caller.
pub fn getpgid(pid: u32) -> Result<u32, i64> {
    let r = unsafe { raw(GETPGID, pid as u64, 0, 0, 0, 0, 0) };
    if r < 0 {
        Err(r)
    } else {
        Ok(r as u32)
    }
}

/// Create a new session led by the caller; returns the new session id.
pub fn setsid() -> Result<u32, i64> {
    let r = unsafe { raw(SETSID, 0, 0, 0, 0, 0, 0) };
    if r < 0 {
        Err(r)
    } else {
        Ok(r as u32)
    }
}

/// The session of `pid`; `pid == 0` means the caller.
pub fn getsid(pid: u32) -> i64 {
    unsafe { raw(GETSID, pid as u64, 0, 0, 0, 0, 0) }
}

// --- termios `tcflag_t` bits (Linux values) --------------------------------
// `c_lflag`
pub const ISIG: u32 = 0x0000_0001;
pub const ICANON: u32 = 0x0000_0002;
pub const ECHO: u32 = 0x0000_0008;
pub const ECHOE: u32 = 0x0000_0010;
pub const ECHOK: u32 = 0x0000_0020;
// `c_iflag`
pub const INLCR: u32 = 0x0000_0040;
pub const IGNCR: u32 = 0x0000_0080;
pub const ICRNL: u32 = 0x0000_0200;
// `c_oflag`
pub const OPOST: u32 = 0x0000_0001;
pub const ONLCR: u32 = 0x0000_0004;

/// ABI-stable terminal settings for the `TC*` ioctls.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Termios {
    pub c_iflag: u32,
    pub c_oflag: u32,
    pub c_cflag: u32,
    pub c_lflag: u32,
    pub c_line: u8,
    pub c_cc: [u8; 32],
    pub c_ispeed: u32,
    pub c_ospeed: u32,
}

/// ABI-stable terminal dimensions for `TIOCGWINSZ` / `TIOCSWINSZ`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Winsize {
    pub ws_row: u16,
    pub ws_col: u16,
    pub ws_xpixel: u16,
    pub ws_ypixel: u16,
}

/// Execute a fixed-size terminal ioctl against `fd`.
pub fn ioctl<T>(fd: usize, request: u32, arg: &mut T) -> Result<(), i64> {
    let r = unsafe { raw(IOCTL, fd as u64, request as u64, arg as *mut T as u64, 0, 0, 0) };
    if r < 0 { Err(r) } else { Ok(()) }
}

/// Issue a 6-argument syscall via the `syscall` instruction.
///
/// # Safety
/// Arguments must be valid per the kernel ABI for the requested number.
#[inline(always)]
pub unsafe fn raw(nr: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, a6: u64) -> i64 {
    let r: u64;
    asm!(
        "syscall",
        inlateout("rax") nr => r,
        in("rdi") a1,
        in("rsi") a2,
        in("rdx") a3,
        in("r10") a4,
        in("r8") a5,
        in("r9") a6,
        lateout("rcx") _,
        lateout("r11") _,
        options(nostack),
    );
    r as i64
}

/// Write bytes to the kernel debug console (serial).
pub fn debug_write(bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    // SAFETY: pointer into our own text/data, fully mapped.
    unsafe {
        raw(
            DEBUG_WRITE,
            bytes.as_ptr() as u64,
            bytes.len() as u64,
            0,
            0,
            0,
            0,
        );
    }
}

/// Kernel uptime in milliseconds.
pub fn uptime_ms() -> u64 {
    // SAFETY: no arguments, pure read.
    unsafe { raw(CLOCK_UPTIME_MS, 0, 0, 0, 0, 0, 0) as u64 }
}

/// Copy the kernel version string into `buf`; returns bytes written.
pub fn kernel_version(buf: &mut [u8]) -> usize {
    // SAFETY: `buf` is our own mapped memory; kernel validates the range.
    let n = unsafe { raw(KERNEL_VERSION, buf.as_ptr() as u64, buf.len() as u64, 0, 0, 0, 0) };
    n.max(0) as usize
}

/// Voluntarily yield the remaining time slice.
pub fn yield_now() {
    // SAFETY: no arguments.
    unsafe {
        raw(YIELD, 0, 0, 0, 0, 0, 0);
    }
}

/// This process's endpoint id (its pid).
pub fn get_epid() -> u64 {
    // SAFETY: no arguments.
    unsafe { raw(GET_EPID, 0, 0, 0, 0, 0, 0) as u64 }
}

/// This process's scheduler task id — the pid accepted by `kill`, `waitpid`
/// and `fork` results. Distinct from [`get_epid`], which returns the IPC
/// endpoint instead.
pub fn get_pid() -> u64 {
    // SAFETY: no arguments.
    unsafe { raw(GET_PID, 0, 0, 0, 0, 0, 0) as u64 }
}

/// Map `frames` anonymous pages into this address space; returns the virtual
/// address or a negative errno.
pub fn map_anon(frames: u64) -> Result<usize, i64> {
    // SAFETY: pure capability grant.
    let r = unsafe { raw(MAP_ANON, frames, 0, 0, 0, 0, 0) };
    if r < 0 {
        Err(r)
    } else {
        Ok(r as usize)
    }
}

/// User-visible framebuffer geometry; matches the kernel's `FbInfo` exactly.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct FbInfo {
    /// Page-aligned physical base of the framebuffer range.
    pub phys: u64,
    /// Total mapped length in bytes.
    pub size: u64,
    /// Byte offset of the first visible pixel from `phys`.
    pub offset: u32,
    /// Visible width in pixels.
    pub width: u32,
    /// Visible height in pixels.
    pub height: u32,
    /// Distance in bytes between scanlines.
    pub pitch: u32,
    /// Bits per pixel.
    pub bpp: u32,
    /// Bit position of the red channel.
    pub red_pos: u32,
    /// Width of the red channel in bits.
    pub red_size: u32,
    /// Bit position of the green channel.
    pub green_pos: u32,
    /// Width of the green channel in bits.
    pub green_size: u32,
    /// Bit position of the blue channel.
    pub blue_pos: u32,
    /// Width of the blue channel in bits.
    pub blue_size: u32,
}

/// Query the active framebuffer geometry.
pub fn fb_info(info: &mut FbInfo) -> Result<(), i64> {
    // SAFETY: `info` is our own mapped memory; the kernel validates the range.
    let r = unsafe { raw(FB_INFO, info as *mut FbInfo as u64, 0, 0, 0, 0, 0) };
    if r < 0 {
        Err(r)
    } else {
        Ok(())
    }
}

/// Ask the kernel to stop drawing its text console so this process can own the
/// framebuffer. Kernel logs continue on the debug UART.
pub fn console_detach() -> Result<(), i64> {
    // SAFETY: pure capability toggle.
    let r = unsafe { raw(CONSOLE_DETACH, 0, 0, 0, 0, 0, 0) };
    if r < 0 {
        Err(r)
    } else {
        Ok(())
    }
}

/// Hand the display back to the kernel text console after a ring-3 application
/// finished owning the framebuffer. The console is rebuilt and the screen
/// cleared, so a later terminal can render through it again.
pub fn console_attach() -> Result<(), i64> {
    // SAFETY: pure capability toggle.
    let r = unsafe { raw(CONSOLE_ATTACH, 0, 0, 0, 0, 0, 0) };
    if r < 0 {
        Err(r)
    } else {
        Ok(())
    }
}

/// Duplicate `oldfd` onto `newfd`, closing whatever descriptor was there.
///
/// The duplicate shares the *open file description*, not just the underlying
/// object, so the two descriptors share a file position. That is what makes this
/// useful for handing a child a terminal: `dup2(pts, 0)` puts the pty slave on
/// the child's standard input while leaving the parent's own descriptor alone
/// and still usable.
///
/// `oldfd == newfd` succeeds and changes nothing, matching POSIX.
pub fn dup2(oldfd: u64, newfd: u64) -> Result<u64, i64> {
    // SAFETY: a pure descriptor-table operation; both arguments are plain
    // integers and the kernel resolves them.
    let r = unsafe { raw(DUP2, oldfd, newfd, 0, 0, 0, 0) };
    if r < 0 {
        Err(r)
    } else {
        Ok(r as u64)
    }
}

/// Map `frames` device-memory pages starting at physical `phys` into this
/// process (MMIO). Returns the mapped virtual base.
pub fn map_phys(phys: u64, frames: u64, flags: u64) -> Result<usize, i64> {
    // SAFETY: the kernel authorizes the range against registered device memory.
    let r = unsafe { raw(MAP_PHYS, phys, frames, flags, 0, 0, 0) };
    if r < 0 {
        Err(r)
    } else {
        Ok(r as usize)
    }
}

/// Create a shared-memory region of `frames` pages mapped into this process.
/// Returns a handle that can be handed to a peer, which maps the *same*
/// physical frames with [`shm_map`].
pub fn shm_create(frames: u64) -> Result<u64, i64> {
    // SAFETY: kernel validates the frame count and ownership.
    let r = unsafe { raw(SHM_CREATE, frames, 0, 0, 0, 0, 0) };
    if r < 0 {
        Err(r)
    } else {
        Ok(r as u64)
    }
}

/// Map an existing shared-memory region by handle into this address space.
/// Returns the virtual base.
pub fn shm_map(handle: u64) -> Result<usize, i64> {
    // SAFETY: kernel validates the handle.
    let r = unsafe { raw(SHM_MAP, handle, 0, 0, 0, 0, 0) };
    if r < 0 {
        Err(r)
    } else {
        Ok(r as usize)
    }
}

/// Drop this process's mapping of `handle`. The backing frames are freed when
/// the last mapping (across all peers) is released.
pub fn shm_destroy(handle: u64) -> Result<(), i64> {
    // SAFETY: kernel validates the handle.
    let r = unsafe { raw(SHM_DESTROY, handle, 0, 0, 0, 0, 0) };
    if r < 0 {
        Err(r)
    } else {
        Ok(())
    }
}

/// Grant this process access to I/O ports `lo..=hi` (low 1024 only).
pub fn port_allow(lo: u16, hi: u16) -> Result<(), i64> {
    // SAFETY: kernel validates 0..=1023.
    let r = unsafe { raw(PORT_ALLOW, lo as u64, hi as u64, 0, 0, 0, 0) };
    if r < 0 {
        Err(r)
    } else {
        Ok(())
    }
}

/// Bind hardware IRQ `irq` to this endpoint; the kernel routes its events here.
pub fn irq_bind(irq: u64) -> Result<(), i64> {
    // SAFETY: kernel validates irq < 16 and ownership.
    let r = unsafe { raw(IRQ_BIND, irq, 0, 0, 0, 0, 0) };
    if r < 0 {
        Err(r)
    } else {
        Ok(())
    }
}

/// Terminate this process (never returns).
pub fn proc_exit() -> ! {
    proc_exit_code(0)
}

/// Terminate this process with an explicit exit status (never returns).
pub fn proc_exit_code(code: u32) -> ! {
    // SAFETY: kernel switches to another process and never returns to us.
    unsafe {
        raw(PROC_EXIT, code as u64, 0, 0, 0, 0, 0);
    }
    loop {
        core::hint::spin_loop();
    }
}

/// Duplicate this process. Returns the child's pid in the parent and `0` in
/// the child, or a negative errno.
pub fn fork() -> i64 {
    // SAFETY: no arguments.
    unsafe { raw(FORK, 0, 0, 0, 0, 0, 0) }
}

/// Serialize `argv` into an explicit, NUL-terminated C-style argument vector:
/// a backing byte buffer holding each argument followed by a `\0`, plus a
/// pointer array into it (terminated by a null pointer).
///
/// Rust `&str` slices are *not* NUL-terminated, and the kernel's argv reader
/// (`read_argv`) walks each string to the first `\0`, so passing `&str` data
/// directly would leak whatever heap bytes follow the slice into the child's
/// arguments. Copying first keeps the kernel's classic C-string contract.
fn nul_terminated<'a>(argv: Option<&[&str]>) -> (Vec<u8>, Vec<*const u8>) {
    let mut backing: Vec<u8> = Vec::new();
    let mut ptrs: Vec<*const u8> = Vec::new();
    if let Some(a) = argv {
        // Reserve the full size up front so the recorded pointers stay valid
        // while the buffer is filled (no reallocation can move `backing`).
        backing.reserve(a.iter().map(|s| s.len() + 1).sum());
        for s in a {
            let base = backing.len();
            backing.extend_from_slice(s.as_bytes());
            backing.push(0);
            ptrs.push(backing[base..].as_ptr());
        }
    }
    ptrs.push(core::ptr::null());
    (backing, ptrs)
}

/// Replace this process's image with embedded program `prog`, handing it
/// `argv` (argv[0] is conventionally the command name).
///
/// The argv array and its strings must remain valid for the duration of the
/// call; the kernel copies them before abandoning this address space. Never
/// returns on success (the kernel abandons this context); the `Ok(())` arm is
/// only reachable if the kernel ever changes that contract. `None` runs the
/// program with an empty argument vector.
pub fn exec(prog: u64, argv: Option<&[&str]>) -> Result<(), i64> {
    let (_backing, ptrs) = nul_terminated(argv);
    // SAFETY: kernel validates the program index, then abandons this context.
    let r = unsafe { raw(EXEC, prog, ptrs.as_ptr() as u64, 0, 0, 0, 0) };
    if r < 0 {
        Err(r)
    } else {
        Ok(())
    }
}

/// Spawn embedded program `prog` (an index into the kernel's program table):
/// a fresh address space and identity, made a child of this process so it can
/// be reaped with [`waitpid`]. The newborn receives `argv` (see [`exec`];
/// `None` = empty args). Returns the new pid or a negative errno.
pub fn proc_spawn(prog: u64, argv: Option<&[&str]>) -> Result<u64, i64> {
    let (_backing, ptrs) = nul_terminated(argv);
    // SAFETY: the kernel validates the program index and adopts the child.
    let r = unsafe { raw(PROC_SPAWN, prog, ptrs.as_ptr() as u64, 0, 0, 0, 0) };
    if r < 0 {
        Err(r)
    } else {
        Ok(r as u64)
    }
}

/// Fetch this process's argument vector. Each entry is a byte slice of one
/// argument (see [`exec`]/[`proc_spawn`] for how they were supplied).
pub fn args() -> Vec<Vec<u8>> {
    fetch_strvec(GET_ARGS)
}

/// Fetch this process's environment as `NAME=value` strings, in the order the
/// kernel holds them.
///
/// The same environment is also staged as `envp` on the initial stack, so a
/// program built against a libc finds it there instead. This accessor exists
/// for Rust callers, which have no libc startup to do the stack walk.
pub fn env() -> Vec<String> {
    fetch_strvec(GET_ENV)
        .into_iter()
        .map(|b| {
            let mut v = b;
            // Bytes came from the kernel as a NUL-terminated string with the
            // terminator already stripped; `String` needs valid UTF-8.
            while v.last() == Some(&0) {
                v.pop();
            }
            String::from_utf8_lossy(&v).into_owned()
        })
        .collect()
}

/// Shared body of [`args`] and [`env`]: the kernel hands back a count word
/// followed by NUL-terminated strings in one buffer.
fn fetch_strvec(nr: u64) -> Vec<Vec<u8>> {
    let need = unsafe { raw(nr, 0, 0, 0, 0, 0, 0) };
    if need < 0 {
        return Vec::new();
    }
    let mut buf = vec![0u8; need as usize];
    let n = unsafe { raw(nr, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0, 0, 0) };
    if n < 0 {
        return Vec::new();
    }
    let count = u64::from_le_bytes(buf[..8].try_into().unwrap()) as usize;
    let mut out = Vec::with_capacity(count);
    let mut i = 8usize;
    for _ in 0..count {
        let start = i;
        while i < buf.len() && buf[i] != 0 {
            i += 1;
        }
        out.push(buf[start..i].to_vec());
        i += 1;
    }
    out
}

/// Change this process's working directory to `path` (absolute or relative to
/// the current working directory).
pub fn chdir(path: &str) -> Result<(), i64> {
    let r = unsafe { raw(CHDIR, path.as_ptr() as u64, path.len() as u64, 0, 0, 0, 0) };
    if r < 0 {
        Err(r)
    } else {
        Ok(())
    }
}

/// Read this process's working directory as an absolute path.
pub fn getcwd() -> Result<String, i64> {
    let need = unsafe { raw(GETCWD, 0, 0, 0, 0, 0, 0) };
    if need < 0 {
        return Err(need);
    }
    let mut buf = vec![0u8; need as usize];
    let n = unsafe { raw(GETCWD, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0, 0, 0) };
    if n < 0 {
        return Err(n);
    }
    let s = core::str::from_utf8(&buf[..n as usize]).map_err(|_| -22i64)?;
    Ok(String::from(s))
}

/// List the children of directory `path` into `buf` as NUL-terminated names.
/// A zero-length `buf` queries the required size. Returns bytes written (or
/// the required size for a query).
pub fn readdir(path: &str, buf: &mut [u8]) -> Result<usize, i64> {
    let r = unsafe {
        raw(
            READDIR,
            path.as_ptr() as u64,
            path.len() as u64,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
            0,
            0,
        )
    };
    if r < 0 {
        Err(r)
    } else {
        Ok(r as usize)
    }
}

/// Create a directory at `path` (fails if it already exists).
pub fn mkdir(path: &str, mode: u32) -> Result<(), i64> {
    let r = unsafe { raw(MKDIR, path.as_ptr() as u64, path.len() as u64, mode as u64, 0, 0, 0) };
    if r < 0 {
        Err(r)
    } else {
        Ok(())
    }
}

/// Block until child `pid` terminates; stores the exit status in `status`.
pub fn waitpid(pid: u64, status: &mut i32) -> Result<i32, i64> {
    // SAFETY: `status` is our own mapped memory; the kernel writes at most 4
    // bytes there after the child exits.
    let r = unsafe { raw(WAITPID, pid, status as *mut i32 as u64, 0, 0, 0, 0) };
    if r < 0 {
        Err(r)
    } else {
        Ok(*status)
    }
}

/// Create a pipe; returns the read and write file descriptor. Both ends are
/// inherited across `fork`, so parent and child share the same byte stream.
pub fn pipe() -> Result<(usize, usize), i64> {
    let mut pair = [0u8; 8];
    // SAFETY: `pair` is our own mapped stack memory; the kernel writes two
    // little-endian fds there.
    let r = unsafe { raw(PIPE, pair.as_mut_ptr() as u64, 0, 0, 0, 0, 0) };
    if r < 0 {
        Err(r)
    } else {
        let rfd = u32::from_le_bytes([pair[0], pair[1], pair[2], pair[3]]) as usize;
        let wfd = u32::from_le_bytes([pair[4], pair[5], pair[6], pair[7]]) as usize;
        Ok((rfd, wfd))
    }
}

/// Read up to `buf.len()` bytes from `fd`; returns the number read. A pipe
/// read returns `0` once every write end is closed (EOF).
pub fn read(fd: usize, buf: &mut [u8]) -> Result<usize, i64> {
    // SAFETY: `buf` is our own mapped memory; the kernel validates the range.
    if buf.is_empty() {
        return Ok(0);
    }
    let r = unsafe { raw(READ, fd as u64, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0, 0) };
    if r < 0 {
        Err(r)
    } else {
        Ok(r as usize)
    }
}

/// Write `buf` to `fd`; returns the number written (may be fewer than
/// `buf.len()` for very large buffers).
pub fn write(fd: usize, buf: &[u8]) -> Result<usize, i64> {
    // SAFETY: `buf` is our own mapped memory; the kernel validates the range.
    if buf.is_empty() {
        return Ok(0);
    }
    let r = unsafe { raw(WRITE, fd as u64, buf.as_ptr() as u64, buf.len() as u64, 0, 0, 0) };
    if r < 0 {
        Err(r)
    } else {
        Ok(r as usize)
    }
}

/// Close a descriptor, releasing its reference on the underlying vnode.
pub fn close(fd: usize) -> Result<(), i64> {
    let r = unsafe { raw(CLOSE, fd as u64, 0, 0, 0, 0, 0) };
    if r < 0 {
        Err(r)
    } else {
        Ok(())
    }
}

/// `stat` result written by the kernel.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Stat {
    /// Permission mode bits (unix `S_*`).
    pub mode: u32,
    /// Owner user id.
    pub uid: u32,
    /// Owner group id.
    pub gid: u32,
    /// Node kind (`NodeKind` in the kernel: 0 file, 1 dir, 2 char, 3 pipe).
    pub kind: u32,
    /// Current size in bytes.
    pub size: u64,
}

/// Open (or create) `path`. `flags` = access mode | `O_*`; `mode` is used
/// with `O_CREAT` (the process umask is applied by the kernel).
pub fn open(path: &str, flags: u64, mode: u32) -> Result<usize, i64> {
    let r = unsafe {
        raw(
            OPEN,
            path.as_ptr() as u64,
            path.len() as u64,
            flags,
            mode as u64,
            0,
            0,
        )
    };
    if r < 0 {
        Err(r)
    } else {
        Ok(r as usize)
    }
}

/// Stat `path`, filling `st`.
pub fn stat(path: &str, st: &mut Stat) -> Result<(), i64> {
    let r = unsafe {
        raw(
            STAT,
            path.as_ptr() as u64,
            path.len() as u64,
            st as *mut Stat as u64,
            0,
            0,
            0,
        )
    };
    if r < 0 {
        Err(r)
    } else {
        Ok(())
    }
}

/// Change `path`'s permission mode bits.
pub fn chmod(path: &str, mode: u32) -> Result<(), i64> {
    let r = unsafe { raw(CHMOD, path.as_ptr() as u64, path.len() as u64, mode as u64, 0, 0, 0) };
    if r < 0 {
        Err(r)
    } else {
        Ok(())
    }
}

/// Change `path`'s owner; pass `u32::MAX` for either component to keep it.
pub fn chown(path: &str, uid: u32, gid: u32) -> Result<(), i64> {
    let r = unsafe {
        raw(
            CHOWN,
            path.as_ptr() as u64,
            path.len() as u64,
            uid as u64,
            gid as u64,
            0,
            0,
        )
    };
    if r < 0 {
        Err(r)
    } else {
        Ok(())
    }
}

/// Signal `pid`. `sig == 0` only checks existence + permission.
pub fn kill(pid: u64, sig: u32) -> Result<(), i64> {
    let r = unsafe { raw(KILL, pid, sig as u64, 0, 0, 0, 0) };
    if r < 0 {
        Err(r)
    } else {
        Ok(())
    }
}

/// Real user id.
pub fn getuid() -> u32 {
    unsafe { raw(GETUID, 0, 0, 0, 0, 0, 0) as u32 }
}

/// Real group id.
pub fn getgid() -> u32 {
    unsafe { raw(GETGID, 0, 0, 0, 0, 0, 0) as u32 }
}

/// Effective user id.
pub fn geteuid() -> u32 {
    unsafe { raw(GETEUID, 0, 0, 0, 0, 0, 0) as u32 }
}

/// Effective group id.
pub fn getegid() -> u32 {
    unsafe { raw(GETEGID, 0, 0, 0, 0, 0, 0) as u32 }
}

/// Real, effective and saved user ids.
pub fn getresuid() -> (u32, u32, u32) {
    let mut trio = [0u8; 12];
    unsafe {
        raw(GETRESUID, trio.as_mut_ptr() as u64, 0, 0, 0, 0, 0);
    }
    let rd = |i: usize| u32::from_le_bytes([trio[i], trio[i + 1], trio[i + 2], trio[i + 3]]);
    (rd(0), rd(4), rd(8))
}

/// Real, effective and saved group ids.
pub fn getresgid() -> (u32, u32, u32) {
    let mut trio = [0u8; 12];
    unsafe {
        raw(GETRESGID, trio.as_mut_ptr() as u64, 0, 0, 0, 0, 0);
    }
    let rd = |i: usize| u32::from_le_bytes([trio[i], trio[i + 1], trio[i + 2], trio[i + 3]]);
    (rd(0), rd(4), rd(8))
}

/// Fetch up to `list.len()` supplementary groups; returns the total count.
pub fn getgroups(list: &mut [u32]) -> i64 {
    unsafe {
        raw(
            GETGROUPS,
            list.len() as u64,
            list.as_mut_ptr() as u64,
            0,
            0,
            0,
            0,
        )
    }
}

/// Replace the supplementary group set (privileged only).
pub fn setgroups(groups: &[u32]) -> Result<(), i64> {
    let r = unsafe {
        raw(
            SETGROUPS,
            groups.len() as u64,
            groups.as_ptr() as u64,
            0,
            0,
            0,
            0,
        )
    };
    if r < 0 {
        Err(r)
    } else {
        Ok(())
    }
}

/// Set user id (all four ids as root).
pub fn setuid(uid: u32) -> Result<(), i64> {
    let r = unsafe { raw(SETUID, uid as u64, 0, 0, 0, 0, 0) };
    if r < 0 {
        Err(r)
    } else {
        Ok(())
    }
}

/// Set group id (all four ids as root).
pub fn setgid(gid: u32) -> Result<(), i64> {
    let r = unsafe { raw(SETGID, gid as u64, 0, 0, 0, 0, 0) };
    if r < 0 {
        Err(r)
    } else {
        Ok(())
    }
}

/// Set effective user id.
pub fn seteuid(euid: u32) -> Result<(), i64> {
    let r = unsafe { raw(SETEUID, euid as u64, 0, 0, 0, 0, 0) };
    if r < 0 {
        Err(r)
    } else {
        Ok(())
    }
}

/// Set effective group id.
pub fn setegid(egid: u32) -> Result<(), i64> {
    let r = unsafe { raw(SETEGID, egid as u64, 0, 0, 0, 0, 0) };
    if r < 0 {
        Err(r)
    } else {
        Ok(())
    }
}

/// Set real and effective user ids (`u32::MAX` keeps a component).
pub fn setreuid(ruid: u32, euid: u32) -> Result<(), i64> {
    let r = unsafe { raw(SETREUID, ruid as u64, euid as u64, 0, 0, 0, 0) };
    if r < 0 {
        Err(r)
    } else {
        Ok(())
    }
}

/// Set real and effective group ids (`u32::MAX` keeps a component).
pub fn setregid(rgid: u32, egid: u32) -> Result<(), i64> {
    let r = unsafe { raw(SETREGID, rgid as u64, egid as u64, 0, 0, 0, 0) };
    if r < 0 {
        Err(r)
    } else {
        Ok(())
    }
}

/// Set real, effective and saved user ids (`u32::MAX` keeps a component).
pub fn setresuid(ruid: u32, euid: u32, suid: u32) -> Result<(), i64> {
    let r = unsafe { raw(SETRESUID, ruid as u64, euid as u64, suid as u64, 0, 0, 0) };
    if r < 0 {
        Err(r)
    } else {
        Ok(())
    }
}

/// Set real, effective and saved group ids (`u32::MAX` keeps a component).
pub fn setresgid(rgid: u32, egid: u32, sgid: u32) -> Result<(), i64> {
    let r = unsafe { raw(SETRESGID, rgid as u64, egid as u64, sgid as u64, 0, 0, 0) };
    if r < 0 {
        Err(r)
    } else {
        Ok(())
    }
}

/// Set the filesystem user id; returns the previous fsuid (Linux semantics:
/// never fails, an unprivileged call that is denied simply changes nothing).
pub fn setfsuid(fsuid: u32) -> u32 {
    unsafe { raw(SETFSUID, fsuid as u64, 0, 0, 0, 0, 0) as u32 }
}

/// Set the filesystem group id; returns the previous fsgid.
pub fn setfsgid(fsgid: u32) -> u32 {
    unsafe { raw(SETFSGID, fsgid as u64, 0, 0, 0, 0, 0) as u32 }
}

/// Set the process umask; returns the previous value.
pub fn umask(mask: u32) -> u32 {
    unsafe { raw(UMASK, mask as u64, 0, 0, 0, 0, 0) as u32 }
}
