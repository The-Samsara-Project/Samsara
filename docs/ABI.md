<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Harsh Nikarsa -->

# The Samsara System Call ABI

Version 1. Numbers are **append-only**: existing numbers
are never re-purposed or renumbered.

## Invocation

User code executes the `syscall` instruction after loading:

| Element      | Register |
|--------------|----------|
| syscall nr   | `rax`    |
| argument 1–6 | `rdi`, `rsi`, `rdx`, `r10`, `r8`, `r9` |

On entry the kernel masks `IF | TF | DF | NT` from RFLAGS. `rcx` receives the
return RIP and `r11` the caller's RFLAGS — both are **clobbered**. All other
registers except `rax` are preserved across the call.

Return value in `rax`: non-negative = success (semantics per call),
negative = negated errno (see table below).

## Stable numbers

| Nr | Name             | Signature                          | Notes |
|----|------------------|------------------------------------|-------|
| 0  | `DEBUG_WRITE`    | `(buf: ptr<u8>, len) -> written`   | diagnostics channel; writes to the kernel console |
| 1  | `YIELD`          | `() -> 0`                          | blocks until the next interrupt |
| 2  | `CLOCK_UPTIME_MS`| `() -> ms`                         | uptime since kernel start |
| 3  | `KERNEL_VERSION` | `(buf: ptr<u8>, len) -> copied`    | copies the kernel identity string |
| 4  | `EXIT`           | `(code)`                           | halt the machine |
| 5  | `OPEN`          | `(path: ptr<u8>, path_len, flags) -> fd` | resolves a path to a descriptor |
| 6  | `CLOSE`         | `(fd) -> 0`                        | releases a descriptor |
| 7  | `READ`          | `(fd, buf: ptr<u8>, len) -> read`  | reads at the descriptor cursor |
| 8  | `WRITE`         | `(fd, buf: ptr<u8>, len) -> written` | writes at the descriptor cursor |
| 9–12 | `IPC_*`       | —                                  | `SEND`/`RECV`/`REPLY`/`CALL` messaging surface |
| 13 | `PROC_SPAWN`    | `(prog) -> pid`                    | spawn an embedded program image |
| 14 | `PROC_EXIT`     | `(status)`                         | terminate the calling process — distinct from `EXIT`, which halts the machine |
| 15 | `GET_EPID`      | `() -> endpoint`                   | this process's endpoint id |
| 16–18 | `PORT_ALLOW`/`IRQ_BIND`/`IRQ_UNBIND` | — | I/O and IRQ grants |
| 19–22 | `MAP_*`/`DMA_*` | —                               | address-space and DMA grants |
| 23 | `FORK`          | `() -> child pid`                  | duplicate the process; child gets `0` |
| 24 | `EXEC`          | `(prog) -> 0`                      | rebuild the image in place |
| 25 | `WAITPID`       | `(pid, status_ptr) -> 0`           | block until `pid` exits, stash status |
| 26 | `PIPE`          | `(pair: ptr to two u32) -> 0`      | create a pipe; stores read then write fd |
| 58 | `POLL`          | `(fds, nfds, timeout_ms) -> ready` | descriptor readiness multiplexing |
| 59 | `IOCTL`         | `(fd, request, arg) -> 0`          | PTY termios, window-size and foreground-pgrp controls |
| 60 | `FB_INFO`       | `(info: ptr<FbInfo>) -> 0`         | active framebuffer geometry (phys, offset, size, w/h, pitch, bpp, channel layout) |
| 61 | `CONSOLE_DETACH`| `() -> 0`                          | hand the display to user space; kernel console stops painting |
| 62 | `CONSOLE_ATTACH`| `() -> 0`                          | give the display back; rebuilds and clears the kernel console |
| 63 | `GET_ARGS`     | `(buf: ptr<u8>, cap) -> copied`   | the calling process's argv, count word then NUL-terminated strings |
| 64 | `CHDIR`        | `(path, path_len) -> 0`           | change the working directory |
| 65 | `GETCWD`       | `(buf, cap) -> 0`                 | two-call protocol: null `buf` returns the required size, including the NUL |
| 66 | `READDIR`      | `(path, path_len, buf, cap) -> bytes` | list a directory, including the `.` and `..` entries POSIX requires |
| 67 | `MKDIR`        | `(path, path_len, mode) -> 0`     | create one directory; parents are not created |
| 68–70 | —           | —                                  | reserved |
| 71 | `GET_ENV`      | `(buf, cap) -> copied`             | the calling process's environment; same encoding as `GET_ARGS` |
| 72 | `SETPGID`      | `(pid, pgid) -> 0`                 | join or create a process group |
| 73 | `GETPGRP`      | `() -> pgid`                       | the calling process's group |
| 74 | `GETPGID`      | `(pid) -> pgid`                    | another process's group |
| 75 | `SETSID`       | `() -> sid`                        | new session, detaching from the controlling terminal |
| 76 | `GETSID`       | `(pid) -> sid`                     | which session a process is in |
| 77 | `SET_FS_BASE`  | `(tcb: ptr) -> 0`                  | set the ring-3 thread pointer; the only way to reach `IA32_FS_BASE` from user mode |
| 78 | `UNLINK`       | `(dirfd, path, path_len) -> 0`     | remove a directory entry; `EISDIR` on a directory |
| 79 | `LSEEK`        | `(fd, offset: i64, whence) -> pos` | move a descriptor's cursor; `ESPIPE` on a stream |
| 80 | `CLOCK_REALTIME_MS` | `() -> ms`                     | wall clock since the Unix epoch, read from the MC146818 RTC |
| 81 | `SET_TIME`     | `(secs: i64) -> 0`                 | set the wall clock; `EPERM` unless privileged |
| 82 | `FSTAT`        | `(fd, buf: ptr<AbiStatEx>) -> 0`   | full `struct stat` for an open descriptor |
| 83 | `GETRANDOM`    | `(buf, len) -> 0`                  | entropy from RDSEED/RDRAND, or measured jitter; `ENOSYS` with no source |
| 84 | `MUNMAP`       | `(va, size) -> 0`                   | release an anonymous mapping; `EPERM` if it is not yours |
| 85 | `MPROTECT`     | `(va, size, prot) -> 0`             | change a mapping's protection; `prot` is Linux's `PROT_*` |
| 86 | `DEVICE_MMAP`  | `(fd, offset, len, prot) -> va`     | map a device node's physical range; the file-backed half of `mmap(2)` |
| 87 | `TTYNAME`      | `(fd, buf, len) -> len`            | write the `/dev/...` path of a descriptor's node, NUL included; `ttyname(3)`. `ENOTTY` if not a terminal, `ENOENT` if devfs never published it, `ERANGE` if `buf` is too small |
| 88 | `DUP`          | `(oldfd) -> newfd`                 | duplicate a descriptor, **sharing** its file offset; the lowest free descriptor |
| 89 | `DUP2`         | `(oldfd, newfd) -> newfd`          | as `DUP` into a specific slot, closing what was there. `oldfd == newfd` succeeds and changes nothing |
| 90 | `PSELECT6`     | `(nfds, r, w, e, timeout, sigmask) -> n` | `pselect(6)`, and what libc `select(3)` routes through. `fd_set` is Linux's: 1024 bits / 128 bytes. Readiness is reported by clearing the bits of descriptors that are not ready. A non-null `sigmask` returns `ENOSYS` |

