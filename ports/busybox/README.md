# busybox, ported to Samsara

busybox 1.37.0, pinned, patched, and built against the Samsara kernel ABI through
the mlibc port in `ports/mlibc`. This is the system's userland: one binary that
provides the shell and the programs around it, reached by name.

    ./ports/busybox/build.sh

## What is here

| | |
|---|---|
| `build.sh` | the build. Pins a commit, applies the patches, configures, compiles, prints a fingerprint. |
| `config` | the kconfig fragment: which applets, and why each group of omissions is omitted. Data, not a build product. |
| `patches/` | one concern per patch, reasoning in the header of each. |
| `include/` | headers mlibc lacks, supplied by the port. Each says why it is here. |
| `support/` | two C files the port links in: the `__dso_handle` anchor, and an always-empty mount table. |
| `toolchain/cc` | the compiler wrapper. |
| `toolchain/merge-config` | merges `config` onto the config kconfig generates. |

The method is the one `ports/mlibc/build.sh` and `ports/fbterm/build.sh` use: pin
one upstream commit, clone into `build/` (gitignored), patch, build with the
port's own rules, print a fingerprint. No busybox source is vendored.

Deterministic within a pinned commit: the output is a function of the pinned
SHA, this directory, and the sysroot. `build.sh` verifies that by setting
`KCONFIG_NOTIMESTAMP`, busybox's own opt-out; without it the version banner
carries the wall clock and the fingerprint changes on every run.

    8ac4b568651297a87df7f3cb4c4d2c79ec3262d090036d93391c8759ee887e4f

## The patches

Five, each one concern, each with its reasoning in the header.

**`0001-samsara-feature-ledger.patch`** — `include/platform.h`

busybox's platform block is a "assume yes" list written for a full libc, and being
wrong about it is silent: busybox does not fail to build when a feature it assumed
is missing, it compiles a call to a function that is not there, and the link — or
worse, the run — is what notices. So the patch states, for each of the features the
block above claims, whether this libc actually has it, derived by checking every
`HAVE_` against the symbols mlibc really exports rather than by discovering them one
build failure at a time.

The distinction matters for the `#define`s as much as the `#undef`s. Claiming a
feature that is absent produces a binary that links and then misbehaves in a way
that looks like a busybox bug; refusing one that is present silently disables the
code that uses it.

**`0002-single-cpu-malloc-affinity.patch`** — `libbb/alloc_affinity.c`

`sched_getaffinity(2)` does not exist here, and the retry loop is worth reading
closely before substituting something: it only continues while `errno` is `EINVAL`,
so a stub returning any other errno would fall out of the loop and into
`bb_perror_msg_and_die`. Reporting the true answer — one CPU, one word — avoids both
the missing syscall and an abort out of a function with no caller in this
configuration.

**`0003-scope-makedev-headers-to-glibc.patch`** — `libbb/makedev.c`

The whole file exists to hand glibc a non-inline `makedev`, and the `<features.h>`
in its non-BSD arm is why it fails to compile on a libc that otherwise has
everything it needs — while the file would be empty anyway, because `libbb.h` guards
both the `bb_makedev` prototype and the `makedev` redirection with the same
`#ifdef __GLIBC__`. Pairing the guards keeps the two files agreeing.

**`0004-do-not-probe-the-host-for-libraries.patch`** — `Makefile.flags`

busybox link-tests a stub to decide whether `libcrypt` and `librt` exist. In a cross
build that is a question about the wrong machine: nothing in the probe carries the
target's link flags, so the compiler falls back on the build host's defaults. Here
that fallback is not theoretical — the probe links `-nostdinc`'d sources with no
`-nostdlib` and no `-static`, so clang uses the host's crt files and searches the
host's library paths, and it *succeeds* against the host's `libcrypt.so.2` and
`libc.so.6`. Both probes report "available" for a system that has neither.

A probe that cannot be trusted to test the target should not be run. The patch
replaces the two probes with an explicit list: `libm` is named, and the libraries
this port's libc actually ships are named. A library that were absent would then
fail loudly at the link instead of quietly at the run.

