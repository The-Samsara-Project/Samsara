/* SPDX-License-Identifier: GPL-3.0-or-later
 * Copyright (C) 2026 Harsh Nikarsa
 *
 * <sys/syscall.h> for the Samsara busybox port.
 *
 * mlibc does not provide this header. It exists on glibc as a way to make a
 * system call directly, bypassing the libc wrapper -- and busybox includes it in
 * coreutils/date.c under ENABLE_FEATURE_DATE_NANO, for no reason the file
 * records: the nanosecond path there calls clock_gettime(3), which mlibc has.
 *
 * The include is harmless upstream because on glibc the header costs nothing.
 * Here it would mean either vendoring a header whose contents are a statement
 * about a *different* kernel, or failing to compile. Both are wrong, so this
 * supplies the third option: a header that exists, declares nothing, and says
 * why.
 *
 * The numbers below are Samsara's, not Linux's, and they are here so that the
 * reason this header is empty is checkable rather than asserted. They are the
 * stable table from kernel/src/abi/mod.rs, mirrored in
 * ports/mlibc/sysdeps/samsara/include/bits/samsara-abi.h, and the numbering is
 * append-only by design -- a program that hard-coded a Linux number would be
 * calling a different syscall, silently, rather than failing.
 *
 * Note what is *not* provided: the `syscall(2)` function itself. Reaching the
 * kernel directly from a program is exactly what a libc exists to prevent, on
 * this platform more than most, because the call convention differs from Linux's
 * and the return convention differs too. A program that wants the clock calls
 * clock_gettime(3), and one that wants a file calls open(2).
 */

#ifndef _SYS_SYSCALL_H
#define _SYS_SYSCALL_H

/* The ABI, for reference. See kernel/src/abi/mod.rs and docs/ABI.md.
 *
 *   syscall nr -> rax
 *   arguments  -> rdi, rsi, rdx, r10, r8, r9
 *   result     -> rax, negated errno on failure
 *   clobbered  -> rcx (return rip), r11 (saved rflags)
 */
#define SYS_DEBUG_WRITE		0
#define SYS_YIELD		1
#define SYS_CLOCK_UPTIME_MS	2
#define SYS_KERNEL_VERSION	3
#define SYS_EXIT		4
#define SYS_OPEN		5
#define SYS_CLOSE		6
#define SYS_READ		7
#define SYS_WRITE		8
#define SYS_PROC_EXIT		14
#define SYS_FORK		23
#define SYS_EXEC		24
#define SYS_WAITPID		25
#define SYS_PIPE		26
#define SYS_STAT		27
#define SYS_CHMOD		28
#define SYS_CHOWN		29
#define SYS_KILL		30
#define SYS_GETUID		31
#define SYS_GETGID		32
#define SYS_GETEUID		33
#define SYS_GETEGID		34
#define SYS_SETUID		39
#define SYS_SETGID		40
#define SYS_SETEUID		41
#define SYS_SETEGID		42
#define SYS_UMASK		49
#define SYS_NANOSLEEP		50
#define SYS_SIGACTION		51
#define SYS_SIGPROCMASK		52
#define SYS_SIGSUSPEND		53
#define SYS_SIGALTSTACK		54
#define SYS_SIGRETURN		55
#define SYS_SIGPENDING		56
#define SYS_GETPID		57
#define SYS_POLL		58
#define SYS_IOCTL		59
#define SYS_GETARGS		63
#define SYS_CHDIR		64
#define SYS_READDIR		66
#define SYS_MKDIR		67
#define SYS_GETENV		71
#define SYS_SETPGID		72
#define SYS_GETPGRP		73
#define SYS_GETPGID		74
#define SYS_SETSID		75
#define SYS_GETSID		76
#define SYS_UNLINK		78
#define SYS_LSEEK		79
#define SYS_CLOCK_REALTIME	80
#define SYS_SETTIME		81
#define SYS_FSTAT		82
#define SYS_GETRANDOM		83
#define SYS_MUNMAP		84
#define SYS_MPROTECT		85
#define SYS_DEVICE_MMAP		86
#define SYS_TTYNAME		87
#define SYS_DUP			88
#define SYS_DUP2		89
#define SYS_PSELECT6		90

#endif /* _SYS_SYSCALL_H */
