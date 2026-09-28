/* SPDX-License-Identifier: GPL-3.0-or-later
 * Copyright (C) 2026 Harsh Nikarsa
 *
 * <sys/sysmacros.h> for the Samsara busybox port.
 *
 * The traditional device-number encoding, split out of <sys/types.h> on glibc
 * and reached for directly by anything that needs to decompose an st_rdev.
 * busybox includes it to get `major` and `minor`; it defines `makedev` itself as
 * `bb_makedev`, so only the two accessors are wanted here.
 *
 * The encoding is glibc's, not the 16-bit historic one: a 20-bit major above a
 * 12-bit minor. Reproduced exactly rather than simplified, because a program that
 * decomposes a device number with one packing and the kernel produced it with
 * another gets a plausible-looking wrong answer -- a character device major of 4
 * becomes a major of 256 -- rather than a failure.
 *
 * Defined as static inline functions rather than macros, and guarded so that a
 * <sys/types.h> that already provides them is left alone. A macro would collide
 * with a declaration, and this header is included from inside libbb.h where a
 * redefinition would break the build in a way that points at the wrong file.
 */

#ifndef _SYS_SYSMACROS_H
#define _SYS_SYSMACROS_H

#include <sys/types.h>

/* Already provided by a <sys/types.h> that has them; do nothing. */
#ifndef major

static inline unsigned int major(dev_t dev)
{
	return (unsigned int)((dev >> 8) & 0xfffU);
}

static inline unsigned int minor(dev_t dev)
{
	return (unsigned int)(dev & 0xffU);
}

#endif /* !major */

#ifndef makedev

static inline dev_t makedev(unsigned int maj, unsigned int min)
{
	return (dev_t)((maj << 8) | (min & 0xffU));
}

#endif /* !makedev */

#ifndef makedev64

/* `dev_t` is already 64 bits here, so there is nothing to widen. Declared so
 * that a caller spelling it compiles, with the same result. */
static inline dev_t makedev64(unsigned int maj, unsigned int min)
{
	return (dev_t)(((dev_t)maj << 8) | (dev_t)(min & 0xffU));
}

#endif /* !makedev64 */

#endif /* _SYS_SYSMACROS_H */
