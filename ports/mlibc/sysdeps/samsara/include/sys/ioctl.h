/* SPDX-License-Identifier: GPL-3.0-or-later
 * Copyright (C) 2026 Harsh Nikarsa
 *
 * <sys/ioctl.h> for the Samsara port.
 *
 * mlibc ships this header only as part of its glibc-compatibility option, and
 * that option cannot be enabled here: it also brings in `strverscmp`,
 * `versionsort` and the `_l` locale variants, none of which this port supports.
 * That is a poor trade for one declaration, because `ioctl(3)` is POSIX and not
 * a glibc extension -- a ported terminal, shell or `stty` reaches the terminal
 * almost entirely through it.
 *
 * So the port provides the header itself. The declaration and the request
 * numbers are the same as mlibc's; what is added is the device ioctls a
 * character device on this system actually answers, which are the ones a
 * program needs in order to drive a pty or a terminal.
 *
 * Only request *numbers* live here. Each driver validates the request and
 * derives the argument size from it, so a number that appears below is a
 * promise about encoding, not a promise that any particular device implements
 * it; an unimplemented request still fails with ENOTTY, which is what Linux
 * does.
 */

#ifndef _SYS_IOCTL_H
#define _SYS_IOCTL_H

/* No <bits/ioctls.h> here. mlibc's own header pulls it in for the termios
 * request numbers, but that header is only installed alongside the glibc
 * option and is not part of this port's sysroot. The termios requests are
 * already declared by <termios.h>, which a program including this header for
 * terminal work will have included anyway, so nothing is lost by leaving it
 * out and nothing is gained by depending on an internal header. */

#ifdef __cplusplus
extern "C" {
#endif

#ifndef __MLIBC_ABI_ONLY

/**
 * Perform a device-specific control request.
 *
 * The third argument is always an `int *`, even for requests that return
 * nothing -- the kernel writes its result through it -- which is what makes
 * the request number, not the call site, the thing that determines the
 * argument's size and meaning. That convention is why a wrong request number
 * is a crash rather than an error, and why the numbers here are spelled out
 * rather than left to a program to guess.
 */
int ioctl(int __fd, unsigned long __request, ...);

#endif /* !__MLIBC_ABI_ONLY */

/* --- Terminal line discipline and window size ----------------------------- */

/* Already provided by <termios.h> on this port: TCGETS, TCSETS, TIOCGWINSZ,
 * TIOCSWINSZ, FIONREAD, TIOCGPGRP, TIOCSPGRP, TIOCGSID, TCXONC, TCFLSH. */

/* Which pair a pty master drives. This is the one that matters most here: it
 * is how a program tells pair 0's master (`/dev/ptmx0`, slave `/dev/pts0`) from
 * an allocating multiplexer (`/dev/ptmx`, which hands out a brand-new pair on
 * every open). Conflating the two is not a subtle bug -- it sends every
 * keystroke to a terminal nobody is reading, and nothing reports an error. */
#define TIOCGPTN 0x80045430

/* Lock/unlock a pty master's slave, as Linux does. Accepted so a program that
 * calls it works; this port does not require the lock, so it always succeeds. */
#define TIOCSPTLCK 0x40045431

/* --- Modem line control (present for completeness; no serial driver uses
 * these yet) ------------------------------------------------------------- */

#define TIOCMGET 0x5415
#define TIOCMBIS 0x5416
#define TIOCMBIC 0x5417

#define FIONBIO 0x5421
#define FIONCLEX 0x5450
#define FIOCLEX 0x5451

/* --- Network interface queries --------------------------------------------
 * Present because a program may reference them when compiled for Linux. There
 * are no sockets on this system yet, so every one of these fails with ENOTTY
 * rather than pretending to succeed. */

#define SIOCGIFNAME 0x8910
#define SIOCGIFCONF 0x8912
#define SIOCGIFFLAGS 0x8913
#define SIOCSIFFLAGS 0x8914
#define SIOCGIFMTU 0x8921
#define SIOCSIFMTU 0x8922
#define SIOCGIFINDEX 0x8933

#define SIOCPROTOPRIVATE 0x89E0
#define SIOCDEVPRIVATE 0x89F0

#ifdef __cplusplus
}
#endif

#endif /* _SYS_IOCTL_H */
