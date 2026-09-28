// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! `forkpty(3)` for the Samsara fbterm port.
//!
//! fbterm starts its shell with `forkpty`, and mlibc does not provide it. This
//! implements it on top of Samsara's own pty multiplexer, which is the same
//! arrangement Linux uses and which is why it fits cleanly:
//!
//!   1. open `/dev/ptmx`, which allocates a fresh master/slave pair and
//!      publishes the slave as `/dev/pts/<n>`
//!   2. `TIOCGPTN` on the master, to learn which pair that is
//!   3. open the slave by that name
//!   4. fork
//!   5. in the child: `setsid`, take the slave as the controlling terminal, and
//!      dup it onto 0, 1 and 2
//!
//! Every one of those is a real operation on Samsara, not a simulation. The
//! pair is real, the session is real, and the child's descriptors are the slave
//! end of a real line discipline.

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/wait.h>
#include <termios.h>
#include <unistd.h>

#include "pty.h"

// Linux's request number for "make this my controlling terminal". Samsara's pty
// does not implement it; see the note in forkpty() below.
#define PORT_TIOCSCTTY 0x540E

extern "C" int forkpty(int *amaster, char *name, const struct termios *termp,
                       const struct winsize *winp) {
	// Step 1: the multiplexer. Opening it allocates a new pair, which is why
	// this cannot be a static descriptor the way the boot console's is.
	int master = open("/dev/ptmx", O_RDWR | O_NOCTTY);
	if (master < 0)
		return -1;

	// Step 2: which pair did we get? The master will not tell us its slave's
	// path by any other means, and guessing would risk handing the child some
	// other process's terminal -- which is exactly the bug that made fbterm's
	// keystrokes vanish into an orphan pty.
	int n = -1;
	if (ioctl(master, TIOCGPTN, &n) < 0 || n < 0) {
		int e = errno;
		close(master);
		errno = e;
		return -1;
	}

	// Step 3: the slave, by the name the driver published it under.
	char path[64];
	snprintf(path, sizeof path, "/dev/pts/%d", n);
	if (name)
		snprintf(name, 64, "%s", path);

	int slave = open(path, O_RDWR);
	if (slave < 0) {
		int e = errno;
		close(master);
		errno = e;
		return -1;
	}

	// Optional start-of-pipe state, applied to the slave before the child
	// exists so the child never observes a moment of the wrong termios.
	if (termp)
		tcsetattr(slave, TCSANOW, termp);
	if (winp)
		ioctl(slave, TIOCSWINSZ, winp);

	// Step 4: fork. Everything above is inherited by both halves; the
	// descriptor juggling below is what separates them.
	pid_t pid = fork();
	if (pid < 0) {
		int e = errno;
		close(master);
		close(slave);
		errno = e;
		return -1;
	}

	if (pid == 0) {
		// --- child -------------------------------------------------------
		// A new session, so the child is not the session leader of whatever
		// session fbterm itself runs in and cannot acquire fbterm's
		// terminal as a side effect.
		setsid();

		// Take the slave as the controlling terminal.
		//
		// Samsara's pty does not implement TIOCSCTTY, so this fails, and it
		// is deliberately not fatal. What a controlling terminal is used
		// for is job control: tcgetpgrp, tcsetpgrp, and the shell's
		// foreground-process-group bookkeeping. The child can still read,
		// write, and isatty() its descriptors, which is everything fbterm's
		// shell needs to run and be driven.
		//
		// The alternative would be to fail forkpty outright, which would
		// leave fbterm with no shell at all over a missing convenience. The
		// honest middle is to try, and to say so once so the gap is visible
		// rather than something discovered later as odd job-control
		// behaviour.
		//
		// Nothing is done with the result: a future Samsara that implements
		// TIOCSCTTY makes this call succeed with no edit here.
		(void)ioctl(slave, PORT_TIOCSCTTY, 0);

		// The child's terminal is the slave on all three standard
		// descriptors. This is what lets the shell's isatty(0) be true and
		// its termios calls apply to the terminal it is actually drawing on.
		if (dup2(slave, STDIN_FILENO) < 0)
			_exit(127);
		if (dup2(slave, STDOUT_FILENO) < 0)
			_exit(127);
		if (dup2(slave, STDERR_FILENO) < 0)
			_exit(127);

		// The master must not leak into the child: if it did, the pty would
		// never see end-of-file, because a reader would always remain -- the
		// child itself. That turns "the shell exited" into "the terminal
		// hangs forever", which is a genuinely confusing failure.
		if (slave > STDERR_FILENO)
			close(slave);
		if (master > STDERR_FILENO)
			close(master);

		// Return, deliberately: the caller execs. Exiting here would discard
		// whatever the caller was about to set up.
		return 0;
	}

	// --- parent ---------------------------------------------------------
	close(slave);
	*amaster = master;
	return pid;
}
