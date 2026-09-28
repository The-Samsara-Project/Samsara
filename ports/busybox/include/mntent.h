/* SPDX-License-Identifier: GPL-2.0-or-later
 * Copyright (C) 2026 Harsh Nikarsa
 *
 * <mntent.h> for the Samsara busybox port.
 *
 * mlibc does not provide this header, and on this port there is nothing for it
 * to describe: the kernel has no user-visible list of mounts, so there is no
 * /etc/mtab and no process writes one. `mount` mounts, and `umount` unmounts,
 * through the syscalls, and neither records anything.
 *
 * The header is still supplied, with a struct that matches the standard one
 * field for field, and a table that is permanently empty. That is a deliberate
 * choice over the alternative -- dropping the applets that want it -- because the
 * question a caller of find_mount_point(3) is asking is "is this file on a
 * different filesystem from its parent", and the truthful answer here is always
 * no. Compiling the code against an empty table makes it answer that, and makes
 * the answer visible in one place. Deleting the code instead would leave the
 * callers to invent their own answer, and they would not all invent the same one.
 *
 * The alternative was also considered and rejected: making /etc/mtab an empty
 * file and parsing it for real. That buys a parser and a code path that can only
 * ever produce zero entries, and it would make the emptiness a property of a file
 * someone could edit rather than a property of the system.
 *
 * Consumers should therefore treat a NULL return from setmntent(3) as "no
 * mount table" and a NULL return from getmntent(3) as "end of table". Both are
 * what a caller already has to handle, which is why no caller needs changing.
 */

#ifndef _MNTENT_H
#define _MNTENT_H

#include <stdio.h>

/* The six fields of a mount table entry, in the order getmntent(3) returns them.
 * The sizes are the traditional ones rather than PATH_MAX: a mount point is a
 * path, but the historical limit is what every implementation agrees on, and a
 * value read from a file should not be trusted to be longer. */
struct mntent {
	char mnt_fsname[1024];
	char mnt_dir[1024];
	char mnt_type[64];
	char mnt_opts[256];
	int mnt_freq;
	int mnt_passno;
};

struct mntent *getmntent(FILE *stream);
struct mntent *getmntent_r(FILE *stream, struct mntent *buf, char *bufstr, int buflen);
int setmntent_add_mount(struct mntent *entry);

FILE *setmntent(const char *file, const char *mode);
int endmntent(FILE *stream);

#endif /* _MNTENT_H */
