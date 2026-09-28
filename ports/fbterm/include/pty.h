/* SPDX-License-Identifier: GPL-3.0-or-later
 * Copyright (C) 2026 Harsh Nikarsa
 *
 * <pty.h> for the Samsara fbterm port.
 *
 * fbterm spawns its shell with forkpty(3). mlibc has no forkpty, so the port
 * supplies it -- and it turns out Samsara already has everything the real one
 * needs, which is a fair result given that the pty driver was written to the
 * same Linux semantics.
 *
 * The Linux arrangement is:
 *
 *   /dev/ptmx      a multiplexer; opening it allocates a fresh master/slave
 *                  pair and publishes the slave as /dev/pts/<n>
 *   TIOCGPTN       asks a master which pair it drives, so the slave's path can
 *                  be constructed without guessing
 *
 * Between them those two give exactly what openpty(3) has to do, so this is a
 * real implementation rather than a stub: the master and slave are genuinely
 * distinct descriptors on a genuinely new pair.
 *
 * The one Linux step with no Samsara equivalent is TIOCSCTTY, which makes the
 * new session's slave its controlling terminal. It is issued and its result
 * checked, but a refusal is reported rather than treated as fatal -- see
 * forkpty() for why.
 */

#ifndef _PTY_H
#define _PTY_H

#include <sys/types.h>
#include <termios.h>

#ifdef __cplusplus
extern "C" {
#endif

/*
 * Fork a child attached to a new pseudo-terminal.
 *
 * `amaster` receives the master descriptor; `name`, if non-NULL, receives the
 * slave's path and must be at least 64 bytes. The termios and win-size
 * arguments may be NULL, meaning "whatever the pty defaults to".
 *
 * Returns the child's pid in the parent, and never returns in the child -- the
 * child is already sitting on its own pty slave as fd 0, 1 and 2. Returns -1
 * with errno set on failure.
 *
 * The child does not call execlp/execvp here; the caller does, which is what
 * lets a caller report a failed exec before the pty disappears.
 */
int forkpty(int *amaster, char *name, const struct termios *termp, const struct winsize *winp);

#ifdef __cplusplus
}
#endif

#endif /* _PTY_H */
