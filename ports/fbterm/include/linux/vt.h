/* SPDX-License-Identifier: GPL-3.0-or-later
 * Copyright (C) 2026 Harsh Nikarsa
 *
 * <linux/vt.h> for the Samsara fbterm port.
 *
 * The virtual-terminal switching interface: activate a console, release the
 * display, ask which one is active, and set the keyboard mode.
 *
 * Samsara has no virtual terminals. There is one display, one console server,
 * and programs draw into the framebuffer they opened; there is no set of VTs to
 * switch between, so every request here is refused.
 *
 * The declarations exist because fbterm references them unconditionally, and
 * because a program must be able to *compile* against a stable name in order to
 * discover at runtime that the feature is absent. Deleting the call sites
 * instead would hide the absence: fbterm would believe it had switched consoles
 * when it had done nothing, and the user would see a frozen screen with no
 * diagnostic. Here the ioctl fails and fbterm's existing error path runs.
 */

#ifndef _LINUX_VT_H
#define _LINUX_VT_H

#include <linux/kd.h> /* for K_UNICODE and friends */

/* Mode argument for VT_SETMODE. */
#define VT_AUTO 0x00 /* restore the default signal handling */
#define VT_PROCESS 0x01 /* deliver console signals to the calling process */
#define VT_ACK 0x02

/*
 * The console-switching requests, with Linux's encoding.
 *
 * The kernel derives an ioctl's argument size from the request number, so these
 * must be exactly Linux's values even though none of them is ever served: a
 * program that miscomputes the number would make the kernel copy a
 * wrong-sized block, and the whole reason this port refuses them is to stay on
 * the safe side of that.
 */
#define VT_GETSTATE 0x5600
#define VT_SETSTATE 0x5601
#define VT_GETMODE 0x5602
#define VT_SETMODE 0x5603
#define VT_GETLOCK 0x5604
#define VT_SETLOCK 0x5605
#define VT_ACTIVATE 0x5606
#define VT_WAITACTIVE 0x5607
#define VT_RELDISP 0x5608

/* Bind the calling process to a console as its controlling terminal. Linux
 * encodes this in the "old" ioctl space rather than the _IOR-encoded block
 * above, which is why it is 0xD3 and not 0x56xx. It is declared for the same
 * reason as the rest: fbterm references it while trying to hand a pty to the
 * foreground, and the call must compile so the attempt is visible and the
 * refusal is honest. */
#define TIOCCONS 0xD3

/* Flags for VT_GETSTATE / VT_SETSTATE. */
#define VT_DISPLAY_LOCK 0x0010

/* struct vt_mode, as VT_SETMODE writes it. Present so a program can build one;
 * Samsara never fills it, because the ioctl is refused.
 *
 * The field is named `waitv` rather than Linux's `waitvct` because that is the
 * name fbterm uses, and a struct field is not negotiable: a program that
 * assigns `vtm.waitv` must find a member of that name or fail to compile. Since
 * VT_SETMODE is refused here, the name is cosmetic either way -- but it is
 * spelled the way the caller expects so the port builds, and noted so nobody
 * later "fixes" it into a mismatch. */
struct vt_mode {
	char mode;
	char waitv;
	short relsig;
	short acqsig;
	unsigned short frsig;
};

/* struct vt_stat, as VT_GETSTATE writes it. */
struct vt_stat {
	unsigned char v_active;
	unsigned char v_signal;
	unsigned char v_state;
};

#endif /* _LINUX_VT_H */
