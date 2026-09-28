/* SPDX-License-Identifier: GPL-3.0-or-later
 * Copyright (C) 2026 Harsh Nikarsa
 *
 * config.h for the Samsara fbterm port.
 *
 * Upstream generates this from config.h.in with autoconf, which asks the build
 * host what it happens to have. That is the wrong question for this port: the
 * answers are not properties of the build machine but decisions about what
 * Samsara implements, and they belong to the port rather than to whatever
 * machine happened to run the build. Keeping them here means a build on a host
 * with epoll and signalfd, and a build on a host without them, produce the same
 * binary.
 *
 * This is the place where the port's capability decisions are recorded, so it
 * is deliberately explicit about each one. The rule throughout: a capability
 * that is absent is reported as absent, so fbterm's own conditional code takes
 * the path that works, rather than being handed a fake that leads it to call
 * something the kernel does not implement.
 */

#ifndef FBTERM_CONFIG_H
#define FBTERM_CONFIG_H

/*
 * The four capability flags below are deliberately *not defined at all*.
 *
 * fbterm tests each one with #ifdef, which is autoconf's convention: a
 * capability is "present" if the macro exists and "absent" if it does not.
 * Writing `#define HAVE_EPOLL 0` looks like the more explicit choice and is
 * actively wrong here -- #ifdef is true for a macro defined to 0, so it would
 * select the epoll path, call epoll_wait, and fail at runtime on a kernel that
 * has no epoll. The absence of the definition is the value.
 *
 * Each block below records what the flag would have enabled and why this port
 * leaves it out, since "missing" and "deliberately omitted" are different things
 * and a reader should not have to guess which this is.
 */

/*
 * HAVE_EPOLL -- would select epoll(7) instead of select(2) in the io
 * dispatcher.
 *
 * Samsara implements poll(2) but not epoll. fbterm's fallback is select over a
 * fixed set of at most 32 descriptors, which is entirely adequate here: the set
 * is small, fixed at startup, and never grows. epoll's advantage is O(1)
 * readiness across thousands of descriptors, which this program does not have.
 *
 * The honest cost: the fallback is O(n) per wakeup, and select's descriptor-set
 * size is a compile-time limit. Neither is reached by fbterm's 32 descriptors.
 */

/*
 * HAVE_SIGNALFD -- would receive signals as a readable descriptor.
 *
 * Not implemented on Samsara. fbterm uses it to bring a signal into the same
 * poll loop as its pty. Without it the port installs a plain handler, which is
 * enough for the signals fbterm handles (window resize, child exit) but is not
 * equivalent: a second signal arriving while the handler is running is lost
 * rather than queued, and the descriptor cannot be shared between processes.
 */

/*
 * ENABLE_GPM -- would enable mouse reporting.
 *
 * fbterm's only mouse path connects to the General Purpose Mouse daemon over a
 * Unix domain socket; it has no evdev mouse support of its own. Samsara has no
 * sockets, so the feature is off.
 *
 * The kernel's PS/2 mouse driver and /dev/input/event0 are unaffected -- they
 * are a separate path this port does not currently use. Wiring fbterm's mouse
 * to evdev instead of gpm is a real piece of future work, not a config change.
 */

/*
 * ENABLE_VESA -- would program a VGA card's mode set through /dev/mem.
 *
 * Samsara's kernel already establishes a linear framebuffer, either from the
 * bootloader's mode or through its own Bochs VGA fallback, and /dev/fb0 reports
 * the result. Letting user space reprogram the card would be a second,
 * conflicting way to set up hardware the kernel has already claimed.
 */

/* Package identification, for fbterm's --version. Reported from the port's own
 * pinned upstream commit rather than from autoconf, so a version string cannot
 * claim a revision the build did not actually use. */
#define PACKAGE "fbterm"
#define PACKAGE_NAME "fbterm"
#define PACKAGE_TARNAME "fbterm"
#define PACKAGE_VERSION "1.0.1"
#define PACKAGE_STRING "fbterm 1.0.1"
#define PACKAGE_BUGREPORT "samsara port: see ports/fbterm/README.md"
#define PACKAGE_URL "https://github.com/sfzhi/fbterm"
#define VERSION "1.0.1"

#endif /* FBTERM_CONFIG_H */
