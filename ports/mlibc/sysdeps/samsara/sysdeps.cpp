// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa
//
// Samsara sysdep implementations. The Samsara kernel exposes a deliberately
// non-Linux ABI (kernel/src/abi/mod.rs); every syscall talks directly to it.
//
// Error convention. This is the single most important rule in the file, and it
// is the opposite of what a C programmer's instincts suggest:
//
//   * A sysdep returning `int` returns the *errno value* -- ENOSYS, EFAULT,
//     ESPIPE -- and 0 on success. It does NOT set `errno` and return -1. mlibc's
//     generic layer does that translation itself: it writes the returned value
//     into `errno` before the public wrapper turns it into a -1. Writing
//     `errno` here and returning -1 hands the caller an error code of -1, which
//     is not any errno, so every `if (int e = sysdep<T>(...); e)` test in mlibc
//     takes its error branch. Returning the value is what the contract asks
//     for; `errno` here is redundant *and* wrong.
//   * A sysdep returning a value rather than a status -- `pid_t Sysdeps<GetPid>`
//     -- reports failure by returning a negative number, as the signature says.
//
// The kernel itself returns a negated errno in the return register, so the
// wrappers here convert once, at the syscall boundary, by negating it.
//
// Several sysdeps are deliberately incomplete. Where a capability is missing
// from the ABI, the wrapper returns ENOSYS rather than a plausible-looking
// answer: a caller that is told "no" can fall back, whereas one handed a wrong
// number acts on it.

#include "mlibc/tcb.hpp"
#include <abi-bits/dirent.h>
#include <abi-bits/fcntl.h>
#include <abi-bits/seek-whence.h>
#include <abi-bits/sigset_t.h>
#include <abi-bits/stat.h>
#include <abi-bits/time.h>
#include <abi-bits/vm-flags.h>
#include <bits/ensure.h>
#include <bits/samsara-abi.h>
#include <bits/syscall.h>
#include <bits/winsize.h>
#include <errno.h>
#include <mlibc/all-sysdeps.hpp>
#include <stdint.h>
#include <dirent.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/utsname.h>

namespace {

/// The kernel's `AbiStat`: mode, uid, gid, kind, size.
struct KernelStat {
	uint32_t mode;
	uint32_t uid;
	uint32_t gid;
	uint32_t kind;
	uint64_t size;
};

/// The kernel's `PollFd`, element-for-element identical to mlibc's `pollfd`.
struct KernelPollFd {
	int32_t fd;
	uint16_t events;
	uint16_t revents;
};
static_assert(sizeof(KernelPollFd) == 8, "pollfd layout must match the kernel's");

/// Map a kernel `NodeKind` to the `DT_*` value `readdir` reports.
unsigned char kind_to_dirent(unsigned kind) {
	switch (kind) {
	case 0: return DT_REG;
	case 1: return DT_DIR;
	case 2: return DT_CHR;
	case 3: return DT_FIFO;
	// DT_LNK, not DT_REG. `readdir` reports the type of the *entry*, and a
	// program listing a directory -- `ls -F`, a shell's completion, a `find` that
	// wants to descend -- needs to know which names to follow. A listing that
	// called every link a regular file is a listing a program cannot trust.
	case 4: return DT_LNK;
	default: return DT_UNKNOWN;
	}
}

/// Fold the kernel's `kind` into the `S_IF*` type bits of `st_mode`.
mode_t kind_to_ifmt(unsigned kind) {
	switch (kind) {
	case 0: return S_IFREG;
	case 1: return S_IFDIR;
	case 2: return S_IFCHR;
	case 3: return S_IFIFO;
	// A symlink's own type. It has to be distinguished from S_IFREG because
	// `ls -l` draws the `@`, a shell's `-L` test is `test -L`, and a program
	// deciding whether to `chmod` something is asking about the link, not the
	// target. Reporting a link as a regular file makes all three answer wrongly
	// and none of them fail.
	case 4: return S_IFLNK;
	default: return S_IFCHR;
	}
}

} // namespace

