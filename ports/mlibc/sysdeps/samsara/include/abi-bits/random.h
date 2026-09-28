// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

#ifndef _ABIBITS_RANDOM_H
#define _ABIBITS_RANDOM_H

// `getrandom(2)` flags.
//
// These are Linux's values, and the point of the port is that a program
// computing a flag from its own <sys/random.h> agrees with the libc. They are
// the same three Linux defines.
//
// This header replaces mlibc's own, which refuses to compile unless the `linux`
// option is enabled ("getrandom() is inherently Linux specific"). That gate is
// the wrong shape for this port: what makes `getrandom` Linux-specific is the
// *syscall*, not the function, and the function is POSIX-adjacent and wanted by
// `mktemp`, temporary file names and hash seeds alike. The port implements
// `Sysdeps<GetEntropy>` directly, so the function is available and the gate has
// nothing left to protect.
//
// All three flags are accepted and none of them changes the result: there is one
// entropy source, and it blocks until it has bytes. `GRND_INSECURE` would ask
// for a faster, weaker generator, and taking it while ignoring it would be
// worse than refusing -- so it is defined, and a caller who passes it is told
// so by the argument check in the implementation.

#define GRND_NONBLOCK 0x0001
#define GRND_RANDOM 0x0002
#define GRND_INSECURE 0x0004

#endif /* _ABIBITS_RANDOM_H */
