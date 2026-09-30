// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! The Samsara system call ABI.
//!
//! Samsara defines its own ABI; it is deliberately *not* Linux-compatible.
//!
//! ## Calling convention
//!
//! | Element      | Register / rule                              |
//! |--------------|----------------------------------------------|
//! | syscall nr   | `rax`                                        |
//! | arguments    | `rdi`, `rsi`, `rdx`, `r10`, `r8`, `r9`       |
//! | return value | `rax` (>= 0 success, < 0 `-errno`)           |
//! | clobbered    | `rcx` (return rip), `r11` (caller rflags)    |
//! | preserved    | all other registers, per SysV callee-saved   |
//!
//! See `docs/ABI.md` for the full contract and the stable number table.

use crate::sig::samsara_post_syscall;
use alloc::boxed::Box;
use alloc::string::String;
use core::arch::asm;

unsafe extern "C" {
    fn syscall_entry();
}

/// Error convention: results are negated errno-style values.
pub mod errno {
    /// Operation not permitted.
    pub const EPERM: i64 = -1;
    /// No such process/resource.
    pub const ENOENT: i64 = -2;
    /// No such process (POSIX `ESRCH`).
    pub const ESRCH: i64 = -3;
    /// No children to wait for (POSIX `ECHILD`).
    pub const ECHILD: i64 = -10;
    /// Blocking system call interrupted by a signal (POSIX `EINTR`).
    pub const EINTR: i64 = -4;
    /// Invalid argument.
    pub const EINVAL: i64 = -22;
    /// Inappropriate ioctl for device.
    pub const ENOTTY: i64 = -25;
    /// Not enough memory.
    pub const ENOMEM: i64 = -12;
    /// Exec format error (bad/unsupported ELF).
    pub const ENOEXEC: i64 = -8;
    /// Permission denied for an access mode (POSIX `EACCES`).
    pub const EACCES: i64 = -13;
    /// File exists (POSIX `EEXIST`).
    pub const EEXIST: i64 = -17;
    /// A path component was not a directory (POSIX `ENOTDIR`).
    pub const ENOTDIR: i64 = -20;
    /// Is a directory where a file was required (POSIX `EISDIR`).
    pub const EISDIR: i64 = -21;
    /// Broken pipe: a write with no reader end open.
    pub const EPIPE: i64 = -32;
    /// Function not implemented.
    pub const ENOSYS: i64 = -38;
    /// Input/output error.
    pub const EIO: i64 = -5;
    /// Resource temporarily unavailable; a non-blocking operation would block
    /// (POSIX `EAGAIN`/`EWOULDBLOCK`).
    pub const EAGAIN: i64 = -11;
    /// Bad file descriptor (POSIX `EBADF`).
    ///
    /// Distinct from `EACCES`: the descriptor is not merely forbidden, it does
    /// not name anything. `read(2)` on a closed fd must report this, or a
    /// program cannot tell "you may not do that" from "that is not there".
    pub const EBADF: i64 = -9;
    /// Bad address (POSIX `EFAULT`).
    ///
    /// A user pointer that is not mapped, or does not span `len` readable
    /// bytes. Kept distinct from `EINVAL` because the argument was well-formed:
    /// the program made a mistake about memory, not about the API.
    pub const EFAULT: i64 = -14;
    /// No such device (POSIX `ENODEV`).
    ///
    /// "The object exists but the operation does not apply to it." Used when a
    /// mapping is requested for a node that has no mappable physical range --
    /// a request that is well-formed and simply cannot be served.
    pub const ENODEV: i64 = -19;
    /// Result too large to fit (POSIX `ERANGE`).
    ///
    /// Distinct from `EINVAL` because nothing is wrong with the request: the
    /// caller asked for a correct thing in a buffer that is too small to hold
    /// the answer. Used by `ttyname(3)`, where returning a truncated device
    /// path would hand back a path that does not open.
    pub const ERANGE: i64 = -34;
    /// Too many symbolic links followed (POSIX `ELOOP`).
    ///
    /// A *path* problem rather than a request problem, which is why it is not
    /// `EINVAL`: the caller asked for something legitimate, and it is the chain
    /// of links in the path that is at fault. A program that distinguishes them
    /// can report "this path is circular" instead of "bad argument", and a program
    /// that only distinguishes success from failure behaves the same either way.
    ///
    /// The path walker returns this from [`crate::vfs::FsError::TooManyLinks`];
    /// the value is duplicated here so that the syscall layer never has to convert
    /// between the two representations.
    pub const ELOOP: i64 = -40;
    /// No such device or address (POSIX `ENXIO`).
    ///
    /// Added for `/dev/tty`, and specifically for the case where the calling
    /// session has no controlling terminal. That is a fact about the session
    /// rather than a failure to open a file, and it is the one error a program can
    /// act on: a shell reaching for `/dev/tty` to find its terminal gets `ENXIO`
    /// and knows to run without job control, where `ENOENT` would send it looking
    /// for a missing file and `EACCES` would send it looking for a permission
    /// problem. Both of those are plausible-sounding and both are wrong.
    pub const ENXIO: i64 = -6;
    /// A path component is too long (POSIX `ENAMETOOLONG`).
    ///
    /// Separate from `EINVAL` because the remedy is different: the caller shortens
    /// a *name*, rather than changing a flag or fixing a type. `ENAMETOOLONG` is
    /// the only error a program can act on to make progress, so folding it into
    /// `EINVAL` would leave a caller with no way to tell "your name is too long"
    /// from "your arguments disagree".
    pub const ENAMETOOLONG: i64 = -36;
}

/// Stable system call numbers. Never renumber; only append.
pub mod nr {
    /// Write a debug buffer to the kernel console.
    pub const DEBUG_WRITE: u64 = 0;
    /// Yield the CPU until the next interrupt.
    pub const YIELD: u64 = 1;
    /// Read kernel uptime in milliseconds.
    pub const CLOCK_UPTIME_MS: u64 = 2;
    /// Query kernel identity/version string into a user buffer.
    pub const KERNEL_VERSION: u64 = 3;
    /// Terminate the calling context cleanly.
    pub const EXIT: u64 = 4;
    /// Open a path: `(path: ptr<u8>, path_len, flags)` -> fd.
    pub const OPEN: u64 = 5;
    /// Close a descriptor.
    pub const CLOSE: u64 = 6;
    /// Read from a descriptor: `(fd, buf, len)` -> bytes read.
    pub const READ: u64 = 7;
    /// Write to a descriptor: `(fd, buf, len)` -> bytes written.
    pub const WRITE: u64 = 8;
    /// Send an asynchronous notification message to an endpoint.
    pub const IPC_SEND: u64 = 9;
    /// Block until a message arrives for this process's endpoint.
    pub const IPC_RECV: u64 = 10;
    /// Reply to a previously received `Call` message.
    pub const IPC_REPLY: u64 = 11;
    /// Synchronous request/response: send a `Call` and block for the reply.
    pub const IPC_CALL: u64 = 12;
    /// Spawn a user process from the embedded program table.
    pub const PROC_SPAWN: u64 = 13;
    /// Terminate the calling process.
    pub const PROC_EXIT: u64 = 14;
    /// Return this process's IPC endpoint id (`0x8000...` on failure).
    pub const GET_EPID: u64 = 15;
    /// Grant the calling process access to I/O ports `lo..=hi`.
    pub const PORT_ALLOW: u64 = 16;
    /// Bind an IRQ line as IPC messages to the calling process's endpoint.
    pub const IRQ_BIND: u64 = 17;
    /// Release a previously bound IRQ line.
    pub const IRQ_UNBIND: u64 = 18;
    /// Map `frames` freshly-allocated (zeroed) frames into this process.
    pub const MAP_ANON: u64 = 19;
    /// Map a physical device-memory range into this process (MMIO for drivers).
    pub const MAP_PHYS: u64 = 20;
    /// Allocate physically-contiguous DMA memory mapped into this process.
    pub const DMA_ALLOC: u64 = 21;
    /// Release a DMA allocation and unmap it.
    pub const DMA_FREE: u64 = 22;
    /// Duplicate the calling process: `()` -> child pid, `0` in the child.
    pub const FORK: u64 = 23;
    /// Replace the calling process's image with embedded program `prog`.
    pub const EXEC: u64 = 24;
    /// Block until process `pid` exits: `(pid, status_ptr)` -> 0.
    pub const WAITPID: u64 = 25;
    /// Create a pipe: `(pair: ptr to two u32)` -> 0; stores read then write fd.
    pub const PIPE: u64 = 26;
    /// Stat a path: `(path, path_len, stat: ptr)` -> 0.
    pub const STAT: u64 = 27;
    /// Change file mode: `(path, path_len, mode)` -> 0.
    pub const CHMOD: u64 = 28;
    /// Change file ownership: `(path, path_len, uid, gid)` -> 0 (`u32::MAX` = keep).
    pub const CHOWN: u64 = 29;
    /// Signal a process: `(pid, sig)` -> 0 (sig 0 = existence check).
    pub const KILL: u64 = 30;
    /// Real user id.
    pub const GETUID: u64 = 31;
    /// Real group id.
    pub const GETGID: u64 = 32;
    /// Effective user id.
    pub const GETEUID: u64 = 33;
    /// Effective group id.
    pub const GETEGID: u64 = 34;
    /// Real/effective/saved user ids: `(ptr to three u32)` -> 0.
    pub const GETRESUID: u64 = 35;
    /// Real/effective/saved group ids: `(ptr to three u32)` -> 0.
    pub const GETRESGID: u64 = 36;
    /// List supplementary groups: `(count, ptr)` -> number of groups.
    pub const GETGROUPS: u64 = 37;
    /// Set supplementary groups: `(count, ptr)` -> 0.
    pub const SETGROUPS: u64 = 38;
    /// Set user id (all four ids as root).
    pub const SETUID: u64 = 39;
    /// Set group id (all four ids as root).
    pub const SETGID: u64 = 40;
    /// Set effective user id.
    pub const SETEUID: u64 = 41;
    /// Set effective group id.
    pub const SETEGID: u64 = 42;
    /// Set real+effective user ids: `(ruid, euid)`.
    pub const SETREUID: u64 = 43;
    /// Set real+effective group ids: `(rgid, egid)`.
    pub const SETREGID: u64 = 44;
    /// Set real/effective/saved user ids: `(ruid, euid, suid)`.
    pub const SETRESUID: u64 = 45;
    /// Set real/effective/saved group ids: `(rgid, egid, sgid)`.
    pub const SETRESGID: u64 = 46;
    /// Set filesystem user id; returns the previous fsuid.
    pub const SETFSUID: u64 = 47;
    /// Set filesystem group id; returns the previous fsgid.
    pub const SETFSGID: u64 = 48;
    /// Set the process umask; returns the previous value.
    pub const UMASK: u64 = 49;
    /// Sleep at least `ms` milliseconds; interrupted by a signal with `EINTR`.
    pub const NANOSLEEP: u64 = 50;
    /// Install a signal handler: `(sig, act: *const SigAction, oldact: *mut SigAction)`.
    pub const SIGACTION: u64 = 51;
    /// Change the blocked-signal mask: `(how, set: *const u64, oldset: *mut u64)`.
    pub const SIGPROCMASK: u64 = 52;
    /// Suspend execution until a catchable/terminating signal arrives.
    pub const SIGSUSPEND: u64 = 53;
    /// Install or query the alternate signal stack.
    pub const SIGALTSTACK: u64 = 54;
    /// Return from a signal handler (trampoline-only).
    pub const SIGRETURN: u64 = 55;
    /// Query the set of pending signals: `(set: *mut u64)`.
    pub const SIGPENDING: u64 = 56;
    /// Return this process's scheduler task id (the pid used by `FORK`,
    /// `WAITPID` and `KILL`). Distinct from `GET_EPID`.
    pub const GET_PID: u64 = 57;
    /// Poll descriptors for readiness:
    /// `(fds: ptr<PollFd>, nfds, timeout_ms)` -> number ready.
    /// `timeout_ms` is signed: `-1` blocks forever, `0` polls now, `>0` waits
    /// up to that many milliseconds.
    pub const POLL: u64 = 58;
    /// Terminal/device control: `(fd, request, arg: ptr<u8>) -> 0`.
    pub const IOCTL: u64 = 59;
    /// Query the active framebuffer geometry: `(info: ptr<FbInfo>) -> 0`.
    pub const FB_INFO: u64 = 60;
    /// Hand the framebuffer to user space: stop the kernel text console from
    /// painting over it. `() -> 0`.
    pub const CONSOLE_DETACH: u64 = 61;
    /// Give the display back to the kernel text console after a ring-3
    /// application finished owning the framebuffer (the inverse of
    /// `CONSOLE_DETACH`). Rebuilds the console and clears the screen. `() -> 0`.
    pub const CONSOLE_ATTACH: u64 = 62;
    /// Copy the calling process's argument vector into `(buf, cap)`: an
    /// `argc` word (u64 LE) followed by the NUL-terminated strings. A null
    /// `buf` or zero `cap` returns the required size. `(buf, cap) -> bytes`.
    pub const GET_ARGS: u64 = 63;
    /// Change the calling process's working directory: `(path, path_len) -> 0`.
    pub const CHDIR: u64 = 64;
    /// Copy the calling process's working directory into `(buf, cap)`; a null
    /// `buf` or zero `cap` returns the required size (excluding the NUL).
    /// `(buf, cap) -> bytes`.
    pub const GETCWD: u64 = 65;
    /// List a directory's children: `(path, path_len, buf, cap)-> bytes`.
    /// `buf` receives the entry names as NUL-terminated strings; a null `buf`
    /// or zero `cap` returns the required size.
    pub const READDIR: u64 = 66;
    /// Create a directory: `(path, path_len, mode) -> 0`.
    pub const MKDIR: u64 = 67;
    /// Create a shared-memory region of `frames` pages mapped into the caller:
    /// `(frames) -> shared handle`. The handle can be passed to another process
    /// so both map the identical physical frames.
    pub const SHM_CREATE: u64 = 68;
    /// Map an existing shared-memory region into the caller by handle:
    /// `(handle) -> virtual base`.
    pub const SHM_MAP: u64 = 69;
    /// Drop the caller's mapping of a shared-memory region; the underlying
    /// frames are freed when the last mapping goes away: `(handle) -> 0`.
    pub const SHM_DESTROY: u64 = 70;
    /// Copy the calling process's environment into `(buf, cap)`, in exactly the
    /// same encoding as [`GET_ARGS`]: a count word (u64 LE) followed by the
    /// NUL-terminated `NAME=value` strings. A null `buf` or zero `cap` returns
    /// the required size. `(buf, cap) -> bytes`.
    ///
    /// The environment is *also* staged as `envp` on the initial stack, so a
    /// libc-based program never needs this. It exists for callers that have no
    /// stack to walk — a runtime that fetches its arguments over a syscall, or
    /// a process inspecting a child it did not spawn.
    pub const GET_ENV: u64 = 71;
    /// Put `pid` into process group `pgid`: `(pid, pgid) -> 0`.
    ///
    /// A process may move only itself or one of its children, and only into a
    /// group belonging to the same session. This is what lets a shell put a
    /// pipeline in the background and a terminal aim `^C` at the foreground job
    /// alone.
    pub const SETPGID: u64 = 72;
    /// The calling process's process group id: `() -> pgid`.
    pub const GETPGRP: u64 = 73;
    /// The process group of `pid`: `(pid) -> pgid`.
    pub const GETPGID: u64 = 74;
    /// Create a new session led by the calling process: `() -> sid`.
    pub const SETSID: u64 = 75;
    /// The session of `pid`: `(pid) -> sid`.
    pub const GETSID: u64 = 76;
    /// Install `base` as the calling process's FS base, which is where a libc
    /// keeps its thread-control block pointer. `(base) -> 0`.
    ///
    /// This exists because there is no other way for ring-3 code to reach the
    /// per-CPU MSR: `IA32_FS_BASE` is not a privileged instruction from ring 3,
    /// and the `wrfsbase` alternative needs `CR4.FSGSBASE`, which is not
    /// enabled (writing CR4 hangs this kernel under QEMU, so it stays off).
    ///
    /// Single-threaded by construction -- there is no SMP to make the MSR
    /// per-CPU in any meaningful sense yet, so the value is global.
    pub const SET_FS_BASE: u64 = 77;
    /// Remove a name from a directory. `(dirfd, path, path_len) -> 0`.
    ///
    /// Added for `unlink(2)`. Removing a directory entry is not optional for a
    /// POSIX libc -- `mkstemp`, `rm`, and every build that cleans up after
    /// itself need it -- and the name-to-vnode mapping here is a flat
    /// parent/child table, so removal is a single map delete.
    pub const UNLINK: u64 = 78;
    /// Move a descriptor's cursor. `(fd, offset: i64, whence) -> new offset`.
    ///
    /// Added for `lseek(2)`. Returns `ESPIPE` for a descriptor with no
    /// meaningful cursor (a pipe, a terminal, the console), which is not an
    /// error so much as an answer: stdio probes with it to decide whether a
    /// stream can be repositioned and picks buffering accordingly.
    pub const LSEEK: u64 = 79;
    /// Wall-clock time in milliseconds since the Unix epoch. `() -> ms`.
    ///
    /// Added so `CLOCK_REALTIME` can be a real date. The uptime counter
    /// (`CLOCK_UPTIME_MS`, 2) answers "how long since boot", which no
    /// user-visible timestamp can be built from -- `ls -l` and `date` both came
    /// out as 1970. This reads the MC146818, so the answer survives a reboot.
    pub const CLOCK_REALTIME_MS: u64 = 80;
    /// Set the wall clock. `(secs: i64 since the Unix epoch) -> 0`.
    ///
    /// Paired with the above because a machine whose RTC battery is flat has no
    /// correct date to report, and the kernel cannot invent one. This is how
    /// `stime(2)` / `clock_settime(2)` hand it one.
    pub const SET_TIME: u64 = 81;
    /// Describe an open descriptor. `(fd, buf: ptr<AbiStatEx>) -> 0`.
    ///
    /// Separate from `STAT` (57) because the two answer different questions
    /// and a program needs the difference: `fstat` is how stdio asks what a
    /// terminal or a pipe is, and it must report the *device*, not the file
    /// that happens to sit at the same path. `STAT` predates this and keeps its
    /// original five-field shape; `FSTAT` carries the full set a POSIX `struct
    /// stat` needs, so a path `stat` can be completed from a descriptor's
    /// answer.
    pub const FSTAT: u64 = 82;
    /// Fill a buffer with entropy. `(buf: ptr<u8>, len) -> 0`.
    ///
    /// `ENOSYS` when the machine has no entropy source at all, which is the
    /// honest answer: a caller that gets random bytes it can predict has a
    /// silent security bug, and one that is told "no" can fall back to
    /// something it knows is weak on purpose.
    pub const GETRANDOM: u64 = 83;
    /// Remove an anonymous mapping. `(va, size) -> 0`.
    ///
    /// Added for `munmap(2)`. Without it every mapping leaks for the life of the
    /// process, and `mmap` returning `MAP_FAILED` can never be honoured, so a
    /// program that maps defensively has no way to back out.
    pub const MUNMAP: u64 = 84;
    /// Change a mapping's protection. `(va, size, prot) -> 0`.
    ///
    /// Added for `mprotect(2)`. A mapping is created read/write/no-execute and
    /// stays that way, so a page that a program means to make read-only (or
    /// executable, for a JIT) silently is not.
    pub const MPROTECT: u64 = 85;
    /// Map a device node's physical range. `(fd, offset, len, prot) -> va`.
    ///
    /// The file-backed half of `mmap(2)`. A terminal reaches a linear
    /// framebuffer by mapping it, not by writing pixels one at a time, and
    /// without this the only way to reach `/dev/fb0` is a `write(2)` per pixel --
    /// which is far too slow to redraw a screen.
    ///
    /// `offset` and `len` are in bytes from the start of the device's range, so
    /// a program mmaps the same region at the same offsets it would on Linux.
    /// Only ranges a device actually declares are mappable, which is what stops
    /// a process turning this into "map any physical address I like".
    pub const DEVICE_MMAP: u64 = 86;
    /// Write the `/dev/...` path of the node behind a descriptor.
    /// `(fd, buf, len) -> bytes written`, or a negative errno.
    ///
    /// `ttyname(3)`: a program needs the *path* of its terminal, not the
    /// descriptor, so it can reopen it, hand it to a child, or report it. The
    /// distinction matters in practice -- a terminal is routinely reached
    /// through a descriptor that is a dup, a pipe end, or a descriptor passed
    /// over IPC, none of which say where they came from.
    ///
    /// The path is written with a terminating NUL, and the returned count
    /// includes that NUL, as Linux does. `len` is the size of the caller's
    /// buffer, and a buffer too small yields ERANGE rather than a truncated
    /// path: half a device path is not a path, and a program that reopened it
    /// would fail somewhere unrelated to the real problem.
    ///
    /// Fails with ENOTTY when the descriptor is not a terminal and ENOENT when
    /// it is one whose path cannot be recovered -- which is the honest answer
    /// for a terminal that did not come from devfs, and better than a
    /// fabricated path that would not open.
    pub const TTYNAME: u64 = 87;
    /// `(oldfd) -> newfd`. Duplicate a descriptor, sharing its file offset.
    ///
    /// The sharing is the whole point and the reason this is not a copy. Two
    /// descriptors over one open file description must advance together, and
    /// that is what every shell redirection and every `dup2`-based protocol
    /// handshake is built on.
    pub const DUP: u64 = 88;
    /// `(oldfd, newfd) -> newfd`. As [`DUP`] but into a specific slot,
    /// closing whatever was there.
    ///
    /// `oldfd == newfd` succeeds and changes nothing, as POSIX requires.
    pub const DUP2: u64 = 89;
    /// `(nfds, rfds, wfds, efds, timeout, sigmask) -> ready count`.
    ///
    /// `pselect(6)`, and the syscall behind libc `select(3)` -- mlibc routes
    /// `select` through this with a null signal mask. `timeout` is a
    /// `struct timespec` or null for an indefinite wait; each of the three
    /// descriptor arguments is a `fd_set` (1024 bits) or null.
    ///
    /// A non-null `sigmask` is refused with ENOSYS. Doing it properly means
    /// swapping the blocked mask atomically with respect to delivery, and a
    /// subtly wrong version is worse than none.
    pub const PSELECT6: u64 = 90;
    /// Create a symbolic link: `(target, target_len, path, path_len) -> 0`.
    ///
    /// `symlink(2)`. The link's own *contents* are `target`, stored verbatim and
    /// not resolved: a relative target is meaningless without knowing which
    /// directory the link sits in, so the kernel records what the caller asked
    /// for and the path walker works it out per lookup. Resolving at creation
    /// time would produce a link that breaks the moment it is moved, which is the
    /// whole reason relative links exist.
    ///
    /// Both strings are counted, not NUL-terminated, and both may contain no NUL
    /// at all -- a path with an embedded NUL is `EINVAL`, not a truncation.
    ///
    /// Fails with `EEXIST` when the link's own name is taken, *even if the
    /// target does not exist*. A symlink is allowed to dangle; the question
    /// `symlink` answers is whether the *name* is free, and making it depend on
    /// the target would mean a caller that fixed the target and retried could not
    /// tell which of the two failures it was looking at.
    pub const SYMLINK: u64 = 91;
    /// Read a symbolic link's target: `(path, path_len, buf, buflen) -> length`.
    ///
    /// `readlink(2)`. Returns the target exactly as stored, relative or absolute,
    /// with no NUL appended -- which is what makes `readlink -f` in a shell able
    /// to compose a path without having to strip anything.
    ///
    /// Fails with `EINVAL` when the path is not a symbolic link. Following the
    /// link and reporting what it names would make `readlink` a second `stat`,
    /// and a caller asking "is this a link, and if so what does it say" would get
    /// an answer to a question it did not ask.
    ///
    /// Fails with `ERANGE` when the buffer is too small, rather than truncating.
    /// A truncated target is a path that does not exist, and a program that
    /// `stat`ed it would report a missing file rather than a short buffer.
    pub const READLINK: u64 = 92;
    /// `lstat(2)`: `(path, path_len, out) -> 0`, like [`STAT`] but a final
    /// symbolic link is not followed.
    ///
    /// A separate number rather than a flag in `STAT`'s unused fourth argument.
    /// `STAT` is documented as a frozen five-field shape that cannot grow, and
    /// the tempting move -- give `a4` a meaning -- would change the behaviour of
    /// every existing caller that happens to pass something there. A program
    /// calling a three-argument syscall leaves `r10` holding whatever it had; if
    /// that value happened to be 1, `stat` would silently become `lstat` and
    /// report the link instead of the file. Nothing would fail. The appended
    /// number costs one line in the table and cannot surprise anyone.
    pub const LSTAT: u64 = 93;
    /// `execve(2)`: `(path, path_len, argv) -> never` on success.
    ///
    /// Exec a program *by path*. [`EXEC`] names a program by its index in the
    /// kernel's embedded table, which is how the boot servers and the self-tests
    /// are started; this is the other thing a Unix has, and without it a file in
    /// the filesystem cannot be run at all.
    ///
    /// Without it the whole of a userland is decorative. `/bin/ls` can be a symlink
    /// to `/bin/busybox` and resolve correctly, and still be unrunnable, because
    /// the only exec available names something the kernel already holds. A shell
    /// could run its builtins and its in-process applets and nothing else, which is
    /// the shape of a system that has a binary and no way to start it.
    ///
    /// `argv` is the usual pointer array, and `argv[0]` is the conventional
    /// program name. The path is followed through symbolic links, as it is for
    /// every other path-taking call here: the caller asked to run what the name
    /// refers to now.
    ///
    /// `envp` is accepted and ignored, as it is for [`EXEC`]: an exec'd image
    /// inherits the caller's environment, which is what `EXEC` does and what makes
    /// the two consistent. A program that wants to *replace* its environment has
    /// no way to here, and that gap is recorded here rather than left to be
    /// discovered.
    pub const EXECVE: u64 = 94;
    /// `fcntl(F_DUPFD)`: `(fd, floor) -> newfd`.
    ///
    /// Duplicates `fd` onto the lowest free descriptor *at or above* `floor`.
    /// [`DUP`] is the same operation without the floor, and it cannot stand in
    /// here: it returns the lowest free descriptor, which is normally 0-2 --
    /// exactly where a shell keeps its own. Picking a slot that is genuinely free
    /// needs to know which slots are in use, and only the descriptor table knows
    /// that, so this is a separate number rather than a loop over `DUP2` in the
    /// libc. A loop would have to probe for emptiness by attempting a `DUP2`,
    /// which *closes* whatever it finds there.
    ///
    /// Added for `fcntl(2)`. A shell cannot enable job control without it: `ash`
    /// opens `/dev/tty`, moves the descriptor somewhere out of its own way, and
    /// reads a failure as "there is no terminal" -- so it printed "can't access
    /// tty; job control turned off" and stayed that way for the whole session,
    /// on a terminal that worked.
    pub const DUPFD: u64 = 95;
    /// First number reserved for out-of-tree/experimental use.
    pub const EXPERIMENTAL_BASE: u64 = 0x8000_0000_0000_0000;
}

