<div align="center">
  <img src="samsara.svg" width="200" height="200" alt="Samsara logo">

# Samsara / Nutcracker

**A 64-bit micro-kernel operating system, written from scratch in Rust.**

Copyright (C) 2026 Harsh Nikarsa. Licensed under the GNU GPL v3 or later.

</div>

---

## Contents

1. [Overview](#overview)
2. [Why this exists](#why-this-exists)
3. [Quick start](#quick-start)
4. [Kernel: Samsara](#kernel-samsara)
5. [Userspace: Nutcracker](#userspace-nutcracker)
6. [The libc port](#the-libc-port)
7. [Testing](#testing)
8. [Building](#building)
9. [Roadmap](#roadmap)
10. [Contributing](#contributing)
11. [License](#license)

---

## Overview

The project is split into two halves and is referred to as a whole as Samsara/Nutcracker.

| Part | What it is |
| --- | --- |
| **Samsara** | The kernel. A micro-kernel with its own boot path, memory layout and syscall ABI. |
| **Nutcracker** | The userspace. Ring-3 processes, a runtime that fronts the syscall ABI, and the libc port. |

Samsara is **not** Linux-compatible and does not try to be. It follows no existing kernel specification. The only spec that occasionally applies is POSIX. If you are looking for something that runs Linux binaries, this is not it.

C programs reach Samsara the same way they reach any other system: through a POSIX libc. Because the kernel is not Linux, there is nothing to point a C program at, so the compatibility layer is a real libc port, [mlibc](ports/mlibc/), with sysdeps written for Samsara. Running C code here is a matter of writing sysdeps, not of emulating Linux.

## Why this exists

Writing a kernel from first principles teaches things that reading about kernels never will. This is a learning project, and the no external crates rule exists to keep it one. Everything in the kernel is in-tree, including spinlocks, the heap and logging.

## Quick start

You need a Rust nightly toolchain with the `x86_64-unknown-none` target, plus `nasm`, `grub-mkrescue`, `xorriso` and `qemu-system-x86_64`.

```sh
make        # build the kernel, the ISO and all embedded userland
make run    # boot it in QEMU, kernel log on serial/stdout
```

The installer starts on boot. Choose a live boot and log in as `root` with the password `root`. See [Logging in](#logging-in) for why that is acceptable.

---

## Kernel: Samsara

### Boot

GRUB and Multiboot2 bring the machine into x86-64 long mode. Everything above the boot stub is linked in the higher half at `0xFFFFFFFF80000000`. The stub walks through 32-bit protected mode, PAE and LME into long mode, building identity and higher-half page tables along the way.

CR0 and CR4 get the feature bits that ring 3 needs before anything else runs. OSFXSR and OSXMMEXCPT enable SSE, because an x86-64 C binary emits SSE by default and raises `#UD` on its first vector instruction without them.

FSGSBASE stays off. It is not a CR4 bit as often assumed, and setting the reserved bit 16 hangs the kernel under QEMU. A `SET_FS_BASE` syscall exists instead, and it is the only way ring-3 code can install a thread pointer.

Ring 3 also gets a real environment. Argv and envp are staged as a genuine SysV frame, so a libc startup finds `PATH` where it expects it.

### Memory

| Area | Design |
| --- | --- |
| Physical frames | Bitmap allocator seeded from the firmware memory map. The kernel image and loader data hold permanent reservations. |
| Paging | A real 4-level manager. Intermediate tables are built on demand. |
| Identity map | Torn down once boot is done, on purpose. A stray physical access after that point should fault immediately instead of silently working. |
| Kernel heap | First-fit allocator with coalescing, behind `GlobalAlloc`, so `Vec`, `BTreeMap` and friends work normally. |

**Memory for userspace.** Anonymous mappings come from `SHM_CREATE` and `SHM_MAP`, with `mmap`, `munmap` and `mprotect` built on top. Unmapping verifies that the mapping belongs to the calling process. The grant table is global, so a process that could name another process virtual address would be a cross-process denial of service reachable from ring 3.

### CPU setup

- GDT with ring-3 descriptors already in place
- TSS with an IST emergency stack
- Full IDT coverage of the x86-64 exceptions, with page faults reporting CR2
- A generic IRQ dispatch table
- Remapped 8259 PICs
- A 100 Hz PIT driving the scheduler clock

### Scheduler

A multi-level feedback queue with 4 levels. Time slices double at each level, at 1, 2, 4 and 8 ticks. CPU-heavy tasks sink to the bottom while interactive tasks stay near the top and stay responsive. Wait-time aging keeps anything from starving forever.

- Preemption is honored at exact Rust frame boundaries, never in the middle of a stack swap.
- Sleep and wake work from deadlines.
- Blocking wait queues serve device I/O.

A context switch carries the two pieces of register state that are genuinely per-process: the address space (CR3) and the thread pointer (`IA32_FS_BASE`). The second is easy to miss and only shows up with more than one process. A libc that installs a TCB would otherwise leave every other process pointing at it, and the next switch would resume somebody else `FS`.

### Terminals

A real line discipline on a PTY, not a byte pump.

- **Canonical mode** with the editing keys a terminal should have: ERASE, WERASE, KILL, LNEXT, REPRINT and DISCARD.
- **Signals from the keyboard** through ISIG, plus VSUSP, VSTART and VSTOP so that `^Z` and `^Q` work.
- **Flow control** through IXON.
- **Process groups, sessions and controlling terminals**, so `^C` reaches the foreground process group and only that group. TIOCGSID and TIOCGPGRP answer the questions a shell actually asks.
- **`/dev/ptmx`** allocates a fresh pair on every open.

`isatty(3)` answers from the kernel instead of guessing, and it answers correctly for a pipe. Stdio picks its buffering from that answer, so a libc that says yes to everything makes redirected output interleave newlines into the middle of itself.

### Filesystem

A vnode-based VFS core resolves paths across mounts and keeps per-task file descriptor tables and cursors. Three filesystems sit on top of it.

| Filesystem | Role |
| --- | --- |
| **ramfs** | Mutable files and directories, mounted at root. |
| **devfs** | Character devices such as `/dev/kbd0`, `/dev/mouse0`, `/dev/fb0` and `/dev/console`. |
| **procfs** | Generates `/proc/version`, `/proc/uptime`, `/proc/meminfo`, `/proc/tasks` and `/proc/devices` live. |

Descriptors are seekable when the thing behind them has a length. `lseek(2)` returns `ESPIPE` when it does not, which is the answer stdio reads to decide that a stream cannot be repositioned. `fstat(2)` reports a real `struct stat` with the node kind, the size, and a `(st_dev, st_ino)` pair that actually identifies the file, so a shell tab completion can tell two files apart.

### Time

Uptime comes from the calibrated TSC for resolution, with the tick counter as a floor. The wall clock comes from the MC146818 RTC on the CMOS ports, read fresh each time, and can be set by a privileged process.

This matters more than it sounds. A libc that reports milliseconds since boot as `CLOCK_REALTIME` makes every file timestamp, log line and archive header read 1970.

### Input

A proper PS/2 keyboard driver: scancode set 1, make and break decoding, E0-prefixed extended keys, modifier tracking with LED sync, and character events framed on `/dev/kbd0`. A PS/2 mouse driver on the aux channel handles sign extension and overflow rejection, and serves `/dev/mouse0`.

On top of both, `/dev/input/event0` presents the two devices as one evdev-style stream of Linux `input_event` structs, which is the only input interface a ported terminal knows how to read. It is a translation layer in front of the native devices, not a second input path. The PS/2 drivers remain the only code that touches the hardware, and events are converted as they leave the driver queue, so there is no intermediate buffer in which an event can be dropped.

### Devices

`/dev/fb0` is the linear framebuffer, mappable through `DEVICE_MMAP` and not merely readable. That is not an optimisation. A terminal redraws a screen, and one `write(2)` per pixel is orders of magnitude too slow to do it. The mapping is uncached and non-executable, because a framebuffer is memory the device writes to behind the kernel back.

`/dev/console` is what a process that was not started by another process gets for fds 0 to 2.

Device nodes declare a mappable physical range instead of every node answering. This is what stops a process from turning `mmap(2)` into a way to map any physical address it likes.

### Syscalls

Samsara has its own ABI, and again it is not Linux. It uses MSR-programmed `syscall` and `sysret` entry, a dedicated kernel stack, full register save and restore, and an append-only syscall number table so that numbers stay stable.

Where POSIX or Linux already fixed a number, that number is used, so a program computing a request from its own headers lands on the value the kernel expects. The terminal ioctls follow Linux, and so do the `PROT_*` and `GRND_*` flags. Everything else is Samsara own shape.

### Logging

The kernel log looks like Linux:

```
[   12.345] INFO: message
```

It has per-level colors on both serial and VGA, and uptime timestamps.

---

## Userspace: Nutcracker

### Runtime

A freestanding Rust runtime in front of the kernel ABI. It has zero dependencies, no unwinder, and no allocator of its own beyond a bump arena that the kernel hands it. It is what keeps the ring-3 examples readable instead of a pile of inline syscalls.

### Servers

| Server | Job |
| --- | --- |
| `consoled` | Owns the console. |
| `inputd` | Turns PS/2 events into IPC for whatever holds the framebuffer. |
| installer | The interactive program a human actually talks to. It takes the display, runs the boot self-tests and shows the results. |

The installer either finishes an installation or offers a live boot. A live boot installs nothing, writes nothing to disk, and the session is gone at reboot. It hands the display to a login prompt where `root` with the password `root` works. That is what a distribution live image does for the same reason: on a session that is discarded and cannot be reached from outside, a known password is the point and not the risk.

An installation never takes that path. It asks for a password and stores a real hash, and a blank password leaves the account locked instead of open.

### Logging in

`/bin/getty` is the login prompt. It reads a name from `/etc/passwd`, hashes the typed password with `crypt(3)` and compares, then drops to that account privileges and execs its login shell. It loops on logout instead of exec-ing, so logging out returns to a prompt and not to nothing. Root login shell is `/bin/sh`, because pointing it at the getty would make the getty exec itself forever.

`/bin/mkpasswd` turns a password into that hash. It is a separate C program because the installer is Rust, and the Rust programs here use a freestanding std that does not link the libc port. It therefore cannot call `crypt(3)` itself, and password hashing does not belong in the kernel. It reads the password from one descriptor and writes the hash to another, both passed as numbers. That keeps the password off the command line, off the filesystem, and out of the installer entirely.

---

## The libc port

mlibc is pinned to one upstream commit and built by `ports/mlibc/build.sh`. The sysdeps are Samsara own ABI, one wrapper per tag. The pinned tree reference, the patches and the sysdeps are all in-tree, so the sysroot is reproducible on any host with git, meson, ninja and a freestanding clang. No mlibc source is vendored. See the README under `ports/mlibc/` for details.

### Rules to know before touching it

1. **Return errno values from int sysdeps.** A sysdep that returns int returns the errno value, not `-1`, and does not set errno. mlibc does that translation itself on the way out. Getting it backwards does not fail loudly. It makes every buffered write fail silently while unbuffered writes keep working, which is a confusing symptom to debug from the wrong end.
2. **Say no when you cannot say yes.** Where a capability is missing, the sysdep returns `ENOSYS` instead of a plausible-looking answer. A caller that is told no can fall back. One handed a wrong number acts on it.

### crypt(3)

Implemented in `ports/mlibc/sysdeps/samsara/crypt.cpp`. Nothing above it can supply it, since the busybox `passwd` applet calls `crypt(3)` and nothing else, and that call has to land somewhere real.

- **Scheme:** SHA-512-crypt (`$6$`). It is the strongest scheme reachable through that exact entry point. bcrypt, scrypt and Argon2 are different APIs entirely (`crypt_blowfish`, `crypt_gensalt`, `crypt_ra`) and would need a patched busybox or an unreadable hash. It is also glibc current default, so hashes are portable.
- **Verification:** checked against glibc over randomised inputs instead of against a test vector, because a transcription error in a hash function passes its own test vectors happily.
- **Limits:** it is not memory-hard. `rounds=N` is the only mitigation `crypt(3)` offers.
- **Fail closed:** DES and MD5-crypt are deliberately absent. `crypt(3)` returns `NULL` for them, so they fail instead of offering something weaker by accident.

### ioctl(3)

The port supplies `ioctl(3)` and `<sys/ioctl.h>` itself, in `ports/mlibc/sysdeps/samsara/ioctl.cpp`. mlibc defines both only under its glibc-compatibility option, which this port cannot enable because it would also require `strverscmp` and `versionsort` support that does not exist here. That is a poor trade for one function, since `ioctl` is POSIX and not a glibc extension, and is the only way a program drives the tty layer at all.

The wrapper is thin on purpose. The kernel derives an ioctl argument size from the request number, so a program cannot talk it into reading out of bounds by asking for a request that no driver implements.

### Thread blocks

The loader lays out a thread block for every image that has thread-local storage, and the libc constructs it. The split is not a preference. The mlibc `Tcb` is a C++ type in a library the kernel does not link, so the kernel cannot build one. The kernel is also what replaced the mlibc dynamic loader, so the kernel is the one that knows where the block is.

The layout is the one the toolchain assumed: thread-local data at the bottom, the `Tcb` immediately above it, and `FS` pointing at the top. A statically linked image addresses its thread-locals as direct `FS`-relative offsets that the linker already resolved, and nothing is left at run time to check that against.

Two things to know before changing it:

- A libc finds its `Tcb` by reading the thread pointer and treating what it finds there as the block address, but the first field inside that block is a self-pointer. The address therefore has to be handed over separately, and the `AT_TCB` entry in the initial stack auxv is where it goes.
- `wrmsr` takes its value in `EDX:EAX`, not `RAX`. Getting that wrong installs the ring-3 code selector as the high half of the thread pointer.

---

## Testing

### Smoke test

`user/chello.c` is a C program linked against the sysroot, and it is the one the installer runs with the other self-tests. It checks what a libc actually needs in order to work: stdio and buffering, the thread pointer, syscalls, directory listing, timestamps, entropy, mmap and `crypt(3)`. If the libc port is broken, this is what notices.

### The gap in self-tests

Every self-test program runs, prints and exits. None of them waits for a person to type a password and then execs a shell. Four separate bugs lived in exactly that gap:

1. A getty spawned where a password hasher was meant to be.
2. A hasher that read a descriptor the stream was not reading from.
3. A single pipe used for both directions of a conversation.
4. A login shell set to the getty itself.

Every one of them presented as the same symptom: a wizard that stops responding. Nothing crashed and nothing returned an error. A program that would have caught any of them has to actually log in and check who it ended up as.

### End to end scripts

| Script | What it does |
| --- | --- |
| `scripts/run-shelltest.sh` | Drives the keyboard and reads the answers back off the framebuffer. |
| `scripts/logintest.sh` | Does the same for a login: a wrong password, then the right one. |

These are the only checks in the tree that would have caught the four bugs above. The tests that ship with Nutcracker run out of the box.

---

## Building

**Requirements:** Rust nightly with the `x86_64-unknown-none` target, `nasm`, `grub-mkrescue`, `xorriso` and `qemu-system-x86_64`.

| Command | Result |
| --- | --- |
| `make` | Builds `target/samsara.elf` and `samsara.iso`, and pulls in the embedded userland: the libc port, fbterm and busybox. |
| `make user-bins` | Builds the ring-3 images on their own. This is the target to use when working on userspace. |
| `make run` | Boots in QEMU with the kernel log on serial/stdout. |
| `make run-fbterm` | Boots with a graphical terminal emulator. |
| `make debug` | Boots with the debug kernel. |
| `make clean` | Wipes build artifacts. |

The libc port is built as part of the normal build, because a kernel without the programs that link against it is not a bootable machine.

### Building C programs

Each C program has its own script, and each is a worked example of the same path: freestanding clang, `-static-pie`, the sysroot headers, and the lld default linker script with only `--image-base` changed.

| Script | Builds |
| --- | --- |
| `user/build-chello.sh` | The libc smoke test |
| `user/build-getty.sh` | The login prompt |
| `user/build-mkpasswd.sh` | The password hasher |

The default linker script is deliberate. It already gets `PT_LOAD`, `PT_TLS`, the init array and `.bss` right, and a hand-written one gets several of them wrong at once.

### Booting on hardware

The kernel is a zero-dependency `#![no_std]` static library, linked by hand with lld against a custom linker script. Any Multiboot2-capable loader can boot it, and the ISO works from a CD or a USB stick.

---

## Roadmap

Roughly in order:

- [ ] SMP bring-up, improve APIC-based timers and interrupt routing
- [ ] Device drivers pushed out into userspace processes, once the scheduler can isolate tasks properly
- [ ] Enough POSIX that busybox builds against the port unmodified
- [ ] Improving NVMe and Ext2 drivers and adding journaling. and implementing FAT32.
- [ ] Port more software
- [ ] Lean more on the dynamic linker

There are no promises on timeline. This is worked on when there is time for it.

---

## Contributing

Thanks for taking an interest in the project. Please read this before sending anything.

### No AI-generated contributions

Code, commit messages, commentary and documentation generated in whole or in part by an AI tool, such as Copilot, ChatGPT, Claude or anything similar, will not be accepted.

This is not about code quality. AI output can compile, pass tests and still be wrong in ways that only show up in a kernel: subtly incorrect memory ordering, syscall paths that work but violate the ABI contract, drivers that handle the common case and silently corrupt state on the edge case. Reviewing that kind of code costs more time than writing it from scratch would have, and this project does not have the reviewer bandwidth to do it safely.

There is a second reason. The point of Samsara is understanding this material from first principles, and a patch that nobody on the submitting end understands defeats that, even when it happens to work.

Using an AI tool to look something up, learn a concept, or sanity check your own understanding while you write a patch yourself is fine. That is research. The line is the code and the words in the patch: they must be yours, written and understood by you.

By submitting a patch, you confirm that it is your own work and not the output of an AI tool.

### What we ask instead

- **Read the subsystem before touching it.** The source is commented for this. The reason a thing is the way it is usually sits next to it, and the reasons code looks strange are almost always load-bearing. This matters most in a subsystem you have not worked in before.
- **Keep patches small and focused.** One logical change per patch.
- **Explain why in the commit message.** The diff already shows what changed.
- **Test in QEMU before submitting** with `make run`. For a driver or hardware-facing change, say what you tested it on.
- **Ask first when unsure.** If you are not sure whether something fits the project direction, open an issue before writing the patch.

### How we check

This policy is not enforced on trust alone. Patches cost review time. A patch that turns out to be shaky or subtly wrong will be returned for resubmission with an explanation, or rejected outright. Repeated violations get you blocked from the project.

This policy may get stricter over time if it needs to.

---

## License

Samsara is free software. You can redistribute it and modify it under the terms of the GNU General Public License as published by the Free Software Foundation, either version 3 of the License, or at your option any later version. See [LICENSE](LICENSE) for the full text.

Developed by Harsh Nikarsa and Senna De Jong.
