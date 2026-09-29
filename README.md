SAMSARA / NUTCRACKER
--------------------

A 64-bit micro-kernel written in Rust. Copyright (C) 2026 Harsh Nikarsa.
Licensed under the GNU GPL v3 or later, see LICENSE.

* Samsara is the kernel: the micro-kernel described throughout this file.
* Nutcracker is the userspace: the ring-3 processes, the runtime that
  fronts the kernel's syscall ABI, and the libc port.

The project together is referred to as Samsara/Nutcracker.

Samsara is NOT Linux-compatible and doesn't try to be. It doesn't follow
any existing kernel spec. The only spec that OCCASIONALLY applies is POSIX
compliance. Boot path, memory layout, syscall ABI - all of it is built from
scratch, on purpose. If you're looking for something that runs Linux
binaries, this isn't it.

The one place that pays off is the libc. Because the kernel is not Linux's,
a C program cannot be ported by pointing it at Samsara - there is nothing to
point it at. So the compatibility layer is a real libc port, mlibc, under
ports/mlibc/. busybox talks to Samsara the way it talks to anything: through
a POSIX libc. That is the whole design, and it is what makes running C code
here a matter of writing sysdeps rather than of emulating Linux.


WHY
---

Because writing a kernel from first principles teaches you things that
reading about kernels never will. This is a learning project, and the
"no external crates" rule below is there to keep it that way.


WHAT'S IN HERE
--------------

Boot:
  GRUB / Multiboot2 gets us into x86-64 long mode. Everything above the
  boot stub is linked in the higher half at 0xFFFFFFFF80000000. The
  stub itself walks through 32-bit protected mode, PAE, LME, then long
  mode, setting up identity and higher-half page tables along the way.

  CR0/CR4 get the feature bits ring 3 needs before anything else runs:
  OSFXSR and OSXMMEXCPT for SSE, because an x86-64 C binary emits SSE by
  default and raises #UD on its first vector instruction without them.
  FSGSBASE stays off - it is an MSR, not a CR4 bit, and setting the
  reserved bit 16 hangs the kernel under QEMU - so SET_FS_BASE exists
  instead, which is the only way ring-3 code can install a thread
  pointer. Ring 3 also gets a real environment: argv and envp are staged
  as a real SysV frame, so a libc's startup finds PATH where it expects.

Memory:
  Physical frames are tracked with a bitmap allocator seeded from the
  firmware memory map. The kernel image and loader data get permanent
  reservations so nothing steps on them. Page tables are a real 4-level
  manager - intermediate tables are built on demand, and once boot is
  done the identity map gets torn down. That's deliberate: any stray
  physical access after that point should fault immediately instead of
  silently working.

  The kernel heap is a first-fit allocator with coalescing, wired up
  behind GlobalAlloc, so Vec, BTreeMap and friends all work normally.

CPU setup:
  GDT with ring-3 descriptors already in place, TSS with an IST
  emergency stack, full IDT coverage for x86-64 exceptions (page faults
  report CR2), a generic IRQ dispatch table, remapped 8259 PICs, and a
  100 Hz PIT driving the scheduler clock.

Scheduler:
  Multi-level feedback queue, 4 levels, slices double at each level
  (1, 2, 4, 8 ticks). CPU-heavy tasks sink to the bottom, interactive
  stuff stays near the top and stays responsive. Wait-time aging keeps
  anything from starving forever. Preemption is honored at exact Rust
  frame boundaries, no half-finished stack swaps. Sleep/wake works off
  deadlines, and there are blocking wait queues for device I/O.

  Context switching carries the two pieces of per-CPU-register state
  that are actually per-process: the address space (CR3) and the thread
  pointer (IA32_FS_BASE). The second one is easy to miss and only shows
  up with more than one process - a libc that installs a TCB would
  otherwise leave every other process pointing at it, and the next
  context switch resumes somebody else's %fs.

