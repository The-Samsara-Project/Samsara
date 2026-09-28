fbterm port for Samsara
=======================

This directory is a self-contained, reproducible port of
[fbterm](https://github.com/sfzhi/fbterm) against the Samsara kernel ABI.
Everything needed to produce the binary lives here; no fbterm source is
vendored.

It uses the same method as `ports/mlibc`, deliberately: pin one upstream
commit, clone it into `build/`, apply patches from `patches/`, build with the
port's own rules, and print a fingerprint. fbterm has been unmaintained for
over a decade, so the tip commit is also the last state anyone tested, which
makes pinning it a statement rather than a compromise.

Layout
------

    build.sh          entry point; see the header comment for the pipeline
                      and optional knobs
    Makefile          the build rules, replacing fbterm's autotools
    patches/          0001-*.patch applied on top of the pinned commit
    include/          the Linux UAPI headers fbterm compiles against, and
                      the port's config.h (the capability decisions)
    support/          libc functions mlibc lacks: forkpty, getopt_long, and
                      the C++ allocation operators

Building
--------

Host requirements: git, clang/clang++ (with `--target=x86_64-pc-samsara`
support) and lld. Run `ports/mlibc/build.sh` first -- the link step needs
`libc.a` and `crt1.o` from the sysroot, and `build.sh` checks for them before
it clones anything, so a missing sysroot fails in a second rather than after a
network round trip.

    ./ports/fbterm/build.sh

Output:

    build/fbterm-build/fbterm.elf

The script is idempotent and reproducible: for the same pinned commit the
output is a function of this directory and the sysroot only, and the final
`sha256sum` lets you verify two builds agree. Verified identical across clean
rebuilds at the time of writing:

    (see the sha256sum printed by build.sh; it changes with any patch)

Set `V=1` to see the compile lines; they are hidden otherwise, because twenty
clang invocations with paths long enough to wrap bury the handful of lines
that carry information.

What this port is not
---------------------

The port depends on nothing but the libc. Upstream fbterm depends on
fontconfig, freetype2, gpm and optionally an input method over a socketpair.
None of those exist on Samsara, and the port removes each one rather than
emulating it. The removals are listed below because each is a real reduction in
capability, and a reader deciding whether fbterm is the right terminal for
Samsara needs to see them rather than discover them.

The kernel work this port depends on is small and was added alongside the port:
`/dev/fb0` with `DEVICE_MMAP` and the `FBIOGET_*` queries, a pty with a real
termios line discipline, `TIOCGPTN`, `TTYNAME`, `DUP`/`DUP2`, `PSELECT6`, and
`fork`/`setsid`. `/dev/input/event0` exists and works but is *not* used -- see
the input note below.

Changes
-------

Each patch is one concern, and the header comment on each says why in prose.

    0001  drop iconv; both ends of every pipe are UTF-8
    0002  inert input-method proxy (needs socketpair)
    0003  inert mouse (needs a Unix socket to gpm)
    0004  built-in bitmap font (replaces fontconfig + freetype2)
    0005  drop the console keymap patch and the raw-key decoder
    0006  null-check getpwuid()
    0007  include the headers for symbols these files actually use
    0008  ask isatty(3) instead of pattern-matching the terminal path

Port-provided code
------------------

`include/` holds the Linux interface definitions fbterm compiles against. The
struct layouts and request numbers are Linux's own, copied field for field,
because the kernel fills some of them: a single wrong field width shifts
everything after it, and the symptom would be a wrong screen rather than a
crash. `linux/fb.h` is the sharpest case -- `fb_fix_screeninfo` and
`fb_var_screeninfo` are exchanged with the kernel as fixed-size blocks.

`include/config.h` replaces the autoconf-generated one. This is the port's
capability ledger, and the four flags in it are left *undefined* rather than
zero because fbterm tests them with `#ifdef`: `#define HAVE_EPOLL 0` would read
as present and select a code path that calls syscalls Samsara does not have.

`support/` holds the three things mlibc does not provide:

  - `forkpty` -- implemented on Samsara's own pty multiplexer. Opening
    `/dev/ptmx` allocates a pair and `TIOCGPTN` says which one, which is
    exactly what `openpty(3)` needs, so this is a real implementation rather
    than a stub.
  - `getopt_long` -- mlibc has `getopt` and all four of its globals but not the
    GNU long-option form. The short-option path is deliberately delegated to the
    libc rather than reimplemented, so there is one owner of the `optind` state
    machine.
  - `new`/`delete` -- Samsara has no libstdc++. Note these keep C++ language
    linkage; `extern "C"` looks harmless and makes every use in the program an
    unresolved symbol.

Known gaps
----------

**No mouse.** Upstream's only mouse path is a Unix domain socket to the gpm
daemon; fbterm has no evdev mouse support of its own to fall back to. The
kernel's PS/2 mouse driver and `/dev/input/event0` both work and both report
motion and buttons -- they are simply not connected to fbterm. Wiring them up
is the right way to spend the effort, since it removes the gpm dependency rather
than stubbing around it.

**No keyboard shortcuts.** Upstream installs Shift+PageUp, Ctrl+Alt+1 and the
rest by editing the *kernel console keymap* through `KDGKBENT`/`KDSKBENT`. That
is a system-wide change needing privilege, and it exists because on Linux the
kernel translates scancodes into keycodes. On Samsara the PS/2 driver already
delivers UTF-8 characters to a pty, so there is no keycode layer for a
shortcut to live in. fbterm's existing `keymapFailure` path reports this to the
user rather than the shortcuts failing silently.

**ASCII only, and no font selection.** The built-in font covers U+0020..U+007E.
`--font-names`, `--font-size` and `--font-width/height/baseline` are accepted
and ignored. A codepoint with no glyph returns NULL, which routes to fbterm's
own placeholder-box path, so unsupported text is visibly wrong rather than
silently blank.

**No controlling terminal for the spawned shell.** `forkpty` issues `TIOCSCTTY`,
which Samsara's pty does not implement, and does not treat the refusal as
fatal. Job control (`tcgetpgrp`, `tcsetpgrp`) is therefore unavailable to the
child, while read, write and `isatty` all work. Making the child die over this
would leave fbterm with no shell at all to fix a convenience.

**`select` rather than `epoll`.** fbterm's fallback waits on at most 32 fixed
descriptors, for which epoll's O(1) readiness is not an advantage. The
limitation is real but not reached.

**No cursor blink, and no shell output yet.** fbterm drives its cursor blink
with `setitimer(ITIMER_REAL)`, and Samsara has no interval timers, so the call
fails and the cursor stays solid. That is the last missing libc function fbterm
touches; everything else it needs is implemented.

fbterm also currently draws a black screen, which is correct rather than broken:
it clears the framebuffer and has nothing to display, because it spawns a shell
with `forkpty` and there is no `/bin/sh` to exec yet. That is the gap busybox
fills, and it is the next piece of work.

The handoff into fbterm is done, and the sequence matters. The installer, on
Finish: puts the console pty on the child's descriptor 0 with `dup2`; detaches
the kernel console with `CONSOLE_DETACH` so nothing paints over fbterm; then
spawns fbterm. Descriptors 1 and 2 stay `/dev/console`, which after the detach
means the serial line only -- fbterm's drawing goes to the framebuffer directly
and its diagnostics belong in the log.

Verified from both ends: the framebuffer goes from the wizard's five colours to
fbterm's clear, and fbterm reports `[input] terminal: /dev/pts0`, which is the
observable proof that the descriptor it inherited is the pty and not
`/dev/console`.

The `ttyname_r` name check that used to be listed here is resolved: patch 0008
replaces it with `isatty(3)`, and syscall 87 (`TTYNAME`) now lets a program ask
the kernel for a descriptor's device path. fbterm starts, reports its terminal,
maps the framebuffer and stays running.

Rebuilding against a newer fbterm
---------------------------------

1. Bump `POINT_OF_TRUTH` in `build.sh` to the new commit SHA.
2. If a patch no longer applies cleanly, regenerate it: check out the new
   commit, re-apply the intent, and `git diff` it into `patches/`.
3. Re-check the capability flags in `include/config.h` -- they describe Samsara,
   not fbterm, so a new upstream version does not change them, but a new
   *feature* may need a new flag.
