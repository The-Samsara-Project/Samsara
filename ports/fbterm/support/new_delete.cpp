// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! C++ allocation operators for the Samsara fbterm port.
//!
//! Samsara has no libstdc++, so the global `operator new` and `operator delete`
//! have to come from somewhere. They live here rather than in a patch because
//! they are not a change to fbterm's logic at all -- they are the C++ runtime
//! the compiler assumes exists, and fbterm just uses `new` the way any C++
//! program does.
//!
//! Three properties are load-bearing, and each corresponds to a way a naive
//! version goes wrong:
//!
//!  - The scalar and array forms must be *distinct* symbols. A class with a
//!    non-trivial destructor records its own size ahead of the array, so
//!    `delete[]` has to be a separate function that knows to read that cookie.
//!    Routing both through one implementation that ignores the distinction
//!    corrupts every array of such a class.
//!
//!  - `delete` must tolerate a null pointer, because `delete p` where `p` is null
//!    is defined to do nothing. The sized forms take the size as a second
//!    argument precisely so a mismatched new/delete pair can be diagnosed
//!    rather than guessed at; here the size is simply ignored, which is correct
//!    for an allocator that hands back exactly what malloc returned.
//!
//!  - The operators keep C++ language linkage. Declaring them `extern "C"` looks
//!    harmless and is not: the allocation operators are defined by the C++
//!    language to have C++ linkage, and giving them C linkage makes every use in
//!    the program an unresolved symbol against a mangled reference.
//!
//! On allocation failure: this is built with -fno-exceptions because Samsara
//! has no unwinder and no part of fbterm catches. A standard `operator new`
//! throws std::bad_alloc; here it aborts with a message instead. That is a real
//! deviation, and it is the honest one -- a throw with no unwinder would reach
//! std::terminate by a longer path that first prints something misleading. For
//! a program that allocates successfully, which is all of fbterm does under
//! normal operation, the effect is nil.
//!
//! No nothrow overloads are defined. They need std::nothrow_t from <new>, which
//! is libstdc++, and nothing could select them anyway: -fno-exceptions and
//! -fno-rtti are both in force, so overload resolution never picks the nothrow
//! forms. A program built without those flags would fail to link here, and would
//! need a real libstdc++ regardless.

#include <stdio.h>
#include <stdlib.h>

// Report and stop. The libc's abort() rather than a bare trap, so the message
// reaches the console first: a terminal that vanishes without explanation is the
// failure worth spending two lines to avoid.
static void oom(size_t n)
{
	fprintf(stderr, "[new] out of memory allocating %lu bytes\n",
			(unsigned long)n);
	abort();
}

void *operator new(size_t n)
{
	void *p = malloc(n);
	if (!p)
		oom(n);
	return p;
}

void *operator new[](size_t n)
{
	void *p = malloc(n);
	if (!p)
		oom(n);
	return p;
}

void operator delete(void *p) noexcept
{
	free(p);
}

void operator delete[](void *p) noexcept
{
	free(p);
}

void operator delete(void *p, size_t) noexcept
{
	free(p);
}

void operator delete[](void *p, size_t) noexcept
{
	free(p);
}
