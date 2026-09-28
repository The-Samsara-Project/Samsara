// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! `ioctl(3)` for the Samsara port.
//!
//! mlibc only defines `ioctl` as part of its glibc-compatibility option, which
//! this port cannot enable -- it would also pull in `strverscmp`, `versionsort`
//! and the `_l` locale variants, none of which are supported. But `ioctl` is
//! POSIX, not a glibc extension, and it is how essentially every program drives
//! a terminal: window size, line-discipline settings, and which pty a master
//! belongs to all go through it. Without it a ported `stty` or terminal has no
//! way to talk to the tty layer at all.
//!
//! So the port supplies the one function. It is deliberately a thin wrapper:
//! the `Ioctl` sysdep already carries the whole implementation, including the
//! part that actually matters -- the kernel derives the argument's size from the
//! request number and copies through a bounded buffer, so `arg` is a real user
//! pointer here and a program cannot make the kernel read or write out of
//! bounds by asking for a request it does not implement.

// C headers, not the C++ wrappers: this port is compiled `-nostdinc` with no
// libstdc++, so `<cerrno>` does not exist. The rest of the port's sources do
// the same.
#include <errno.h>
#include <stdarg.h>

#include <mlibc/all-sysdeps.hpp>

using namespace mlibc;

extern "C" int ioctl(int fd, unsigned long request, ...) {
	// Variadic because that is the prototype every caller already knows, and
	// because the request number is what determines the argument's real type.
	// The `int *` is therefore read out of the varargs rather than being a
	// declared parameter -- which is precisely why an unknown request number
	// is undefined behaviour on any system, ours included.
	va_list args;
	va_start(args, request);
	auto *arg = va_arg(args, void *);
	va_end(args);

	int result;
	// `sysdep_or_enosys` rather than a bare `sysdep`: this port's `Ioctl`
	// is implemented, but a request the driver does not recognise has to
	// surface as ENOTTY, and the fallback keeps that path honest if the sysdep
	// is ever absent.
	if (int e = sysdep_or_enosys<Ioctl>(fd, request, arg, &result); e) {
		errno = e;
		return -1;
	}
	// The kernel reports a request that parses but is not implemented as
	// ENOTTY through the sysdep, so a successful call here means the device
	// accepted the request. `result` is 0 for every request the kernel
	// currently implements; it is returned rather than discarded so a future
	// request with a non-zero result needs no change here.
	return result;
}
