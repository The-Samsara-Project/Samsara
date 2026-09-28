/* SPDX-License-Identifier: GPL-3.0-or-later
 * Copyright (C) 2026 Harsh Nikarsa
 *
 * <sys/vt.h> for the Samsara fbterm port.
 *
 * The userland spelling of the virtual-terminal interface. On glibc this header
 * simply re-exports <linux/vt.h>; there is nothing else in it. It is provided
 * separately so a program that includes the conventional path compiles without
 * knowing which spelling Samsara actually implements.
 */

#ifndef _SYS_VT_H
#define _SYS_VT_H

/* One header, two names, on purpose: fbterm includes <sys/vt.h> while
 * <linux/vt.h> is what actually declares everything. Keeping them in sync by
 * having one include the other is the same arrangement glibc uses, and it means
 * a patch that adds a request number only has to touch one file. */
#include <linux/vt.h>

#endif /* _SYS_VT_H */
