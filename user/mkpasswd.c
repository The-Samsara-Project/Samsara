/* SPDX-License-Identifier: GPL-3.0-or-later */
/* Copyright (C) 2026 Harsh Nikarsa */

/*
 * mkpasswd -- turn a password into a `crypt(3)` hash, on stdout.
 *
 * This exists because the installer is Rust and the Rust programs in this tree
 * do not link mlibc: they use rust-embedded's freestanding `std` and talk to
 * the kernel through `nutcracker-rt`. So the installer cannot call `crypt(3)`
 * itself, and it must not -- the alternative is implementing password hashing
 * inside the kernel, which puts a security-critical computation in the one
 * place it has least privilege and no way to be tested against a reference.
 *
 * So the installer runs this, which is linked against mlibc exactly as
 * `chello` is, and writes the resulting hash into the passwd database. Hashing
 * stays in user space, in a program that can be checked against a reference
 * implementation.
 *
 * Input: the descriptors to read the password from and to write the hash to, as
 * two arguments. See `main` for why they are passed rather than assumed to be 0
 * and 1. The password on that descriptor is one line, terminated by a newline.
 * The newline is not part of the password, which is why it is read as a line
 * rather than as "everything until end of file" -- a password may legitimately
 * contain spaces, and one that ended in a space would otherwise be silently
 * altered.
 *
 * Output: one line, the hash, ready to be a `pw_passwd` field. Nothing else is
 * printed, so the installer can take the whole of the output as the hash. Any
 * diagnostic goes to stderr.
 *
 * The salt is generated here rather than supplied, from `getrandom(3)`, and is
 * drawn from the base64 alphabet that `crypt(3)` encodes with. That alphabet is
 * not decorative: the hash is a base64 permutation over the digest, and a salt
 * containing a character outside it produces a string some implementations
 * refuse to read back -- which would lock the owner out of their own password.
 */

#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <stdlib.h>
#include <crypt.h>
#include <sys/random.h>

/* Longest password accepted. `crypt(3)` has no limit; this bounds the buffers. */
#define MAX_PASSWORD 256
#define SALT_LEN 16

/* The alphabet `crypt(3)`'s base64 encoding uses, and therefore the only
 * characters a salt may contain. */
static const char B64[] =
    "./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

int main(int argc, char **argv)
{
	char password[MAX_PASSWORD];
	char setting[32];
	unsigned char rnd[SALT_LEN];
	char salt[SALT_LEN + 1];
	size_t len = 0;
	int c;

	/* The descriptors arrive as arguments and are re-pointed here rather than
	 * being arranged by the caller.
	 *
	 * The obvious arrangement -- have the installer put the pipe on descriptors
	 * 0 and 1 before spawning -- is not available to it: the installer is
	 * reading the keyboard on its own descriptor 0 and painting the framebuffer,
	 * and moving either would take the wizard's own input away mid-setup. It
	 * cannot fork-and-re-point either, because it has no way to exec an
	 * arbitrary program; it can only spawn one of the kernel's embedded images.
	 *
	 * So it passes the descriptor *numbers* and this program adopts them. That
	 * also keeps the password off the command line, where every process on the
	 * system could read it, and off the filesystem.
	 *
	 * This is what the first version got wrong: it assumed the pipe was already
	 * on 0 and 1, so it read the keyboard instead of the password and printed
	 * the hash to the console instead of the pipe. The installer then read an
	 * empty pipe, and reported "could not hash the password" -- which is what
	 * every password, including a perfectly good one, produced.
	 */
	if (argc < 3) {
		fprintf(stderr, "usage: mkpasswd <password-fd> <output-fd>\n");
		return 2;
	}
	int in_fd = atoi(argv[1]);
	int out_fd = atoi(argv[2]);
	if (in_fd < 0 || out_fd < 0) {
		fprintf(stderr, "mkpasswd: bad descriptor arguments\n");
		return 2;
	}
	if (in_fd != 0 && dup2(in_fd, 0) < 0) {
		fprintf(stderr, "mkpasswd: could not adopt the password descriptor\n");
		return 1;
	}
	if (out_fd != 1 && dup2(out_fd, 1) < 0) {
		fprintf(stderr, "mkpasswd: could not adopt the output descriptor\n");
		return 1;
	}

	/* Read one line, dropping the newline. A NUL cannot appear here: the pipe
	 * carries exactly what the installer wrote, and the installer does not put
	 * one in. */
	while (len < sizeof password - 1) {
		c = getchar();
		if (c == EOF || c == '\n')
			break;
		password[len++] = (char)c;
	}
	password[len] = '\0';

	if (len == 0) {
		fprintf(stderr, "mkpasswd: refusing to hash an empty password\n");
		return 1;
	}

	if (getrandom(rnd, sizeof rnd, 0) != (ssize_t)sizeof rnd) {
		fprintf(stderr, "mkpasswd: no entropy for a salt\n");
		return 1;
	}
	/* Rejection sampling, so every salt character is uniform. Taking the low six
	 * bits of a random byte would be uniform for 64 of the 256 values and would
	 * have to mask -- and a masked value is only uniform if the modulus divides
	 * the range. Discarding the top two bits of each byte costs a quarter of the
	 * entropy per character and is exactly uniform. */
	for (size_t i = 0; i < SALT_LEN; i++) {
		unsigned char b = rnd[i] & 0x3f;
		salt[i] = B64[b];
	}
	salt[SALT_LEN] = '\0';

	/* SHA-512-crypt at the default cost. See ports/mlibc/sysdeps/samsara/crypt.cpp
	 * for why this scheme and not bcrypt or Argon2id: it is the strongest one
	 * reachable through `crypt(3)`, which is the API everything that verifies a
	 * password here calls. */
	snprintf(setting, sizeof setting, "$6$%s", salt);

	char *hash = crypt(password, setting);
	if (!hash || hash[0] != '$') {
		fprintf(stderr, "mkpasswd: crypt(3) refused the setting\n");
		return 1;
	}

	/* The salt is generated here, so a hash that does not contain it means the
	 * two disagree -- and a password that cannot be verified later is exactly
	 * the failure this program exists to prevent. */
	if (strncmp(hash + 3, salt, SALT_LEN) != 0) {
		fprintf(stderr, "mkpasswd: crypt(3) did not use the salt it was given\n");
		return 1;
	}

	puts(hash);
	return 0;
}
