// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

// `getrandom(2)`, taken from mlibc's `linux` option.
//
// That option is not enabled for this port, but the function is not a Linux
// invention any more: POSIX added `getentropy` and `arc4random` alongside it,
// and the practical consumers -- `mktemp`, temporary file names, hash seeds --
// all want this one. It is a thin wrapper over `Sysdeps<GetEntropy>`, which the
// Samsara port implements, so pulling in the one file is enough.
//
// Kept byte-for-byte in behaviour with the upstream version: only
// GRND_RANDOM / GRND_NONBLOCK are accepted, because neither of the flags it
// rejects has an implementation here, and accepting them silently would hand
// back something other than what the caller asked for.

#include <bits/ensure.h>
#include <errno.h>
#include <sys/random.h>

#include <mlibc/all-sysdeps.hpp>
#include <mlibc/debug.hpp>

ssize_t getrandom(void *buffer, size_t max_size, unsigned int flags) {
	if (flags & ~(GRND_RANDOM | GRND_NONBLOCK)) {
		errno = EINVAL;
		return -1;
	}
	// Both accepted flags describe a *source* or a *blocking* behaviour that
	// this kernel has one answer for: a hardware entropy generator, blocking
	// until it has bytes. Neither flag changes the result here, so honouring
	// them costs nothing and keeps a caller that passes GRND_RANDOM working.
	if (int e = mlibc::sysdep_or_enosys<GetEntropy>(buffer, max_size); e) {
		errno = e;
		return -1;
	}
	return max_size;
}