**`0005-trylink-accepts-plain-objects.patch`** — `scripts/trylink`

`trylink` builds its link line by prefixing `-l` onto every word of
`CONFIG_EXTRA_LDLIBS`, which assumes they are all library names. Two of this port's
link inputs are not, and cannot be named by a `-l` flag at all: `crt1.o` holds
`_start`, the ELF entry point, which nothing references and so which no linker would
pull out of an archive; and `dso.o` defines `__dso_handle`, which nothing defines.

The patch routes a word that looks like a path through verbatim, and puts it *inside*
the group that holds the archives. That placement is not cosmetic: an object after
a closed group cannot cause an archive member to be pulled in, because a linker
extracts an archive entry only to satisfy a reference that already exists. `crt1.o`
references `main`, and `main` lives in an archive member, so `crt1.o` outside the
group leaves the program with an entry point whose only symbol is undefined —
reported as "undefined reference to `main`", which reads like a missing program
function rather than a misplaced startup file.

`CONFIG_EXTRA_LDFLAGS` is the other place a plain object could go, and it is the
wrong one: `scripts/Makefile.build` feeds `LDFLAGS` to the intermediate `ld -r`
links that assemble each directory's `built-in.o`, so a `crt1.o` there gets its
`_start` copied into every one of them, and they then collide with the real one.

## The headers

mlibc does not provide these, and each is here because the alternative was a build
failure or, worse, a build that succeeds with the wrong meaning.

- **`paths.h`** — `_PATH_DEFPATH` and friends. `_PATH_DEFPATH` has to be *correct*,
  not merely present: busybox falls back to it when `$PATH` is unset, so a wrong
  value produces an applet that appears to work and then cannot find the applets it
  shells out to.
- **`sys/sysmacros.h`** — `major`/`minor`, with glibc's 20/12-bit packing rather than
  a simplified one. A program that decomposes a device number with one packing while
  the kernel produced another gets a plausible-looking wrong answer — a major of 4
  becomes 256 — rather than a failure.
- **`malloc.h`** — deliberately declares no tunables. Programs `#ifdef` the `M_*`
  constants before calling `mallopt`, so a header that exists and names none is what
  makes that pattern resolve correctly. Declaring `mallopt` itself would invite a
  caller that skips its own feature test to link against a function the allocator
  does not have.
- **`mntent.h`** — the struct, and a table that is permanently empty. There is no
  `/etc/mtab` and nothing writes one. The callers of `find_mount_point(3)` are asking
  "is this file on a different filesystem from its parent", and the truthful answer
  here is always no; compiling them against an empty table makes them answer that,
  visibly, in one place. Deleting the code instead would leave the callers to invent
  their own answer, and they would not all invent the same one.
- **`sys/syscall.h`** — exists, declares nothing, and carries the ABI numbers as
  documentation. busybox includes it in `coreutils/date.c` under
  `ENABLE_FEATURE_DATE_NANO` for no reason the file records: the nanosecond path
  calls `clock_gettime(3)`, which mlibc has.

## Configuration

`config` is a kconfig fragment, and the three-step sequence is not a preference.
`KCONFIG_ALLCONFIG` is the mechanism the kernel uses for "start from nothing, then
turn on this list", and busybox inherited the code path — but in busybox's `conf.c`,
`allnoconfig` runs conf in `set_no` mode and `conf_askvalue` answers `n` for every
symbol it walks, including the ones just filled in from the fragment. The fragment is
read and discarded. So `build.sh` runs `allnoconfig` for a complete all-off config,
merges the fragment onto it, and lets `oldconfig` resolve the dependencies.

Two things about that last step are load-bearing:

`oldconfig` is fed blank answers rather than `/dev/null`, because it is an
interactive program. `conf_askvalue` falls through to `fgets()` for any symbol with
no value yet and calls `exit(1)` at end of file — and symbols with no value are
exactly the ones the fragment brought into existence, because `allnoconfig` writes a
string option as `""` when its parent is off, so turning the parent on leaves the
child looking unanswered.

