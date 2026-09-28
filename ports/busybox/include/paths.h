/* SPDX-License-Identifier: GPL-3.0-or-later
 * Copyright (C) 2026 Harsh Nikarsa
 *
 * <paths.h> for the Samsara busybox port.
 *
 * A legacy BSD header that mlibc does not provide, carrying only the handful of
 * well-known file names a program falls back to when configuration is missing.
 * busybox includes it unconditionally for `_PATH_DEFPATH` and friends.
 *
 * Provided here rather than patched out of busybox because it is a standard
 * header a libc ought to have, and because the values are the same on any Unix:
 * a program asking "where is the default PATH" wants the answer every other
 * system already gives it, not a Samsara-specific one.
 *
 * `_PATH_DEFPATH` is the interesting one. busybox falls back to it when `$PATH`
 * is unset, so its value has to be *correct* rather than merely present -- a
 * wrong fallback PATH produces an applet that appears to work and then cannot
 * find the other applets it shells out to, which is the kind of failure that
 * only shows up once something is missing.
 */

#ifndef _PATHS_H
#define _PATHS_H

/* Where to look for programs when the environment says nothing. Trailing colon
 * is significant to the shell: it means the current directory is searched too,
 * which for a root-owned system is a needless hazard, so it is omitted. */
#define _PATH_DEFPATH "/bin:/usr/bin:/sbin:/usr/sbin"

/* The system-wide executable directory. Applets and shells live here. */
#define _PATH_BSHELL "/bin/sh"

/* Per-process runtime state. */
#define _PATH_TMP "/tmp"

/* The one file every program reads for a name-to-uid mapping. */
#define _PATH_PASSWD "/etc/passwd"

/* Local time zone data. */
#define _PATH_ZONEINFO "/usr/share/zoneinfo"

/* Where the random device lives, for programs that seed from it directly. */
#define _PATH_RANDOM "/dev/random"

/* The controlling terminal, by convention rather than by mount. */
#define _PATH_TTY "/dev/tty"

#endif /* _PATHS_H */
