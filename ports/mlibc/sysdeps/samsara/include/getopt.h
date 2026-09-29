/* SPDX-License-Identifier: GPL-3.0-or-later */
/* Copyright (C) 2026 Harsh Nikarsa */

/*
 * <getopt.h> for the Samsara port.
 *
 * mlibc ships this header only under its glibc option, which this port cannot
 * enable -- it would also require `strverscmp`/`versionsort`, which are not
 * implemented here. But the two long-option entry points are not glibc
 * extensions in any meaningful sense: `getopt_long` is the parser behind
 * `--long-options` in every program a user would expect a shell to run, and
 * busybox is the program this port cares most about. Without the header, busybox
 * does not build with `CONFIG_LONG_OPTS=y` at all -- `libbb/getopt32.c` and
 * `util-linux/getopt.c` both include it unconditionally once that is on.
 *
 * The same reasoning as <sys/ioctl.h>, and the same cost: two functions and a
 * header, in exchange for a large amount of program actually working.
 *
 * Note what is *not* here. mlibc's POSIX option already exports `getopt` itself
 * and the `optarg`/`optind`/`opterr`/`optopt` globals -- `getopt_long` is
 * declared in <unistd.h> as POSIX. This header adds only what was missing: the
 * two `_long` spellings and `struct option`, which lives in
 * <bits/getopt.h> in mlibc's internal headers and is installed already.
 */

#ifndef _GETOPT_H
#define _GETOPT_H

#ifdef __cplusplus
extern "C" {
#endif

/* `struct option`, plus `no_argument`/`required_argument`/`optional_argument`.
 * mlibc's internal bits header; installed in the sysroot, so no copy here. */
#include <bits/getopt.h>

/* The parser itself and the globals it maintains, all of which mlibc's POSIX
 * option already defines. Declared rather than included so that this header
 * agrees with the definitions in the library even if a future mlibc moves
 * them. */
extern char *optarg;
extern int optind;
extern int opterr;
extern int optopt;

/* Supplied by this port; see sysdeps/samsara/getopt.cpp. */
int getopt_long(int argc, char *const argv[], const char *optstring,
		const struct option *longopts, int *longindex);
int getopt_long_only(int argc, char *const argv[], const char *optstring,
		const struct option *longopts, int *longindex);

#ifdef __cplusplus
}
#endif

#endif /* _GETOPT_H */