Terminals:
  A real line discipline on a PTY, not a byte pump. Canonical mode with
  the editing keys a terminal is supposed to have (ERASE, WERASE, KILL,
  LNEXT, REPRINT, DISCARD), signals from the keyboard (ISIG, plus
  VSUSP/VSTART/VSTOP so ^Z and ^Q work), and IXON flow control. On top
  of that: process groups, sessions, controlling terminals, so ^C reaches
  the foreground process group and only the foreground process group, and
  TIOCGSID/TIOCGPGRP answer the question a shell actually asks. /dev/ptmx
  allocates a fresh pair per open.

  isatty(3) answers from the kernel rather than guessing, and it answers
  correctly for a pipe: stdio picks its buffering from that answer, so a
  libc that says "yes" to everything makes redirected output interleave
  newlines into the middle of itself.

Filesystem:
  A vnode-based VFS core does path resolution across mounts, with
  per-task file descriptor tables and cursors. Three filesystems ride
  on top of it right now: ramfs (mutable files/dirs, mounted at root),
  devfs (character devices like /dev/kbd0 and /dev/mouse0), and procfs,
  which generates /proc/version, /proc/uptime, /proc/meminfo,
  /proc/tasks and /proc/devices live.

  Descriptors are seekable when the thing behind them has a length, and
  lseek(2) says ESPIPE when it does not - which is the answer stdio
  reads to decide a stream cannot be repositioned. fstat(2) reports a
  real struct stat for a descriptor: the node kind, the size, and a
  (st_dev, st_ino) pair that actually identifies the file, so a shell's
  tab completion can tell two files apart.

Time:
  Uptime comes from the calibrated TSC for resolution and the tick
  counter as a floor. The wall clock comes from the MC146818 RTC on the
  CMOS ports, read fresh, and can be set by a privileged process. That
  matters more than it sounds: a libc that reports "milliseconds since
  boot" as CLOCK_REALTIME makes every timestamp in every file, log line
  and archive header read 1970.

Memory for userspace:
  Anonymous mappings via SHM_CREATE/SHM_MAP, and mmap/munmap/mprotect
  on top of them. Unmapping checks that the mapping belongs to the
  calling process, because the grant table is global and a process that
  could name somebody else's virtual address would be a cross-process
  denial of service reachable from ring 3.

Input:
  A proper PS/2 keyboard driver - scancode set 1, make/break decoding,
  E0-prefixed extended keys, modifier tracking with LED sync, character
  events framed on /dev/kbd0. Also a PS/2 mouse driver on the aux
  channel with sign-extension and overflow rejection, on /dev/mouse0.

  On top of that, /dev/input/event0 presents the same two devices as one
  evdev-style stream of Linux input_event structs, which is the only input
  interface a ported terminal knows how to read. It is a translation layer
  sitting in front of the two native devices rather than a second input
  path - the PS/2 drivers are still the only code that touches the
  hardware - and events are converted as they leave the driver's queue, so
  there is no intermediate buffer in which an event can be dropped.

Devices:
  /dev/fb0 is the linear framebuffer, mappable via DEVICE_MMAP rather than
  only readable. That is not an optimisation: a terminal redraws a screen,
  and one write(2) per pixel is orders of magnitude too slow to do it. The
  mapping is uncached and non-executable, because a framebuffer is memory
  the device writes to behind the kernel's back. /dev/console is what a
  process that was not started by another process gets for fds 0-2.

  Device nodes declare a mappable physical range rather than every node
  answering, which is what stops a process turning mmap(2) into "map any
  physical address I like".

Syscalls:
  Samsara has its own ABI - again, not Linux's. MSR-programmed
  syscall/sysret entry, a dedicated kernel stack, full register
  save/restore, and a syscall number table that's append-only so it
  stays stable. OPEN/READ/WRITE/CLOSE are wired to the VFS fd tables.

  Where POSIX or Linux already fixed a number, that number is used, so a
  program computing a request from its own headers lands on the value the
  kernel expects: the terminal ioctls are Linux's, and so are the
  PROT_* and GRND_* flags. Everything else is Samsara's own shape.

