# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Harsh Nikarsa
#
# Samsara sysdeps for mlibc.
#
# The Samsara port models itself on the mlibc `demo` sysdeps: it exposes the
# POSIX option with the minimum set of sysdep tags needed to build and run a
# freestanding static program against the Samsara system call ABI
# (see kernel/src/abi/mod.rs).
#
# The build is compiled with `-D_GNU_SOURCE` (see samsara.cross-file) so that
# mlibc-config.h elevates _DEFAULT_SOURCE/_POSIX_C_SOURCE/_XOPEN_SOURCE; that
# exposes Dl_info/dladdr() (via __MLIBC_POSIX2024) for
# options/posix/generic/dlfcn.cpp without enabling the glibc option (whose
# strverscmp/versionsort surface we do not support).

sysdep_supported_options = {
	'posix': true,
}

# The build identity, stamped in by ports/mlibc/build.sh and reported by
# `uname`. It travels through the environment rather than as a `-Dcpp_args`
# value because a timestamp contains colons, and meson reads a colon in an
# unquoted argument as a key separator -- the value has to arrive already
# inside the C string literal, quotes and all.
samsara_build = run_command(
	'sh', '-c',
	'printf %s "$SAMSARA_BUILD"',
	check: true,
).stdout().strip()
if samsara_build == ''
	samsara_build = 'unreleased'
endif
add_project_arguments('-DSAMSARA_BUILD="' + samsara_build + '"', language: ['c', 'cpp'])

# `ioctl(3)`, which mlibc gates behind its glibc option -- an option this port
# cannot enable, since it also requires `strverscmp`/`versionsort` support we do
# not have. `ioctl` is POSIX rather than a glibc extension and is the only way
# a program drives the tty layer, so the port supplies the single function
# rather than losing it. See ioctl.cpp.
libc_sources += files(
	'ioctl.cpp',
)

# The matching header. `install_headers` is a meson builtin and this meson.build
# shares meson's scope with mlibc's, so the port can install its own header into
# the sysroot alongside the ones mlibc provides.
install_headers(
	'include/sys/ioctl.h',
	subdir: 'sys',
)

rtld_sources += files(
	'syscall.cpp',
	'sysdeps.cpp',
)
rtld_include_dirs += include_directories('include')

libc_sources += files(
	'entry.cpp',
	'syscall.cpp',
	'sysdeps.cpp',
)
libc_include_dirs += include_directories('include')

# `getrandom(2)`. It lives in mlibc's `linux` option rather than `posix`,
# because the *syscall* is a Linux invention -- but the function is what
# unprivileged programs need, and `mktemp`, temporary file names and hash seeds
# are all built on it. Enabling the whole `linux` option would drag in a
# Linux-shaped surface this port does not have, so the one file is taken
# directly. It is a single wrapper around `Sysdeps<GetEntropy>`, which the port
# implements; the matching headers are installed further down.
libc_sources += files(
	'getrandom.cpp',
)

if not no_headers
	install_headers(
		'include/abi-bits/access.h',
		'include/abi-bits/aio.h',
		'include/abi-bits/auxv.h',
		'include/abi-bits/blkcnt_t.h',
		'include/abi-bits/blksize_t.h',
		'include/abi-bits/clockid_t.h',
		'include/abi-bits/dev_t.h',
		'include/abi-bits/dirent.h',
		'include/abi-bits/errno.h',
		'include/abi-bits/fcntl.h',
		'include/abi-bits/fd_set.h',
		'include/abi-bits/fsblkcnt_t.h',
		'include/abi-bits/fsfilcnt_t.h',
		'include/abi-bits/gid_t.h',
		'include/abi-bits/in.h',
		'include/abi-bits/ino_t.h',
		'include/abi-bits/ipc.h',
		'include/abi-bits/limits.h',
		'include/abi-bits/mode_t.h',
		'include/abi-bits/msg.h',
		'include/abi-bits/mqueue.h',
		'include/abi-bits/nlink_t.h',
		'include/abi-bits/pid_t.h',
		'include/abi-bits/poll.h',
		'include/abi-bits/reclen_t.h',
		'include/abi-bits/resource.h',
		'include/abi-bits/rlim_t.h',
		'include/abi-bits/sa_family_t.h',
		'include/abi-bits/sched_param.h',
		'include/abi-bits/seek-whence.h',
		'include/abi-bits/sem.h',
		'include/abi-bits/shm.h',
		'include/abi-bits/sigevent.h',
		'include/abi-bits/sig-limits.h',
		'include/abi-bits/sigset_t.h',
		'include/abi-bits/signal.h',
		'include/abi-bits/sigval.h',
		'include/abi-bits/socket.h',
		'include/abi-bits/sockaddr_storage.h',
		'include/abi-bits/socklen_t.h',
		'include/abi-bits/random.h',
		'include/abi-bits/stat.h',
		'include/abi-bits/statvfs.h',
		'include/abi-bits/suseconds_t.h',
		'include/abi-bits/termios.h',
		'include/abi-bits/time.h',
		'include/abi-bits/uid_t.h',
		'include/abi-bits/utmp-defines.h',
		'include/abi-bits/utmpx.h',
		'include/abi-bits/utsname.h',
		'include/abi-bits/vm-flags.h',
		'include/abi-bits/wait.h',
		subdir: 'abi-bits',
		follow_symlinks: true
	)
	# <sys/random.h>, for the same reason as abi-bits/random.h above: the
	# `linux` option that normally supplies it is not enabled, but `getrandom`
	# is implemented and programs need to be able to declare it.
	install_headers(
		'include/sys/random.h',
		subdir: 'sys',
		follow_symlinks: true
	)
endif

if not headers_only
	crt = custom_target('crt1',
		build_by_default: true,
		command: c_compiler.cmd_array() + ['-c', '-o', '@OUTPUT@', '@INPUT@'],
		input: 'crt1.S',
		output: 'crt1.o',
		install: true,
		install_dir: get_option('libdir')
	)
endif