Its warnings are read, not skimmed. `oldconfig` is the only thing that knows the real
symbol table, so it is the only place a mistake in the fragment can be caught, and
each warning names a different kind: `nonexistent symbol` (renamed or invented),
`reassign symbol` (two fragments disagreeing), `symbol value ... invalid` (right
name, wrong shape — `=y` for an int option, which kconfig leaves unanswered rather
than guessing).

### What is enabled

A shell, and the programs needed to actually use one. `ash`, because it is the
smallest of busybox's shells that is a real shell; its line editor and history file
are part of ash itself rather than a separate library, which is why it is a megabyte
of program and `hush` is not smaller by enough to matter.

Job control is on deliberately, not speculatively: the pty layer implements process
groups, `setsid` and `TIOCSPGRP`, and the three tty bugs fixed in the kernel's
`setpgid` were found by testing exactly this. `CONFIG_BUSYBOX_EXEC_PATH` is set to
`/bin/busybox` because the upstream default is `/proc/self/exe` and there is no
procfs, so an applet that shells out to another applet would otherwise have no way
to find itself.

### What is not, and why

Every omission is a capability the kernel does not have, so the applet would not be a
reduced version of itself but a program that fails when used.

- **Anything that reads `/proc`** — `ps`, `free`, `uptime`, `top`, `pmap`, `smemcap`.
  There is no procfs. These are not "ps without columns"; they are programs whose
  only source of data does not exist, and they would print an empty list rather than
  admit they cannot answer.
- **Anything that reads a utmp file** — `who`, `users`, `last`. No login accounting to
  record or to read.
- **Anything that needs `statfs`** — `df`. The VFS has no per-filesystem statistics,
  and a fabricated number would be worse than no number. This is also why the platform
  patch drops `HAVE_SYS_STATFS_H`.
- **`mount`/`umount`** — the kernel has no mount syscall at all: no `SYS_MOUNT`, no
  `SYS_UMOUNT`, nothing in the table. There is one filesystem and it is the root.
  This is a different reason from the missing `/etc/mtab`, which is why the header is
  still supplied.
- **Anything that opens a socket** — the entire networking menu. Not a reduced
  `telnet` but a program that cannot be started.
- **Terminal editors and pagers** — `vi`, `less`, `more`. ncurses clients, and ncurses
  is a library the size of the shell it would serve. `ed` is enabled instead, which is
  small and does the job.
- **Daemons and init** — no supervision to attach to, and nothing that wants to be
  pid 1. The kernel spawns the first program directly.
- **setuid applets** — one user, so a privileged helper would be a way to run a
  program as root rather than a tool.
- **Locale** — busybox 1.37 offers no locale options at all, and mlibc has no
  `setlocale` either, so every program agrees on byte order and no program is being
  denied a choice it could have used.
- **`nice`, `taskset`, `ionice`** — one CPU, and a nice value nothing reads.

## Known limitations

- `id`, `whoami` and `groups` are enabled and resolve the caller's name through the
  passwd database, which is why the mlibc port carries the `getpwnam`/`getpwuid`
  sysdeps for this port. They are the only applets here that need it, and a shell with
  no `id` is a shell you cannot check anything with.
- `CONFIG_FEATURE_SH_READLINE` and `CONFIG_FEATURE_SH_HISTORY` do not exist in 1.37.
  ash's line editor is not optional and not a separate symbol; nothing was lost, and
  the absence of the symbols is why they are absent from `config` rather than set to
  `n`.

## Reproducing

```sh
cd ports/mlibc && ./build.sh     # the sysroot; do this first
cd ../.. && make                 # the kernel and the ISO
cd ports/busybox && ./build.sh   # this port
```

Host requirements: `git`, `clang` with `--target=x86_64-pc-samsara` support, `lld`,
and a built mlibc sysroot.
