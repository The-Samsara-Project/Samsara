<div align="center">
  <img src="samsara.svg" width="200" height="200" alt="Samsara logo">

# Samsara / Nutcracker

**A 64-bit operating system written from scratch in Rust.**

Copyright (C) 2026 Harsh Nikarsa. Licensed under the GNU GPL v3 or later.

</div>

---

## What it is

Two halves, usually named together as Samsara/Nutcracker.

- **Samsara**, the kernel. Boot path, memory, scheduler, drivers, syscall ABI.
- **Nutcracker**, userspace. Ring-3 processes, a freestanding Rust runtime, and
  an mlibc port so C programs can run on it.

It is not Linux and does not try to be. There is no compatibility layer
pretending otherwise: C programs reach Samsara through a real libc port, because
there is no Linux to point them at. Running C here means writing sysdeps, not
emulating a kernel that isn't there.

It boots to a login prompt. You type `root`, it asks for a password, you type
`root`, and you get a shell that runs ordinary commands.

---

## Quick start

You need Rust nightly with the `x86_64-unknown-none` target, plus `nasm`,
`grub-mkrescue`, `xorriso` and `qemu-system-x86_64`.

```sh
make        # kernel, ISO, and the embedded userland (libc port, fbterm, busybox)
make run    # boot in QEMU, kernel log on serial/stdout
```

Log in as `root` with the password `root`. Try `ls /`, then `whoami`.

The password is fixed because there is nothing to install. The filesystem is
ramfs, gone at reboot and unreachable from outside, so there is no credential
here worth protecting.

---

## The kernel

### Boot

GRUB hands off through Multiboot2. A stub walks 32-bit protected mode, PAE and
LME into long mode, building identity and higher-half page tables on the way.
Everything above the stub is linked in the higher half.

`CR0`/`CR4` get the feature bits ring 3 needs before anything else runs:
OSFXSR and OSXMMEXCPT, because an x86-64 C binary emits SSE by default and traps
on its first vector instruction without them.

FSGSBASE is left off. It is not a CR4 bit, and setting the reserved bit hangs the
kernel under QEMU, so there is a `SET_FS_BASE` syscall instead.

### Memory

| Area | Design |
| --- | --- |
| Frames | Bitmap allocator seeded from the firmware map. The kernel image holds permanent reservations. |
| Paging | 4-level, intermediate tables built on demand. |
| Identity map | Torn down after boot, deliberately. A stray physical access should fault rather than silently work. |
| Heap | First-fit with coalescing, behind `GlobalAlloc`. |

Userspace mappings come from `SHM_CREATE`/`SHM_MAP`, with `mmap`, `munmap` and
`mprotect` on top. The grant table is global, so a process that could name
another process's virtual address would be a denial of service reachable from
ring 3.

### Scheduler

Four-level feedback queue. Slices double per level at 1, 2, 4 and 8 ticks, so
CPU-heavy work sinks and interactive work stays responsive. Wait-time aging
keeps anything from starving.

A context switch carries the two pieces of state that are genuinely
per-process: CR3 and `IA32_FS_BASE`. The second only matters once there is more
than one process, and skipping it leaves a libc's TCB installed for every other
process in turn.

### Terminals

A real line discipline on a PTY, not a byte pump:

- Canonical mode with the editing keys a terminal should have: ERASE, WERASE,
  KILL, LNEXT, REPRINT, DISCARD.
- ISIG, plus VSUSP/VSTART/VSTOP so `^Z` and `^Q` work.
- IXON flow control.
- Process groups, sessions and controlling terminals, so `^C` reaches the
  foreground group and only that group.
- `/dev/ptmx` allocates a fresh pair per open.

`isatty(3)` asks the kernel and gets the truth, including "no" for a pipe. Stdio
picks its buffering from that answer, so a libc that says yes to everything
makes redirected output interleave newlines into its own middle.

### Filesystem

A vnode-based VFS resolves paths across mounts and keeps per-task descriptor
tables and cursors. Three filesystems sit on it:

| Filesystem | Role |
| --- | --- |
| ramfs | Mutable files and directories, at root |
| devfs | `/dev/kbd0`, `/dev/mouse0`, `/dev/fb0`, `/dev/console`, `/dev/ptmx` |
| procfs | `/proc/version`, `/proc/uptime`, `/proc/meminfo`, `/proc/tasks`, `/proc/devices` |

`lseek` returns `ESPIPE` on a stream, which is the answer stdio reads to decide
it cannot reposition. `fstat` reports a real `struct stat`, including a
`(st_dev, st_ino)` pair that actually identifies the file, so tab completion can
tell two files apart.

