/* SPDX-License-Identifier: GPL-3.0-or-later
 * Copyright (C) 2026 Harsh Nikarsa
 *
 * <linux/keyboard.h> for the Samsara fbterm port.
 *
 * The Linux *console* keycode space: a key type in the high byte and a value in
 * the low byte, packed by the K() macro. fbterm uses it to name a range of
 * synthetic keycodes (see src/input_key.h) for its own keyboard shortcuts --
 * Shift+PageUp, Ctrl+Alt+1, and so on.
 *
 * None of that can work on Samsara, and it is worth being precise about why,
 * because it is not a missing header but a different model of where translation
 * happens.
 *
 * On Linux the console driver owns scancode-to-keycode translation. A program
 * that switches the console to K_UNICODE receives *keycodes* from its tty, not
 * characters, which is what lets it implement shortcuts independently of any
 * keyboard layout. fbterm's AC_START..AC_END range lives in that keycode space.
 *
 * On Samsara a pty is not attached to a keyboard at all. The input server
 * translates scancodes to characters before they reach the terminal, and the
 * line discipline delivers characters. So a program on a Samsara pty receives
 * UTF-8, and a comparison against a console keycode can never match.
 *
 * The declarations are therefore provided so the file compiles, and the
 * shortcuts are simply inert. fbterm already handles this: it detects that it
 * could not set the keyboard mode, sets a keymapFailure flag, and tells the
 * user that shortcuts will not work. Nothing here fakes a match.
 */

#ifndef _LINUX_KEYBOARD_H
#define _LINUX_KEYBOARD_H

/*
 * The packing macro. Linux's own definition, unchanged: the type occupies the
 * high byte and the value the low one.
 *
 * This has to be Linux's packing, not a convenient one, because AC_START is
 * compared against bytes that arrive from elsewhere. A different packing would
 * still compile and would simply never match -- silently disabling every
 * shortcut rather than failing.
 */
#define K(tok, val) (((tok) << 8) + (val))

/* Key types. Only KT_LATIN is named, because it is the only one fbterm uses;
 * the rest of the console key-type space is not declared, since an unused
 * declaration is only a promise to keep in sync for no benefit. */
#define KT_LATIN 2

#endif /* _LINUX_KEYBOARD_H */