Logging:
  Kernel log looks like Linux's: [ 12.345] LEVEL: message, with
  per-level colors on both serial and VGA, and uptime timestamps.

  Everything above is in-tree. No external crates - spinlocks, the
  heap, logging, all of it hand-rolled.

The tests that ship with the Nutcracker userspace (see below) run out of the box


NUTCRACKER
----------

Runtime:
  A freestanding Rust runtime in front of the kernel's ABI - zero
  dependencies, no unwinder, no allocator of its own beyond a bump arena
  the kernel hands it. It is what makes the ring-3 examples readable
  rather than a pile of inline syscalls.

Servers:
  consoled owns the console; inputd turns PS/2 events into IPC for
  whatever holds the framebuffer. The interactive installer is what a
  human actually talks to: it takes the display, runs the boot self-tests
  and shows the results.

The libc port:
  mlibc, pinned to one commit and built from ports/mlibc/build.sh. The
  sysdeps are Samsara's own ABI, one wrapper per tag, and the pinned
  tree plus patches plus sysdeps are all in-tree so the sysroot is
  reproducible on any host with git, meson, ninja and a freestanding
  clang.

  Two things are worth knowing if you touch it. First, a sysdep that
  returns int returns the errno value, not -1, and does not set errno -
  mlibc does that translation itself on the way out. Getting it backwards
  does not fail loudly, it makes every buffered write fail silently while
  unbuffered writes keep working, which is a genuinely confusing symptom
  to debug from the wrong end.

  Second, where a capability is missing the sysdep returns ENOSYS rather
  than a plausible-looking answer. A caller that is told "no" can fall
  back; one handed a wrong number acts on it.

  The port also supplies ioctl(3) and <sys/ioctl.h> itself
  (ports/mlibc/sysdeps/samsara/ioctl.cpp). mlibc defines both only
  under its glibc-compatibility option, which this port cannot enable
  because it would also require strverscmp/versionsort support that
  does not exist here - a poor trade for one function, since ioctl is
  POSIX rather than a glibc extension and is the only way a program
  drives the tty layer at all. The wrapper is thin on purpose: the
  kernel derives an ioctl's argument size from the request number, so
  a program cannot talk it into reading out of bounds by asking for a
  request no driver implements.

Thread blocks:
  The loader lays one out for every image that has thread-local storage,
  and the libc constructs it. The split is not a preference: mlibc's
  Tcb is a C++ type in a library the kernel does not link, so the kernel
  cannot build one, and the kernel is what replaced mlibc's dynamic
  loader, so the kernel is the one that knows where the block is. The
  layout is the one the toolchain assumed - thread-local data at the
  bottom, the Tcb immediately above it, %fs at the top - because a
  statically linked image addresses its thread-locals as direct
  %fs-relative offsets the linker already resolved, and there is nothing
  left at run time to check that against.

  Worth knowing before touching it: a libc finds its Tcb by reading the
  thread pointer and treating what it finds there as the block's
  address, but the first field inside that block is a self-pointer. So
  the address has to be handed over separately, and the initial stack's
  auxv (AT_TCB) is where it goes. Also: wrmsr takes its value in
  EDX:EAX, not RAX, and getting that wrong installs the ring-3 code
  selector as the high half of the thread pointer.

Smoke test:
  user/chello.c is a C program linked against that sysroot, and it is the
  only non-Rust user image. It checks the things a libc actually needs to
  work - stdio and buffering, the thread pointer, syscalls, directory
  listing, timestamps, entropy, mmap - and the installer runs it with the
  other self-tests. If the libc port is broken, this is what notices.

  A passing suite is not the same as a working machine, and the gap is
  worth naming because two real failures lived in it. Every one of those
  tests runs a program that prints to a pipe; none of them types at a
  terminal and waits for a shell to answer. A shell that could not be
  exec'd at all passed the whole suite, and so did a terminal that
  cleared the screen and drew a cursor but no glyphs - both read as "the
  terminal is dead" from the outside and neither is visible to a test that
  never renders anything. scripts/run-shelltest.sh drives the keyboard
  and reads the answers back off the framebuffer, which is the only check
  here that would have caught either.