## Descriptor inheritance

A process spawned from user space inherits its parent's descriptor table
wholesale, including anything the parent did to it. A process spawned during
kernel boot, having no parent to inherit from, gets descriptors 0-2 pointing at
`/dev/console`.

This is what makes `dup2(pts, 0)` before a spawn meaningful: the child comes up
with the pty on standard input, which is how the installer hands the console
terminal to fbterm. Installing `/dev/console` unconditionally instead would
discard the caller's arrangement, and *not* copying the table leaves the child
with nothing at all -- every read and write failing with `EBADF` and `isatty(0)`
false, so a program that checks its descriptors first gives up silently.
| ≥ `0x8000_0000_0000_0000` | experimental range | — | reserved for out-of-tree experiments, never standardized |

`GET_ENV` (71) copies the calling process's environment in the same encoding as
`GET_ARGS`: a count word (u64 LE) followed by NUL-terminated `NAME=value`
strings. A null `buf` or zero `cap` returns the required size.

`UNLINK` (78) enforces the two POSIX rules that make removal more than a
directory edit: the caller needs **write** permission on the *parent directory*
(not on the file), and in a **sticky** directory the caller must also own the
entry. `dirfd` is accepted and validated, but only `AT_FDCWD` (-100) is
honored — this ABI has no directory-relative resolution.

Numbers `5..4095` are reserved for future stable growth.

## errno values (returned negative)

| Value | Name    | Meaning                    |
|-------|---------|----------------------------|
| -1    | EPERM   | operation not permitted    |
| -2    | ENOENT  | no such process/resource   |
| -3    | ESRCH   | no such process            |
| -4    | EINTR   | interrupted by a signal    |
| -5    | EIO     | input/output error         |
| -8    | ENOEXEC | exec format error          |
| -9    | EBADF   | bad file descriptor        |
| -10   | ECHILD  | no children to wait for    |
| -11   | EAGAIN  | would block; try again     |
| -12   | ENOMEM  | not enough memory          |
| -13   | EACCES  | permission denied          |
| -17   | EEXIST  | already exists             |
| -20   | ENOTDIR | not a directory            |
| -21   | EISDIR  | is a directory             |
| -22   | EINVAL  | invalid argument           |
| -25   | ENOTTY  | not a terminal             |
| -32   | EPIPE   | write with no reader open  |
| -38   | ENOSYS  | function not implemented   |
| -29   | ESPIPE  | illegal seek on a stream  |