### Time

Uptime comes from the calibrated TSC with the tick counter as a floor. The wall
clock is the MC146818 RTC, read fresh each time and settable by a privileged
process. Worth getting right: a libc reporting milliseconds since boot as
`CLOCK_REALTIME` makes every timestamp in the system read 1970.

### Input

PS/2 keyboard on the main channel (scancode set 1, make/break, E0 extensions,
modifier tracking with LED sync) and a mouse on the aux port. On top,
`/dev/input/event0` presents both as one evdev-style stream, which is the only
input interface a ported terminal knows how to read.

### Devices

`/dev/fb0` is mappable, not merely readable. A terminal redraws a whole screen
and one `write(2)` per pixel is far too slow to do it. The mapping is uncached
and non-executable.

Device nodes declare a mappable physical range rather than every node answering,
which is what stops `mmap(2)` becoming a way to map any physical address.

### Syscalls

Its own ABI: MSR-programmed `syscall`/`sysret`, a dedicated kernel stack, full
register save, and an append-only number table so numbers stay stable.

Where POSIX or Linux already fixed a number, that number is used, so a program
computing a request from its own headers lands where the kernel expects. The
terminal ioctls follow Linux, as do `PROT_*`. Everything else is Samsara's own
shape.

### Logging

```
[   12.345] INFO: message
```

Per-level colour on serial and VGA, with uptime timestamps.

---

## Userspace

### Runtime

A freestanding Rust runtime over the syscall ABI. No dependencies, no unwinder,
no allocator of its own beyond the arena the kernel hands it.

### Servers

| Server | Job |
| --- | --- |
| `consoled` | Owns the console |
| `inputd` | Turns PS/2 events into IPC for whatever holds the framebuffer |

### Booting into a session

The kernel brings up the console and input drivers, asks busybox to make its own
applet links in `/bin`, starts the terminal, and runs `/bin/getty` on a pty
slave.

`/bin/getty` is the login prompt. It reads a name, hashes the typed password
with `crypt(3)`, compares, drops privileges and execs the account's login shell.
It loops on logout rather than exiting, so logging out returns to a prompt
instead of to nothing. Root's shell is `/bin/sh`, not the getty, because
otherwise the getty execs itself forever.

`/bin/mkpasswd` is a separate C program because the Rust programs use a
freestanding std that does not link the libc port, so it cannot call `crypt(3)`
itself, and password hashing does not belong in the kernel. It reads the password
from one descriptor and writes the hash to another, both passed as numbers, so
the password never reaches the command line or the filesystem.

### Self-tests

The kernel seeds the test programs into `/test` at boot, and they are run by
typing `/test/<name>` at a login prompt. `/test/chello` is the libc smoke test;
the others are `forkx`, `pipetest`, `credtst`, `signaltst`, `termiostst` and
`exectst`.

Running them at a login prompt rather than spawning them is deliberate. Every
one of them runs, prints and exits. None of them waits for somebody to type a
password and then exec a shell. Four bugs lived in exactly that gap, and all
four presented the same way: a program that stops responding, with nothing
crashed and no error returned. The only check that catches them is one that
actually logs in and asks who it ended up as.

---

## The libc port

mlibc is pinned to one upstream commit and built by `ports/mlibc/build.sh`. The
sysdeps are Samsara's own ABI, one wrapper per tag. No mlibc source is
vendored; the sysroot is reproducible on any host with git, meson, ninja and a
freestanding clang.

Two rules worth knowing before touching it:

1. **An int sysdep returns the errno value**, not `-1`, and does not set errno.
   mlibc does that translation itself. Getting it backwards does not fail
   loudly. It makes buffered writes fail silently while unbuffered ones keep
   working, which is miserable to debug from the wrong end.
2. **Say no when the answer is no.** A missing capability returns `ENOSYS` rather
   than a plausible-looking number. A caller told no can fall back; one handed a
   wrong number acts on it.

### crypt(3)

In `ports/mlibc/sysdeps/samsara/crypt.cpp`. Nothing above it can supply it, and
the busybox `passwd` applet calls `crypt(3)` and nothing else.

SHA-512-crypt (`$6$`), the strongest scheme reachable through that entry point
and glibc's current default, so hashes are portable. It was verified against
glibc over randomised inputs rather than against a test vector, because a
transcription error in a hash function passes its own test vectors happily. It
is not memory-hard. DES and MD5-crypt return `NULL` rather than being offered
by accident.

### ioctl(3)