/// Maximum stable syscall number reserved by the ABI.
pub const NR_MAX: usize = 4096;

/// User-visible IPC message frame. The kernel copies this structure verbatim
/// across address spaces; `data_len` bounds the trailing byte blob.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MsgFrame {
    /// Application-defined tag.
    pub tag: u32,
    /// Source endpoint id; meaningful on frames returned by `IPC_RECV`.
    pub from: u32,
    /// Call correlation id (kernel-issued for `Call`s, echoed in `Reply`).
    pub call_id: u64,
    /// Twelve argument words; protocol-defined meaning.
    pub args: [u64; 12],
    /// Number of valid bytes in `data`.
    pub data_len: u32,
    /// Message kind on frames returned by `IPC_RECV` ([`crate::ipc::MsgKind`]).
    pub kind: u32,
    /// Payload bytes (see [`crate::ipc::MAX_MSG_DATA`]).
    pub data: [u8; crate::ipc::MAX_MSG_DATA],
}

impl MsgFrame {
    /// Build an empty frame.
    pub const fn new() -> Self {
        MsgFrame {
            tag: 0,
            from: 0,
            call_id: 0,
            args: [0; 12],
            data_len: 0,
            kind: 0,
            data: [0; crate::ipc::MAX_MSG_DATA],
        }
    }

    /// Extract the trailing payload as a slice.
    pub fn payload(&self) -> &[u8] {
        &self.data[..self.data_len.min(crate::ipc::MAX_MSG_DATA as u32) as usize]
    }
}

/// User-visible framebuffer geometry, filled in by `FB_INFO`.
///
/// The mapping window starts at `phys` (page-aligned); the first visible pixel
/// is `offset` bytes into it. `size` is the total byte length to map. Channels
/// are described so a compositor can pack colors for any direct-RGB mode.
#[repr(C)]
#[derive(Clone, Copy)]
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

type SyscallFn = fn(u64, u64, u64, u64, u64, u64) -> i64;

static mut SYSCALL_TABLE: [Option<SyscallFn>; NR_MAX] = [None; NR_MAX];

/// Register a handler under a stable syscall number.
///
/// Later registration of the same number is ignored (the first registrant
/// wins); this will become an explicit error once the syscall registry
/// moves to a formal capability model.
pub fn register(number: u64, handler: SyscallFn) {
    if number >= NR_MAX as u64 {
        crate::log::kwarn!("abi: refusing experimental syscall {:#x}", number);
        return;
    }
    unsafe {
        let slot = &mut SYSCALL_TABLE[number as usize];
        if slot.is_none() {
            *slot = Some(handler);
        }
    }
}

/// Core dispatcher invoked from the assembly entry stub.
///
/// # Safety
/// Called only from `syscall_entry` with an established kernel stack and
/// saved user register frame.
#[no_mangle]
unsafe extern "C" fn samsara_dispatch(nr: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, a6: u64) -> i64 {
    if nr >= NR_MAX as u64 {
        return errno::ENOSYS;
    }
    match unsafe { SYSCALL_TABLE[nr as usize] } {
        Some(f) => f(a1, a2, a3, a4, a5, a6),
        None => errno::ENOSYS,
    }
}

/// Kernel identity string returned by `KERNEL_VERSION`.
pub const KERNEL_VERSION_STRING: &[u8] =
    b"Samsara/Nutcracker 0.1.0; (C) 2026 Harsh Nikarsa; GPLv3+\n";

// ---------------------------------------------------------------------------
// Default syscall implementations
// ---------------------------------------------------------------------------

/// Number of leading bytes in `[ptr, ptr+len)` that live in mapped pages of
/// the current address space. Used as a safety net when a syscall touches a
/// user-supplied buffer: every page boundary in the range is checked, and
/// access is constrained to the contiguous mapped prefix.
///
/// The check runs against the page tables active in CR3 *now* — the calling
/// process's own. The kernel's static `ACTIVE_SPACE` is the *kernel* address
/// space and contains no user mappings, so translating through it would flag
/// every user buffer as unmapped.
fn validate_range(ptr: usize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let first = ptr & !0xFFF;
    let last = (ptr + len - 1) & !0xFFF;
    for page in (first..=last).step_by(0x1000) {
        if crate::memory::vmm::translate_current(page).is_none() {
            return page.saturating_sub(ptr).min(len);
        }
    }
    len
}

fn sys_debug_write(buf: u64, len: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let len = len.min(4096) as usize;
    let safe = validate_range(buf as usize, len);
    if safe == 0 {
        return 0;
    }
    let bytes = unsafe { core::slice::from_raw_parts(buf as *const u8, safe) };
    crate::io::uart::write(bytes);
    crate::console::write(bytes);
    safe as i64
}

fn sys_yield(_a1: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    crate::task::sched::yield_now();
    0
}

fn sys_clock_uptime_ms(_a1: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    crate::time::millis() as i64
}

fn sys_kernel_version(buf: u64, len: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let want = (len as usize).min(KERNEL_VERSION_STRING.len());
    let safe = validate_range(buf as usize, want);
    if safe == 0 {
        return 0;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(KERNEL_VERSION_STRING.as_ptr(), buf as *mut u8, safe);
    }
    safe as i64
}