BUILDING
--------

You'll need a Rust nightly toolchain with the x86_64-unknown-none
target, plus nasm, grub-mkrescue, xorriso, and qemu-system-x86_64.

  make          builds target/samsara.elf and samsara.iso
  make mlibc    builds the mlibc libc port (sysroot under build/sysroot)
  make run      boots it in QEMU, kernel log goes to serial/stdout
  make clean    wipes build artifacts

The libc port is fully self-contained under ports/mlibc/ (see its README):
a pinned, deterministic build of mlibc - patches, sysdeps and cross file are
all committed there, and `make mlibc` reproduces the sysroot on any host with
git, meson, ninja and a freestanding clang toolchain.

It's a zero-dependency #![no_std] static library, linked by hand with
lld against a custom linker script. Any Multiboot2-capable loader can
boot it, and the ISO works fine off a CD or USB stick.

C programs are built with user/build-chhello.sh, which is a worked example
of the whole path: freestanding clang, -static-pie, the sysroot headers, and
lld's default linker script with only --image-base changed. That last part is
deliberate - the default script already gets PT_LOAD, PT_TLS, the init array
and .bss right, and a hand-written one gets several of them wrong at once.


DOCS
----

  docs/ARCHITECTURE.md   subsystem tour, memory layout
  docs/ABI.md            the syscall contract, and what "stable" means


WHERE THIS IS GOING
--------------------

Roughly in order:

  - SMP bring-up, APIC-based timers and interrupt routing
  - pushing device drivers out into userspace processes, once the
    scheduler can actually isolate tasks properly
  - enough of POSIX that busybox builds against the port unmodified
  - a real block filesystem story, and drivers for real hardware

No promises on timeline. This is worked on when there's time for it.


CONTRIBUTING
------------

Thanks for taking an interest in the project. Before sending anything,
read this.

NO AI-GENERATED CONTRIBUTIONS

Code, commit messages, commentary, and documentation generated (in
whole or in part) by an AI tool - Copilot, ChatGPT, Claude, or anything
similar - will not be accepted.

This isn't about code quality. AI output can compile, pass tests, and
still be wrong in ways that only show up in a kernel: subtly incorrect
memory ordering, syscall paths that "work" but violate the ABI
contract, drivers that handle the common case and silently corrupt
state on the edge case. Reviewing that kind of code costs more time
than writing it from scratch would have, and this project doesn't have
the reviewer bandwidth to do that safely.

Beyond the correctness problem: the point of Samsara is understanding
this stuff from first principles. A patch nobody on the submitting end
actually understands defeats that, even when it happens to work.

If you used an AI tool to look something up, learn a concept, or sanity
check your own understanding while writing a patch yourself, that's
fine - that's just research. The line is the code and the words in the
patch: they need to be yours, written and understood by you.

By submitting a patch, you're confirming it's your own work, not the
output of an AI tool.

WHAT WE ASK INSTEAD

  - Read the relevant section of docs/ARCHITECTURE.md before touching
    a subsystem you haven't worked in.
  - Keep patches small and focused. One logical change per patch.
  - Explain *why* in the commit message, not just what changed. The
    diff already shows what changed.
  - Test in QEMU before submitting (make run). If it's a driver or
    hardware-facing change, say what you tested it on.
  - If you're not sure whether something fits the project's direction,
    open an issue and ask before writing the patch.

HOW WE CHECK

We don't run this on trust alone. Patches that read as AI-generated -
by style, by structure, by the kind of mistakes they make - will be
asked to be resubmitted with an explanation, or rejected outright.
Repeated violations get you blocked from the project.

This policy may get stricter over time if it needs to.


LICENSE
-------

Samsara is free software: you can redistribute it and/or modify it
under the terms of the GNU General Public License as published by the
Free Software Foundation, either version 3 of the License, or (at your
option) any later version. See LICENSE for the full text.