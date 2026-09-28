/* SPDX-License-Identifier: LGPL-2.1-or-later
 * Copyright (C) 2026 Harsh Nikarsa
 *
 * <malloc.h> for the Samsara busybox port.
 *
 * This exists to be *empty of features* rather than to declare anything. The
 * header a libc provides here is conventionally the place the allocator's
 * tuning knobs are declared -- mallopt(3) and the M_* constants it takes -- and
 * this port's libc has no such knobs: mlibc's allocator grows an arena and does
 * not expose a way to ask it to trim, to raise or lower its mmap threshold, or
 * to reserve top-of-heap slack.
 *
 * Programs test for those constants before calling mallopt, which is the right
 * way to write portable code, and a program that follows that pattern needs the
 * header to exist and the constants to be absent. Providing the header is
 * therefore not a stub for a missing function: the correct answer on this
 * platform is that there is nothing to tune, and the #ifdefs in the caller
 * resolve that way precisely because the names are not defined here.
 *
 * Declaring mallopt(3) itself would be the wrong move. An unused prototype
 * invites a caller that skips its own feature test to link successfully and
 * then get a link-time or -- worse, at a call site that is never exercised in
 * testing -- runtime surprise, in exchange for a tuning facility the allocator
 * does not have.
 */

#ifndef _MALLOC_H
#define _MALLOC_H

#include <stddef.h>

/* Nothing below this line, on purpose. See the comment above: the tunables
 * mlibc does not have are not declared as if it did. */

#endif /* _MALLOC_H */