fn sys_exit(_a1: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> ! {
    crate::log::kwarn!("abi: EXIT called; halting machine");
    loop {
        unsafe { asm!("cli; hlt", options(nomem, nostack)) };
    }
}

/// Install the default syscall surface.
pub fn register_defaults() {
    register(nr::DEBUG_WRITE, sys_debug_write);
    register(nr::YIELD, |_, _, _, _, _, _| sys_yield(0, 0, 0, 0, 0, 0));
    register(nr::CLOCK_UPTIME_MS, |_, _, _, _, _, _| {
        sys_clock_uptime_ms(0, 0, 0, 0, 0, 0)
    });
    register(nr::KERNEL_VERSION, sys_kernel_version);
    register(nr::EXIT, |_, _, _, _, _, _| sys_exit(0, 0, 0, 0, 0, 0));
    register(nr::OPEN, sys_open);
    register(nr::CLOSE, sys_close);
    register(nr::READ, sys_read);
    register(nr::WRITE, sys_write);
    register(nr::PIPE, sys_pipe);

    // File metadata + credentials / permissions surface.
    register(nr::STAT, sys_stat);
    register(nr::CHMOD, sys_chmod);
    register(nr::CHOWN, sys_chown);
    register(nr::KILL, sys_kill);
    register(nr::GETUID, sys_getuid);
    register(nr::GETGID, sys_getgid);
    register(nr::GETEUID, sys_geteuid);
    register(nr::GETEGID, sys_getegid);
    register(nr::GETRESUID, sys_getresuid);
    register(nr::GETRESGID, sys_getresgid);
    register(nr::GETGROUPS, sys_getgroups);
    register(nr::SETGROUPS, sys_setgroups);
    // The credential getters. Handlers for all four already existed; they were
    // simply never registered, so the numbers were reserved and the calls went
    // nowhere.
    //
    // The symptom was unusually indirect. mlibc turns a *missing sysdep* into a
    // panic rather than a failed call, so a program whose first statement was
    // `seteuid(getuid())` -- which is what fbterm's main() does -- died with a
    // libc assertion and no message of its own. An unregistered syscall number
    // producing a panic inside the libc, rather than an ENOSYS the program could
    // handle, is worth remembering when adding numbers to the table.
    register(nr::GETUID, sys_getuid);
    register(nr::GETEUID, sys_geteuid);
    register(nr::GETGID, sys_getgid);
    register(nr::GETEGID, sys_getegid);
    register(nr::SETUID, sys_setuid);
    register(nr::SETGID, sys_setgid);
    register(nr::SETEUID, sys_seteuid);
    register(nr::SETEGID, sys_setegid);
    register(nr::SETREUID, sys_setreuid);
    register(nr::SETREGID, sys_setregid);
    register(nr::SETRESUID, sys_setresuid);
    register(nr::SETRESGID, sys_setresgid);
    register(nr::SETFSUID, sys_setfsuid);
    register(nr::SETFSGID, sys_setfsgid);
    register(nr::UMASK, sys_umask);

    // Signal surface.
    register(nr::NANOSLEEP, sys_nanosleep);
    register(nr::SIGACTION, sys_sigaction);
    register(nr::SIGPROCMASK, sys_sigprocmask);
    register(nr::SIGSUSPEND, sys_sigsuspend);
    register(nr::SIGALTSTACK, sys_sigaltstack);
    register(nr::SIGRETURN, sys_sigreturn);
    register(nr::SIGPENDING, sys_sigpending);
    register(nr::GET_PID, sys_get_pid);

    // Microkernel surface: IPC, processes, interrupts and memory grants.
    register(nr::IPC_SEND, sys_ipc_send);
    register(nr::IPC_RECV, sys_ipc_recv);
    register(nr::IPC_REPLY, sys_ipc_reply);
    register(nr::IPC_CALL, sys_ipc_call);
    register(nr::PROC_SPAWN, sys_proc_spawn);
    register(nr::PROC_EXIT, sys_proc_exit);
    register(nr::GET_EPID, sys_get_epid);
    register(nr::PORT_ALLOW, sys_port_allow);
    register(nr::IRQ_BIND, sys_irq_bind);
    register(nr::IRQ_UNBIND, sys_irq_unbind);
    register(nr::MAP_ANON, sys_map_anon);
    register(nr::MAP_PHYS, sys_map_phys);
    register(nr::DMA_ALLOC, sys_dma_alloc);
    register(nr::DMA_FREE, sys_dma_free);
    register(nr::FORK, sys_fork);
    register(nr::EXEC, sys_exec);
    register(nr::WAITPID, sys_waitpid);
    register(nr::POLL, sys_poll);
    register(nr::IOCTL, sys_ioctl);
    register(nr::FB_INFO, sys_fb_info);
    register(nr::CONSOLE_DETACH, sys_console_detach);
    register(nr::CONSOLE_ATTACH, sys_console_attach);
    register(nr::GET_ARGS, sys_get_args);
    register(nr::GET_ENV, sys_get_env);
    register(nr::SETPGID, sys_setpgid);
    register(nr::GETPGRP, sys_getpgrp);
    register(nr::GETPGID, sys_getpgid);
    register(nr::SETSID, sys_setsid);
    register(nr::GETSID, sys_getsid);
    register(nr::SET_FS_BASE, sys_set_fs_base);
    register(nr::CHDIR, sys_chdir);
    register(nr::GETCWD, sys_getcwd);
    register(nr::READDIR, sys_readdir);
    register(nr::UNLINK, sys_unlink);
    register(nr::LSEEK, sys_lseek);
    register(nr::CLOCK_REALTIME_MS, sys_clock_realtime_ms);
    register(nr::SET_TIME, sys_set_time);
    register(nr::FSTAT, sys_fstat);
    register(nr::GETRANDOM, sys_getrandom);
    register(nr::MUNMAP, sys_munmap);
    register(nr::MPROTECT, sys_mprotect);
    register(nr::DEVICE_MMAP, sys_device_mmap);
    register(nr::TTYNAME, sys_ttyname);
    register(nr::DUP, sys_dup);
    register(nr::DUP2, sys_dup2);
    register(nr::PSELECT6, sys_pselect6);
    register(nr::MKDIR, sys_mkdir);
    register(nr::SHM_CREATE, sys_shm_create);
    register(nr::SHM_MAP, sys_shm_map);
    register(nr::SHM_DESTROY, sys_shm_destroy);
    register(nr::SYMLINK, sys_symlink);
    register(nr::READLINK, sys_readlink);
    register(nr::LSTAT, sys_lstat);
    register(nr::EXECVE, sys_execve);
    register(nr::DUPFD, sys_dupfd);

    unsafe {
        enable_syscall_instruction();
    }
    // Counted from the table rather than written down, because a hand-maintained
    // number here is wrong the moment a syscall is added and nothing notices: it
    // is a log line, so it is exactly the kind of fact that can rot in silence.
    // The count that used to be printed was 68 while 97 were registered, which is
    // the failure this replaces.
    let registered = unsafe {
        SYSCALL_TABLE
            .iter()
            .filter(|s| s.is_some())
            .count()
    };
    crate::log::kdebug!(
        "abi: {} syscalls registered; `syscall` enabled",
        registered
    );
}

// ---------------------------------------------------------------------------
// File descriptor syscall implementations
// ---------------------------------------------------------------------------

/// # Safety
/// The buffer must reference mapped memory in the calling process's address
/// space (validated per page against the CR3 currently in use).
unsafe fn user_bytes(buf: u64, len: u64) -> Option<&'static mut [u8]> {
    let len = len.min(1 << 20) as usize;
    // A zero-length buffer is a real case -- `ioctl(fd, TIOCSCTTY, 0)` and
    // `read(fd, buf, 0)` both ask for one -- and it has to be answered without
    // touching `buf` at all.
    //
    // It used to fall through to `from_raw_parts_mut(buf, 0)`, which is instant
    // undefined behaviour: a null pointer is not a valid `&mut [u8]` even when
    // the length is zero, and Rust's rules say so for exactly this case. The
    // observable consequence was worse than "technically UB". LLVM is entitled
    // to assume the pointer a reference is built from is non-null, so the
    // optimiser could conclude the `Option` was always `Some` -- and did: the
    // function returned `Some`, and the caller observed `None`, for the same
    // call, in the same build. Nothing in the source explains that, which is
    // what made it expensive to find.
    //
    // An empty slice is returned instead, from a literal. It is a valid empty
    // borrow with no pointer to be wrong about, and it costs one branch that was
    // already being taken.
    if len == 0 {
        return Some(&mut []);
    }
    let ptr = buf as usize;
    // Validate every page in range through the current address space.
    let first = ptr & !0xFFF;
    let last = (ptr + len - 1) & !0xFFF;
    for page in (first..=last).step_by(0x1000) {
        crate::memory::vmm::translate_current(page)?;
    }
    Some(core::slice::from_raw_parts_mut(buf as *mut u8, len))
}

