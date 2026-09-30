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
	Mkdir,
	Mkdirat,
	// Directory removal, renaming and hard links. See Sysdeps<Rmdir> in
	// sysdeps.cpp: without these a directory could be created but never
	// removed, `mkdtemp` had nothing to build on, and a program that writes a
	// temporary file and renames it into place -- the safe way to replace a
	// file -- could not.
	Rmdir,
	Rename,
	Renameat,
	Link,
	Linkat,
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
	FutexWait,
	// Credential getters and setters. See Sysdeps<GetUid> in sysdeps.cpp: these
	// were absent, and mlibc reports an absent sysdep with a panic rather than a
	// failed call, so a program that merely asked "who am I?" died.
	GetUid,
	GetEuid,
	GetGid,
	GetEgid,
	SetUid,
	SetEuid,
	SetGid,
	SetEgid,
	Dup,
	Dup2,
	Pselect,
	// Symbolic links. See Sysdeps<Symlink> in sysdeps.cpp: these are what let
	// `/bin/ls` *be* the busybox image, which is how applet dispatch works on a
	// system with one binary.
	//
	// The `Symlink`/`Readlink` pair and not `Symlinkat`/`Readlinkat`, because only
	// these two are reachable without a directory descriptor here. mlibc's
	// `symlinkat` and `readlinkat` ask for the `at` tags, which stay unimplemented
	// and therefore answer ENOSYS through `sysdep_or_enosys` rather than
	// panicking. That is the better failure: a dirfd-relative symlink has no
	// meaning on a filesystem reached by path, and honouring one would mean
	// openat's resolution becoming a full path walk of its own.
	Symlink,
	Readlink,
	// Interval timers, which this kernel does not have. See
	// Sysdeps<SetItimer> in sysdeps.cpp: the tag existing is the point, not
	// the capability, because mlibc aborts on *reaching* a missing sysdep.
	SetItimer,
	// Exec by path. See Sysdeps<Execve> in sysdeps.cpp: this is what makes a
	// file in the filesystem runnable, and therefore what makes /bin a thing
	// rather than a directory of decoration.
	//
	// `Execve` and not `Execveat`: there is no directory-relative exec to
	// honour, for the same reason there is no `Symlinkat`.
	Execve
{};

template<typename Tag>
using Sysdeps = SysdepOf<SamsaraSysdepTags, Tag>;

struct SysdepTraits {
	static constexpr bool usesRtNetlink = false;
};

} // namespace mlibc