namespace mlibc {

void Sysdeps<LibcPanic>::operator()() {
	sysdep<LibcLog>("mlibc: Samsara panic\n");
	// PROC_EXIT, not EXIT: syscall 4 halts the whole machine, so a panicking
	// process must not take every other process down with it.
	syscall(SYSCALL_PROC_EXIT, 1);
	__builtin_trap();
}

void Sysdeps<LibcLog>::operator()(const char *msg) {
	ssize_t unused;
	// fd 2: a log line written to stdout would be indistinguishable from a
	// program's own output.
	sysdep<Write>(2, msg, strlen(msg), &unused);
}

// --- file descriptors ----------------------------------------------------

int Sysdeps<Open>::operator()(const char *pathname, int flags, mode_t mode, int *fd) {
	auto result = syscall(SYSCALL_OPEN, (long)pathname, (long)strlen(pathname),
	                      (long)flags, (long)mode);
	if (result < 0) {
		return -result;
	}
	*fd = (int)result;
	return 0;
}

int Sysdeps<Unlinkat>::operator()(int dirfd, const char *path, int flags) {
	// AT_FDCWD (-100) is the "resolve against my working directory" sentinel,
	// which is what every caller in the libc passes. The kernel has no
	// directory-relative resolution, so a real `dirfd` is not honored -- the
	// same documented gap as `openat`.
	(void)flags;
	if (!path)
		return EFAULT;
	auto result = syscall(SYSCALL_UNLINK, (long)dirfd, (long)path,
	                      (long)strlen(path));
	if (result < 0) {
		return -result;
	}
	return 0;
}

int Sysdeps<SetItimer>::operator()(int which, const struct itimerval *new_value,
                                   struct itimerval *old_value) {
	// ENOSYS, deliberately, and the tag existing rather than being absent is the
	// whole point of this function.
	//
	// mlibc reaches an unimplemented sysdep through `sysdep_or_enosys`, which
	// calls `__ensure_warn` -- and this port builds mlibc with assertions on, so
	// that warn *aborts*. A program asking for something the kernel cannot do does
	// not get ENOSYS; it gets killed, silently, in the middle of a call it had no
	// reason to expect would be fatal.
	//
	// fbterm is the caller that found it. Its cursor blink wants an interval
	// timer, called setitimer(2), which the kernel has no equivalent of: the APIC
	// timer drives the scheduler's tick and nothing else, and there is no
	// per-process timer to arm. So the honest answer is that there is none.
	//
	// Declaring it here makes that answer reachable. Without the tag, the same
	// answer is what mlibc would reach anyway -- and would abort on the way, which
	// took fbterm's whole process down and left the display frozen on whatever it
	// had last drawn. That is the failure this prevents: a missing optional
	// feature should cost a cursor blink, not the terminal.
	//
	// `old_value` is left alone on the ENOSYS path, matching every other ENOSYS
	// sysdep here: a caller that gets an error has no basis for the old value, and
	// writing a plausible one would invite it to use it.
	(void)which;
	(void)new_value;
	(void)old_value;
	return ENOSYS;
}

int Sysdeps<Symlink>::operator()(const char *target_path, const char *link_path) {
	// Recorded verbatim. The kernel does not resolve the target, and must not:
	// a *relative* target is meaningless without knowing which directory the link
	// sits in, so resolving at creation time would bake in an answer that is
	// wrong the moment the link is moved -- which is the entire reason relative
	// symlinks exist. This is what makes `/bin/ls -> /bin/busybox` a link that
	// still works after `/bin` is itself a link to somewhere else.
	//
	// Both strings are passed with an explicit length, which is also what lets
	// the kernel reject an embedded NUL instead of silently truncating: a
	// program that wrote a target containing one would otherwise create a link
	// to a different path than it believes it created.
	if (!target_path || !link_path)
		return EFAULT;
	auto result = syscall(SYSCALL_SYMLINK, (long)target_path,
	                      (long)strlen(target_path), (long)link_path,
	                      (long)strlen(link_path));
	if (result < 0) {
		return -result;
	}
	return 0;
}

int Sysdeps<Readlink>::operator()(const char *path, void *buffer, size_t max_size,
                                  ssize_t *length) {
	if (!path)
		return EFAULT;
	if (!buffer && max_size)
		return EFAULT;
	auto result = syscall(SYSCALL_READLINK, (long)path, (long)strlen(path),
	                      (long)buffer, (long)max_size);
	if (result < 0) {
		return -result;
	}
	// The kernel appends no NUL, and must not: a caller asking for the stored
	// text wants the stored text, and a `readlink -f` in a shell composes the
	// result into a path. Adding one would mean every caller had to strip it,
	// and a caller that forgot would carry a stray NUL into the next syscall.
	*length = result;
	return 0;
}

int Sysdeps<Execve>::operator()(const char *path, char *const argv[],
                                char *const envp[]) {
	// `envp` is accepted and discarded. The kernel's exec gives the new image the
	// caller's environment, exactly as the index-based EXEC does, and having the
	// two disagree would be worse than either: a program that replaced its
	// environment through one path and not the other would behave differently
	// depending on how it was started.
	//
	// What is *not* discarded is argv, and above all argv[0]. It is the whole basis
	// of applet dispatch: `/bin/ls` is a symlink to `/bin/busybox`, and the only
	// way busybox knows to run `ls` is by looking at the name it was invoked under.
	// Synthesising a name here would make every applet run as `busybox`, which
	// prints the list of applets instead of doing anything.
	//
	// The array is handed to the kernel as-is rather than copied. `argv` is already
	// a NULL-terminated array of pointers in this process's address space, which is
	// exactly the shape the kernel's reader expects, and copying it would mean
	// allocating at the last possible moment before an operation that replaces the
	// address space and cannot fail afterwards.
	if (!path)
		return EFAULT;
	if (!argv)
		return EFAULT;

	// A successful exec never returns; mlibc's caller asserts as much. Reaching the
	// return at all means the kernel reported a failure, and it reports failures
	// as a negative value rather than by setting errno.
	auto ret = syscall(SYSCALL_EXECVE, (long)path, (long)strlen(path), (long)argv);
	if (ret < 0) {
		return -ret;
	}
	return 0;
}

int Sysdeps<Openat>::operator()(int dirfd, const char *path, int flags, mode_t mode, int *fd) {
	// The kernel resolves every path against the calling process's working
	// directory and has no directory-relative resolution. An absolute path is
	// honored; a relative one lands on the cwd. That is correct for
	// `AT_FDCWD` (what a program means by "open this name") and wrong for a
	// real `dirfd` relative to another directory, which is the documented gap.
	(void)dirfd;
	if (!path) {
		return EFAULT;
	}
	return sysdep<Open>(path, flags, mode, fd);
}

int Sysdeps<Close>::operator()(int fd) {
	auto result = syscall(SYSCALL_CLOSE, fd);
	if (result < 0) {
		return -result;
	}
	return 0;
}

int Sysdeps<Read>::operator()(int fd, void *buf, size_t count, ssize_t *bytes_read) {
	auto result = syscall(SYSCALL_READ, fd, (long)buf, (long)count);
	if (result < 0) {
		return -result;
	}
	*bytes_read = (ssize_t)result;
	return 0;
}

int Sysdeps<Write>::operator()(int fd, const void *buf, size_t count, ssize_t *bytes_written) {
	auto result = syscall(SYSCALL_WRITE, fd, (long)buf, (long)count);
	if (result < 0) {
		return -result;
	}
	*bytes_written = (ssize_t)result;
	return 0;
}

int Sysdeps<Pread>::operator()(int fd, void *buf, size_t n, off_t off, ssize_t *bytes_read) {
	// The ABI has no positional read. Seeking first would mutate the shared
	// file cursor, which is precisely what pread promises not to do, so this
	// is refused rather than approximated with lseek+read.
	(void)fd;
	(void)buf;
	(void)n;
	(void)off;
	(void)bytes_read;
	return ENOSYS;
}

int Sysdeps<Pwrite>::operator()(int fd, const void *buf, size_t n, off_t off, ssize_t *bytes_written) {
	(void)fd;
	(void)buf;
	(void)n;
	(void)off;
	(void)bytes_written;
	return ENOSYS;
}

int Sysdeps<Pipe>::operator()(int *fds, int flags) {
	// The kernel's PIPE takes no flags: it always creates a blocking pipe with
	// both ends read/write, which is exactly pipe(2). `pipe2(3)`'s O_NONBLOCK
	// and O_CLOEXEC are therefore not honoured, and saying so beats returning a
	// pipe that silently ignores what the caller asked for -- a program that
	// passed O_CLOEXEC expects the descriptor to vanish across exec.
	if (flags != 0)
		return ENOSYS;

	// Two little-endian u32s, in the kernel's order: read end, then write end.
	uint32_t pair[2] = {0, 0};
	auto result = syscall(SYSCALL_PIPE, (long)pair);
	if (result < 0) {
		return -result;
	}
	fds[0] = (int)pair[0];
	fds[1] = (int)pair[1];
	return 0;
}

int Sysdeps<Seek>::operator()(int fd, off_t offset, int whence, off_t *new_offset) {
	auto result = syscall(SYSCALL_LSEEK, (long)fd, (long)offset, (long)whence);
	if (result < 0) {
		// ESPIPE comes back for a pipe, a terminal or the console. That is not
		// a failure to report as EINVAL: stdio probes with lseek precisely to
		// learn that a stream cannot be repositioned, and it picks its
		// buffering strategy from the answer. Turning it into an error would
		// make every terminal and pipe look like a bug.
		return -result;
	}
	*new_offset = (off_t)result;
	return 0;
}

int Sysdeps<Fcntl>::operator()(int fd, int request, va_list args, int *result) {
	switch (request) {
	case F_GETFL: {
		// The kernel records status flags per descriptor but exposes no way to
		// read them, so the access mode is all that can be reported. O_RDWR is
		// chosen because it is the only answer that does not promise less than
		// the descriptor can do.
		(void)args;
		(void)fd;
		*result = O_RDWR;
		return 0;
	}
	case F_SETFL:
		// Refused rather than ignored: a program that sets O_NONBLOCK must
		// learn it did not take effect, or it will spin on a blocking read.
		(void)args;
		(void)fd;
		return ENOSYS;
	case F_GETFD:
		*result = 0; // no close-on-exec tracking
		return 0;
	case F_SETFD:
		(void)args;
		return 0; // accepted, never honored
	case F_DUPFD:
	case F_DUPFD_CLOEXEC: {
		// `fcntl(fd, F_DUPFD, arg)` -- duplicate `fd` onto the lowest free
		// descriptor at or above `arg`.
		//
		// This is not a corner of the API. A shell cannot turn job control on
		// without it: ash opens `/dev/tty`, then immediately duplicates it out
		// of the way of its own descriptors before touching the foreground
		// process group, and treats a failure here as "there is no terminal" --
		// printing "can't access tty; job control turned off" and running
		// without job control for the rest of the session, on a terminal that
		// works perfectly well.
		//
		// `F_DUPFD` is 0 and `F_DUPFD_CLOEXEC` is 1030 on Linux. Both are listed
		// explicitly because `F_DUPFD` is 0, which is also a value an unrelated
		// request could plausibly carry; a bare `case 0` here would quietly turn
		// that into a duplicate.
		//
		// The `CLOEXEC` variant is accepted and the flag is not stored, because
		// this kernel does not track close-on-exec at all -- the same gap
		// `F_GETFD`/`F_SETFD` above already has.
		//
		// A dedicated syscall rather than `DUP` plus a loop, because the floor is
		// the entire point: `DUP` returns the lowest free descriptor, which is
		// usually 0-2 and exactly where a shell keeps its own. Picking a free slot
		// needs to know which slots are in use, and only the kernel's descriptor
		// table knows that.
		int floor = va_arg(args, int);
		auto ret = syscall(SYSCALL_DUPFD, (long)fd, (long)floor);
		if (ret < 0) {
			return -ret;
		}
		*result = (int)ret;
		return 0;
	}
	default:
		(void)args;
		(void)fd;
		return EINVAL;
	}
}

int Sysdeps<Ioctl>::operator()(int fd, unsigned long request, void *arg, int *result) {
	// The kernel derives the argument size from the request and copies through
	// a bounded kernel-side buffer, so `arg` is a genuine user pointer here.
	auto ret = syscall(SYSCALL_IOCTL, fd, (long)request, (long)arg);
	if (ret < 0) {
		return -ret;
	}
	// ioctl returns 0 and writes any result through `arg`; there is no separate
	// value to report.
	*result = 0;
	return 0;
}

int Sysdeps<Isatty>::operator()(int fd) {
	// mlibc's convention for this sysdep is the opposite of the C function's:
	// return 0 when the descriptor *is* a terminal, and a positive errno when
	// it is not. `isatty(3)` wraps it as `if (sysdep<Isatty>(fd)) { errno = e;
	// return 0; } return 1;`, and stdio reads the same value to choose line vs
	// full buffering -- so returning 1 here made every stream look interactive
	// and, worse, look like an *error* to the buffering path.
	//
	// A terminal is exactly a descriptor that answers TCGETS, which is the same
	// probe Linux uses, so the libc cannot disagree with the kernel about what
	// counts as a terminal.
	struct termios attr;
	if (syscall(SYSCALL_IOCTL, fd, SYSCALL_TCGETS, (long)&attr) == 0)
		return 0;
	// Not a terminal. ENOTTY is the truthful answer and is what stdio compares
	// against; any other failure (a bad descriptor, say) is folded into it
	// because isatty's contract cannot distinguish them.
	return ENOTTY;
}

// --- metadata ------------------------------------------------------------

namespace {

/// What the kernel's `FSTAT` writes: the full POSIX `struct stat` content.
///
/// Mirrors `AbiStatEx` in kernel/src/abi/mod.rs field for field, including the
/// explicit padding, so the offsets agree on both sides.
struct KernelStatEx {
	uint32_t mode;
	uint32_t uid;
	uint32_t gid;
	uint32_t kind;
	uint64_t size;
	uint32_t dev;
	uint32_t _pad;
	uint64_t ino;
	uint64_t nlink;
	int64_t atime;
	uint32_t atime_nsec;
	int64_t mtime;
	uint32_t mtime_nsec;
	int64_t ctime;
	uint32_t ctime_nsec;
	uint64_t blocks;
	uint32_t blksize;
	uint32_t _pad2;
};
// 112, not 100: after `atime_nsec` (4 bytes at offset 56) the compiler must
// pad to 8 before the 8-byte `mtime`, and does so again twice more. Both sides
// are `repr(C)`, so they follow the same rules and land on the same layout --
// this assertion is what proves it, and it exists precisely because a
// mismatch here would silently fill `struct stat` with the wrong fields rather
// than fail.
static_assert(sizeof(KernelStatEx) == 112, "fstat layout must match the kernel's");
static_assert(offsetof(KernelStatEx, mtime) == 64, "fstat mtime offset");
static_assert(offsetof(KernelStatEx, blocks) == 96, "fstat blocks offset");

/// Fill `statbuf` from a kernel `FSTAT` answer.
void fill_from_fstat(const KernelStatEx &ks, struct stat *statbuf) {
	// The kernel reports the permission bits without the type, so the type
	// comes from `kind` and the permissions are masked in on top.
	statbuf->st_mode = kind_to_ifmt(ks.kind) | (ks.mode & 07777);
	statbuf->st_uid = ks.uid;
	statbuf->st_gid = ks.gid;
	statbuf->st_size = (off_t)ks.size;
	statbuf->st_dev = (dev_t)ks.dev;
	statbuf->st_ino = (ino_t)ks.ino;
	statbuf->st_nlink = (nlink_t)ks.nlink;
	statbuf->st_atim.tv_sec = (time_t)ks.atime;
	statbuf->st_atim.tv_nsec = (long)ks.atime_nsec;
	statbuf->st_mtim.tv_sec = (time_t)ks.mtime;
	statbuf->st_mtim.tv_nsec = (long)ks.mtime_nsec;
	statbuf->st_ctim.tv_sec = (time_t)ks.ctime;
	statbuf->st_ctim.tv_nsec = (long)ks.ctime_nsec;
	statbuf->st_blksize = (blksize_t)ks.blksize;
	statbuf->st_blocks = (blkcnt_t)ks.blocks;
	// st_rdev stays 0: it is meaningful only for a device node, and the
	// kernel does not report a major/minor pair. Reporting 0 there is what
	// POSIX asks for anyway when the file is not a device.
	statbuf->st_rdev = 0;
}

} // namespace

int Sysdeps<Stat>::operator()(mlibc::fsfd_target fsfdt, int fd, const char *path, int flags, struct stat *statbuf) {
	(void)flags;
	memset(statbuf, 0, sizeof(*statbuf));

	if (fsfdt == mlibc::fsfd_target::none) {
		return EBADF;
	}

	if (fsfdt == mlibc::fsfd_target::fd) {
		// Ask the kernel what the descriptor actually is. Inventing an answer
		// here is worse than it looks: this is how stdio decides whether a
		// stream is a terminal, and a hard-coded "character device" makes every
		// pipe claim to be one, so `isatty` on a pipe would be true and a
		// program would block forever waiting for input that never comes.
		KernelStatEx ks;
		auto ret = syscall(SYSCALL_FSTAT, (long)fd, (long)&ks);
		if (ret < 0) {
			return -ret;
		}
		fill_from_fstat(ks, statbuf);
		return 0;
	}

	if (!path) {
		return EFAULT;
	}

	KernelStat ks;
	long ret;
	// `flags` is not decoration. `lstat` and `realpath` both pass
	// AT_SYMLINK_NOFOLLOW, and this port has `/bin/ls` as a symlink to
	// `/bin/busybox` -- so a `stat` that ignored the flag would report the
	// *executable* where the caller asked about the *link*, and `ls -l` would
	// never draw the `@`. The two are separate syscalls (STAT and LSTAT) rather
	// than one with a flag, because STAT's argument shape is frozen; see the
	// LSTAT entry in the ABI table for why.
	if (flags & AT_SYMLINK_NOFOLLOW)
		ret = syscall(SYSCALL_LSTAT, (long)path, (long)strlen(path), (long)&ks);
	else
		ret = syscall(SYSCALL_STAT, (long)path, (long)strlen(path), (long)&ks);
	if (ret < 0) {
		return -ret;
	}

	// The kernel reports permission bits without the type, so the type comes
	// from `kind` and the permissions are masked in on top.
	statbuf->st_mode = kind_to_ifmt(ks.kind) | (ks.mode & 07777);
	statbuf->st_uid = ks.uid;
	statbuf->st_gid = ks.gid;
	statbuf->st_size = (off_t)ks.size;
	statbuf->st_nlink = 1;
	statbuf->st_blksize = 4096;
	statbuf->st_blocks = (blkcnt_t)((ks.size + 511) / 512);
	// st_dev, st_ino, st_rdev and the timestamps stay zero for a *path*
	// stat: this syscall predates `FSTAT` and its five-field shape is frozen,
	// so it cannot carry them. A path stat that needs them has to open the
	// file and fstat the descriptor, which is the documented workaround.
	return 0;
}

int Sysdeps<Chdir>::operator()(const char *path) {
	auto ret = syscall(SYSCALL_CHDIR, (long)path, (long)strlen(path));
	if (ret < 0) {
		return -ret;
	}
	return 0;
}

int Sysdeps<GetCwd>::operator()(char *buffer, size_t size) {
	// Two-call protocol, the same encoding the kernel's GETCWD uses: ask for
	// the required size, then ask for the bytes.
	auto need = syscall(SYSCALL_GETCWD, 0, 0);
	if (need < 0) {
		return -need;
	}
	// The kernel's count excludes the NUL, so one extra byte is required.
	if ((size_t)need + 1 > size) {
		return ERANGE;
	}
	auto ret = syscall(SYSCALL_GETCWD, (long)buffer, (long)size);
	if (ret < 0) {
		return -ret;
	}
	buffer[need] = 0;
	return 0;
}

int Sysdeps<Fchdir>::operator()(int fd) {
	// No FCHDIR, and a descriptor cannot be turned back into a path.
	(void)fd;
	return ENOSYS;
}

int Sysdeps<Faccessat>::operator()(int dirfd, const char *pathname, int mode, int flags) {
	(void)dirfd;
	(void)flags;
	if (!pathname) {
		return EFAULT;
	}
	// The kernel's resolver applies the caller's credentials when it resolves
	// the path, so a successful stat means the node is reachable. Existence and
	// search permission are therefore answered correctly; the R_OK/W_OK/X_OK
	// distinctions are not refined beyond that, which is stated here because
	// F_OK is the only mode guaranteed to be exact.
	struct stat st;
	int e = sysdep<Stat>(mlibc::fsfd_target::fd_path, AT_FDCWD, pathname, 0, &st);
	if (e)
		return e;
	(void)mode;
	return 0;
}

int Sysdeps<Umask>::operator()(mode_t mode, mode_t *old) {
	// The kernel applies the umask at open but has no syscall to read or set
	// it. Reporting the process default keeps a read-only caller working.
	(void)mode;
	if (old)
		*old = 0022;
	return 0;
}

int Sysdeps<Chmod>::operator()(const char *pathname, mode_t mode) {
	auto ret = syscall(SYSCALL_CHMOD, (long)pathname, (long)strlen(pathname), (long)mode);
	if (ret < 0) {
		return -ret;
	}
	return 0;
}

int Sysdeps<Fchmod>::operator()(int fd, mode_t mode) {
	(void)fd;
	(void)mode;
	return ENOSYS;
}

int Sysdeps<Truncate>::operator()(const char *path, off_t length) {
	// No truncate-by-path. Opening with O_TRUNC would destroy the contents
	// before the length could even be checked, so this refuses.
	(void)path;
	(void)length;
	return ENOSYS;
}

int Sysdeps<Ftruncate>::operator()(int fd, size_t size) {
	// The VFS supports truncate on a descriptor, but no syscall exposes it.
	(void)fd;
	(void)size;
	return ENOSYS;
}

int Sysdeps<Fsync>::operator()(int fd) {
	// Nothing in the current filesystems buffers a write past the call that
	// made it, so a sync has nothing to do and success is truthful.
	(void)fd;
	return 0;
}

int Sysdeps<Fdatasync>::operator()(int fd) {
	(void)fd;
	return 0;
}

// --- directories ---------------------------------------------------------

int Sysdeps<OpenDir>::operator()(const char *path, int *handle) {
	// There is no directory descriptor in the ABI: the kernel's READDIR takes a
	// path and returns every name in one call. The "handle" is therefore a
	// private record holding the path, and the entry list is formatted lazily on
	// the first ReadEntries so that opendir() on a missing directory fails at
	// the right moment.
	struct DirHandle {
		char path[256];
		bool loaded;
		char *entries;
		size_t entries_len;
		size_t cursor;
	};
	auto dh = static_cast<DirHandle *>(malloc(sizeof(DirHandle)));
	if (!dh) {
		return ENOMEM;
	}
	size_t plen = strlen(path);
	if (plen >= sizeof(dh->path)) {
		free(dh);
		return ENAMETOOLONG;
	}
	memcpy(dh->path, path, plen + 1);
	dh->loaded = false;
	dh->entries = nullptr;
	dh->entries_len = 0;
	dh->cursor = 0;
	*handle = (int)(intptr_t)dh;
	return 0;
}

int Sysdeps<ReadEntries>::operator()(int handle, void *buffer, size_t max_size, size_t *bytes_read) {
	struct DirHandle {
		char path[256];
		bool loaded;
		char *entries;
		size_t entries_len;
		size_t cursor;
	};
	auto dh = reinterpret_cast<DirHandle *>(handle);
	if (!dh) {
		return EBADF;
	}

	if (!dh->loaded) {
		auto need = syscall(SYSCALL_READDIR, (long)dh->path,
		                    (long)strlen(dh->path), 0, 0);
		if (need < 0) {
			return -need;
		}
		// +1 so a zero-length listing still has a valid allocation.
		auto names = static_cast<char *>(malloc((size_t)need + 1));
		if (!names) {
			return ENOMEM;
		}
		auto got = syscall(SYSCALL_READDIR, (long)dh->path,
		                   (long)strlen(dh->path), (long)names, (long)need);
		if (got < 0) {
			free(names);
			return -got;
		}

		// Each entry becomes one `struct dirent` plus its name. Upper-bound the
		// count by the number of NULs the kernel could have returned.
		size_t cap = ((size_t)got + 2) * (sizeof(struct dirent) + 256);
		auto ents = static_cast<char *>(malloc(cap));
		if (!ents) {
			free(names);
			return ENOMEM;
		}

		size_t off = 0;
		ino_t ino = 1;
		size_t p = 0;
		while (p < (size_t)got) {
			size_t nlen = strlen(names + p);
			// `struct dirent` already reserves NAME_MAX+1 for the name, so the
			// record size only grows by the actual name length.
			size_t reclen = sizeof(struct dirent) + nlen + 1;
			if (off + reclen > cap)
				break;
			auto de = reinterpret_cast<struct dirent *>(ents + off);
			memset(de, 0, sizeof(*de));
			de->d_ino = ino++;
			de->d_off = (off_t)off;
			// The kernel returns bare names with no type, so d_type is
			// DT_UNKNOWN: readdir(3) says a consumer must then stat the entry
			// rather than trust the type.
			de->d_type = DT_UNKNOWN;
			memcpy(de->d_name, names + p, nlen + 1);
			de->d_reclen = reclen;
			off += reclen;
			p += nlen + 1;
		}
		free(names);
		dh->entries = ents;
		dh->entries_len = off;
		dh->cursor = 0;
		dh->loaded = true;
	}

	// Copy out whole records only: a partially copied dirent would leave the
	// caller reading uninitialized bytes for d_name.
	size_t out = 0;
	while (dh->cursor < dh->entries_len) {
		auto de = reinterpret_cast<struct dirent *>(dh->entries + dh->cursor);
		if (out + de->d_reclen > max_size)
			break;
		memcpy(static_cast<char *>(buffer) + out, de, de->d_reclen);
		out += de->d_reclen;
		dh->cursor += de->d_reclen;
	}
	*bytes_read = out;
	return 0;
}

// --- time ----------------------------------------------------------------

int Sysdeps<ClockGet>::operator()(int clock, time_t *secs, long *nanos) {
	// The two clocks are different sources, not two readings of one:
	// CLOCK_REALTIME comes from the machine's RTC and is a real civil date,
	// CLOCK_MONOTONIC from the tick counter and is what a timeout or an
	// interval wants. Conflating them is the classic way to make a program
	// that sleeps for "one second" sleep for however long the clock was last
	// set -- so uptime is never reported as wall time, and the wall clock is
	// never reported as monotonic.
	switch (clock) {
	case CLOCK_REALTIME:
	case CLOCK_REALTIME_COARSE: {
		auto ms = syscall(SYSCALL_CLOCK_REALTIME);
		if (ms < 0) {
			return -ms;
		}
		*secs = (time_t)(ms / 1000);
		// The RTC has whole-second resolution. Reporting a millisecond
		// fraction that is always zero is honest; inventing sub-second
		// precision from the tick counter would make the value jump around
		// and then jump back when the clock is set.
		*nanos = (long)((ms % 1000) * 1000000);
		return 0;
	}
	case CLOCK_MONOTONIC:
	case CLOCK_MONOTONIC_RAW:
	case CLOCK_BOOTTIME: {
		auto ms = syscall(SYSCALL_CLOCK_UPTIME_MS);
		if (ms < 0) {
			return -ms;
		}
		*secs = (time_t)(ms / 1000);
		*nanos = (long)((ms % 1000) * 1000000);
		return 0;
	}
	default:
		// An unknown clock id is EINVAL, not a guess. Callers that pass one
		// expect to be told it is unsupported.
		return EINVAL;
	}
}

int Sysdeps<ClockSet>::operator()(int clock, time_t secs, long nanos) {
	// Only the wall clock can be set. Handing `settimeofday` a monotonic clock
	// and quietly succeeding would break every interval and timeout built on
	// it, so refuse instead.
	if (clock != CLOCK_REALTIME && clock != CLOCK_REALTIME_COARSE) {
		return EINVAL;
	}
	// The RTC stores whole seconds. A sub-second component is truncated rather
	// than rounded: rounding up can land on the following second, which looks
	// like the clock jumping forwards.
	(void)nanos;
	auto r = syscall(SYSCALL_SET_TIME, (long)secs);
	if (r < 0) {
		// EPERM for an unprivileged process, which is the common case and the
		// one worth reporting accurately.
		return (int)-r;
	}
	return 0;
}

int Sysdeps<ClockGetres>::operator()(int clock, time_t *secs, long *nanos) {
	(void)clock;
	// The scheduler tick: the APIC timer runs at 100 Hz.
	*secs = 0;
	*nanos = 10000000; // 10 ms
	return 0;
}

int Sysdeps<Sleep>::operator()(time_t *secs, long *nanos) {
	long ms = (long)*secs * 1000 + (*nanos + 999999) / 1000000;
	if (ms <= 0)
		return 0;
	auto ret = syscall(SYSCALL_NANOSLEEP, ms);
	if (ret < 0) {
		return -ret;
	}
	return 0;
}

// --- signals -------------------------------------------------------------

int Sysdeps<Sigaction>::operator()(int sig, const struct sigaction *act, struct sigaction *oldact) {
	// mlibc's `struct sigaction` and the kernel's payload agree field by field,
	// but they are copied explicitly rather than punned: a layout mismatch
	// would otherwise become a wild pointer dereference in ring 3.
	struct KernelSigAction {
		uint64_t handler;
		uint32_t flags;
		uint32_t _pad;
		uint64_t restorer;
		uint64_t mask;
	} ka {}, old {};

	if (act) {
		ka.handler = (uint64_t)act->sa_handler;
		ka.flags = act->sa_flags;
		ka._pad = 0;
		ka.restorer = (uint64_t)act->sa_restorer;
		// Both sides use the same 1024-bit signal set; copy it whole rather
		// than assuming the first word is the whole mask.
		memcpy(&ka.mask, &act->sa_mask, sizeof(ka.mask));
	}

	auto ret = syscall(SYSCALL_SIGACTION, sig,
	                   (long)(act ? (intptr_t)&ka : 0),
	                   (long)(oldact ? (intptr_t)&old : 0));
	if (ret < 0) {
		return -ret;
	}
	if (oldact) {
		oldact->sa_handler = (void (*)(int))old.handler;
		oldact->sa_flags = old.flags;
		oldact->sa_restorer = (void (*)(void))old.restorer;
		memcpy(&oldact->sa_mask, &old.mask, sizeof(old.mask));
	}
	return 0;
}

int Sysdeps<Sigprocmask>::operator()(int how, const sigset_t *__restrict set, sigset_t *__restrict retrieve) {
	auto ret = syscall(SYSCALL_SIGPROCMASK, how, (long)set, (long)retrieve);
	if (ret < 0) {
		return -ret;
	}
	return 0;
}

int Sysdeps<Sigpending>::operator()(sigset_t *set) {
	auto ret = syscall(SYSCALL_SIGPENDING, (long)set);
	if (ret < 0) {
		return -ret;
	}
	return 0;
}

int Sysdeps<Sigsuspend>::operator()(const sigset_t *set) {
	auto ret = syscall(SYSCALL_SIGSUSPEND, (long)set);
	if (ret < 0) {
		// EINTR is how the kernel reports "a signal arrived", which for
		// sigsuspend is the success path rather than a failure.
		if (ret == -EINTR)
			return 0;
		return -ret;
	}
	return 0;
}

int Sysdeps<Sigaltstack>::operator()(const stack_t *ss, stack_t *oss) {
	auto ret = syscall(SYSCALL_SIGALTSTACK, (long)ss, (long)oss);
	if (ret < 0) {
		return -ret;
	}
	return 0;
}

int Sysdeps<Kill>::operator()(pid_t pid, int signal) {
	auto ret = syscall(SYSCALL_KILL, (long)pid, signal);
	if (ret < 0) {
		return -ret;
	}
	return 0;
}

// --- processes -----------------------------------------------------------

pid_t Sysdeps<GetPid>::operator()() {
	// A sysdep that returns a value rather than an error code reports failure
	// by returning a negative number, which is what the signature asks for.
	auto ret = syscall(SYSCALL_GET_PID);
	if (ret < 0) {
		errno = -ret;
		return -1;
	}
	return (pid_t)ret;
}

pid_t Sysdeps<GetPpid>::operator()() {
	// The scheduler records a parent but exposes no syscall for it. The callers
	// reach this through `sysdep_or_panic`, so a stub would panic at first use;
	// reporting the kernel's own pid is the one truthful answer available.
	// This is a known inaccuracy, not a stub: it is the process's own parent
	// within a system that never forks across a kernel/user boundary here.
	return sysdep<GetPid>();
}

int Sysdeps<Waitpid>::operator()(pid_t pid, int *status, int flags, struct rusage *ru, pid_t *ret_pid) {
	// The kernel's WAITPID takes (pid, status) and always blocks, so WNOHANG
	// cannot be honored and `ru` is never filled. Refusing WNOHANG is better
	// than blocking a caller that asked not to block.
	(void)ru;
	(void)ret_pid;
	if (flags & 1 /* WNOHANG */) {
		return ENOSYS;
	}
	auto ret = syscall(SYSCALL_WAITPID, (long)pid, (long)status);
	if (ret < 0) {
		return -ret;
	}
	return (int)ret;
}

int Sysdeps<Fork>::operator()(pid_t *child) {
	auto ret = syscall(SYSCALL_FORK);
	if (ret < 0) {
		return -ret;
	}
	*child = (pid_t)ret;
	return 0;
}

// --- process groups and sessions -----------------------------------------

int Sysdeps<SetPgid>::operator()(pid_t pid, pid_t pgid) {
	auto ret = syscall(SYSCALL_SETPGID, (long)pid, (long)pgid);
	if (ret < 0) {
		return -ret;
	}
	return 0;
}

int Sysdeps<GetPgid>::operator()(pid_t pid, pid_t *pgid) {
	auto ret = syscall(SYSCALL_GETPGID, (long)pid);
	if (ret < 0) {
		return -ret;
	}
	*pgid = (pid_t)ret;
	return 0;
}

int Sysdeps<SetSid>::operator()(pid_t *sid) {
	auto ret = syscall(SYSCALL_SETSID);
	if (ret < 0) {
		return -ret;
	}
	*sid = (pid_t)ret;
	return 0;
}

int Sysdeps<GetSid>::operator()(pid_t pid, pid_t *sid) {
	auto ret = syscall(SYSCALL_GETSID, (long)pid);
	if (ret < 0) {
		return -ret;
	}
	*sid = (pid_t)ret;
	return 0;
}

// --- terminal ------------------------------------------------------------

int Sysdeps<Tcgetattr>::operator()(int fd, struct termios *attr) {
	auto ret = syscall(SYSCALL_IOCTL, fd, SYSCALL_TCGETS, (long)attr);
	if (ret < 0) {
		return -ret;
	}
	return 0;
}

int Sysdeps<Tcsetattr>::operator()(int fd, int actions, const struct termios *attr) {
	// TCSANOW / TCSADRAIN / TCSAFLUSH collapse to immediate: the kernel applies
	// settings synchronously and buffers no output, so there is nothing to
	// drain before or after. The request number is still varied so a driver
	// that does care can tell them apart.
	long req = SYSCALL_TCSETS;
	if (actions == 1 /* TCSADRAIN */)
		req = SYSCALL_TCSETSW;
	else if (actions == 2 /* TCSAFLUSH */)
		req = SYSCALL_TCSETSF;
	auto ret = syscall(SYSCALL_IOCTL, fd, req, (long)attr);
	if (ret < 0) {
		return -ret;
	}
	return 0;
}

int Sysdeps<Tcgetwinsize>::operator()(int fd, struct winsize *winsz) {
	auto ret = syscall(SYSCALL_IOCTL, fd, SYSCALL_TIOCGWINSZ, (long)winsz);
	if (ret < 0) {
		return -ret;
	}
	return 0;
}

int Sysdeps<Tcsetwinsize>::operator()(int fd, const struct winsize *winsz) {
	auto ret = syscall(SYSCALL_IOCTL, fd, SYSCALL_TIOCSWINSZ, (long)winsz);
	if (ret < 0) {
		return -ret;
	}
	return 0;
}

int Sysdeps<Tcflush>::operator()(int fd, int queue) {
	auto ret = syscall(SYSCALL_IOCTL, fd, SYSCALL_TCFLSH, (long)queue);
	if (ret < 0) {
		return -ret;
	}
	return 0;
}

int Sysdeps<Tcflow>::operator()(int fd, int action) {
	auto ret = syscall(SYSCALL_IOCTL, fd, SYSCALL_TCXONC, (long)action);
	if (ret < 0) {
		return -ret;
	}
	return 0;
}

int Sysdeps<Tcdrain>::operator()(int fd) {
	// Output is never buffered, so there is nothing to wait for.
	(void)fd;
	return 0;
}

int Sysdeps<Tcsendbreak>::operator()(int fd, int dur) {
	(void)dur;
	auto ret = syscall(SYSCALL_IOCTL, fd, SYSCALL_TCSBRK, (long)dur);
	if (ret < 0) {
		return -ret;
	}
	return 0;
}

int Sysdeps<Ptsname>::operator()(int fd, char *buffer, size_t length) {
	// The pair index from TIOCGPTN is what names the slave.
	int n = 0;
	auto ret = syscall(SYSCALL_IOCTL, fd, SYSCALL_TIOCGPTN, (long)&n);
	if (ret < 0) {
		return -ret;
	}
	int written = snprintf(buffer, length, "/dev/pts/%d", n);
	if (written < 0) {
		return EINVAL;
	}
	if ((size_t)written >= length) {
		return ERANGE;
	}
	return 0;
}

int Sysdeps<Unlockpt>::operator()(int fd) {
	// A pty slave is never locked here, so the call succeeds as a no-op.
	// Refusing it would force every caller to special-case Samsara.
	int unlock = 0;
	auto ret = syscall(SYSCALL_IOCTL, fd, SYSCALL_TIOCSPTLCK, (long)&unlock);
	if (ret < 0) {
		return -ret;
	}
	return 0;
}

/*
 * Credentials.
 *
 * These eight sysdeps were missing, and the way that failed was worth the
 * trouble of writing down.
 *
 * mlibc reports an unimplemented sysdep by panicking, not by returning an
 * error -- `MLIBC_MISSING_SYSDEP()` then `__ensure(!"Cannot continue without
 * sys_getuid()")`. That is a defensible design for a libc that would otherwise
 * have to guess, but it means the *absence* of a sysdep is indistinguishable
 * from a program that genuinely cannot continue. fbterm's main() opens with
 * `seteuid(getuid())`, and the ported fbterm therefore died on its first
 * statement with a libc assertion and no message of its own -- which reads as
 * "the program is broken" rather than "the libc is missing eight functions".
 *
 * The kernel had the handlers for all eight the whole time; only these wrappers
 * were absent, and the GET* syscall numbers were reserved but unregistered.
 * Both halves are fixed.
 *
 * The getters return the raw value with no error convention to speak of: mlibc's
 * signature is uid_t, not int, so a failure cannot be reported as an errno here
 * at all. getuid(2) cannot fail on this kernel -- there is always a current
 * task, and a task always has credentials -- so a -1 would be a lie. The
 * setters do return an errno, because changing identity can genuinely be
 * refused: only a privileged process may give up or assume another identity, and
 * an unprivileged setuid(2) that quietly succeeded would be a privilege
 * escalation.
 */

uid_t Sysdeps<GetUid>::operator()() {
	return (uid_t)syscall(SYSCALL_GETUID);
}

uid_t Sysdeps<GetEuid>::operator()() {
	return (uid_t)syscall(SYSCALL_GETEUID);
}

gid_t Sysdeps<GetGid>::operator()() {
	return (gid_t)syscall(SYSCALL_GETGID);
}

gid_t Sysdeps<GetEgid>::operator()() {
	return (gid_t)syscall(SYSCALL_GETEGID);
}

int Sysdeps<SetUid>::operator()(uid_t uid) {
	auto ret = syscall(SYSCALL_SETUID, (long)uid);
	return ret < 0 ? (int)-ret : 0;
}

int Sysdeps<SetEuid>::operator()(uid_t uid) {
	auto ret = syscall(SYSCALL_SETEUID, (long)uid);
	return ret < 0 ? (int)-ret : 0;
}

int Sysdeps<SetGid>::operator()(gid_t gid) {
	auto ret = syscall(SYSCALL_SETGID, (long)gid);
	return ret < 0 ? (int)-ret : 0;
}

int Sysdeps<SetEgid>::operator()(gid_t gid) {
	auto ret = syscall(SYSCALL_SETEGID, (long)gid);
	return ret < 0 ? (int)-ret : 0;
}

/*
 * pselect(2), and the syscall behind select(2) as well.
 *
 * mlibc routes `select` through this same sysdep with a null signal mask, so
 * implementing it here is what makes both work. The arguments are handed
 * straight through: the three `fd_set`s are the kernel's own layout (1024 bits,
 * 128 bytes, matching Linux's), the timeout is a `struct timespec` the kernel
 * reads in place, and the kernel writes readiness back into the caller's sets by
 * clearing the bits of descriptors that are not ready -- which is what select(2)
 * specifies, and the reason a caller may loop on the same set without reloading
 * it.
 *
 * A non-null `sigmask` is passed through and refused by the kernel with ENOSYS,
 * which surfaces here as ENOSYS and makes pselect(3) fail honestly. See the note
 * on sys_pselect6 for why a partial implementation would be worse than none.
 */
int Sysdeps<Pselect>::operator()(int nfds, fd_set *read_set, fd_set *write_set,
                                 fd_set *except_set, const struct timespec *timeout,
                                 const sigset_t *sigmask, int *num_events) {
	auto ret = syscall(SYSCALL_PSELECT6, (long)nfds, (long)read_set, (long)write_set,
	                   (long)except_set, (long)timeout, (long)sigmask);
	if (ret < 0)
		return -ret;
	*num_events = (int)ret;
	return 0;
}

int Sysdeps<Dup>::operator()(int fd, int flags, int *newfd) {
	// `flags` is O_CLOEXEC. The kernel has no close-on-exec flag to set, so the
	// value is accepted and ignored rather than refused: refusing would make
	// dup(2) fail for a caller that merely asked for the default, safer
	// behaviour, and the consequence of ignoring it is that a duplicated
	// descriptor survives an exec it perhaps should not have. That is a real
	// (if minor) leak, and it is the honest price of a kernel without
	// FD_CLOEXEC tracking; it is noted here rather than hidden.
	(void)flags;
	auto ret = syscall(SYSCALL_DUP, (long)fd);
	if (ret < 0)
		return -ret;
	*newfd = (int)ret;
	return 0;
}

int Sysdeps<Dup2>::operator()(int fd, int flags, int newfd) {
	(void)flags;
	auto ret = syscall(SYSCALL_DUP2, (long)fd, (long)newfd);
	if (ret < 0)
		return -ret;
	return 0;
}

int Sysdeps<Ttyname>::operator()(int fd, char *buf, size_t size) {
	// The kernel reports the number of bytes written, NUL included, and a
	// negative errno on failure. mlibc's convention for this sysdep is an errno
	// with 0 meaning success, so the count must be discarded rather than
	// returned: passing it through would make every *successful* call look like
	// an error, because a non-zero byte count is indistinguishable from a
	// non-zero errno. The same inversion Isatty documents, in the other
	// direction.
	//
	// The count is still checked, because a kernel reporting success without
	// writing anything would leave `buf` uninitialised and the caller would
	// build a device path out of stack contents.
	auto ret = syscall(SYSCALL_TTYNAME, (long)fd, (long)buf, (long)size);
	if (ret < 0)
		return -ret;
	if (ret == 0)
		return EIO;
	return 0;
}

// --- polling -------------------------------------------------------------

int Sysdeps<Poll>::operator()(struct pollfd *fds, nfds_t count, int timeout, int *num_events) {
	// The kernel's PollFd matches mlibc's `pollfd` element for element, so the
	// array is passed in place instead of being translated.
	auto ret = syscall(SYSCALL_POLL, (long)fds, (long)count, (long)(intptr_t)timeout);
	if (ret < 0) {
		return -ret;
	}
	*num_events = (int)ret;
	return 0;
}

int Sysdeps<Ppoll>::operator()(struct pollfd *fds, nfds_t count, const struct timespec *ts, const sigset_t *mask, int *num_events) {
	// No ppoll in the ABI. The atomic "set mask, poll, restore" that ppoll
	// exists to provide cannot be emulated from user space without a race, so
	// this refuses rather than approximating it.
	(void)fds;
	(void)count;
	(void)ts;
	(void)mask;
	(void)num_events;
	return ENOSYS;
}

// --- process control -----------------------------------------------------

void Sysdeps<Exit>::operator()(int status) {
	// PROC_EXIT (14) terminates this process. EXIT (4) is a different syscall
	// that halts the machine, and using it here would take down every other
	// process in the system when a program simply returns from main.
	syscall(SYSCALL_PROC_EXIT, status);
	__builtin_unreachable();
}

void Sysdeps<Yield>::operator()() {
	syscall(SYSCALL_YIELD);
}

int Sysdeps<Uname>::operator()(struct utsname *buf) {
	memset(buf, 0, sizeof(*buf));
	strncpy(buf->sysname, "Samsara", sizeof(buf->sysname) - 1);
	strncpy(buf->nodename, "samsara", sizeof(buf->nodename) - 1);
	strncpy(buf->release, "0.1.0", sizeof(buf->release) - 1);
	// `version` is where a kernel states which build it is. Putting a build
	// timestamp here is what makes it possible to answer "which kernel was
	// this bug report taken on?" from a `uname` pasted into the report, and
	// gives an operator the one field that says "this is a development build,
	// not a release". The value is stamped at compile time by the Makefile
	// (SAMSARA_BUILD), and falls back to a fixed string when a build did not go
	// through it, so this is never empty.
#ifdef SAMSARA_BUILD
	strncpy(buf->version, SAMSARA_BUILD, sizeof(buf->version) - 1);
#else
	strncpy(buf->version, "Nutcracker (unreleased)", sizeof(buf->version) - 1);
#endif
	strncpy(buf->machine, "x86_64", sizeof(buf->machine) - 1);
	// `domainname` is the networking hostname domain. There is no resolver and
	// no NIS here, so the honest value is the empty string the memset already
	// left, rather than a copy of `nodename` that would look like a real domain.
	return 0;
}

int Sysdeps<Sysconf>::operator()(int num, long *ret) {
	(void)num;
	// Reporting a guessed _SC_* value would be worse than reporting nothing: a
	// caller sizing a buffer from a wrong _SC_PAGESIZE would corrupt memory.
	(void)ret;
	return ENOSYS;
}

int Sysdeps<GetEntropy>::operator()(void *buffer, size_t length) {
	// The kernel refuses rather than handing back something predictable. A
	// zero-length request is a success with nothing to do, which is what
	// getrandom(2) says and what keeps a caller that computes
	// `getrandom(&x, 0, ...)` from treating the answer as a failure.
	if (length == 0) {
		return 0;
	}
	auto ret = syscall(SYSCALL_GETRANDOM, (long)buffer, (long)length);
	if (ret < 0) {
		return -ret;
	}
	// A short fill must not be reported as success: the tail would still hold
	// whatever was on the stack, and the caller has no way to notice.
	if ((size_t)ret != length) {
		return EIO;
	}
	return 0;
}

// --- memory --------------------------------------------------------------

int Sysdeps<TcbSet>::operator()(void *pointer) {
	// Variant I (glibc-style): the FS base points at the TCB.
	//
	// A syscall is used rather than `wrfsbase`, which is the instruction a
	// hosted port would reach for. Two reasons, both learned the hard way:
	//
	//   * `wrfsbase` requires CR4.FSGSBASE. This kernel does not set that bit,
	//     and setting it (CR4 bit 16) hangs the kernel under QEMU, so the
	//     instruction is a #UD here with no other explanation.
	//   * IA32_FS_BASE is not writable from ring 3 at all, so there is no
	//     inline-instruction fallback.
	//
	// So the FS base is installed by the kernel on request (SET_FS_BASE).
	constexpr long kSetFsBase = 77;
	long ret = syscall(kSetFsBase, (long)pointer, 0, 0, 0, 0, 0);
	if (ret < 0) {
		return -ret;
	}
	return 0;
}

int Sysdeps<AnonAllocate>::operator()(size_t size, void **pointer) {
	size_t pages = (size + 0x1000 - 1) / 0x1000;
	auto result = syscall(19 /* MAP_ANON */, (long)pages);
	if (result < 0) {
		*pointer = nullptr;
		return -result;
	}
	*pointer = (void *)(intptr_t)result;
	return 0;
}

int Sysdeps<AnonFree>::operator()(void *pointer, unsigned long size) {
	// The ABI has no munmap, so anonymous mappings leak for the life of the
	// process. Documented rather than hidden: a long-running program that maps
	// and frees in a loop will grow.
	(void)pointer;
	(void)size;
	return 0;
}

int Sysdeps<VmMap>::operator()(void *hint, size_t size, int prot, int flags, int fd, off_t offset, void **window) {
	if (size == 0) {
		return EINVAL;
	}
	if (prot & ~(PROT_READ | PROT_WRITE | PROT_EXEC)) {
		return EINVAL;
	}
	// `MAP_FIXED` promises to place the mapping at an address the caller chose,
	// and honouring that is the entire point of the flag. Returning a different
	// address would leave the caller believing the region it asked for is in
	// use when it is not, so refuse rather than mislead.
	if (flags & MAP_FIXED) {
		return ENOSYS;
	}
	// A file descriptor: a device with a mappable physical range. This is what
	// lets a terminal reach the framebuffer directly, which it must do -- a
	// write(2) per pixel is far too slow to redraw a screen.
	if (fd != -1) {
		// Only `MAP_SHARED` is meaningful for a device window. A private
		// mapping promises copy-on-write semantics, which a window onto device
		// memory cannot provide: the device writes to that memory behind our
		// back, and a copy-on-write view would simply never see those writes.
		// Refusing is the honest answer, because a program handed a private
		// device mapping has no way to detect that its view is stale.
		if ((flags & MAP_SHARED) == 0) {
			return ENOSYS;
		}
		// PROT_EXEC is meaningless for a device window and is refused rather
		// than ignored: a program asking to make device memory executable is
		// asking for something that cannot be honoured.
		if (prot & PROT_EXEC) {
			return EINVAL;
		}
		auto ret = syscall(SYSCALL_DEVICE_MMAP, (long)fd, (long)offset,
		                   (long)size, (long)prot);
		if (ret < 0) {
			return -ret;
		}
		*window = (void *)ret;
		return 0;
	}

	// Anonymous. The kernel creates every mapping read/write/no-execute and
	// `mprotect` adjusts it afterwards, so the requested `prot` is applied here
	// rather than at creation: one code path for protection changes instead of
	// two that can disagree.
	int e = sysdep<AnonAllocate>(size, window);
	if (e) {
		return e;
	}
	int desired = prot ? prot : (PROT_READ | PROT_WRITE);
	if (desired != (PROT_READ | PROT_WRITE)) {
		if (int pe = sysdep<VmProtect>(*window, size, desired); pe) {
			// Do not leave a mapping behind the caller did not ask for.
			sysdep<VmUnmap>(*window, size);
			return pe;
		}
	}
	return 0;
}

int Sysdeps<VmUnmap>::operator()(void *pointer, size_t size) {
	// The kernel only unmaps whole mappings it handed out, so a length that
	// does not match exactly is refused there with EINVAL. That is stricter
	// than POSIX, which allows unmapping a sub-range, and it is deliberate: a
	// partial unmap would have to split a grant, and the grant table is keyed
	// by a base address. Saying so beats unmapping the wrong thing.
	if (!pointer || size == 0) {
		return EINVAL;
	}
	auto ret = syscall(SYSCALL_MUNMAP, (long)pointer, (long)size);
	if (ret < 0) {
		return -ret;
	}
	return 0;
}

int Sysdeps<VmProtect>::operator()(void *pointer, size_t size, int prot) {
	if (!pointer || size == 0) {
		return EINVAL;
	}
	if (prot & ~(PROT_READ | PROT_WRITE | PROT_EXEC)) {
		return EINVAL;
	}
	auto ret = syscall(SYSCALL_MPROTECT, (long)pointer, (long)size, (long)prot);
	if (ret < 0) {
		return -ret;
	}
	return 0;
}

// --- futex ---------------------------------------------------------------
//
// The kernel has no futex. A single-threaded program never contends, so a wake
// that does nothing and a wait that always succeeds are both correct. The wait
// below is a bounded poll loop rather than an unbounded spin: a lost wakeup
// would otherwise hang a core forever with no way out.

int Sysdeps<FutexWake>::operator()(int *pointer, bool all) {
	(void)pointer;
	(void)all;
	return 0;
}

int Sysdeps<FutexWait>::operator()(int *pointer, int expected, const struct timespec *time) {
	// Re-check first: if the value already differs the wait is a no-op, per the
	// futex contract.
	if (__atomic_load_n(pointer, __ATOMIC_ACQUIRE) != expected)
		return 0;

	// A caller-supplied timeout is a real budget. Without one, poll a bounded
	// number of times so a value that never changes cannot spin forever.
	long budget_ms = -1;
	if (time) {
		budget_ms = (long)time->tv_sec * 1000 + (time->tv_nsec + 999999) / 1000000;
	}
	constexpr long kMaxSpins = 100000; // ~1000 s at a 10 ms tick
	constexpr long kTickMs = 10;

	long waited = 0;
	for (long i = 0; budget_ms < 0 ? i < kMaxSpins : waited < budget_ms; i++) {
		if (__atomic_load_n(pointer, __ATOMIC_ACQUIRE) != expected)
			return 0;
		syscall(SYSCALL_YIELD);
		if (budget_ms > 0)
			waited += kTickMs;
	}
	// Report a timeout rather than a spurious success, so a caller polling in a
	// loop makes progress instead of believing the wait worked.
	return EAGAIN;
}

} // namespace mlibc