/// Read a `u32` from a user pointer, validating the page it lives on.
///
/// Separate from [`user_bytes`] because `TIOCSCTTY` needs to dereference a
/// caller-supplied pointer at exactly one place, and doing it through the usual
/// borrow-of-the-whole-buffer path would be a way to make that read look routine
/// when it is the one read in this syscall that trusts an address nobody has
/// checked. Returning `None` for an unmapped pointer is the answer a caller wants
/// anyway: a `TIOCSCTTY` argument pointing at nothing is a failed session
/// assertion, not a kernel fault.
pub(crate) fn user_read_u32(ptr: u64) -> Option<u32> {
    let bytes = unsafe { user_bytes(ptr, 4) }?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn current_task() -> Result<usize, i64> {
    crate::task::sched::current_task_id()
        .map(|t| t.0)
        .ok_or(crate::abi::errno::EPERM)
}

/// Copy a NUL-tolerating path string out of user memory.
fn user_str(ptr: u64, len: u64) -> Result<alloc::string::String, i64> {
    let bytes = unsafe { user_bytes(ptr, len) }.ok_or(errno::EINVAL)?;
    let s = core::str::from_utf8(bytes)
        .map_err(|_| errno::EINVAL)?
        .trim_end_matches('\0');
    Ok(alloc::string::String::from(s))
}

/// Make a path absolute: as-is when it already starts with `/`, otherwise
/// prefixed with the calling process's working directory. The VFS normalizes
/// the joined result (collapsing `.`/`..` and repeated slashes).
fn task_abs_path(path: &str) -> alloc::string::String {
    if path.starts_with('/') || path.is_empty() {
        alloc::string::String::from(path)
    } else {
        alloc::format!("{}/{}", crate::task::sched::current_cwd(), path)
    }
}

/// Copy a NUL-terminated string out of user memory, bounded to 4 KiB.
///
/// # Safety
/// `ptr` must lie in the calling process's address space; each byte is
/// validated against the current CR3 before it is read.
unsafe fn read_user_strz(ptr: u64) -> Option<alloc::string::String> {
    let mut v = alloc::vec::Vec::new();
    for i in 0..4096u64 {
        let slice = user_bytes(ptr + i, 1)?;
        let b = slice[0];
        if b == 0 {
            return Some(alloc::string::String::from_utf8_lossy(&v).into_owned());
        }
        v.push(b);
    }
    None
}

/// Read a classic `char *argv[]` (a NUL-terminated array of NUL-terminated
/// string pointers) out of the calling process's memory. A null `arr` pointer
/// yields an empty vector (the legacy no-argument form).
///
/// # Safety
/// Every cell and string byte is validated against the current CR3 before
/// dereference; malformed layouts stop the walk early instead of faulting.
unsafe fn read_argv(arr: u64) -> alloc::vec::Vec<alloc::string::String> {
    let mut out = alloc::vec::Vec::new();
    if arr == 0 {
        return out;
    }
    for i in 0..128u64 {
        let cell = match user_bytes(arr + i * 8, 8) {
            Some(b) => b,
            None => break,
        };
        let ptr = u64::from_le_bytes(cell[..8].try_into().unwrap());
        if ptr == 0 {
            break;
        }
        match unsafe { read_user_strz(ptr) } {
            Some(s) => out.push(s),
            None => break,
        }
    }
    out
}

/// Serialize a string vector the way `GET_ARGS` and `GET_ENV` hand it out: a
/// count word (u64 LE) followed by the NUL-terminated strings. Returns the
/// number of bytes written, or the required size when `buf` is null / `cap` is
/// zero.
fn copy_strvec_out(items: &[String], buf: u64, cap: u64) -> Result<i64, i64> {
    let need = 8 + items.iter().map(|a| a.len() + 1).sum::<usize>();
    if buf == 0 || cap == 0 {
        return Ok(need as i64);
    }
    let cap = cap.min(need as u64) as usize;
    if cap < 8 {
        return Ok(need as i64);
    }
    let bytes = unsafe { user_bytes(buf, cap as u64) }.ok_or(errno::EINVAL)?;
    bytes[..8].copy_from_slice(&(items.len() as u64).to_le_bytes());
    let mut off = 8usize;
    for a in items {
        if off >= cap {
            break;
        }
        let room = cap - off;
        let want = a.len() + 1;
        let take = want.min(room);
        let sbytes = take.saturating_sub(1).min(a.len());
        bytes[off..off + sbytes].copy_from_slice(&a.as_bytes()[..sbytes]);
        if take == want {
            bytes[off + a.len()] = 0;
        }
        off += take;
    }
    Ok(off as i64)
}

/// Copy the serialized argument vector for task `task` into user memory at
/// `buf` (capacity `cap`), in the `GET_ARGS` encoding.
fn copy_args_out(task: usize, buf: u64, cap: u64) -> Result<i64, i64> {
    let args = crate::task::sched::task_args(task);
    copy_strvec_out(&args, buf, cap)
}

/// Credentials of the calling process.
fn current_cred() -> Result<crate::cred::Credentials, i64> {
    Ok(crate::cred::get(current_task()?))
}

fn sys_open(path: u64, path_len: u64, flags: u64, mode: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    let flags = flags as u32;
    let accmode = flags & crate::vfs::O_ACCMODE;
    let bytes = match unsafe { user_bytes(path, path_len) } {
        Some(b) => b,
        None => return crate::abi::errno::EINVAL,
    };
    let path_str = match core::str::from_utf8(bytes) {
        Ok(s) => s.trim_end_matches('\0'),
        Err(_) => return crate::abi::errno::EINVAL,
    };
    let cred = crate::cred::get(task);

    // Resolve (the final component is *not* access-checked by the resolver).
    // Relative paths resolve against the calling process's working directory.
    let abs = task_abs_path(path_str);

    // The pty multiplexer allocates on open, so it is intercepted before the
    // ordinary path walk: there is no node to resolve to a terminal.
    if abs == "/dev/ptmx" {
        return match open_ptmx(task, accmode, flags & crate::vfs::F_SETFL_MASK) {
            Some(fd) => fd,
            None => errno::ENOMEM,
        };
    }

    // `/dev/tty` names the calling session's controlling terminal, so which node
    // it resolves to depends on *who is asking* -- it cannot be a fixed entry in
    // the device table the way `/dev/fb0` is. It is intercepted here for the same
    // reason `/dev/ptmx` is, and for the same reason the answer may be an error:
    // a session with no controlling terminal has to be told so.
    if abs == "/dev/tty" {
        return open_ctty(task, accmode, flags & crate::vfs::F_SETFL_MASK);
    }

    let node = match crate::vfs::resolve_checked(&abs, &cred) {
        Ok(n) => n,
        Err(crate::vfs::FsError::NotFound) if flags & crate::vfs::O_CREAT != 0 => {
            let want = ((mode as u32) & driver_common::S_PERM_MASK) & !crate::cred::umask_of(task);
            match crate::vfs::create_checked(
                &abs,
                crate::vfs::NodeKind::File,
                cred.fsuid,
                cred.fsgid,
                want,
                &cred,
            ) {
                Ok(n) => n,
                Err(e) => return e.into(),
            }
        }
        Err(e) => return e.into(),
    };

    // O_CREAT|O_EXCL: the file must not already exist.
    if flags & (crate::vfs::O_CREAT | crate::vfs::O_EXCL) == crate::vfs::O_CREAT | crate::vfs::O_EXCL
    {
        return crate::abi::errno::EEXIST;
    }

    // Check the requested access modes against the resolved file.
    let can_r = accmode != crate::vfs::O_WRONLY
        && crate::cred::may_access(&cred, node.uid(), node.gid(), node.mode(), crate::cred::Access::Read);
    let can_w = accmode != crate::vfs::O_RDONLY
        && crate::cred::may_access(&cred, node.uid(), node.gid(), node.mode(), crate::cred::Access::Write);
    if (accmode == crate::vfs::O_RDONLY && !can_r)
        || (accmode == crate::vfs::O_WRONLY && !can_w)
        || (accmode == crate::vfs::O_RDWR && !(can_r && can_w))
    {
        return crate::abi::errno::EACCES;
    }

    if flags & crate::vfs::O_TRUNC != 0 {
        if node.kind() == crate::vfs::NodeKind::File {
            if let Err(e) = node.truncate() {
                return e.into();
            }
        }
    }

    acquire_controlling_tty(task, &node);

    crate::vfs::fdtab::install_full(
        task,
        node,
        accmode,
        flags & crate::vfs::F_SETFL_MASK,
    ) as i64
}

/// Claim `node` as the calling session's controlling terminal, if it is a
/// terminal and nothing owns it yet.
///
/// This is the POSIX controlling-terminal rule, and it is what makes
/// `TIOCGSID` meaningful and terminal-generated signals reach the session
/// rather than a stray process.
fn acquire_controlling_tty(task: usize, node: &crate::vfs::VnodeRef) {
    if !node.is_terminal() {
        return;
    }
    let sid = crate::task::sched::task_groups(task).sid;
    node.acquire_session(sid);
}

/// Open the calling session's controlling terminal, for `/dev/tty`.
///
/// Which terminal that is depends on the caller, so this cannot be a node in the
/// device table. The terminal is found by asking the pty driver for the slave
/// whose recorded session is the caller's -- the same pairing `TIOCSCTTY`
/// established.
///
/// `ENXIO` when the session has no controlling terminal, which is the POSIX
/// answer and the one a program can act on: `ash` opens `/dev/tty` to find the
/// terminal it is attached to, and "no controlling terminal" is a fact about the
/// session rather than a failure to open a file. Returning `ENOENT` here would
/// be the more common choice and would be wrong, because the file does exist.
///
/// A session that has never taken a terminal still gets one here if it is
/// attached to a terminal another way -- specifically, if the calling task
/// already has a terminal open on descriptor 0, 1 or 2. That is the case a
/// `setsid`-ed program lands in, and it is what makes `daemon`-style code work
/// without it having to guess a `/dev/pts/N`.
fn open_ctty(task: usize, accmode: u32, status: u32) -> i64 {
    let sid = crate::task::sched::current_sid();
    // The session's own terminal first, then the descriptor fallback. The
    // wrapper is built here rather than in the driver because the driver crate
    // cannot see the VFS, and because `open_ptmx` already wraps a terminal this
    // way -- two wrappers for one terminal would make the VFS's node identity
    // checks disagree with themselves.
    let node = match crate::drivers::pty::controlling_tty(sid) {
        Some(slave) => crate::vfs::devfs::anonymous(alloc::sync::Arc::new(
            crate::drivers::pty::PtySlaveDevice::new(slave),
        )),
        None => match terminal_from_standard_fds(task) {
            Some(n) => n,
            None => return errno::ENXIO,
        },
    };
    crate::vfs::fdtab::install_full(task, node, accmode, status) as i64
}

/// The terminal on descriptor 0, 1 or 2 of `task`, if any of them is one.
///
/// A fallback for [`open_ctty`], and the reason `/dev/tty` works for a session
/// that never called `TIOCSCTTY`: the process inherited those descriptors from
/// whoever started it, and on a system where a terminal is a pty slave that is
/// exactly the terminal in question. Returns the node so the descriptor sees the
/// same terminal rather than a copy of its state.
fn terminal_from_standard_fds(task: usize) -> Option<crate::vfs::VnodeRef> {
    for fd in 0..3 {
        if let Ok(node) = crate::vfs::fdtab::get(task, fd) {
            if node.is_terminal() {
                return Some(node);
            }
        }
    }
    None
}

/// Open the pty multiplexer at `abs`.
///
/// `/dev/ptmx` is not a terminal: opening it allocates a *new* master/slave
/// pair, publishes the slave as `/dev/pts<N>`, and installs the master as the
/// descriptor's node. The caller finds `N` with `TIOCGPTN`, or derives it from
/// the slave name it opens next.
///
/// A fixed `/dev/ptmx` bound to one pair would make every program that opened
/// it share a single terminal, and whichever reader won the race would consume
/// the other's keystrokes.
fn open_ptmx(task: usize, accmode: u32, status: u32) -> Option<i64> {
    let (master, index) = crate::drivers::pty::open_ptmx()?;
    let name = alloc::format!("{}", index);
    let slave = crate::drivers::pty::slave_of(&master);
    // Published as `/dev/pts/<n>`, which is the path `ptsname(3)` reports.
    // A program handed that name by the libc then has to be able to open it,
    // and a flat `/dev/pts<n>` would leave every `ptsname` naming nothing.
    if crate::vfs::devfs::register_in_dir(
        "pts",
        &name,
        crate::vfs::devfs::anonymous(alloc::sync::Arc::new(
            crate::drivers::pty::PtySlaveDevice::new(slave),
        )),
    )
    .is_err()
    {
        return Some(errno::EIO);
    }
    let node = crate::vfs::devfs::anonymous(alloc::sync::Arc::new(
        crate::drivers::pty::PtyMasterDevice::new(master),
    ));
    Some(crate::vfs::fdtab::install_full(task, node, accmode, status) as i64)
}

fn sys_close(fd: u64, _a1: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    match crate::vfs::fdtab::close(task, fd as usize) {
        Ok(_) => 0,
        Err(e) => e.into(),
    }
}

fn sys_read(fd: u64, buf: u64, len: u64, _a3: u64, _a4: u64, _a5: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    let mem = match unsafe { user_bytes(buf, len) } {
        Some(m) => m,
        None => return crate::abi::errno::EINVAL,
    };
    loop {
        match crate::vfs::fdtab::read(task, fd as usize, mem) {
            Ok(n) => return n as i64,
            Err(e) if e.is_interrupted() => {
                // `EINTR`: retry transparently when every deliverable signal
                // requested `SA_RESTART`, otherwise surface it.
                if crate::sig::should_restart() {
                    crate::sig::defer_pending();
                    continue;
                }
                return crate::abi::errno::EINTR;
            }
            Err(e) => return e.into(),
        }
    }
}

fn sys_write(fd: u64, buf: u64, len: u64, _a3: u64, _a4: u64, _a5: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    let mem = match unsafe { user_bytes(buf, len) } {
        Some(m) => m,
        None => return crate::abi::errno::EINVAL,
    };
    loop {
        match crate::vfs::fdtab::write(task, fd as usize, mem) {
            Ok(n) => return n as i64,
            Err(e) if e.is_interrupted() => {
                if crate::sig::should_restart() {
                    crate::sig::defer_pending();
                    continue;
                }
                return crate::abi::errno::EINTR;
            }
            Err(e) => return e.into(),
        }
    }
}

/// Linux-compatible framebuffer ioctl requests, and the two structures they
/// exchange with `/dev/fb0`.
///
/// These are Linux's UAPI values, copied rather than invented, and the struct
/// layouts below are laid out field for field and type for type as Linux has
/// them. That is not a style preference: the kernel writes one of these into a
/// user buffer and a ported terminal reads it with its own `<linux/fb.h>`, and
/// the two definitions are compiled independently. A single wrong field width
/// shifts every field after it, and the symptom is a terminal that believes it
/// has a 0-bit framebuffer and refuses to start -- not a crash, and not anything
/// that points at the cause.
///
/// The two C headers that must agree with these are:
///   - ports/fbterm/include/linux/fb.h
///   - any future consumer's own <linux/fb.h>
///
/// A mismatch between them is the failure mode to watch for when either side is
/// edited. There is no way for the compiler to catch it, which is why the
/// layout is duplicated in full rather than trimmed to the fields in use: a
/// trimmed struct would move every following field and hide the disagreement
/// behind a struct that merely happens to be smaller.
pub mod framebuffer_ioctl {
    use crate::framebuffer::FbGeometry;

    /// Report the display mode. Linux's request number.
    pub const FBIOGET_VSCREENINFO: u32 = 0x4600;
    /// Report the display's fixed properties. Linux's request number.
    pub const FBIOGET_FSCREENINFO: u32 = 0x4602;

    /// Pixel storage classes, from `linux/fb.h`.
    pub const FB_TYPE_PACKED_PIXELS: u32 = 0;
    /// Visual type for a packed truecolour display.
    pub const FB_VISUAL_TRUECOLOR: u32 = 2;

    /// One colour channel within a pixel.
    ///
    /// `msb_right` is left zero, which is correct rather than merely convenient:
    /// x86 is little-endian and the bootloader's framebuffer tag describes
    /// left-aligned channels, so zero is the true value. Setting it would tell a
    /// program to reverse the channel it is about to write.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct FbBitfield {
        /// Bit position of the channel within one pixel.
        pub offset: u32,
        /// Width of the channel in bits.
        pub length: u32,
        /// Non-zero when the channel's most significant bit is rightmost.
        ///
        /// Always zero here: x86 is little-endian and the bootloader's tag
        /// describes left-aligned channels. Setting it would tell a program to
        /// reverse the channel it is about to write.
        pub msb_right: u32,
    }

    /// `struct fb_var_screeninfo` from `linux/fb.h`.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct FbVarScreeninfo {
        /// Visible width in pixels.
        pub xres: u32,
        /// Visible height in pixels.
        pub yres: u32,
        /// Width of the backing buffer. Equal to `xres`: there is no overscan.
        pub xres_virtual: u32,
        /// Height of the backing buffer. Equal to `yres`: there is no overscan.
        pub yres_virtual: u32,
        /// Horizontal pan offset. Always zero: the display cannot pan.
        pub xoffset: u32,
        /// Vertical pan offset. Always zero: the display cannot pan.
        pub yoffset: u32,
        /// Bits per pixel. 32 on every mode the bootloader or the Bochs
        /// fallback produces.
        pub bits_per_pixel: u32,
        /// Non-zero for a greyscale mode. Always zero here.
        pub grayscale: u32,
        /// Red channel placement.
        pub red: FbBitfield,
        /// Green channel placement.
        pub green: FbBitfield,
        /// Blue channel placement.
        pub blue: FbBitfield,
        /// Alpha channel placement. Unused; the modes here are opaque.
        pub transp: FbBitfield,
        /// Non-zero if the mode is not one of the standard types.
        pub nonstd: u32,
        /// Bit mask of which fields should be changed on a mode set. The set
        /// requests that can be honoured are refused rather than ignored.
        pub activate: u32,
        /// Physical display height in millimetres. Zero: unknown.
        pub height: u32,
        /// Physical display width in millimetres. Zero: unknown.
        pub width: u32,
        /// Acceleration flags. Zero: no hardware acceleration.
        pub accel_flags: u32,
        /// Pixel clock in picoseconds. Zero: not applicable, and a terminal
        /// reading a plausible-looking clock would derive a refresh rate for
        /// hardware that has none.
        pub pixclock: u32,
        /// Left margin in pixels. Zero: no pan range.
        pub left_margin: u32,
        /// Right margin in pixels. Zero: no pan range.
        pub right_margin: u32,
        /// Upper margin in pixels. Zero: no pan range.
        pub upper_margin: u32,
        /// Lower margin in pixels. Zero: no pan range.
        pub lower_margin: u32,
        /// Horizontal sync length in pixels.
        pub hsync_len: u32,
        /// Vertical sync length in pixels.
        pub vsync_len: u32,
        /// Bit mask of synchronisation polarity and drive type.
        pub sync: u32,
        /// Bit mask of `FB_VMODE_*` flags. Zero: no vertical panning.
        pub vmode: u32,
        /// Rotation. Zero: none.
        pub rotate: u32,
        /// Colour space. Zero: the default.
        pub colorspace: u32,
        /// Reserved for future use. Always zero.
        pub reserved: [u32; 4],
    }

    /// `struct fb_fix_screeninfo` from `linux/fb.h`.
    ///
    /// `smem_len` is deliberately named for the length it holds rather than
    /// following modern Linux's `smem_start`. A ported fbterm reads this member
    /// as the length argument to `mmap(2)`, so the kernel fills it with a size;
    /// renaming the member to `smem_start` without changing that would make a
    /// terminal map a physical address as a length.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct FbFixScreeninfo {
        /// Driver identifier string, NUL-padded. Zeroed rather than invented:
        /// a program may print it, and a made-up name would be a false claim
        /// about which driver is in use.
        pub id: [u8; 16],
        /// Length in bytes of the mappable range.
        ///
        /// Not a base address, despite the position modern Linux gives
        /// `smem_start`: a ported terminal reads this member as the length
        /// argument to `mmap(2)`, so a physical address here would make it try
        /// to map a terabyte.
        pub smem_len: usize,
        /// Pixel storage class. `FB_TYPE_PACKED_PIXELS`.
        pub type_: u32,
        /// Auxiliary type. Zero.
        pub type_aux: u32,
        /// Visual type. `FB_VISUAL_TRUECOLOR`.
        pub visual: u32,
        /// Horizontal pan step in pixels. Zero: cannot pan horizontally.
        pub xpanstep: u16,
        /// Vertical pan step in pixels. Zero: cannot pan vertically.
        ///
        /// The true value, not a placeholder. The bootloader's tag describes a
        /// single visible window with no larger virtual screen behind it, so a
        /// non-zero step here would make a terminal believe it could scroll by
        /// shifting a window -- and it would then scroll into whatever the next
        /// page happened to hold.
        pub ypanstep: u16,
        /// Vertical wrap step in lines. Zero: cannot wrap vertically.
        pub ywrapstep: u16,
        /// Bytes per scanline. May exceed `xres * bits_per_pixel / 8`.
        pub line_length: usize,
        /// Physical base of an MMIO region. Zero: this is memory, not registers.
        pub mmio_start: usize,
        /// Length of the MMIO region. Zero: none.
        pub mmio_len: u32,
        /// Acceleration type. Zero: none.
        pub accel: u32,
        /// Capability flags. Zero.
        pub capabilities: u16,
        /// Reserved for future use. Always zero.
        pub reserved: [u16; 2],
    }

    impl FbFixScreeninfo {
        /// Build the fixed-properties block for an active framebuffer.
        pub fn from_geometry(g: &FbGeometry) -> Self {
            FbFixScreeninfo {
                id: [0; 16],
                // The mappable range, not a base address. See the note on the
                // field.
                smem_len: g.size,
                type_: FB_TYPE_PACKED_PIXELS,
                type_aux: 0,
                visual: FB_VISUAL_TRUECOLOR,
                // Zero on all three, and that is the true answer rather than a
                // placeholder. The bootloader's tag describes a single visible
                // window with no larger virtual screen behind it, so the
                // hardware cannot pan. A non-zero step here would make a
                // terminal believe it could scroll by shifting a window, and it
                // would then scroll into whatever the next page happens to hold.
                xpanstep: 0,
                ypanstep: 0,
                ywrapstep: 0,
                line_length: g.pitch,
                mmio_start: 0,
                mmio_len: 0,
                accel: 0,
                capabilities: 0,
                reserved: [0; 2],
            }
        }
    }

    impl FbVarScreeninfo {
        /// Build the mode block for an active framebuffer.
        pub fn from_geometry(g: &FbGeometry) -> Self {
            // The virtual resolution equals the visible one: there is no
            // overscan area to pan into, and claiming a larger virtual screen
            // would advertise scroll range that does not exist.
            FbVarScreeninfo {
                xres: g.width as u32,
                yres: g.height as u32,
                xres_virtual: g.width as u32,
                yres_virtual: g.height as u32,
                xoffset: 0,
                yoffset: 0,
                bits_per_pixel: g.bpp as u32,
                // The colour channel placement is not optional. A terminal
                // derives the shift and mask for each channel from these, and
                // all-zero means "a channel of zero width" -- so leaving them
                // zeroed does not draw black, it draws nothing at all, while
                // looking like a successful query. The multiboot2 framebuffer
                // tag states the positions, so pass them through rather than
                // assuming a layout.
                red: FbBitfield {
                    offset: g.red_pos as u32,
                    length: g.red_size as u32,
                    msb_right: 0,
                },
                green: FbBitfield {
                    offset: g.green_pos as u32,
                    length: g.green_size as u32,
                    msb_right: 0,
                },
                blue: FbBitfield {
                    offset: g.blue_pos as u32,
                    length: g.blue_size as u32,
                    msb_right: 0,
                },
                // Zeroed is correct for every remaining field: no greyscale
                // mode, no non-standard timing, unknown physical dimensions, and
                // no hardware acceleration. A terminal reading a plausible-
                // looking clock or margin would compute a refresh rate for
                // hardware that has none.
                ..Default::default()
            }
        }
    }
}

/// Linux-compatible terminal ioctl request numbers supported by PTY slaves.
/// The syscall itself is Samsara ABI; these request values make existing
/// termios-oriented userspace straightforward to port.
mod terminal_ioctl {
    pub const TCGETS: u32 = 0x5401;
    pub const TCSETS: u32 = 0x5402;
    pub const TCSETSW: u32 = 0x5403;
    pub const TCSETSF: u32 = 0x5404;
    pub const TIOCGPGRP: u32 = 0x540F;
    pub const TIOCSPGRP: u32 = 0x5410;
    pub const TIOCGWINSZ: u32 = 0x5413;
    pub const TIOCSWINSZ: u32 = 0x5414;
    pub const FIONREAD: u32 = 0x541B;
    pub const TIOCGSID: u32 = 0x5429;
    /// Set the controlling terminal of the calling session.
    ///
    /// `TIOCSCTTY`. The argument is an `int *` and not an `int`, which is why it
    /// is the one request that cannot go through the usual argument copy: NULL
    /// means "take this terminal from whoever holds it", and that is the spelling
    /// every shell uses. The pointer is carried past the copy instead, and only
    /// its nullness is ever consulted -- a non-null value is compared against the
    /// caller's own session id before anything reads through it.
    pub const TIOCSCTTY: u32 = 0x540E;
    pub const TCGETA: u32 = 0x5405;
    pub const TCSETA: u32 = 0x5406;
    pub const TCSETAW: u32 = 0x5407;
    pub const TCSETAF: u32 = 0x5408;
    pub const TCSBRK: u32 = 0x5409;
    pub const TCXONC: u32 = 0x540A;
    pub const TCFLSH: u32 = 0x540B;
    pub const TIOCGPTN: u32 = 0x80045430;
}

/// Return the ABI argument size for a terminal ioctl request.
///
/// An unrecognized request returns `None`, which the caller turns into
/// `ENOTTY`. That is what makes `isatty` work the portable way: a program
/// calls `TCGETS` and a non-terminal answers `ENOTTY` because only terminals
/// implement it.
fn ioctl_arg_len(cmd: u32) -> Option<usize> {
    use terminal_ioctl::*;
    use framebuffer_ioctl::*;
    let termios = core::mem::size_of::<crate::drivers::pty::Termios>();
    match cmd {
        TCGETS | TCGETA | TCSETS | TCSETSW | TCSETSF | TCSETA | TCSETAW | TCSETAF => {
            Some(termios)
        }
        TIOCGWINSZ | TIOCSWINSZ => Some(core::mem::size_of::<crate::drivers::pty::Winsize>()),
        // These all take or return a single `int`/`pid_t`.
        TIOCGPGRP | TIOCSPGRP | TIOCGSID | FIONREAD | TCXONC | TCFLSH | TIOCGPTN => Some(4),
        TCSBRK => Some(4),
        // `TIOCSCTTY` is the one request whose argument is a *pointer* rather than
        // a value living in the caller's buffer, and the pointer is null in the
        // common case: `ioctl(fd, TIOCSCTTY, 0)` is the ordinary spelling and means
        // "give me this terminal". Copying four bytes through the buffer to find
        // that out would read address zero inside a legitimate call, so the
        // request is given a zero-length argument and the raw pointer is carried
        // past the copy for the driver to ask about separately.
        //
        // A zero length is what makes `user_bytes(arg, 0)` succeed for *any*
        // `arg`, null included, which is the whole requirement. The driver never
        // dereferences it: it asks the kernel whether the pointer was null, and
        // the kernel dereferences it only for a non-null pointer whose value
        // matches the caller's own session, checked before any read.
        TIOCSCTTY => Some(0),
        FBIOGET_VSCREENINFO => Some(core::mem::size_of::<FbVarScreeninfo>()),
        FBIOGET_FSCREENINFO => Some(core::mem::size_of::<FbFixScreeninfo>()),
        _ => None,
    }
}

/// Dispatch a terminal/device control request through the descriptor's vnode.
fn sys_ioctl(fd: u64, request: u64, arg: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(task) => task,
        Err(e) => return e,
    };
    let cmd = request as u32;
    if request != cmd as u64 {
        return errno::EINVAL;
    }
    let len = match ioctl_arg_len(cmd) {
        Some(len) => len,
        None => return errno::ENOTTY,
    };
    // Stash the raw argument for `TIOCSCTTY`, the one request that takes a
    // pointer. It has to be readable from the driver without being copied,
    // because the pointer is null in the ordinary case and copying would mean
    // reading address zero. Only `current_ioctl_arg` consults this, and only for
    // that request; every other request copies its argument as before.
    IOCTL_ARG.store(arg, core::sync::atomic::Ordering::Relaxed);
    let user = match unsafe { user_bytes(arg, len as u64) } {
        Some(user) => user,
        None => return errno::EINVAL,
    };
    // Drivers operate only on a bounded kernel-side copy; this prevents a
    // driver from retaining or racing a userspace pointer.
    let mut data = alloc::vec::Vec::from(&*user);
    let node = match crate::vfs::fdtab::get(task, fd as usize) {
        Ok(node) => node,
        Err(e) => return e.into(),
    };
    match node.ioctl(cmd, &mut data) {
        Ok(()) => {
            user.copy_from_slice(&data);
            0
        }
        Err(crate::vfs::FsError::NotSupported) => errno::ENOTTY,
        Err(e) => e.into(),
    }
}