Supplied by the port in `ports/mlibc/sysdeps/samsara/ioctl.cpp`. mlibc defines
it only under its glibc-compatibility option, which this port cannot enable
because that would also drag in `strverscmp` and `versionsort`. A poor trade for
one function, since `ioctl` is POSIX and it is the only way a program drives the
tty layer at all.

The wrapper is thin on purpose: the kernel derives an argument size from the
request number, so a program cannot talk it into reading out of bounds.

### Thread blocks

The loader lays out a thread block for every image with thread-local storage and
the libc constructs it. The split is not a preference. The mlibc `Tcb` is a C++
type in a library the kernel does not link, and the kernel is what replaced the
dynamic loader, so the kernel is what knows where the block is.

Layout is the one the toolchain assumed: thread-locals at the bottom, the `Tcb`
directly above, `FS` pointing at the top. Two things bite if you change it. A
libc finds its `Tcb` by treating the thread pointer as the block address, but
the first field inside that block is a self-pointer, so the real address has to
be handed over separately: `AT_TCB` in the initial stack auxv. And `wrmsr`
takes its value in `EDX:EAX`, not `RAX`; getting that wrong installs the ring-3
code selector as the high half of the thread pointer.

---

## Building

**Requirements:** Rust nightly with `x86_64-unknown-none`, `nasm`,
`grub-mkrescue`, `xorriso`, `qemu-system-x86_64`.

| Command | Result |
| --- | --- |
| `make` | `samsara.elf` and `samsara.iso`, including the libc port, fbterm and busybox |
| `make user-bins` | The ring-3 images alone. Use this when working on userspace. |
| `make run` | Boots in QEMU, kernel log on serial/stdout |
| `make run-fbterm` | Boots with the graphical terminal emulator |
| `make debug` | Boots the debug kernel |
| `make clean` | Wipes build artifacts |

The libc port is part of the normal build: a kernel without the programs that
link against it is not a bootable machine.

Each C program has its own script (`user/build-chhello.sh`,
`user/build-getty.sh`, `user/build-mkpasswd.sh`), and each is a worked example
of the same path: freestanding clang, `-static-pie`, sysroot headers, and lld's
default linker script with only `--image-base` changed. The default script is
deliberate. It already gets `PT_LOAD`, `PT_TLS`, the init array and `.bss`
right. A hand-written one tends to get several of them wrong at once.

The kernel is a zero-dependency `#![no_std]` static library linked by hand with
lld. Any Multiboot2 loader can boot it, and the ISO works from CD or USB.

---

## Roadmap

Roughly in order:

- [ ] SMP bring-up, better APIC timers and interrupt routing
- [ ] Push device drivers into userspace processes
- [ ] Enough POSIX that busybox builds against the port unmodified
- [ ] Improve the NVMe and Ext2 drivers, add journaling, implement FAT32
- [ ] Port more software
- [ ] Lean more on the dynamic linker

No promises on timeline. It gets worked on when there is time.

---

## Contributing

### No AI-generated contributions

Code, commit messages, commentary and documentation generated in whole or in part
by an AI tool will not be accepted.

This is not about code quality, even though AI output can compile and pass tests
and still be wrong in ways that only show up in a kernel: wrong memory ordering,
syscall paths that work but break the ABI contract, drivers that handle the common
case and corrupt state on the edge. Reviewing that costs more time than writing
it properly would, and there is not the reviewer bandwidth to do it safely.

The other reason is the point of the project: understanding this material from
first principles. A patch nobody on the submitting end understands defeats that,
even when it works.

Using an AI tool to look something up, learn a concept, or sanity check your own
understanding while writing a patch yourself is fine. That is research. The line
is the code and the words in the patch. They have to be yours, written and
understood by you.

Submitting a patch confirms it is your own work and not the output of a tool.

### What is asked instead

- **Read the subsystem before touching it.** The source is commented for this.
  The reason a thing is the way it is usually sits next to it, and code that
  looks strange usually is. This matters most somewhere you have not worked
  before.
- **Keep patches small and focused.** One logical change each.
- **Explain why in the commit message.** The diff already shows what changed.
- **Test in QEMU before submitting.** For hardware-facing work, say what you
  tested on.
- **Ask first when unsure.** Open an issue before writing the patch.

Patches cost review time. One that turns out to be shaky or subtly wrong comes
back with an explanation or is rejected. Repeated violations get you blocked.

This policy may get stricter if it needs to.

---

## License

Free software, redistributable and modifiable under the GNU General Public
License version 3 or later. See [LICENSE](LICENSE).

Developed by Harsh Nikarsa and Senna De Jong.