// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa
//
// x86-64 Samsara CRT bootstrap.
//
// The kernel executes `_start` as `e_entry` in ring 3 with `%rsp` pointing at
// the SysV initial stack (argc/argv/envp/auxv lazily mapped by the loader).
// The Samsara syscall ABI uses the `syscall` instruction; see
// kernel/src/abi/mod.rs.

.section .text
.global _start
.type _start, @function
_start:
	// ABI: __mlibc_entry(entry_stack = rsp, main).
	movq %rsp, %rdi
	leaq main(%rip), %rsi
	call __mlibc_entry

	// __mlibc_entry only returns if exit() failed; trap so the failure is loud.
	ud2
.size _start, . - _start

.section .note.GNU-stack,"",%progbits