/* SPDX-License-Identifier: GPL-2.0-or-later
 * Copyright (C) 2026 Harsh Nikarsa
 *
 * The mount table, for the Samsara busybox port. It is always empty.
 *
 * See include/mntent.h for why this file exists at all when the answer it gives
 * is always "no". The short version: the kernel has no user-visible list of
 * mounts, so there is no mtab to read, and a caller asking whether a file is on
 * its own filesystem is asking a question this system answers uniformly.
 *
 * Every function here is therefore a total function that reports the empty
 * answer, and none of them can fail in a way a caller has to distinguish:
 *
 *   setmntent     NULL. There is no file to open. Callers already treat NULL as
 *                 "no table" -- find_mount_point(3) returns NULL on it -- so this
 *                 is the same path they take when the mtab is simply absent, and
 *                 it is the path a system without one should take.
 *
 *   getmntent     NULL. The conventional end-of-table return, so a loop written as
 *                 `while ((e = getmntent(fp)))` terminates on the first call
 *                 rather than looping forever.
 *
 *   endmntent     0, as the standard specifies for success. Unreachable in
 *                 practice, and correct if reached.
 *
 * Deliberately not implemented: setmntent_add_mount(3). It is declared in the
 * header because busybox's mke2fs support calls it, and this port does not build
 * mke2fs, but declaring a function that writes a table which can never be read
 * back would be worse than leaving the declaration unmet at link time -- a link
 * error names the caller, whereas a function that silently discards what it is
 * given would not.
 */

#include <mntent.h>
#include <stdio.h>

FILE *setmntent(const char *file, const char *mode)
{
	/* Both parameters are accepted and neither is used. `file` is the path that
	 * does not exist, and `mode` would be "r" for every caller here. */
	(void)file;
	(void)mode;
	return NULL;
}

struct mntent *getmntent(FILE *stream)
{
	(void)stream;
	return NULL;
}

struct mntent *getmntent_r(FILE *stream, struct mntent *buf, char *bufstr, int buflen)
{
	(void)stream;
	(void)buf;
	(void)bufstr;
	(void)buflen;
	return NULL;
}

int endmntent(FILE *stream)
{
	(void)stream;
	return 0;
}
