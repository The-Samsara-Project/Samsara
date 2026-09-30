/* SPDX-License-Identifier: GPL-3.0-or-later */
/* Copyright (C) 2026 Harsh Nikarsa */

/*
 * `crypt(3)`, for the Samsara port.
 *
 * mlibc has no `crypt`, and without it there is no way to store a password that
 * anything can check -- so there was no password login, and `getty` and `login`
 * were disabled. busybox's `login` verifies a password by calling `crypt(3)` and
 * comparing the result against the hash in the passwd database, so this
 * declaration is the whole of what it needs to be able to log in.
 *
 * The declaration lives here rather than in an mlibc header because `crypt` is
 * not POSIX: it is an XSI/BSD function that mlibc does not implement and this
 * port now does. Programs include <unistd.h> for it on glibc; there is no
 * portable home for it, so <crypt.h> is provided and the implementation lives in
 * crypt.cpp alongside it.
 *
 * SHA-512-crypt (`$6$`) is produced and SHA-256-crypt (`$5$`) is verified; see
 * crypt.cpp for why those two and not bcrypt or Argon2id.
 */

#ifndef _SAMSARA_CRYPT_H
#define _SAMSARA_CRYPT_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/*
 * Hash `key` using the scheme named in `setting`, returning a NUL-terminated
 * string in a static buffer that is overwritten by the next call.
 *
 * `setting` is either a bare salt (`$6$abc`) to generate a new hash, or a full
 * hash (`$6$rounds=5000$abc$...`) to reproduce one. Both produce the same output
 * for the same salt, which is what makes verification work.
 *
 * Returns NULL if `setting` names a scheme that is not implemented, or asks for
 * a number of rounds outside the legal range. NULL is the safe direction: a
 * caller comparing the result against a stored hash sees a mismatch and refuses
 * the password, rather than accepting one against a scheme weaker than the hash
 * claims to be.
 */
char *crypt(const char *key, const char *setting);

/*
 * The reentrant form, writing into the caller's buffer of at least 256 bytes.
 * This is the only one of the two that is safe to call from two threads at once,
 * because `crypt` alone would have them share a buffer.
 */
char *crypt_r(const char *key, const char *setting, char *buf);

#ifdef __cplusplus
}
#endif

#endif /* _SAMSARA_CRYPT_H */
