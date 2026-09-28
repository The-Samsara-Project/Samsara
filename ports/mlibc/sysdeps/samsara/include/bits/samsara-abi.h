#ifndef _BITS_SAMSARA_ABI_H
#define _BITS_SAMSARA_ABI_H

/*
 * The Samsara system call ABI (kernel/src/abi/mod.rs; see also docs/ABI.md):
 *
 *   syscall nr   -> `rax`
 *   arguments    -> `rdi`, `rsi`, `rdx`, `r10`, `r8`, `r9`
 *   return value -> `rax`, negated errno on failure
 *   clobbered    -> `rcx` (return rip), `r11` (saved rflags)
 *
 * These numbers are Samsara's own and are append-only. They are deliberately
 * *not* Linux's, so nothing here can be reused by a program that talks to the
 * kernel directly; it goes through this libc instead.
 */

#define SYSCALL_DEBUG_WRITE      0
#define SYSCALL_YIELD            1
#define SYSCALL_CLOCK_UPTIME_MS  2
#define SYSCALL_KERNEL_VERSION   3
#define SYSCALL_EXIT             4
#define SYSCALL_OPEN             5
/* Terminate the calling *process*. Distinct from SYSCALL_EXIT above, which
 * halts the machine: a libc exiting one program must not stop the kernel. */
#define SYSCALL_PROC_EXIT       14
#define SYSCALL_CLOSE            6
#define SYSCALL_READ             7
#define SYSCALL_WRITE            8
#define SYSCALL_FORK            23
#define SYSCALL_EXEC            24
#define SYSCALL_WAITPID         25
#define SYSCALL_PIPE            26
#define SYSCALL_STAT            27
#define SYSCALL_CHMOD           28
#define SYSCALL_CHOWN           29
#define SYSCALL_KILL            30
#define SYSCALL_UMASK           49
#define SYSCALL_NANOSLEEP       50
#define SYSCALL_SIGACTION       51
#define SYSCALL_SIGPROCMASK     52
#define SYSCALL_SIGSUSPEND      53
#define SYSCALL_SIGALTSTACK     54
#define SYSCALL_SIGRETURN       55
#define SYSCALL_SIGPENDING      56
#define SYSCALL_GET_PID         57
#define SYSCALL_POLL            58
#define SYSCALL_IOCTL           59
#define SYSCALL_GET_ARGS        63
#define SYSCALL_CHDIR           64
#define SYSCALL_GETCWD          65
#define SYSCALL_READDIR         66
#define SYSCALL_MKDIR           67
#define SYSCALL_GET_ENV         71
#define SYSCALL_SETPGID         72
#define SYSCALL_GETPGRP         73
#define SYSCALL_GETPGID         74
#define SYSCALL_SETSID          75
#define SYSCALL_GETSID          76
#define SYSCALL_UNLINK          78
#define SYSCALL_LSEEK           79
#define SYSCALL_CLOCK_REALTIME 80
#define SYSCALL_SET_TIME       81
#define SYSCALL_FSTAT          82
#define SYSCALL_GETRANDOM      83
#define SYSCALL_MUNMAP         84
#define SYSCALL_MPROTECT       85
#define SYSCALL_DEVICE_MMAP    86

/*
 * Terminal ioctl requests. These *are* Linux's, so a program that computes a
 * request from its own <termios.h> lands on the same value the kernel expects.
 */
#define SYSCALL_TCGETS        0x5401
#define SYSCALL_TCSETS        0x5402
#define SYSCALL_TCSETSW       0x5403
#define SYSCALL_TCSETSF       0x5404
#define SYSCALL_TCGETA        0x5405
#define SYSCALL_TCSETA        0x5406
#define SYSCALL_TCSETAW       0x5407
#define SYSCALL_TCSETAF       0x5408
#define SYSCALL_TCSBRK        0x5409
#define SYSCALL_TCXONC        0x540A
#define SYSCALL_TCFLSH        0x540B
#define SYSCALL_TIOCGPGRP     0x540F
#define SYSCALL_TIOCSPGRP     0x5410
#define SYSCALL_TIOCGWINSZ    0x5413
#define SYSCALL_TIOCSWINSZ    0x5414
#define SYSCALL_FIONREAD      0x541B
#define SYSCALL_TIOCGSID      0x5429
#define SYSCALL_TIOCGPTN      0x80045430
#define SYSCALL_TIOCSPTLCK    0x40045431

#endif