/// The raw `ioctl` argument of the call in progress, stashed by [`sys_ioctl`].
///
/// Exists for exactly one request. `TIOCSCTTY` takes an `int *` rather than an
/// `int`, and the ordinary spelling passes NULL, so the argument cannot be copied
/// through the descriptor's user buffer: doing so would read four bytes from
/// address zero. A driver that needs to know whether the pointer was null asks
/// here instead.
///
/// Storing the pointer's *value* and not its contents is the point. A non-null
/// `TIOCSCTTY` argument names a session id the caller claims to be in, and the
/// kernel compares that value against the caller's own session before anything
/// dereferences it -- so a pointer to nonsense is rejected as a bad session id
/// rather than read.
static IOCTL_ARG: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// The raw argument of the `ioctl` currently being dispatched.
pub fn current_ioctl_arg() -> u64 {
    IOCTL_ARG.load(core::sync::atomic::Ordering::Relaxed)
}

fn sys_pipe(pair: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    let mem = match unsafe { user_bytes(pair, 8) } {
        Some(m) => m,
        None => return crate::abi::errno::EINVAL,
    };
    let (read, write) = crate::pipe::create();
    let fd_r = crate::vfs::fdtab::install(task, read);
    let fd_w = crate::vfs::fdtab::install(task, write);
    mem[..4].copy_from_slice(&(fd_r as u32).to_le_bytes());
    mem[4..8].copy_from_slice(&(fd_w as u32).to_le_bytes());
    0
}

/// One `poll` descriptor, shared verbatim with user space.
#[repr(C)]
struct PollFd {
    /// Descriptor to poll; negative entries are skipped.
    fd: i32,
    /// Requested `POLL_*` event bits.
    events: u16,
    /// Result flags; the kernel fills this in before returning.
    revents: u16,
}

/// Maximum number of descriptors accepted in one `POLL` call (bounds the
/// kernel-side snapshot).
const MAX_POLLFDS: usize = 128;

/// Milliseconds per scheduler tick (the 100 Hz timer drive our timeout).
fn ms_to_ticks(ms: u64) -> u64 {
    ms.saturating_add(9) / 10
}

/// `poll(fds, nfds, timeout_ms)`: report which descriptors are ready, waiting
/// (event-driven or timer-paced) for the first readiness among them.
///
/// Blocking policy follows the rest of the kernel: the readiness probe and the
/// poller registration happen under each vnode's lock ([`Vnode::poll_park`]),
/// so a concurrent state change can never be missed; the task parks with
/// [`crate::task::sched::block_until`] and wakes on the first event or the
/// timeout deadline.
fn sys_poll(fds: u64, nfds: u64, timeout_ms: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    // `timeout_ms` is interpreted as signed: -1 = wait indefinitely.
    let tmo = timeout_ms as i64;
    let n = nfds as usize;
    if n > MAX_POLLFDS {
        return crate::abi::errno::EINVAL;
    }

    // No descriptors: never ready; honor the timeout as a plain sleep.
    if n == 0 {
        if tmo < 0 {
            loop {
                crate::task::sched::block_until(None);
                if crate::sig::deliverable_now() {
                    return if crate::sig::should_restart() {
                        crate::sig::defer_pending();
                        0
                    } else {
                        crate::abi::errno::EINTR
                    };
                }
            }
        }
        if tmo > 0 && crate::task::sched::sleep_ticks_interruptible(ms_to_ticks(tmo as u64)) {
            return crate::abi::errno::EINTR;
        }
        return 0;
    }

    let sz = n * core::mem::size_of::<PollFd>();
    let mem = match unsafe { user_bytes(fds, sz as u64) } {
        Some(m) => m,
        None => return crate::abi::errno::EINVAL,
    };

    // Kernel-side snapshot; `revents` starts zeroed so nothing stale is ever
    // reported back to the caller.
    let mut polls: alloc::vec::Vec<PollFd> = alloc::vec::Vec::with_capacity(n);
    let pfsize = core::mem::size_of::<PollFd>();
    for i in 0..n {
        let off = i * pfsize;
        polls.push(PollFd {
            fd: i32::from_ne_bytes([mem[off], mem[off + 1], mem[off + 2], mem[off + 3]]),
            events: u16::from_ne_bytes([mem[off + 4], mem[off + 5]]),
            revents: 0,
        });
    }

    let ret = wait_on_polls(task, &mut polls, tmo);

    // Write the results back before reporting the count, so a caller that sees
    // a non-zero return always finds revents populated.
    for (i, p) in polls.iter().enumerate() {
        let off = i * pfsize + 6;
        mem[off..off + 2].copy_from_slice(&p.revents.to_ne_bytes());
    }
    ret
}

