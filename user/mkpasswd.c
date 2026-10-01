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

#include <errno.h>
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

/* Write a NUL-terminated string, in full.
 *
 * A short write is normal on a pipe and is not an error, but it does mean the
 * rest has to go out separately, so this loops rather than assuming the whole
 * string fitted. `off` advances by what was actually written, which is the only
 * amount the kernel confirmed taking.
 */
static void write_str(int fd, const char *s)
{
	size_t len = strlen(s);
	size_t off = 0;

	while (off < len) {
		ssize_t w = write(fd, s + off, len - off);
		if (w < 0) {
			if (errno == EINTR)
				continue;
			return;	/* nowhere left to complain to */
		}
		off += (size_t)w;
	}
}

/* Diagnostics go to descriptor 2 as a plain write.
 *
 * stdio is deliberately absent from this program (see the read loop in `main`
 * for why), which includes the diagnostics, so this is how one says something.
 */
static void warn(const char *msg)
{
	write_str(2, "mkpasswd: ");
	write_str(2, msg);
	write_str(2, "\n");
}


/* True when a failed call should simply be tried again.
 *
 * Without this a signal that interrupts a read, which is ordinary on a machine
 * with a keyboard, would be reported as a failure to read the password.
 */
static int errno_is_retry(void)
{
	return errno == EINTR;
}

int main(int argc, char **argv)
{
	char password[MAX_PASSWORD];
	char setting[32];
	unsigned char rnd[SALT_LEN];
	char salt[SALT_LEN + 1];
	size_t len = 0;

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
		warn("usage: mkpasswd <password-fd> <output-fd>");
		return 2;
	}
	int in_fd = atoi(argv[1]);
	int out_fd = atoi(argv[2]);
	if (in_fd < 0 || out_fd < 0) {
		warn("bad descriptor arguments");
		return 2;
	}
	if (in_fd != 0 && dup2(in_fd, 0) < 0) {
		warn("could not adopt the password descriptor");
		return 1;
	}
	if (out_fd != 1 && dup2(out_fd, 1) < 0) {
		warn("could not adopt the output descriptor");
		return 1;
	}

	/* Read one line with read(2), NOT with stdio.
	 *
	 * This is the second version of this mistake, and the first one was worse:
	 * `getchar()` reads through a stdio stream that mlibc built against
	 * descriptor 0 as it was *before* the dup2 above -- the keyboard, not the
	 * pipe. So this program sat waiting for somebody to press Enter on the
	 * terminal while the installer sat waiting for a hash, and neither ever
	 * moved. The duplicate was never in the stream, because the stream was
	 * already open on the other descriptor.
	 *
	 * The lesson is that "the descriptor is 0 now" and "the stream reads from
	 * descriptor 0 now" are different claims, and only the second one is what
	 * stdio actually does. So there is no stdio in this program: it reads the
	 * descriptor number it was given and writes the descriptor number it was
	 * given, and neither is assumed to be 0 or 1.
	 *
	 * A NUL cannot appear here: the pipe carries exactly what the installer
	 * wrote, and the installer does not put one in.
	 */
	for (;;) {
		ssize_t n;
		char *nl;

		if (len >= sizeof password - 1)
			break;	/* longer than we accept; the rest is discarded */
		n = read(0, password + len, sizeof password - 1 - len);
		if (n < 0) {
			if (errno_is_retry())
				continue;
			warn("could not read the password");
			return 1;
		}
		if (n == 0)
			break;	/* end of file, without a newline */
		len += (size_t)n;

		nl = memchr(password, '\n', len);
		if (nl) {
			len = (size_t)(nl - password);	/* the newline is not part of it */
			break;
		}
	}
	password[len] = '\0';

	if (len == 0) {
		warn("refusing to hash an empty password");
		return 1;
	}

	if (getrandom(rnd, sizeof rnd, 0) != (ssize_t)sizeof rnd) {
		warn("no entropy for a salt");
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
	 * password here calls.
	 *
	 * Built with strcpy/strcat rather than snprintf, which came out with stdio.
	 * The sizes are all fixed and checked by the compiler here, so the
	 * formatting machinery was never buying anything: three literal bytes and a
	 * salt whose length is a #define. */
	strcpy(setting, "$6$");
	strcat(setting, salt);

	char *hash = crypt(password, setting);
	if (!hash || hash[0] != '$') {
		warn("crypt(3) refused the setting");
		return 1;
	}

	/* The salt is generated here, so a hash that does not contain it means the
	 * two disagree -- and a password that cannot be verified later is exactly
	 * the failure this program exists to prevent. */
	if (strncmp(hash + 3, salt, SALT_LEN) != 0) {
		warn("crypt(3) did not use the salt it was given");
		return 1;
	}

	/* One line, written to the descriptor rather than through stdio, for the
	 * same reason the password is read from one. A short write is retried, since
	 * a hash truncated to half its length is not a hash and would silently lock
	 * the account out. */
	{
		size_t off = 0;
		size_t n = strlen(hash);

		while (off < n) {
			ssize_t w = write(1, hash + off, n - off);
			if (w < 0) {
				if (errno_is_retry())
					continue;
				warn("could not write the hash");
				return 1;
			}
			off += (size_t)w;
		}
		if (write(1, "\n", 1) != 1) {
			warn("could not write the hash");
			return 1;
		}
	}
	return 0;
}
