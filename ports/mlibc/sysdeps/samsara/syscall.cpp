// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa
//
// The Samsara system call ABI (kernel/src/abi/mod.rs):
//
//   syscall nr   -> `rax`
//   arguments    -> `rdi`, `rsi`, `rdx`, `r10`, `r8`, `r9`
//   return value -> `rax` (>= 0 success, < 0 negated errno)
//   clobbered    -> `rcx` (return rip), `r11` (saved rflags)

#include <bits/syscall.h>

using sc_word_t = long;

namespace {
#define SYSCALL_CLOBBERS "memory", "rcx", "r11"
} // namespace

extern "C" long __do_syscall_ret(unsigned long ret) {
	return static_cast<long>(ret);
}

extern "C" sc_word_t __do_syscall0(long sc) {
	register sc_word_t ret asm("rax");
	asm volatile("syscall" : "=a"(ret) : "a"(sc) : SYSCALL_CLOBBERS);
	return ret;
}

extern "C" sc_word_t __do_syscall1(long sc, sc_word_t a1) {
	register sc_word_t ret asm("rax");
	asm volatile("syscall" : "=a"(ret) : "a"(sc), "D"(a1) : SYSCALL_CLOBBERS);
	return ret;
}

extern "C" sc_word_t __do_syscall2(long sc, sc_word_t a1, sc_word_t a2) {
	register sc_word_t ret asm("rax");
	asm volatile("syscall" : "=a"(ret) : "a"(sc), "D"(a1), "S"(a2) : SYSCALL_CLOBBERS);
	return ret;
}

extern "C" sc_word_t __do_syscall3(long sc, sc_word_t a1, sc_word_t a2, sc_word_t a3) {
	register sc_word_t ret asm("rax");
	asm volatile("syscall" : "=a"(ret) : "a"(sc), "D"(a1), "S"(a2), "d"(a3) : SYSCALL_CLOBBERS);
	return ret;
}

extern "C" sc_word_t __do_syscall4(long sc, sc_word_t a1, sc_word_t a2, sc_word_t a3, sc_word_t a4) {
	register sc_word_t ret asm("rax");
	register sc_word_t arg4 asm("r10") = a4;
	asm volatile("syscall" : "=a"(ret) : "a"(sc), "D"(a1), "S"(a2), "d"(a3), "r"(arg4) : SYSCALL_CLOBBERS);
	return ret;
}

extern "C" sc_word_t __do_syscall5(
		long sc, sc_word_t a1, sc_word_t a2, sc_word_t a3, sc_word_t a4, sc_word_t a5) {
	register sc_word_t ret asm("rax");
	register sc_word_t arg4 asm("r10") = a4;
	register sc_word_t arg5 asm("r8") = a5;
	asm volatile("syscall" : "=a"(ret) : "a"(sc), "D"(a1), "S"(a2), "d"(a3), "r"(arg4), "r"(arg5) : SYSCALL_CLOBBERS);
	return ret;
}

extern "C" sc_word_t __do_syscall6(
		long sc, sc_word_t a1, sc_word_t a2, sc_word_t a3, sc_word_t a4, sc_word_t a5, sc_word_t a6) {
	register sc_word_t ret asm("rax");
	register sc_word_t arg4 asm("r10") = a4;
	register sc_word_t arg5 asm("r8") = a5;
	register sc_word_t arg6 asm("r9") = a6;
	asm volatile("syscall" : "=a"(ret) : "a"(sc), "D"(a1), "S"(a2), "d"(a3), "r"(arg4), "r"(arg5), "r"(arg6) : SYSCALL_CLOBBERS);
	return ret;
}

extern "C" sc_word_t __do_syscall7(
		long sc, sc_word_t a1, sc_word_t a2, sc_word_t a3, sc_word_t a4, sc_word_t a5, sc_word_t a6,
		sc_word_t a7) {
	// The Samsara ABI supports six arguments; clamp the seventh to zero.
	(void)a7;
	return __do_syscall6(sc, a1, a2, a3, a4, a5, a6);
}