## Kernel-side contract

Handlers are plain Rust functions registered via `abi::register()`:

```rust
fn handler(a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, a6: u64) -> i64;
abi::register(abi::nr::DEBUG_WRITE, my_debug_write);
```

The assembly entry stub (`syscall_entry`) saves the complete register frame
on a dedicated kernel stack, dispatches, and restores state before
`sysretq`.

## CPU feature state required of ring 3

A user process may only execute an instruction the CPU will accept at its
current privilege level, so the kernel must enable the state that compiled code
assumes. Two bits are load-bearing, and both are set during GDT setup in
`interrupts/gdt.rs`:

| Control | Bit | Why a ring-3 program needs it |
|---------|-----|------------------------------|
| `CR4.OSFXSR`  | 9  | Permits SSE at CPL 3. Without it every SSE instruction raises `#UD` in user mode. |
| `CR4.OSXMMEXCPT` | 10 | Reports a bad SSE operation as `#XM` rather than a double fault. |
| `CR0.EM` (clear) | 2 | Must be clear for any FPU/SSE use; set means "emulate", which rings #UD. |

This is not a theoretical concern. An x86-64 C program compiled by clang emits
SSE by default — `xorps %xmm0, %xmm0` to zero a register, 16-byte moves in
`memcpy` — so a libc-based binary died on its first vector instruction, before
reaching `main`, with a `#UD` whose faulting `rip` pointed at valid code.

`FSGSBASE` is deliberately **not** enabled. It is not a `CR4` bit: it lives in
the separate `IA32_CR4_FSGSBASE` MSR (0x10A), and `CR4` bit 16 is reserved.
Writing a reserved `CR4` bit hangs this kernel under QEMU, which is how the
mistake was found. `SET_FS_BASE` (77) exists so ring-3 code can install its
thread pointer without `wrfsbase`.

## Process image and initial stack

Ring-3 programs are **static-position-independent ELF64 images**
(`ET_DYN` / `-pie`) built by the `user/` workspace and embedded in the kernel
at build time. The kernel loads them exactly as an in-kernel dynamic linker:

* Every `PT_LOAD` is mapped at its link-time `p_vaddr` (load bias is zero)
  with per-segment permissions (`PF_W` → writable, `PF_X` → executable,
  otherwise NX). `p_memsz - p_filesz` is zero filled for `.bss`.
* The `DT_DYNAMIC` table (via `PT_DYNAMIC`) is walked; `.rela.dyn`/
  `.rela.plt` are resolved eagerly (`BIND_NOW`). `R_X86_64_RELATIVE` stores
  `base + A`; `R_X86_64_64`/`GLOB_DAT`/`JUMP_SLOT` resolve symbols from
  `DT_SYMTAB`/`DT_STRTAB`. Unsupported or unmapped relocation targets abort
  the load with `ENOEXEC`.
* The program entry point is `e_entry`; the user stack is mapped at
  `0x0000_0080_0000_0000` growing downward (1 MiB).

The initial `%rsp` points at a SysV-style frame written by the loader, with
`%rsp` itself 16-byte aligned as the ABI requires:

```
+----------+  ← rsp (16-byte aligned)
|  argc    |   argument count
|  argv[0] |   → "hello", "--selftest", …  (NULL-terminated)
|  …       |
|  NULL    |   argv terminator
|  envp[0] |   → "PATH=/bin:/usr/bin", "HOME=/root", …  (NULL-terminated)
|  …       |
|  NULL    |   envp terminator
|  auxv…   |   tag/value pairs until AT_NULL
+----------+  ← stack top
| string bytes for env, then argv (each NUL-terminated)
+----------+
```

`argv` and `envp` are both real: a libc-based program finds its environment by
walking this frame, exactly as on any other SysV system, and does not need a
syscall to get it. `GET_ENV` exists for callers with no startup code to do that
walk — a runtime that fetches its arguments over a syscall, or a process
inspecting a child it did not spawn.

argv and envp are inherited across `fork`, and preserved across `EXEC` (which
names a program but carries no new environment, so it cannot supply one).

auxv entries delivered: `AT_PHDR`, `AT_PHENT`, `AT_PHNUM`, `AT_PAGESZ`,
`AT_BASE` (0 for static PIE), `AT_ENTRY`.

## pty device layout

Pty slaves live in a `/dev/pts` subdirectory: `/dev/pts/0` is the boot console
pair, and each `open("/dev/ptmx")` allocates a new pair published as
`/dev/pts/<n>`. The masters are `/dev/ptmx0` (the master of pair 0) and
`/dev/ptmx` (the allocating multiplexer).

This is the Linux layout and it is load-bearing rather than cosmetic:
`ptsname(3)` returns a *path*, so a program that has been handed `/dev/pts/3`
must be able to open exactly that. The slaves used to be published flat as
`/dev/pts<n>`, which left every `ptsname` reporting a path that did not exist.
