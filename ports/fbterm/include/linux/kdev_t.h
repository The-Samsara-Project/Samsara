/* SPDX-License-Identifier: GPL-3.0-or-later
 * Copyright (C) 2026 Harsh Nikarsa
 *
 * <linux/kdev_t.h> for the Samsara fbterm port.
 *
 * The major/minor encoding for a device number, used by fbterm to ask "is the
 * console I am attached to the active one?" by comparing MINOR(tty.st_rdev)
 * against the active virtual terminal's number.
 *
 * The encoding is the traditional one -- a dev_t packs a 12-bit minor into the
 * low bits and a 20-bit major above -- and it is reproduced exactly rather than
 * simplified, because a program comparing the result against a number the
 * kernel produced is only correct if both sides use the same packing. On Samsara
 * the answer is always "no" (there are no virtual terminals), but it must be
 * the *right* kind of no, and that means using the real encoding.
 */

#ifndef _LINUX_KDEV_T_H
#define _LINUX_KDEV_T_H

#include <sys/types.h>

/* New-style dev_t encoding: 20-bit major, 12-bit minor. */
#define MINORBITS 20
#define MINORMASK ((1U << MINORBITS) - 1)

#define MAJOR(dev) ((unsigned int)(((dev) >> MINORBITS) & 0xfffU))
#define MINOR(dev) ((unsigned int)((dev) & MINORMASK))

#define MKDEV(major, minor) (((major) << MINORBITS) | (minor))

/* Old-style 16-bit dev_t, kept because a program that checks __KERNEL__ or
 * assumes the small encoding will silently mis-decode anything above 0xffff.
 * Exposed so it can be detected, not so it can be used by accident. */
#define OLD_MINOR(dev) ((unsigned int)((dev) & 0xffU))
#define OLD_MAJOR(dev) ((unsigned int)(((dev) >> 8) & 0xffU))
#define OLD_NEW_DEV(dev) (((dev) << 8) | OLD_MINOR(dev))

#endif /* _LINUX_KDEV_T_H */
