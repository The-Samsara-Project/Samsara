// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! `getopt_long(3)` and `getopt_long_only(3)` for the Samsara port.
//!
//! mlibc defines both only under its glibc-compatibility option, which this port
//! cannot enable -- it would also pull in `strverscmp`, `versionsort` and the `_l`
//! locale variants, none of which are supported. But they are not glibc
//! extensions in any sense a user would recognise: `getopt_long` is the parser
//! behind `--long-options`, which is how almost every command line anyone types is
//! written, and busybox in particular cannot even be *built* without the header
//! once `CONFIG_LONG_OPTS=y`.
//!
//! So the port supplies the two functions, and the header alongside them. What
//! makes that cheap is that none of the work is new: mlibc's POSIX option already
//! contains the entire parser, including the long-option path, in
//! `options/internal/generic/getopt.cpp`. All that lives behind the glibc option
//! is the two-line public wrapper that names the mode. This is that wrapper.
//!
//! The alternative -- enabling mlibc's glibc option to get them -- would have
//! brought a locale-variant surface this kernel's ABI does not support, to obtain
//! two functions whose implementation is already compiled into `libc.a`.

// C headers, not the C++ wrappers: this port is compiled `-nostdinc` with no
// libstdc++, so `<cerrno>` does not exist. The rest of the port's sources do the
// same.
#include <getopt.h>

#include <mlibc/getopt.hpp>

using namespace mlibc;

extern "C" int getopt_long(int argc, char *const argv[], const char *optstring,
                           const struct option *longopts, int *longindex) {
	return getopt_common(argc, argv, optstring, longopts, longindex, GetoptMode::Long);
}

extern "C" int getopt_long_only(int argc, char *const argv[], const char *optstring,
                                const struct option *longopts, int *longindex) {
	return getopt_common(argc, argv, optstring, longopts, longindex,
	                     GetoptMode::LongOnly);
}
