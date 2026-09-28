/* SPDX-License-Identifier: GPL-3.0-or-later
 * Copyright (C) 2026 Harsh Nikarsa
 *
 * <linux/input.h> for the Samsara fbterm port.
 *
 * The evdev key codes, with Linux's own values. fbterm's input code names
 * these constants to build a keymap table, and the values have to be Linux's
 * because the same numbers are what the kernel's `/dev/input/event0` emits
 * (see kernel/src/devices/input.rs). Two independent definitions of an evdev
 * code that disagree would make a key do the wrong thing in every program
 * rather than fail, which is the worst possible outcome.
 *
 * Only the codes fbterm references are declared. This is not a complete evdev
 * header: each declaration here is a promise that the two sides agree, and
 * there is no value in making promises about codes nothing uses.
 */

#ifndef _LINUX_INPUT_H
#define _LINUX_INPUT_H

/* Event types and values, for completeness with the key codes below. fbterm
 * reads characters from a tty rather than evdev events, so it needs the codes
 * as names for its keymap table, not the event stream. */
#define EV_SYN 0x00
#define EV_KEY 0x01
#define EV_REL 0x02

#define SYN_REPORT 0

/* Key codes. Linux's evdev numbering, which is neither contiguous nor
 * alphabetical: the digits run 2..11, the letter block starts at 30, and
 * punctuation is scattered. Spelled out rather than computed so a mistake is
 * visible in review instead of hiding in an arithmetic expression. */
#define KEY_ESC 1
#define KEY_1 2
#define KEY_2 3
#define KEY_3 4
#define KEY_4 5
#define KEY_5 6
#define KEY_6 7
#define KEY_7 8
#define KEY_8 9
#define KEY_9 10
#define KEY_0 11
#define KEY_MINUS 12
#define KEY_EQUAL 13
#define KEY_BACKSPACE 14
#define KEY_TAB 15
#define KEY_LEFTCTRL 29
#define KEY_LEFTSHIFT 42
#define KEY_SPACE 57
#define KEY_CAPSLOCK 58
#define KEY_F1 59
#define KEY_F2 60
#define KEY_F3 61
#define KEY_F4 62
#define KEY_F5 63
#define KEY_F6 64
#define KEY_F7 65
#define KEY_F8 66
#define KEY_F9 67
#define KEY_F10 68
#define KEY_LEFTALT 56
#define KEY_HOME 102
#define KEY_UP 103
#define KEY_PAGEUP 104
#define KEY_LEFT 105
#define KEY_RIGHT 106
#define KEY_END 107
#define KEY_DOWN 108
#define KEY_PAGEDOWN 109
#define KEY_F11 87
#define KEY_F12 88

/* The letter block. evdev deliberately does NOT number these contiguously --
 * the codes follow the physical typewriter rows, so KEY_E is 18 and KEY_I is
 * 23, sitting among the digit codes rather than after KEY_A. That looks like a
 * mistake and is not one, so every letter is spelled out: computing them from
 * KEY_A + n would be both wrong and plausible-looking, which is the worst
 * combination. Cross-checked against the kernel's own table in
 * kernel/src/devices/input.rs. */
#define KEY_A 30
#define KEY_B 48
#define KEY_C 46
#define KEY_D 32
#define KEY_E 18
#define KEY_F 33
#define KEY_G 34
#define KEY_H 35
#define KEY_I 23
#define KEY_J 36
#define KEY_K 37
#define KEY_L 38
#define KEY_M 50
#define KEY_N 49
#define KEY_O 24
#define KEY_P 25
#define KEY_Q 16
#define KEY_R 19
#define KEY_S 31
#define KEY_T 20
#define KEY_U 22
#define KEY_V 47
#define KEY_W 17
#define KEY_X 45
#define KEY_Y 21
#define KEY_Z 44

#endif /* _LINUX_INPUT_H */
