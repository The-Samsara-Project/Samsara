/* SPDX-License-Identifier: GPL-3.0-or-later
 * Copyright (C) 2026 Harsh Nikarsa
 *
 * <linux/kd.h> for the Samsara fbterm port.
 *
 * These are the Linux *console* keyboard interface: they report and set the
 * mode in which the console driver delivers keystrokes to a process -- scan
 * codes, medium-raw, or Unicode. They exist because a Linux console turns
 * scancodes into characters in the kernel.
 *
 * Samsara has no such console. A pty is not attached to a keyboard: keystrokes
 * arrive already translated to characters by the input server and are delivered
 * through the line discipline like any other terminal. So there is no keyboard
 * mode for a program to set, and a pty correctly answers none of these.
 *
 * The declarations are provided anyway, for one reason: fbterm's input code
 * references them unconditionally, and it already treats a failure as
 * non-fatal -- it sets a `keymapFailure` flag and tells the user that keyboard
 * shortcuts will not work. Compiling fbterm against the real names and letting
 * the ioctl fail honestly is better than deleting the calls, which would hide
 * the fact that the feature is unavailable.
 *
 * This is the same rule the rest of the port follows: report the request, let
 * the kernel refuse it, and never fake a success that a program would then act
 * on.
 */

#ifndef _LINUX_KD_H
#define _LINUX_KD_H

/* Keyboard mode, as reported and set by KDGKBMODE / KDSKBMODE. */
#define K_RAW 0
#define K_MEDIUMRAW 1
#define K_UNICODE 2
#define K_OFF 4

/* Modifier flags used in the console keymap's translation table. */
#define KG_SHIFT (1 << 0)
#define KG_CTRL (1 << 1)
#define KG_ALT (1 << 2)
#define KG_ALTGR (1 << 3)
#define KG_META (1 << 4)

/* The requests themselves. Linux's encoding. A Samsara pty answers neither. */
#define KDGKBMODE 0x4B44 /* get keyboard mode */
#define KDSKBMODE 0x4B45 /* set keyboard mode */

/* Switch the console between a text mode and a graphics mode. */
#define KDSETMODE 0x4B3A

#define KD_TEXT 0x00
#define KD_GRAPHICS 0x01

#endif /* _LINUX_KD_H */