/// Block until at least one of `polls` is ready, `tmo` milliseconds elapse, or
/// a signal arrives. Fills in each `revents` and returns the ready count.
///
/// The shared body of `poll(2)` and `pselect(2)`. The two syscalls differ only
/// in how a caller describes what it wants to watch -- an array of `pollfd`
/// versus three descriptor bitmaps -- and in how results are written back. The
/// part that is easy to get subtly wrong is deciding readiness, parking without
/// losing a wakeup, and honouring the timeout, and none of that should be
/// written twice.
///
/// `tmo` is milliseconds, signed: negative waits indefinitely, zero probes once
/// and returns.
fn wait_on_polls(task: usize, polls: &mut alloc::vec::Vec<PollFd>, tmo: i64) -> i64 {
    let n = polls.len();

    let deadline: Option<u64> = if tmo > 0 {
        Some(crate::time::ticks() + ms_to_ticks(tmo as u64))
    } else {
        None
    };

    loop {
        // A deliverable signal interrupts the wait (`EINTR`), or restarts it
        // transparently when every deliverable handler asked for `SA_RESTART`.
        if crate::sig::deliverable_now() {
            if crate::sig::should_restart() {
                crate::sig::defer_pending();
            } else {
                return crate::abi::errno::EINTR;
            }
        }

        // Snapshot the target vnodes so a descriptor closed mid-poll is stable
        // across the whole iteration.
        let mut nodes: alloc::vec::Vec<Option<(crate::vfs::VnodeRef, u32)>> =
            alloc::vec::Vec::with_capacity(n);
        for p in polls.iter() {
            if p.fd < 0 {
                nodes.push(None);
                continue;
            }
            nodes.push(crate::vfs::fdtab::get_with_access(task, p.fd as usize).ok());
        }

        // Compute readiness; `POLLNVAL` for bad descriptors, and a descriptor
        // opened without read/write access can never satisfy the masked side.
        let mut ready = 0usize;
        for (i, p) in polls.iter_mut().enumerate() {
            let rev = match &nodes[i] {
                None => driver_common::POLLNVAL,
                Some((node, access)) => {
                    let full = node.poll_events(p.events);
                    let mut r = full & p.events;
                    // EOF / error are always reported, requested or not.
                    r |= full & (driver_common::POLLHUP | driver_common::POLLERR);
                    if *access == crate::vfs::O_WRONLY {
                        r &= !driver_common::POLLIN;
                    }
                    if *access == crate::vfs::O_RDONLY {
                        r &= !driver_common::POLLOUT;
                    }
                    r
                }
            };
            p.revents = rev;
            if rev != 0 {
                ready += 1;
            }
        }
        if ready > 0 {
            return ready as i64;
        }
        if tmo == 0 {
            return 0;
        }
        if let Some(dl) = deadline {
            if crate::time::ticks() >= dl {
                return 0;
            }
        }

        // Park on every pollable that is still not ready. Each vnode re-checks
        // and registers under its own lock, so an event between this probe and
        // the registration wakes us instead of being lost.
        let mut became = false;
        for (i, slot) in nodes.iter().enumerate() {
            if polls[i].fd < 0 {
                continue;
            }
            if let Some((node, _)) = slot {
                if node.poll_park(task, polls[i].events) {
                    became = true;
                }
            }
        }
        if became {
            // A node reported ready while parking: drop the registrations we
            // just made and re-probe from the top.
            for slot in &nodes {
                if let Some((node, _)) = slot {
                    node.poll_cancel(task);
                }
            }
            continue;
        }

        // Block until a registered waiter fires or the timeout deadline passes.
        // PTY vnodes have no event-driven wakeup, so a deadline is what makes
        // the loop re-probe them.
        crate::task::sched::clear_timeout();
        crate::task::sched::block_until(deadline);
        crate::task::sched::clear_timeout();

        // Drop our registrations; the loop re-probes readiness from scratch.
        for slot in &nodes {
            if let Some((node, _)) = slot {
                node.poll_cancel(task);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Stat / permission / credential syscalls
// ---------------------------------------------------------------------------

/// Minimal `stat` result written into user memory by `STAT`.
#[repr(C)]
struct AbiStat {
    /// Permission mode bits (see `S_*`).
    mode: u32,
    /// Owner user id.
    uid: u32,
    /// Owner group id.
    gid: u32,
    /// Node kind (`NodeKind` discriminant).
    kind: u32,
    /// Current size in bytes.
    size: u64,
}

/// Full `stat` result written into user memory by `FSTAT`.
///
/// [`AbiStat`] answers "what is at this path" with the five fields the oldest
/// part of the ABI needed. This is the whole POSIX `struct stat` content, kept
/// to what the kernel actually knows:
///
///   * `dev` and `ino` give a file its identity. `ls -i`, a shell's tab
///     completion, and `cp -l` all key off the pair, and a filesystem that
///     reports neither makes every file look like every other file.
///   * `nlink` is 1 because this kernel has no hard links. That is the truth,
///     and a program testing `nlink > 1` to find "other names for this file"
///     correctly finds none.
///   * the timestamps come from the node's own clock, so `ls -l` and `tar` have
///     something real to print. All three are set to the same value: atime
///     tracking is not implemented, and inventing a separate atime that never
///     moves would be worse than reporting mtime for both.
#[repr(C)]
struct AbiStatEx {
    mode: u32,
    uid: u32,
    gid: u32,
    kind: u32,
    size: u64,
    dev: u32,
    /// Padding so `ino` lands on its natural 8-byte boundary, matching the
    /// alignment a C `struct` with these members in this order would produce.
    _pad: u32,
    ino: u64,
    nlink: u64,
    atime: i64,
    atime_nsec: u32,
    mtime: i64,
    mtime_nsec: u32,
    ctime: i64,
    ctime_nsec: u32,
    /// Total bytes allocated, in 512-byte blocks, as `st_blocks`.
    blocks: u64,
    blksize: u32,
    _pad2: u32,
}

impl AbiStatEx {
    /// Build the answer for `node`.
    fn of(node: &dyn crate::vfs::Vnode) -> Self {
        let size = node.size_hint();
        let (secs, nanos) = node.mtime().unwrap_or((0, 0));
        AbiStatEx {
            mode: node.mode(),
            uid: node.uid(),
            gid: node.gid(),
            kind: node.kind() as u32,
            size,
            dev: node.device(),
            _pad: 0,
            ino: node.inode(),
            nlink: 1,
            atime: secs,
            atime_nsec: nanos,
            mtime: secs,
            mtime_nsec: nanos,
            ctime: secs,
            ctime_nsec: nanos,
            blocks: (size + 511) / 512,
            blksize: 4096,
            _pad2: 0,
        }
    }
}

fn sys_device_mmap(fd: u64, offset: u64, len: u64, prot: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    if len == 0 {
        return errno::EINVAL;
    }
    // Linux's PROT_* bits, unmodified, so a program computing them from its own
    // <sys/mman.h> agrees with us.
    const PROT_READ: u64 = 1;
    const PROT_WRITE: u64 = 2;
    const PROT_EXEC: u64 = 4;
    if prot & !(PROT_READ | PROT_WRITE | PROT_EXEC) != 0 {
        return errno::EINVAL;
    }
    let node = match crate::vfs::fdtab::node_of(task, fd as usize) {
        Ok(n) => n,
        Err(e) => return e.into(),
    };
    // Only a node that declares a physical range can be mapped this way. A
    // regular file answers `None`, and inventing a range for one would hand a
    // process a way to map kernel memory.
    let (base_phys, base_len) = match node.mmap_phys() {
        Some(r) => r,
        None => return errno::ENODEV,
    };
    // Reject a request that runs off the end of the device rather than
    // clamping it: a partial map would fault on the last page, which is a much
    // worse failure than a refusal at mmap time.
    let end = match offset.checked_add(len) {
        Some(e) if e <= base_len => e,
        _ => return errno::EINVAL,
    };
    // Map a device range as uncached and write-through. That is not an
    // optimization detail: a framebuffer is memory the device writes to behind
    // our back, so a cached mapping can show stale pixels indefinitely. This is
    // the same flag set the Rust runtime uses for `MAP_PHYS`.
    let mut flags = crate::memory::vmm::USER_ACCESSIBLE | crate::memory::vmm::NO_EXECUTE;
    if prot & PROT_WRITE != 0 {
        flags |= crate::memory::vmm::WRITABLE;
    }
    // `map_phys` takes a frame count; round up so the final partial frame is
    // mapped. The device range is already whole-frame aligned in practice, but
    // rounding here means a caller asking for an odd length still gets a
    // usable mapping rather than a page fault on its last page.
    let frames = (len as usize).div_ceil(crate::memory::pmm::FRAME_SIZE);
    match crate::memory::user_map::map_phys(base_phys + offset, frames, flags) {
        Some(va) => va as i64,
        None => errno::EPERM,
    }
}

fn sys_munmap(va: u64, size: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    match crate::memory::user_map::unmap_anon(va as usize, size as usize) {
        Ok(_) => 0,
        Err(e) => e,
    }
}

fn sys_mprotect(va: u64, size: u64, prot: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    match crate::memory::user_map::protect_anon(va as usize, size as usize, prot as u32) {
        Ok(()) => 0,
        Err(e) => e,
    }
}

fn sys_getrandom(buf: u64, len: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    if len == 0 {
        return 0;
    }
    // Bound the request. A caller asking for an absurd amount is either buggy
    // or hostile, and copying it would let a ring-3 process dictate how long
    // the kernel spends in a loop with interrupts off.
    const MAX_RANDOM: u64 = 1 << 20;
    if len > MAX_RANDOM {
        return errno::EINVAL;
    }
    let mem = match unsafe { user_bytes(buf, len) } {
        Some(m) => m,
        None => return errno::EFAULT,
    };
    // Any entropy instruction can report failure transiently; `fill` retries and
    // then declines rather than leaving part of the buffer untouched. A short
    // fill returned as success would hand back stale stack bytes, which is the
    // same failure as not filling it at all.
    if !crate::entropy::fill(mem) {
        return errno::ENOSYS;
    }
    len as i64
}

fn sys_fstat(fd: u64, out: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    // Log the layout once. The libc side asserts the same numbers, but a
    // disagreement would show up as a `struct stat` full of plausible garbage
    // rather than as a crash, so make the size visible in the log.
    static ONCE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
    if !ONCE.swap(true, core::sync::atomic::Ordering::Relaxed) {
        crate::log::kdebug!(
            "abi: AbiStatEx size={} mtime@{} blocks@{}",
            core::mem::size_of::<AbiStatEx>(),
            core::mem::offset_of!(AbiStatEx, mtime),
            core::mem::offset_of!(AbiStatEx, blocks),
        );
    }
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    // Look the descriptor up rather than resolving a path: this is the whole
    // point of the call, since a descriptor may name a pipe or a terminal,
    // neither of which has a name.
    let node = match crate::vfs::fdtab::node_of(task, fd as usize) {
        Ok(n) => n,
        Err(e) => return e.into(),
    };
    let st = AbiStatEx::of(node.as_ref());
    let mem = match unsafe { user_bytes(out, core::mem::size_of::<AbiStatEx>() as u64) } {
        Some(m) => m,
        None => return errno::EINVAL,
    };
    // SAFETY: `AbiStatEx` is `repr(C)` with no padding holes the compiler
    // chose (every gap is explicit), and `mem` is exactly its size.
    unsafe {
        core::ptr::write_unaligned(
            mem.as_mut_ptr() as *mut AbiStatEx,
            st,
        );
    }
    0
}

fn sys_stat(path: u64, path_len: u64, out: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    stat_impl(path, path_len, out, true)
}

/// `lstat(2)`: `stat` that describes the link rather than what it names.
fn sys_lstat(path: u64, path_len: u64, out: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    stat_impl(path, path_len, out, false)
}

fn stat_impl(path: u64, path_len: u64, out: u64, follow_final: bool) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    let path = match user_str(path, path_len) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let cred = crate::cred::get(task);
    let node = match crate::vfs::resolve_with(
        &task_abs_path(&path),
        follow_final,
        Some(&cred),
    ) {
        Ok(n) => n,
        Err(e) => return e.into(),
    };
    let st = AbiStat {
        mode: node.mode(),
        uid: node.uid(),
        gid: node.gid(),
        kind: node.kind() as u32,
        size: node.size_hint(),
    };
    let sz = core::mem::size_of::<AbiStat>();
    if unsafe { user_bytes(out, sz as u64) }.is_none() {
        return errno::EINVAL;
    }
    // SAFETY: [out, out+sz) was validated by `user_bytes` above.
    unsafe {
        core::ptr::copy_nonoverlapping(&st as *const AbiStat as *const u8, out as *mut u8, sz);
    }
    0
}

/// `symlink(2)`: record `target` as the contents of a new link at `path`.
///
/// The link's mode is `0o777`, and that is not a grant. Following a symlink is a
/// traversal decision made about the target and the directories leading to it, so
/// a link's own permission bits are ignored on every system that has them. The
/// value is visible in `ls -l` and is the one users expect to see; `0o666` would
/// imply write access that is equally meaningless and reads as though the link
/// were an ordinary file.
fn sys_symlink(
    target: u64,
    target_len: u64,
    path: u64,
    path_len: u64,
    _a5: u64,
    _a6: u64,
) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    let target_str = match user_str(target, target_len) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let link_path = match user_str(path, path_len) {
        Ok(s) => s,
        Err(e) => return e,
    };
    // Both arguments are counted, so an embedded NUL is a caller bug rather than
    // a terminator. Trimming it silently would create a link whose target no
    // longer matches what the program believes it wrote, and the program would
    // only find out when it resolved the link.
    if target_str.contains('\0') || link_path.contains('\0') {
        return errno::EINVAL;
    }
    if target_str.is_empty() {
        // A link to nowhere is a mistake, and an invisible one: every later
        // operation on it fails with ENOENT pointing at a path nobody wrote.
        return errno::EINVAL;
    }
    // A path component longer than the kernel can name is ENAMETOOLONG, which is
    // the one error that tells a caller what to change.
    const NAME_MAX: usize = 255;
    if link_path.split('/').any(|c| c.len() > NAME_MAX) {
        return errno::ENAMETOOLONG;
    }
    let cred = crate::cred::get(task);
    let abs = task_abs_path(&link_path);
    match crate::vfs::symlink_as(&abs, &target_str, cred.fsuid, cred.fsgid, 0o777, &cred) {
        Ok(()) => 0,
        Err(e) => e.into(),
    }
}

/// `readlink(2)`: copy a symlink's stored target into the caller's buffer.
///
/// The target is returned verbatim -- relative stays relative, no NUL is
/// appended -- because the caller is entitled to learn what was written rather
/// than a resolved form of it. A program that wants the resolved path has to
/// resolve it, and a program that wants the literal text has a way to get it.
fn sys_readlink(path: u64, path_len: u64, buf: u64, buflen: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    let path_str = match user_str(path, path_len) {
        Ok(s) => s,
        Err(e) => return e,
    };
    if path_str.contains('\0') {
        return errno::EINVAL;
    }
    let cred = crate::cred::get(task);
    let abs = task_abs_path(&path_str);
    // No-follow: readlink describes the link. Following it here would make
    // readlink a second stat, and a caller asking "is this a link, and what does
    // it say" would get an answer to a question it did not ask.
    let node = match crate::vfs::resolve_checked_nofollow(&abs, &cred) {
        Ok(n) => n,
        Err(e) => return e.into(),
    };
    let target = match node.symlink_target() {
        Some(t) => t,
        // Not a link. EINVAL, not ENOENT: the file is right there, the request is
        // what does not apply to it.
        None => return errno::EINVAL,
    };
    // ERANGE rather than a truncated copy. Half a target is a path that does not
    // exist, so a caller that went on to stat it would report a missing file
    // instead of a short buffer.
    if (target.len() as u64) > buflen {
        return errno::ERANGE;
    }
    let dst = match unsafe { user_bytes(buf, target.len() as u64) } {
        Some(b) => b,
        None => return errno::EFAULT,
    };
    // SAFETY: [buf, buf+target.len()) was validated by `user_bytes` above.
    unsafe {
        core::ptr::copy_nonoverlapping(target.as_ptr(), dst.as_mut_ptr(), target.len());
    }
    target.len() as i64
}

fn sys_chmod(path: u64, path_len: u64, mode: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    let path = match user_str(path, path_len) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let cred = crate::cred::get(task);
    let node = match crate::vfs::resolve_checked(&task_abs_path(&path), &cred) {
        Ok(n) => n,
        Err(e) => return e.into(),
    };
    // Only the owner (or a privileged process) may chmod.
    if cred.fsuid != node.uid() && !cred.is_privileged() {
        return errno::EPERM;
    }
    let mut m = (mode as u32) & driver_common::S_PERM_MASK;
    // Unprivileged chmod clears the setuid/setgid bits.
    if !cred.is_privileged() {
        m &= !(driver_common::S_ISUID | driver_common::S_ISGID);
    }
    match node.set_mode(m) {
        Ok(()) => 0,
        Err(e) => e.into(),
    }
}

fn sys_chown(path: u64, path_len: u64, uid: u64, gid: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    let path = match user_str(path, path_len) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let uid = uid as u32;
    let gid = gid as u32;
    let cred = crate::cred::get(task);
    let node = match crate::vfs::resolve_checked(&task_abs_path(&path), &cred) {
        Ok(n) => n,
        Err(e) => return e.into(),
    };
    let uid_changed = uid != u32::MAX;
    let gid_changed = gid != u32::MAX;
    let priv_ = cred.is_privileged();
    let owner = cred.fsuid == node.uid();

    // Changing the owner requires privilege; the owner may change the group
    // only to a group the caller belongs to.
    if uid_changed && !priv_ {
        return errno::EPERM;
    }
    if gid_changed && !priv_ {
        let gid_ok = owner && (cred.fsgid == gid || cred.groups.iter().any(|&g| g == gid));
        if !gid_ok {
            return errno::EPERM;
        }
    }
    let target_uid = if uid_changed { uid } else { u32::MAX };
    let target_gid = if gid_changed { gid } else { u32::MAX };
    if let Err(e) = node.set_owner(target_uid, target_gid) {
        return e.into();
    }
    // Ownership changes clear the setuid/setgid bits for unprivileged callers.
    if !priv_ {
        let _ = node.set_mode(node.mode() & !(driver_common::S_ISUID | driver_common::S_ISGID));
    }
    0
}

fn sys_kill(pid: u64, sig: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    crate::sig::kill(pid, sig as u32)
}

fn sys_getuid(_a1: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    match current_cred() {
        Ok(c) => c.uid as i64,
        Err(e) => e,
    }
}

fn sys_getgid(_a1: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    match current_cred() {
        Ok(c) => c.gid as i64,
        Err(e) => e,
    }
}

fn sys_geteuid(_a1: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    match current_cred() {
        Ok(c) => c.euid as i64,
        Err(e) => e,
    }
}

fn sys_getegid(_a1: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    match current_cred() {
        Ok(c) => c.egid as i64,
        Err(e) => e,
    }
}

fn sys_getresuid(out: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let c = match current_cred() {
        Ok(c) => c,
        Err(e) => return e,
    };
    write_u32s(out, &[c.uid, c.euid, c.suid])
}

fn sys_getresgid(out: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let c = match current_cred() {
        Ok(c) => c,
        Err(e) => return e,
    };
    write_u32s(out, &[c.gid, c.egid, c.sgid])
}

/// Copy a slice of `u32`s into a user buffer as little-endian words.
fn write_u32s(out: u64, words: &[u32]) -> i64 {
    let sz = words.len() * 4;
    let mem = match unsafe { user_bytes(out, sz as u64) } {
        Some(m) => m,
        None => return errno::EINVAL,
    };
    let n = words.len().min(mem.len() / 4);
    for (i, w) in words.iter().take(n).enumerate() {
        mem[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
    }
    0
}

fn sys_getgroups(count: u64, list: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let c = match current_cred() {
        Ok(c) => c,
        Err(e) => return e,
    };
    let n = c.groups.len() as i64;
    if count == 0 || list == 0 {
        return n;
    }
    let take = (count.min(c.groups.len() as u64)) as usize;
    let _ = write_u32s(list, &c.groups[..take]);
    n
}

fn sys_setgroups(count: u64, list: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    const MAX_GROUPS: usize = 64;
    let count = count.min(MAX_GROUPS as u64) as usize;
    let sz = count * 4;
    let mem = match unsafe { user_bytes(list, sz as u64) } {
        Some(m) => m,
        None => return errno::EINVAL,
    };
    let mut groups = alloc::vec::Vec::with_capacity(count);
    for i in 0..count {
        groups.push(u32::from_le_bytes([
            mem[i * 4],
            mem[i * 4 + 1],
            mem[i * 4 + 2],
            mem[i * 4 + 3],
        ]));
    }
    match crate::cred::update(task, |c| c.setgroups(groups)) {
        Ok(()) => 0,
        Err(e) => e,
    }
}

fn sys_setuid(uid: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    match crate::cred::update(task, |c| c.setuid(uid as u32)) {
        Ok(()) => 0,
        Err(e) => e,
    }
}

fn sys_setgid(gid: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    match crate::cred::update(task, |c| c.setgid(gid as u32)) {
        Ok(()) => 0,
        Err(e) => e,
    }
}

fn sys_seteuid(euid: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    match crate::cred::update(task, |c| c.seteuid(euid as u32)) {
        Ok(()) => 0,
        Err(e) => e,
    }
}

fn sys_setegid(egid: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    match crate::cred::update(task, |c| c.setegid(egid as u32)) {
        Ok(()) => 0,
        Err(e) => e,
    }
}

fn sys_setreuid(ruid: u64, euid: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    match crate::cred::update(task, |c| c.setreuid(ruid as u32, euid as u32)) {
        Ok(()) => 0,
        Err(e) => e,
    }
}

fn sys_setregid(rgid: u64, egid: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    match crate::cred::update(task, |c| c.setregid(rgid as u32, egid as u32)) {
        Ok(()) => 0,
        Err(e) => e,
    }
}

fn sys_setresuid(ruid: u64, euid: u64, suid: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    match crate::cred::update(task, |c| c.setresuid(ruid as u32, euid as u32, suid as u32)) {
        Ok(()) => 0,
        Err(e) => e,
    }
}

fn sys_setresgid(rgid: u64, egid: u64, sgid: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    match crate::cred::update(task, |c| c.setresgid(rgid as u32, egid as u32, sgid as u32)) {
        Ok(()) => 0,
        Err(e) => e,
    }
}

fn sys_setfsuid(fsuid: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    let mut old = 0u32;
    let _ = crate::cred::update(task, |c| {
        old = c.setfsuid(fsuid as u32);
        Ok(())
    });
    old as i64
}

fn sys_setfsgid(fsgid: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    let mut old = 0u32;
    let _ = crate::cred::update(task, |c| {
        old = c.setfsgid(fsgid as u32);
        Ok(())
    });
    old as i64
}

fn sys_umask(mask: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    crate::cred::set_umask(task, mask as u32) as i64
}

// ---------------------------------------------------------------------------
// Signal syscalls
// ---------------------------------------------------------------------------

/// # Safety
/// Unaligned-copies a user-provided [`crate::sig::SigAction`] after range
/// validation.
unsafe fn read_sigaction(ptr: u64, out: &mut crate::sig::SigAction) -> bool {
    let sz = core::mem::size_of::<crate::sig::SigAction>() as u64;
    match unsafe { user_bytes(ptr, sz) } {
        Some(buf) => {
            // SAFETY: buffer is validated, `out` is `u64`-aligned kernel stack.
            unsafe {
                *out = (buf.as_ptr() as *const crate::sig::SigAction).read_unaligned();
            }
            true
        }
        None => false,
    }
}

/// # Safety
/// Range-validated write of a [`crate::sig::SigAction`] to user memory.
unsafe fn write_sigaction(ptr: u64, a: &crate::sig::SigAction) -> bool {
    let sz = core::mem::size_of::<crate::sig::SigAction>() as u64;
    match unsafe { user_bytes(ptr, sz) } {
        Some(buf) => {
            // SAFETY: buffer is validated, `a` is a kernel-owned struct.
            unsafe {
                (buf.as_mut_ptr() as *mut crate::sig::SigAction).write_unaligned(*a);
            }
            true
        }
        None => false,
    }
}

/// # Safety
/// Range-validated user read of a 64-bit value.
unsafe fn read_u64(ptr: u64, out: &mut u64) -> bool {
    match unsafe { user_bytes(ptr, 8) } {
        Some(buf) => {
            let bytes = unsafe { core::slice::from_raw_parts(buf.as_ptr(), 8) };
            *out = u64::from_le_bytes(bytes.try_into().unwrap());
            true
        }
        None => false,
    }
}

/// # Safety
/// Range-validated user write of a 64-bit value.
unsafe fn write_u64(ptr: u64, v: u64) -> bool {
    match unsafe { user_bytes(ptr, 8) } {
        Some(buf) => {
            buf[..8].copy_from_slice(&v.to_le_bytes());
            true
        }
        None => false,
    }
}

fn sys_nanosleep(ms: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    // 100 Hz timer tick = 10 ms.
    let ticks = ms.saturating_add(9) / 10;
    if crate::task::sched::sleep_ticks_interruptible(ticks) {
        errno::EINTR // never restarted (POSIX)
    } else {
        0
    }
}

fn sys_get_pid(_a1: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    match crate::task::sched::current_task_id() {
        Some(t) => t.0 as i64,
        None => errno::EPERM,
    }
}

fn sys_sigaction(sig: u64, act: u64, oldact: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    if sig == 0 {
        // Signal 0 is the POSIX existence probe: "may I signal this?". It is
        // not a signal that can be installed, so `EINVAL` is the honest answer.
        return errno::EINVAL;
    }
    if sig > crate::sig::SIG_MAX as u64 {
        // A signal number this kernel does not implement -- glibc's `SIGCANCEL`
        // (32) is the one that actually turns up, because a libc probing for
        // thread-cancellation support installs it.
        //
        // ENOSYS, not EINVAL, and the distinction is load-bearing rather than
        // pedantic: mlibc's pthread setup treats ENOSYS as "cancellation is
        // unavailable, carry on without it" and calls `__ensure` on anything
        // else, so reporting EINVAL turns a routine capability probe into a
        // panic before `main` ever runs. `EINVAL` is for a malformed argument;
        // a well-formed signal this kernel simply has no support for is
        // precisely "not implemented".
        return errno::ENOSYS;
    }
    let mut a = crate::sig::SigAction {
        sa_handler: 0,
        sa_flags: 0,
        sa_restorer: 0,
        sa_mask: 0,
    };
    let have_act = act != 0 && unsafe { read_sigaction(act, &mut a) };
    if act != 0 && !have_act {
        return errno::EINVAL;
    }
    let mut old = crate::sig::SigAction {
        sa_handler: 0,
        sa_flags: 0,
        sa_restorer: 0,
        sa_mask: 0,
    };
    let r = crate::sig::sigaction(
        task,
        sig as u32,
        if have_act { Some(&a) } else { None },
        if oldact != 0 { Some(&mut old) } else { None },
    );
    if r < 0 {
        return r;
    }
    if oldact != 0 && !unsafe { write_sigaction(oldact, &old) } {
        return errno::EINVAL;
    }
    0
}

fn sys_sigprocmask(how: u64, set: u64, oldset: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    let mut new = 0u64;
    let have_set = set != 0 && unsafe { read_u64(set, &mut new) };
    if set != 0 && !have_set {
        return errno::EINVAL;
    }
    let mut old = 0u64;
    let r = crate::sig::sigprocmask(
        task,
        how,
        if have_set { Some(new) } else { None },
        if oldset != 0 { Some(&mut old) } else { None },
    );
    if r < 0 {
        return r;
    }
    if oldset != 0 && !unsafe { write_u64(oldset, old) } {
        return errno::EINVAL;
    }
    0
}

fn sys_sigpending(set: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    if set == 0 {
        return errno::EINVAL;
    }
    if !unsafe { write_u64(set, crate::sig::sigpending(task)) } {
        return errno::EINVAL;
    }
    0
}

fn sys_sigsuspend(mask: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    crate::sig::sigsuspend(mask)
}

fn sys_sigaltstack(ss: u64, old_ss: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    let sz = core::mem::size_of::<crate::sig::AltStack>() as u64;
    let mut a = crate::sig::AltStack {
        ss_base: 0,
        ss_size: 0,
        ss_flags: 0,
    };
    let have_ss = ss != 0
        && match unsafe { user_bytes(ss, sz) } {
            Some(buf) => {
                unsafe {
                    a = (buf.as_ptr() as *const crate::sig::AltStack).read_unaligned();
                }
                true
            }
            None => false,
        };
    if ss != 0 && !have_ss {
        return errno::EINVAL;
    }
    let mut old = crate::sig::AltStack {
        ss_base: 0,
        ss_size: 0,
        ss_flags: 0,
    };
    let r = crate::sig::sigaltstack(
        task,
        if have_ss { Some(&a) } else { None },
        if old_ss != 0 { Some(&mut old) } else { None },
    );
    if r < 0 {
        return r;
    }
    if old_ss != 0 {
        match unsafe { user_bytes(old_ss, sz) } {
            Some(buf) => {
                unsafe {
                    (buf.as_mut_ptr() as *mut crate::sig::AltStack).write_unaligned(old);
                }
            }
            None => return errno::EINVAL,
        }
    }
    0
}

fn sys_sigreturn(_a1: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    crate::sig::sigreturn()
}

// ---------------------------------------------------------------------------
// IPC / process syscalls (the microkernel surface)
// ---------------------------------------------------------------------------

/// Copy a user-provided [`MsgFrame`] into a kernel-owned buffer.
fn read_msg_frame(ptr: u64) -> Result<Box<MsgFrame>, i64> {
    let sz = core::mem::size_of::<MsgFrame>();
    if validate_range(ptr as usize, sz) < sz {
        return Err(errno::EINVAL);
    }
    let m = Box::new(MsgFrame::new());
    // SAFETY: the whole range [ptr, ptr+sz) is validated above, and `m` is a
    // kernel-owned buffer we may write freely.
    unsafe {
        core::ptr::copy_nonoverlapping(ptr as *const u8, (&*m as *const MsgFrame) as *mut u8, sz);
    }
    Ok(m)
}

/// Copy an in-kernel [`MsgFrame`] back into a user buffer.
fn write_msg_frame(ptr: u64, f: &MsgFrame) -> Result<(), i64> {
    let sz = core::mem::size_of::<MsgFrame>();
    if validate_range(ptr as usize, sz) < sz {
        return Err(errno::EINVAL);
    }
    // SAFETY: the whole range is validated above.
    unsafe {
        core::ptr::copy_nonoverlapping(f as *const MsgFrame as *const u8, ptr as *mut u8, sz);
    }
    Ok(())
}

/// Translate an [`MsgFrame`] into an in-kernel [`crate::ipc::Message`].
fn frame_to_message(frame: &MsgFrame, kind: crate::ipc::MsgKind, from: u64, to: u64) -> crate::ipc::Message {
    crate::ipc::Message {
        kind,
        from,
        to,
        call_id: frame.call_id,
        tag: frame.tag,
        args: frame.args,
        data: frame.payload().to_vec(),
    }
}

/// Serialize a kernel [`crate::ipc::Message`] into a user [`MsgFrame`].
fn message_to_frame(m: &crate::ipc::Message) -> MsgFrame {
    let mut f = MsgFrame::new();
    f.tag = m.tag;
    f.from = m.from as u32;
    f.call_id = m.call_id;
    f.args = m.args;
    f.kind = m.kind as u32;
    let n = m.data.len().min(crate::ipc::MAX_MSG_DATA);
    f.data_len = n as u32;
    f.data[..n].copy_from_slice(&m.data[..n]);
    f
}

fn sys_ipc_send(dst: u64, frame: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let ep = match crate::task::sched::current_endpoint() {
        Some(e) => e,
        None => return errno::EPERM,
    };
    let f = match read_msg_frame(frame) {
        Ok(f) => f,
        Err(e) => return e,
    };
    let msg = frame_to_message(&f, crate::ipc::MsgKind::Notify, ep, dst);
    match crate::ipc::send(dst, msg) {
        Ok(()) => 0,
        Err(e) => e.to_abi(),
    }
}

fn sys_ipc_reply(dst: u64, call_id: u64, frame: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let ep = match crate::task::sched::current_endpoint() {
        Some(e) => e,
        None => return errno::EPERM,
    };
    let f = match read_msg_frame(frame) {
        Ok(f) => f,
        Err(e) => return e,
    };
    let mut msg = frame_to_message(&f, crate::ipc::MsgKind::Reply, ep, dst);
    msg.call_id = call_id;
    match crate::ipc::send(dst, msg) {
        Ok(()) => 0,
        Err(e) => e.to_abi(),
    }
}

fn sys_ipc_recv(frame: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let ep = match crate::task::sched::current_endpoint() {
        Some(e) => e,
        None => return errno::EPERM,
    };
    // Block until a message is delivered to this endpoint.
    loop {
        match crate::ipc::recv_wait(ep) {
            crate::ipc::RecvWait::Message(msg) => {
                let f = message_to_frame(&msg);
                return match write_msg_frame(frame, &f) {
                    Ok(()) => 0,
                    Err(e) => e,
                };
            }
            crate::ipc::RecvWait::Interrupted => {
                if crate::sig::should_restart() {
                    crate::sig::defer_pending();
                    continue;
                }
                return errno::EINTR;
            }
            crate::ipc::RecvWait::Gone => return errno::ENOENT,
        }
    }
}

fn sys_ipc_call(dst: u64, frame: u64, reply: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let ep = match crate::task::sched::current_endpoint() {
        Some(e) => e,
        None => return errno::EPERM,
    };
    let f = match read_msg_frame(frame) {
        Ok(f) => f,
        Err(e) => return e,
    };
    let call_id = crate::ipc::gen_call_id();
    {
        let mut msg = frame_to_message(&f, crate::ipc::MsgKind::Call, ep, dst);
        msg.call_id = call_id;
        if let Err(e) = crate::ipc::send(dst, msg) {
            return e.to_abi();
        }
    }
    // Wait for the matching Reply; requeue anything else we wake up for.
    loop {
        match crate::ipc::recv_wait(ep) {
            crate::ipc::RecvWait::Message(msg) => {
                if msg.kind == crate::ipc::MsgKind::Reply && msg.call_id == call_id && msg.from == dst
                {
                    let f = message_to_frame(&msg);
                    return match write_msg_frame(reply, &f) {
                        Ok(()) => 0,
                        Err(e) => e,
                    };
                }
                crate::ipc::requeue(msg);
            }
            crate::ipc::RecvWait::Interrupted => {
                if crate::sig::should_restart() {
                    crate::sig::defer_pending();
                    continue;
                }
                return errno::EINTR;
            }
            crate::ipc::RecvWait::Gone => return errno::ENOENT,
        }
    }
}

fn sys_get_epid(_a1: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    match crate::task::sched::current_endpoint() {
        Some(e) => e as i64,
        None => errno::EPERM,
    }
}

fn sys_proc_exit(code: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    crate::log::kinfo!("proc: process exiting (status {})", code as i32);
    crate::task::sched::exit_current(code as i32)
}

fn sys_fork(_a1: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    match crate::task::sched::fork_current() {
        Ok(pid) => pid.0 as i64,
        Err(e) => e,
    }
}

fn sys_exec(prog: u64, argv: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    // argv is a user pointer to the new image's argument vector; the array and
    // its strings are copied before the address space is abandoned.
    let args = unsafe { read_argv(argv) };
    match crate::task::sched::exec_current(prog as usize, args) {
        // exec_current only returns on failure; success abandons this frame.
        Err(e) => e,
        Ok(()) => errno::ENOENT,
    }
}

/// `execve(2)`: replace this process with the program at `path`.
///
/// The image is read out of the filesystem and loaded like any other, so the
/// program is whatever the path resolves to. That is the whole difference from
/// [`EXEC`], which loads one of the kernel's own embedded images, and it is what
/// lets a shell run a file it found rather than one the kernel happened to carry.
fn sys_execve(path: u64, path_len: u64, argv: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    let path_str = match user_str(path, path_len) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if path_str.contains('\0') {
        return errno::EINVAL;
    }
    let args = unsafe { read_argv(argv) };
    // An empty argv is not "no arguments", it is a program that cannot report its
    // own name. glibc substitutes the path, and so does every program that later
    // looks at argv[0] to decide what it is -- which is the entire basis of applet
    // dispatch. A shell running `/bin/ls` with an empty argv would otherwise get
    // a program that cannot tell it is `ls`.
    let args = if args.is_empty() {
        alloc::vec![task_abs_path(&path_str)]
    } else {
        args
    };

    let cred = crate::cred::get(task);
    let node = match crate::vfs::resolve_checked(&task_abs_path(&path_str), &cred) {
        Ok(n) => n,
        Err(e) => return e.into(),
    };
    // A directory is a path that resolves and cannot be run. EACCES rather than
    // EISDIR because the ELF load would fail anyway and the caller learns more
    // from being told it may not execute this than from being told the file type
    // was wrong -- and because `execve` on a directory is a permission question on
    // every system that answers it at all.
    if node.kind() == crate::vfs::NodeKind::Dir {
        return errno::EACCES;
    }

    // The whole file, then load from memory.
    //
    // Read in full rather than in pieces because `elf::load` wants a slice, and
    // because a program is not going to be exec'd while it is being written: the
    // alternative is a loader that takes a reader, and the size is bounded by the
    // same 4 MiB cap every other ramfs write is bounded by.
    let size = node.size_hint() as usize;
    if size == 0 {
        return errno::ENOEXEC;
    }
    let mut image = alloc::vec![0u8; size];
    let mut filled = 0usize;
    while filled < size {
        match node.read_at(filled as u64, &mut image[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) => return e.into(),
        }
    }
    image.truncate(filled);
    if image.is_empty() {
        return errno::ENOEXEC;
    }
    // One line, after the read rather than before: the size the caller asked to
    // read and the size actually read are different numbers when the file is
    // truncated underneath, and only the second one is what got loaded.
    crate::log::kdebug!("execve: {} -> {} bytes", path_str, image.len());

    match crate::task::sched::exec_path(image, args, &path_str) {
        // exec_path only returns on failure.
        Err(e) => e,
        Ok(()) => errno::ENOENT,
    }
}

fn sys_waitpid(pid: u64, status_ptr: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    loop {
        // A deliverable signal interrupts the wait (or restarts it).
        if crate::sig::deliverable_now() {
            if crate::sig::should_restart() {
                crate::sig::defer_pending();
            } else {
                return errno::EINTR;
            }
        }
        match crate::task::sched::waitpid(pid as usize) {
            crate::task::sched::WaitResult::Reaped(status) => {
                let bytes = match unsafe { user_bytes(status_ptr, 8) } {
                    Some(b) => b,
                    None => return errno::EINVAL,
                };
                bytes[..4].copy_from_slice(&(status).to_le_bytes());
                // The reaped pid goes in the return register, not a bare success
                // code. Returning 0 tells a shell it reaped "pid 0" -- a process
                // that does not exist -- so its next waitpid(0) looks for a child
                // that was never forked and reports ENOENT. That is the whole of
                // "fork works, then becomes unavailable": the first command
                // runs, every later one is told its child is missing, and a
                // program that gives up on the second ENOENT stops issuing
                // commands at all.
                return pid as i64;
            }
            // We were blocked; the child died while we slept. Retry to reap.
            crate::task::sched::WaitResult::Wait => continue,
            crate::task::sched::WaitResult::Err(e) => return e,
        }
    }
}

fn sys_proc_spawn(prog: u64, argv: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let args = unsafe { read_argv(argv) };
    match crate::user::spawn_program_args(prog as usize, args) {
        Ok(pid) => pid.0 as i64,
        Err(_) => errno::ENOENT,
    }
}

fn sys_get_args(buf: u64, cap: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    match copy_args_out(task, buf, cap) {
        Ok(n) => n,
        Err(e) => e,
    }
}

fn sys_get_env(buf: u64, cap: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    // A process spawned before the environment existed (or one that inherited
    // an empty set) still gets a usable answer rather than a zero-length one,
    // so a caller walking the result never has to special-case it.
    let env = crate::task::sched::task_env(task);
    let env = if env.is_empty() {
        crate::user::default_env(&crate::task::sched::task_cwd(task))
    } else {
        env
    };
    match copy_strvec_out(&env, buf, cap) {
        Ok(n) => n,
        Err(e) => e,
    }
}

fn sys_setpgid(pid: u64, pgid: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    // `pid == 0` addresses the caller, matching POSIX.
    let target = if pid == 0 { task } else { pid as usize };
    // `pgid == 0` means "use the target's pid", i.e. make it a group leader.
    let want = if pgid == 0 {
        target as u32
    } else {
        pgid as u32
    };
    if crate::task::sched::setpgid(target, want) {
        0
    } else {
        errno::EPERM
    }
}

fn sys_getpgrp(_a1: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    if current_task().is_err() {
        return errno::EPERM;
    }
    crate::task::sched::current_pgid() as i64
}

fn sys_getpgid(pid: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    let target = if pid == 0 { task } else { pid as usize };
    let pgid = crate::task::sched::task_groups(target).pgid;
    if pgid == 0 {
        errno::ESRCH
    } else {
        pgid as i64
    }
}

fn sys_setsid(_a1: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    match crate::task::sched::setsid() {
        Ok(sid) => sid as i64,
        Err(e) => e,
    }
}

fn sys_getsid(pid: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    let target = if pid == 0 { task } else { pid as usize };
    let sid = crate::task::sched::task_groups(target).sid;
    if sid == 0 {
        errno::ESRCH
    } else {
        sid as i64
    }
}

/// Install `base` as the calling process's FS base.
///
/// A libc keeps its TCB pointer in FS and reloads it on every call through the
/// red zone, so this has to work before the first libc function runs -- not
/// merely eventually. It is the only route available: the MSR is not writable
/// from ring 3, and `wrfsbase` needs `CR4.FSGSBASE`, which this kernel does not
/// enable.
fn sys_set_fs_base(base: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    // IA32_FS_BASE. A zero base is legal (it means "no TCB"), so it is not
    // rejected here; a libc that wants to tear a TCB down passes 0.
    unsafe {
        core::arch::asm!("wrmsr", in("ecx") 0xc000_0100u32, in("eax") base as u32, in("edx") (base >> 32) as u32, options(nostack, preserves_flags));
    }
    0
}

fn sys_chdir(path: u64, path_len: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    let path = match user_str(path, path_len) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let cred = crate::cred::get(task);
    let abs = task_abs_path(&path);
    let node = match crate::vfs::resolve_checked(&abs, &cred) {
        Ok(n) => n,
        Err(e) => return e.into(),
    };
    if node.kind() != crate::vfs::NodeKind::Dir {
        return errno::ENOTDIR;
    }
    // Store the normalized absolute form (collapses `.`/`..` segments).
    crate::task::sched::set_current_cwd(crate::vfs::normalize_abs(&abs));
    0
}

fn sys_getcwd(buf: u64, cap: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let cwd = crate::task::sched::current_cwd();
    if buf == 0 || cap == 0 {
        return cwd.len() as i64;
    }
    let cap = (cap as usize).min(cwd.len());
    let bytes = match unsafe { user_bytes(buf, cap as u64) } {
        Some(b) => b,
        None => return errno::EINVAL,
    };
    bytes.copy_from_slice(&cwd.as_bytes()[..cap]);
    cap as i64
}

fn sys_readdir(path: u64, path_len: u64, buf: u64, cap: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    let path = match user_str(path, path_len) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let cred = crate::cred::get(task);
    let node = match crate::vfs::resolve_checked(&task_abs_path(&path), &cred) {
        Ok(n) => n,
        Err(e) => return e.into(),
    };
    if node.kind() != crate::vfs::NodeKind::Dir {
        return errno::ENOTDIR;
    }
    let mut entries = match node.list() {
        Ok(e) => e,
        Err(e) => return e.into(),
    };
    // POSIX requires `.` and `..` in every directory listing, and plenty of
    // real code depends on them: `ls -a` prints them, `find` starts from `.`,
    // and a shell's tab completion reads `.` to find the cwd. `Vnode::list`
    // reports only real children -- it is also used for mount-point bookkeeping,
    // where a synthetic `.` would be wrong -- so they are added here, at the
    // syscall boundary where the contract actually lives.
    //
    // `..` is reported by name. Whether it *resolves* to the parent is a
    // separate matter handled by the path resolver, which walks no parent links
    // on this flat tree (see vfs::resolve).
    entries.insert(0, (String::from(".."), crate::vfs::NodeKind::Dir));
    entries.insert(0, (String::from("."), crate::vfs::NodeKind::Dir));
    let need = entries.iter().map(|(n, _)| n.len() + 1).sum::<usize>();
    if buf == 0 || cap == 0 {
        return need as i64;
    }
    let cap = cap.min(need as u64) as usize;
    let bytes = match unsafe { user_bytes(buf, cap as u64) } {
        Some(b) => b,
        None => return errno::EINVAL,
    };
    let mut off = 0usize;
    for (name, _) in &entries {
        if off >= cap {
            break;
        }
        let n = (name.len() + 1).min(cap - off);
        bytes[off..off + n.saturating_sub(1)].copy_from_slice(&name.as_bytes()[..n.saturating_sub(1)]);
        if n == name.len() + 1 {
            bytes[off + name.len()] = 0;
        }
        off += n;
    }
    off as i64
}

fn sys_unlink(dirfd: u64, path: u64, path_len: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    let path = match user_str(path, path_len) {
        Ok(p) => p,
        Err(e) => return e,
    };
    // A relative path is interpreted against `dirfd` when one is given, and
    // against the caller's working directory otherwise. Every current caller
    // passes an absolute path with the reserved AT_FDCWD, so the descriptor is
    // accepted and validated but not otherwise consulted.
    const AT_FDCWD: i64 = -100;
    let fd = dirfd as i64;
    if fd != AT_FDCWD && fd < 0 {
        return errno::EBADF;
    }
    let cred = crate::cred::get(task);
    let abs = task_abs_path(&path);
    // A directory may not be unlinked -- POSIX added `rmdir` for that, and this
    // ABI does not have it yet. `unlink` on a directory is EISDIR, not EPERM,
    // so a caller can tell "wrong call" from "not allowed".
    if let Ok(node) = crate::vfs::resolve_checked(&abs, &cred) {
        if node.kind() == crate::vfs::NodeKind::Dir {
            return errno::EISDIR;
        }
    }
    match crate::vfs::remove_checked(&abs, &cred) {
        Ok(_) => 0,
        Err(e) => e.into(),
    }
}

fn sys_lseek(fd: u64, offset: u64, whence: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    // `offset` is signed on the wire; the caller passes the two's complement of
    // a negative value, which arrives here as a large u64.
    let offset = offset as i64;
    match crate::vfs::fdtab::seek(task, fd as usize, offset, whence as u32) {
        Ok(pos) => pos as i64,
        Err(e) => e.into(),
    }
}

fn sys_clock_realtime_ms(_a1: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    // Read the RTC fresh on every call rather than serving a tick counter: the
    // point of this call is that it is the *correct* time, not a cheap
    // approximation. A read is a handful of port writes.
    crate::rtc::epoch_millis()
}

fn sys_set_time(secs: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    // Setting the clock is privileged: an unprivileged process that could do it
    // could move every other process's idea of the present, which defeats
    // anything that checks a timestamp. `settimeofday` is `CAP_SYS_TIME` for
    // exactly this reason.
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    if !crate::cred::get(task).is_privileged() {
        return errno::EPERM;
    }
    crate::rtc::set_epoch(secs as i64);
    0
}

fn sys_mkdir(path: u64, path_len: u64, mode: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    let path = match user_str(path, path_len) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let cred = crate::cred::get(task);
    let abs = task_abs_path(&path);
    // mkdir fails when the target already exists (no recursive creation).
    match crate::vfs::resolve_checked(&abs, &cred) {
        Ok(_) => return errno::EEXIST,
        Err(crate::vfs::FsError::NotFound) => {}
        Err(e) => return e.into(),
    }
    let want = ((mode as u32) & driver_common::S_PERM_MASK) & !crate::cred::umask_of(task);
    match crate::vfs::create_checked(
        &abs,
        crate::vfs::NodeKind::Dir,
        cred.fsuid,
        cred.fsgid,
        want,
        &cred,
    ) {
        Ok(_) => 0,
        Err(e) => e.into(),
    }
}

fn sys_port_allow(lo: u64, hi: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    if lo > hi || hi > 1023 {
        return errno::EINVAL;
    }
    let mut ok = false;
    crate::task::sched::with_current(|t| {
        t.grant_io_ports(lo as u16, hi as u16);
        ok = true;
    });
    if ok {
        crate::task::sched::refresh_io_bitmap();
    }
    0
}

fn sys_irq_bind(irq: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    if irq >= 16 {
        return errno::EINVAL;
    }
    let ep = match crate::task::sched::current_endpoint() {
        Some(e) => e,
        None => return errno::EPERM,
    };
    match crate::interrupts::idt::bind_irq(irq as u8, ep) {
        Ok(()) => 0,
        Err(_) => errno::EPERM,
    }
}

fn sys_irq_unbind(irq: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    crate::interrupts::idt::unbind_irq(irq as u8);
    0
}

fn sys_map_anon(frames: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    match crate::memory::user_map::map_anon(frames as usize) {
        Some(va) => va as i64,
        None => errno::ENOSYS,
    }
}

fn sys_map_phys(phys: u64, frames: u64, flags: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    match crate::memory::user_map::map_phys(phys, frames as usize, flags) {
        Some(va) => va as i64,
        None => crate::abi::errno::EPERM,
    }
}

fn sys_shm_create(frames: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    match crate::memory::user_map::shm_create(frames as usize) {
        Ok(handle) => handle as i64,
        Err(e) => e,
    }
}

fn sys_shm_map(handle: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    match crate::memory::user_map::shm_map(handle) {
        Ok(va) => va as i64,
        Err(e) => e,
    }
}

fn sys_shm_destroy(handle: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    match crate::memory::user_map::shm_destroy(handle) {
        Ok(()) => 0,
        Err(e) => e,
    }
}

/// `(nfds, rfds, wfds, efds, timeout, sigmask) -> ready count`.
///
/// `pselect(6)`, and the syscall behind libc `select(3)` as well: mlibc routes
/// `select` through this one with a null signal mask.
///
/// The three descriptor bitmaps are Linux's `fd_set`: 1024 bits, one per
/// possible descriptor, 128 bytes. The same limit is imposed on the caller
/// rather than silently truncating, because a descriptor above the limit would
/// otherwise be dropped from the watch set and the program would then block
/// forever on something it believed it was watching.
///
/// `timeout` is a `struct timespec`, or null to wait indefinitely.
///
/// A non-null `sigmask` is refused with ENOSYS. Implementing it means swapping
/// the blocked mask atomically with respect to signal delivery, and getting
/// that subtly wrong is worse than not having it: a signal that arrives in the
/// window would be delivered against the wrong mask, or lost. `select(3)` --
/// which passes null -- works, and `pselect(3)` reports that it cannot. That is
/// the "absent rather than plausible" rule applied to a case where a plausible
/// implementation would be a lie.
fn sys_pselect6(
    nfds: u64,
    rfds: u64,
    wfds: u64,
    efds: u64,
    timeout: u64,
    sigmask: u64,
) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    if sigmask != 0 {
        return crate::abi::errno::ENOSYS;
    }

    // One fd_set is 128 bytes of bitmap. Reject an out-of-range descriptor
    // count rather than clamping it: a caller watching descriptor 2000 must be
    // told no, not left blocking on a set that quietly omitted it.
    const FD_SETSIZE: usize = 1024;
    if nfds as usize > FD_SETSIZE {
        return crate::abi::errno::EINVAL;
    }
    let setsz = FD_SETSIZE / 8;

    // The three sets are read-only here; readiness is reported back by clearing
    // the bits of descriptors that are *not* ready, which is what select(2)
    // specifies. Each is copied in first so a null pointer means "not watching"
    // rather than a fault.
    let mut sets: [Option<alloc::vec::Vec<u8>>; 3] = [None, None, None];
    for (i, addr) in [rfds, wfds, efds].iter().enumerate() {
        if *addr == 0 {
            continue;
        }
        match unsafe { user_bytes(*addr, setsz as u64) } {
            Some(m) => sets[i] = Some(m.to_vec()),
            None => return crate::abi::errno::EINVAL,
        }
    }

    // Milliseconds, or -1 for an indefinite wait. A timespec is converted with
    // rounding *up* on the sub-millisecond remainder: a caller asking to wait
    // 500 microseconds and being woken after 0 would spin, which for a select
    // loop is a livelock rather than a rounding error.
    let tmo: i64 = if timeout == 0 {
        -1
    } else {
        let sz = 2 * core::mem::size_of::<u64>();
        let mem = match unsafe { user_bytes(timeout, sz as u64) } {
            Some(m) => m,
            None => return crate::abi::errno::EINVAL,
        };
        let sec = i64::from_ne_bytes([mem[0], mem[1], mem[2], mem[3], mem[4], mem[5], mem[6], mem[7]]);
        let nsec = i64::from_ne_bytes([
            mem[8], mem[9], mem[10], mem[11], mem[12], mem[13], mem[14], mem[15],
        ]);
        if sec < 0 || nsec < 0 {
            return crate::abi::errno::EINVAL;
        }
        let ms = sec.saturating_mul(1000) + (nsec + 999_999) / 1_000_000;
        ms.clamp(0, i64::from(u32::MAX)) as i64
    };

    // Fold the three bitmaps into one pollfd array. A descriptor watched on more
    // than one set gets the union of its interests, which is what select(2)
    // means by appearing in several sets.
    let mut polls: alloc::vec::Vec<PollFd> = alloc::vec::Vec::new();
    for fd in 0..nfds as usize {
        let mut events = 0u16;
        for (i, set) in sets.iter().enumerate() {
            if let Some(bits) = set {
                if bits[fd / 8] & (1 << (fd % 8)) != 0 {
                    events |= match i {
                        0 => driver_common::POLLIN,
                        1 => driver_common::POLLOUT,
                        _ => 0, // exceptional conditions: never requested
                    };
                }
            }
        }
        if events != 0 {
            polls.push(PollFd {
                fd: fd as i32,
                events,
                revents: 0,
            });
        }
    }

    let ret = wait_on_polls(task, &mut polls, tmo);
    if ret < 0 {
        return ret;
    }

    // Report back by clearing every bit of every set, then setting the ones that
    // fired. Clearing first is what makes a second call with the same (now
    // modified) set see only what is still ready, which is the documented
    // behaviour and the reason a caller may loop on select without reloading.
    for (i, set) in sets.iter_mut().enumerate() {
        let Some(bits) = set.as_mut() else { continue };
        let ready_on_this = match i {
            0 => driver_common::POLLIN,
            1 => driver_common::POLLOUT,
            _ => 0,
        };
        for fd in 0..nfds as usize {
            let bit = 1u8 << (fd % 8);
            if bits[fd / 8] & bit == 0 {
                continue;
            }
            let fired = polls
                .iter()
                .any(|p| p.fd as usize == fd && p.revents & ready_on_this != 0);
            if !fired {
                bits[fd / 8] &= !bit;
            }
        }
        if let Some(bits) = set.as_ref() {
            let addr = [rfds, wfds, efds][i];
            // SAFETY: `addr` and `setsz` were validated when the set was read
            // in, and the kernel is writing back to the same range and nothing
            // else. `user_bytes` hands back a mutable view of exactly that many
            // bytes.
            if let Some(dst) = unsafe { user_bytes(addr, setsz as u64) } {
                dst.copy_from_slice(bits.as_slice());
            }
        }
    }

    ret
}

fn sys_dup(oldfd: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    match crate::vfs::fdtab::dup(task, oldfd as usize) {
        Ok(fd) => fd as i64,
        Err(e) => e.into(),
    }
}

/// `fcntl(F_DUPFD)`: duplicate `oldfd` onto the lowest free descriptor at or
/// above `floor`.
fn sys_dupfd(oldfd: u64, floor: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    // A floor this large cannot be a real request and would make the search walk
    // a table sized to fit it. Refused rather than clamped, so a caller passing
    // garbage finds out.
    if floor > 1 << 20 {
        return errno::EINVAL;
    }
    match crate::vfs::fdtab::dup_at_least(task, oldfd as usize, floor as usize) {
        Ok(fd) => fd as i64,
        Err(e) => e.into(),
    }
}

fn sys_dup2(oldfd: u64, newfd: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    match crate::vfs::fdtab::dup2(task, oldfd as usize, newfd as usize) {
        Ok(fd) => fd as i64,
        Err(e) => e.into(),
    }
}

fn sys_ttyname(fd: u64, buf: u64, len: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let task = match current_task() {
        Ok(t) => t,
        Err(e) => return e,
    };
    if len == 0 {
        return errno::EINVAL;
    }
    let node = match crate::vfs::fdtab::node_of(task, fd as usize) {
        Ok(n) => n,
        Err(e) => return e.into(),
    };
    // Not a terminal. Checked before the path lookup so a program gets the
    // specific, actionable errno for "that is not a terminal" rather than the
    // vaguer "no such file" that a failed lookup would produce.
    if !node.is_terminal() {
        return errno::ENOTTY;
    }
    // A terminal that devfs did not publish -- reached through some other
    // mount, say -- has no path this kernel can name. ENOENT says "there is no
    // such name", which is true, and a program that falls back to its
    // descriptor still works.
    let Some(path) = crate::vfs::devfs::path_of(&node) else {
        return errno::ENOENT;
    };
    // +1 for the NUL, which is part of the reported length.
    let needed = path.len() as u64 + 1;
    if needed > len {
        return errno::ERANGE;
    }
    if validate_range(buf as usize, needed as usize) < needed as usize {
        return errno::EFAULT;
    }
    let dst = buf as *mut u8;
    // SAFETY: validate_range above proved `needed` bytes at `buf` are both
    // mapped and writable by this process, and `needed` is exactly the size of
    // the copy.
    unsafe {
        core::ptr::copy_nonoverlapping(path.as_ptr(), dst, path.len());
        *dst.add(path.len()) = 0;
    }
    needed as i64
}

fn sys_fb_info(buf: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    let sz = core::mem::size_of::<FbInfo>();
    if validate_range(buf as usize, sz) < sz {
        return errno::EINVAL;
    }
    let Some(fb) = crate::framebuffer::get() else {
        return errno::ENOSYS;
    };
    let g = fb.geometry();
    let info = FbInfo {
        phys: g.phys as u64,
        size: g.size as u64,
        offset: g.offset as u32,
        width: g.width as u32,
        height: g.height as u32,
        pitch: g.pitch as u32,
        bpp: g.bpp as u32,
        red_pos: g.red_pos as u32,
        red_size: g.red_size as u32,
        green_pos: g.green_pos as u32,
        green_size: g.green_size as u32,
        blue_pos: g.blue_pos as u32,
        blue_size: g.blue_size as u32,
    };
    // SAFETY: `[buf, buf+size)` was validated as mapped user memory above.
    unsafe {
        core::ptr::write_unaligned(buf as *mut FbInfo, info);
    }
    0
}

fn sys_console_detach(_a1: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    crate::console::detach();
    0
}

fn sys_console_attach(_a1: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    crate::console::init();
    0
}

fn sys_dma_alloc(frames: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    match crate::memory::user_map::dma_alloc(frames as usize) {
        Some(va) => va as i64,
        None => errno::ENOSYS,
    }
}

fn sys_dma_free(va: u64, frames: u64, _a3: u64, _a4: u64, _a5: u64, _a6: u64) -> i64 {
    match crate::memory::user_map::dma_free(va as usize, frames as usize) {
        Ok(()) => 0,
        Err(_) => errno::EINVAL,
    }
}

// ---------------------------------------------------------------------------
// MSR setup + assembly entry stub
// ---------------------------------------------------------------------------

const MSR_EFER: u32 = 0xC000_0080;
const EFER_SCE: u64 = 1 << 0;
const MSR_LSTAR: u32 = 0xC000_0082;
const MSR_SFMASK: u32 = 0xC000_0084;
const MSR_STAR: u32 = 0xC000_0081;

/// Bits cleared from RFLAGS on syscall entry (IF, TF, DF, NT).
const SYSCALL_RFLAGS_MASK: u64 = (1 << 9) | (1 << 8) | (1 << 10) | (1 << 14);

unsafe fn wrmsr(msr: u32, value: u64) {
    asm!(
        "wrmsr",
        in("ecx") msr,
        in("eax") value as u32,
        in("edx") (value >> 32) as u32,
        options(nostack)
    );
}

/// Enable the `syscall`/`sysret` instructions and point them at our stub.
///
/// STAR layout used by Samsara:
/// * bits 47:32 — the kernel CS *selector* of `KERNEL_CODE`; the CPU masks it
///   with `0xFFFC` on `syscall` and derives `SS = CS + 8` (`KERNEL_DATA`).
/// * bits 63:48 — the `sysret` base selector; SYSRET derives the ring-3
///   selectors as SS = base+8 and CS = base+16.
unsafe fn enable_syscall_instruction() {
    let kcode_sel = super::interrupts::gdt::KERNEL_CODE as u64;
    // SYSRET loads CS = STAR[63:48] + 16 and SS = STAR[63:48] + 8 (RPL forced
    // to 3). We keep USER_DATA one selector below USER_CODE, so choosing
    // base = USER_DATA - 8 yields SS = USER_DATA and CS = USER_CODE.
    let sysret_base = (super::interrupts::gdt::USER_DATA - 8) as u64 & 0xFFFF;

    let star = (sysret_base << 48) | (kcode_sel << 32);

    // Read-modify-write EFER to set SCE without disturbing long mode bits.
    let efer_lo: u32;
    let efer_hi: u32;
    asm!("rdmsr", in("ecx") MSR_EFER, out("eax") efer_lo, out("edx") efer_hi, options(nostack));
    let efer = ((efer_hi as u64) << 32) | efer_lo as u64;
    wrmsr(MSR_EFER, efer | EFER_SCE);

    wrmsr(MSR_STAR, star);
    wrmsr(
        MSR_LSTAR,
        syscall_entry as usize as u64,
    );
    wrmsr(MSR_SFMASK, SYSCALL_RFLAGS_MASK);

    // The kernel stack the syscall stub runs on is taken from the scheduler's
    // per-thread `CURRENT_KSTACK_TOP`, updated on every context switch so
    // each thread syscalls onto its own kernel stack.
}


core::arch::global_asm!(
    ".section .text",
    ".globl syscall_entry",
    ".type syscall_entry, @function",
    "syscall_entry:",
    // Entry state: rax = nr, rcx = user rip, r11 = user rflags.
    //
    // Trap frame built on the kernel stack (offsets from final rsp):
    //   +000 original rax          +056 original r10
    //   +008 original rcx          +064 r12
    //   +016 original rdx          +072 r13
    //   +024 original rsi          +080 r14
    //   +032 original rdi          +088 r15
    //   +040 original r8           +096 rbx
    //   +048 original r9           +104 rbp
    //   +112 syscall nr            +120 user rip
    //   +128 user cs (placeholder) +136 user rflags
    //   +144 user rsp              +152 ss (placeholder)
    "mov [rip + {scratch}], rsp",
    "mov rsp, [rip + {kstack}]",
    "push qword ptr 0",                      // ss placeholder
    "push qword ptr [rip + {scratch}]",          // user rsp
    "push r11",                              // user rflags
    "push qword ptr 0x18",                       // user cs placeholder
    "push rcx",                              // user rip
    "push rax",                              // syscall nr
    "push rbp",
    "push rbx",
    "push r15",
    "push r14",
    "push r13",
    "push r12",
    "push r10",
    "push r9",
    "push r8",
    "push rdi",
    "push rsi",
    "push rdx",
    "push rcx",
    "push rax",
    // samsara_dispatch(nr, a1..a6): rdi rsi rdx rcx r8 r9 [stack]
    "mov rdi, [rsp + 112]",                  // nr
    "mov rsi, [rsp + 32]",                   // a1 = user rdi
    "mov rdx, [rsp + 24]",                   // a2 = user rsi
    "mov rcx, [rsp + 16]",                   // a3 = user rdx
    "mov r8,  [rsp + 56]",                   // a4 = user r10
    "mov r9,  [rsp + 40]",                   // a5 = user r8
    "push qword ptr [rsp + 48]",                 // a6 = user r9
    "call {dispatch}",
    // Post-`ret` rsp points at the 7th arg (a6); the original-rax slot is at
    // [rsp + 8]. Stash the result there so the `pop rax` below returns it.
    "mov [rsp + 8], rax",					// stash result in original-rax slot
    "add rsp, 8",							// drop 7th argument
    // Signal delivery hook: give `samsara_post_syscall` the trap frame base
    // (the original-rax slot). It may rewrite rip/rsp/rdi to jump into a
    // handler or terminate the process, and returns the rax value to load.
    "mov rdi, rsp",
    "call {post}",
    "mov [rsp], rax",
    "pop rax",
    "pop rcx",
    "pop rdx",
    "pop rsi",
    "pop rdi",
    "pop r8",
    "pop r9",
    "pop r10",
    "pop r12",
    "pop r13",
    "pop r14",
    "pop r15",
    "pop rbx",
    "pop rbp",
    "add rsp, 8",                            // discard syscall nr
    "pop rcx",                               // user rip
    "add rsp, 8",                            // discard user cs placeholder
    "pop r11",                               // user rflags
    "pop rsp",                               // back to the user stack
    "sysretq",
    ".size syscall_entry, . - syscall_entry",
    scratch = sym SCRATCH_SLOT,
    kstack = sym crate::task::sched::CURRENT_KSTACK_TOP,
    dispatch = sym samsara_dispatch,
    post = sym samsara_post_syscall,
);

static mut SCRATCH_SLOT: usize = 0;
