// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

#ifndef _SYS_RANDOM_H
#define _SYS_RANDOM_H

#ifdef __cplusplus
extern "C" {
#endif

#include <abi-bits/random.h>
#include <bits/ssize_t.h>
#include <bits/size_t.h>

#ifndef __MLIBC_ABI_ONLY

/// Fill up to `__max_size` bytes of `__buffer` with entropy.
///
/// Declared here rather than pulled from mlibc's `linux` option, which the port
/// does not enable; see `abi-bits/random.h` for why that gate does not apply.
ssize_t getrandom(void *__buffer, size_t __max_size, unsigned int __flags);

#endif /* !__MLIBC_ABI_ONLY */

#ifdef __cplusplus
}
#endif

#endif /*_SYS_RANDOM_H */
