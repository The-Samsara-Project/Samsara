// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa
//
// Samsara sysdep dispatch.
//
// A tag must be listed here for `Sysdeps<Tag>` to resolve to a real
// implementation; anything absent falls back to `NoImpl`, and defining a
// sysdep for an unlisted tag fails to compile. The list below is the set this
// port actually implements in sysdeps.cpp -- nothing is declared speculatively,
// so a sysdep that compiles is one that will be called.

#pragma once

#include <mlibc/sysdep-signatures.hpp>

namespace mlibc {

struct SamsaraSysdepTags :
	// Diagnostics and process teardown.
	LibcPanic,
	LibcLog,
	Exit,
	// File descriptors.
	Open,
	Openat,
	Pipe,
	Unlinkat,
	Close,
	Read,
	Write,
	Pread,
	Pwrite,
	Seek,
	Fcntl,
	Ioctl,
	Isatty,
	Fsync,
	Fdatasync,
	// Metadata.
	Stat,
	Chdir,
	GetCwd,
	Fchdir,
	Faccessat,
	Umask,
	Chmod,
	Fchmod,
	Truncate,
	Ftruncate,
	// Directories.
	OpenDir,
	ReadEntries,
	// Time.
	ClockGet,
	ClockSet,
	ClockGetres,
	Sleep,
	// Signals.
	Sigaction,
	Sigprocmask,
	Sigpending,
	Sigsuspend,
	Sigaltstack,
	Kill,
	// Processes.
	GetPid,
	GetPpid,
	Waitpid,
	Fork,
	Yield,
	// Process groups and sessions.
	SetPgid,
	GetPgid,
	SetSid,
	GetSid,
	// Terminal.
	Tcgetattr,
	Tcsetattr,
	Tcgetwinsize,
	Tcsetwinsize,
	Tcflush,
	Tcflow,
	Tcdrain,
	Tcsendbreak,
	Ptsname,
	Unlockpt,
	Ttyname,
	// Multiplexing.
	Poll,
	Ppoll,
	// Environment.
	Uname,
	Sysconf,
	GetEntropy,
	// Memory.
	TcbSet,
	AnonAllocate,
	AnonFree,
	VmMap,
	VmUnmap,
	VmProtect,
	// Futex.
	FutexWake,
	FutexWait
{};

template<typename Tag>
using Sysdeps = SysdepOf<SamsaraSysdepTags, Tag>;

struct SysdepTraits {
	static constexpr bool usesRtNetlink = false;
};

} // namespace mlibc
