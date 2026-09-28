/* SPDX-License-Identifier: GPL-2.0-or-later
 * Copyright (C) 2026 Harsh Nikarsa
 *
 * The static-link anchor for the Samsara busybox port.
 *
 * `__dso_handle` is how a C++ object says "run this at exit". Every static
 * object in mlibc's libc.a that has a destructor -- the locale machinery, the
 * DSO-exit list -- refers to it, and so does any C++ translation unit compiled
 * with -fno-exceptions that clang decided needed static init. Nothing defines
 * it: on a dynamically linked system it lives in crtbegin.o, which glibc ships
 * and which has no counterpart here.
 *
 * The value matters, not just the existence. The ABI says a null handle means
 * "the main program", and every implementation of the static-destructor walk
 * special-cases null to mean "there is no separate DSO, so register me in the
 * program's own list rather than in a shared object's". So zero is the correct
 * value for a statically linked program and not merely a placeholder that
 * happens to satisfy the linker: a non-null value would send these destructors
 * to a list nothing ever walks, and the failure mode is memory still held at
 * exit rather than an error.
 *
 * This is the same anchor user/build-chhello.sh and ports/fbterm/Makefile use.
 * Three programs needed the same one-line object and three copies of it would
 * be three places to keep in agreement about a detail nobody will notice is
 * wrong, so it lives here in the port and the other build rules name it.
 *
 * Deliberately not a .S or a linker --defsym: --defsym would be a second,
 * subtler way of spelling this that only works if the symbol is never defined,
 * and a real definition is both checkable and overridable.
 */

void *__dso_handle = 0;
