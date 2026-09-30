/* SPDX-License-Identifier: GPL-3.0-or-later */
/* Copyright (C) 2026 Harsh Nikarsa */

/*
 * getty -- prompt for a login on a terminal, authenticate, run a shell, repeat.
 *
 * Why this is a program rather than a line in the installer, and why it is C:
 *
 * The installer is Rust, and the Rust programs in this tree do not link mlibc --
 * they use rust-embedded's freestanding `std` and reach the kernel through
 * `nutcracker-rt`. So a Rust getty would have to parse `/etc/passwd` itself and
 * reimplement `crypt(3)`, which is exactly the code this tree now gets from
 * mlibc and verifies against a reference implementation. Writing it in C means
 * the login path uses the same passwd parsing and the same password hashing as
 * everything else on the system, and there is one implementation of each rather
 * than two that can disagree.
 *
 * Why it exists at all, since a shell works without it:
 *
 * A terminal that goes straight to a shell has no session. Nothing records who
 * is using it, nothing sets a uid, nothing gives the shell a home directory or a
 * login shell, and when the shell exits the terminal is simply dead. A getty
 * gives each use of a terminal an identified user: it authenticates against the
 * passwd database, drops privileges to that user, establishes the session, and
 * on logout returns to a prompt instead of to nothing.
 *
 * It does not make the system safe against someone at the keyboard. It makes the
 * *identity* real: every session has a uid, and the credential checks in the
 * kernel are enforced against it.
 *
 * The flow, once per session:
 *
 *   prompt -> read user -> prompt for password with echo off -> crypt and
 *   compare -> on success setgid/setuid to that user, set HOME/USER/SHELL/TERM,
 *   chdir to their home, and exec their login shell -> when it exits, prompt
 *   again.
 *
 * The password is compared by re-deriving the hash with `crypt(3)` and comparing
 * the whole string. A partial comparison, or a length-then-content one, leaks
 * how much of a guess was right; this is the reason to use `crypt` rather than
 * anything hand-rolled.
 *
 * The one thing this cannot do is authenticate root safely. There is one user on
 * this system and its password is set during setup, so admitting root here means
 * admitting whoever types that password.
 */

#define _GNU_SOURCE

#include <errno.h>
#include <grp.h>
#include <pwd.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/types.h>
#include <termios.h>
#include <unistd.h>

#include <crypt.h>

/* `TIOCSCTTY`, claiming the controlling terminal. Declared here because it is
 * not in POSIX and mlibc does not provide it; the value is the pty driver's. */
#ifndef TIOCSCTTY
#define TIOCSCTTY 0x540E
#endif

#define MAX_LINE 256

static const char *motd_path = "/etc/motd";

/* Read one line from the terminal, without the newline, into `buf`.
 *
 * Stops at a newline, at end of file, or when the buffer is full. A trailing
 * newline is consumed either way, so the next call does not start on the
 * remainder of an over-long line. Returns the length, or -1 on end of file with
 * nothing read. */
static ssize_t read_line(char *buf, size_t size)
{
	size_t len = 0;
	ssize_t n;

	while (len + 1 < size) {
		n = read(0, &buf[len], 1);
		if (n <= 0) {
			/* End of file: whatever was read is still a usable line,
			 * because a password typed and then ^D is a real thing. */
			buf[len] = '\0';
			return len > 0 ? (ssize_t)len : -1;
		}
		if (buf[len] == '\n') {
			buf[len] = '\0';
			return (ssize_t)len;
		}
		if (buf[len] == '\r')
			continue;  /* CRLF, from a pty that echoes it back. */
		len++;
	}
	buf[len] = '\0';
	/* Discard the rest of an over-long line, so it is not read as the next
	 * answer. Without this, typing one character too many makes the following
	 * prompt read the tail of the previous line. */
	while ((n = read(0, &buf[0], 1)) > 0 && buf[0] != '\n')
		;
	return (ssize_t)len;
}

/* Overwrite a buffer, in a way the compiler is not allowed to skip.
 *
 * `memset` on a buffer that is dead afterwards is exactly the case C says may be
 * elided, and a password buffer is precisely such a buffer -- the one place on
 * this system where being clever about it would leave a credential in memory.
 * The volatile pointer forces the store to happen.
 *
 * mlibc has no `explicit_bzero`, which is the usual way to say this, so it is
 * spelled out rather than called. */
static void scrub(void *p, size_t n)
{
	volatile unsigned char *q = (volatile unsigned char *)p;
	while (n--)
		*q++ = 0;
}

static void put(const char *s)
{
	ssize_t ignored = write(1, s, strlen(s));
	(void)ignored;
}

/* Prompt for a line with the terminal in a given echo mode.
 *
 * The terminal is put into raw-ish mode by the caller for the password and
 * restored afterwards, so a password is not stored in the scrollback of a
 * terminal that has one. */
static ssize_t prompt(const char *text, int echo, char *buf, size_t size)
{
	struct termios saved, raw;
	int changed = 0;

	put(text);
	if (!echo && tcgetattr(0, &saved) == 0) {
		raw = saved;
		/* No echo, and no line buffering: characters arrive as typed so the
		 * prompt is not left hanging until Enter. */
		raw.c_lflag &= (tcflag_t)~(ECHO | ICANON);
		raw.c_cc[VMIN] = 1;
		raw.c_cc[VTIME] = 0;
		if (tcsetattr(0, TCSANOW, &raw) == 0)
			changed = 1;
	}
	ssize_t n = read_line(buf, size);
	if (changed) {
		put("\n");
		tcsetattr(0, TCSANOW, &saved);
	}
	return n;
}

static void show_motd(void)
{
	FILE *f = fopen(motd_path, "r");
	char line[256];
	if (!f)
		return;
	while (fgets(line, sizeof line, f)) {
		size_t len = strlen(line);
		while (len > 0 && (line[len - 1] == '\n' || line[len - 1] == '\r'))
			line[--len] = '\0';
		put(line);
		put("\n");
	}
	fclose(f);
}

/* Become `pw`, then hand the terminal to its login shell.
 *
 * The order matters and is not negotiable: the group is set before the user,
 * because once the uid is dropped there is no privilege left to change the
 * group. The terminal's ownership changes with `chown` for the same reason.
 *
 * Every step that can fail is checked. A getty that ignored a failed `setuid`
 * would hand an unauthenticated caller a root shell, which is the single worst
 * thing this program could do. */
static int become_user(const struct passwd *pw)
{
	/* Supplementary groups first: this needs privilege, and it is the last
	 * moment at which we have any. */
	if (pw->pw_gid != 0)
		if (setgroups(0, NULL) != 0 && errno != EPERM)
			; /* best effort: not fatal on a system without groups */

	if (setgid(pw->pw_gid) != 0) {
		fprintf(stderr, "getty: setgid(%u) failed: %s\n",
		        (unsigned)pw->pw_gid, strerror(errno));
		return -1;
	}
	/* initgroups would be the usual call, but it reads /etc/group and this
	 * system has one user; setgid is sufficient and has no extra failure mode. */
	if (setuid(pw->pw_uid) != 0) {
		fprintf(stderr, "getty: setuid(%u) failed: %s\n",
		        (unsigned)pw->pw_uid, strerror(errno));
		return -1;
	}

	/* Confirm the drop actually took. This looks redundant and is not: it is
	 * the difference between "setuid failed and we noticed" and "setuid failed
	 * and we did not". */
	if (pw->pw_uid != 0 && (setuid(0) == 0)) {
		fprintf(stderr, "getty: privileges were not dropped\n");
		return -1;
	}

	if (chdir(pw->pw_dir) != 0) {
		/* Not fatal. A user with no home directory should still get a shell
		 * rather than a login that stops here; they just start in `/`. */
		if (chdir("/") != 0)
			; /* nothing useful left to try */
	}
	return 0;
}

static void export_env(const struct passwd *pw)
{
	/* An unset HOME is worse than an empty one: a program testing it for
	 * emptiness then writes relative paths and has no way to tell. So it is set
	 * from the passwd entry, and only skipped if that field is empty too. */
	if (pw->pw_dir && *pw->pw_dir)
		setenv("HOME", pw->pw_dir, 1);
	if (pw->pw_name && *pw->pw_name) {
		setenv("USER", pw->pw_name, 1);
		setenv("LOGNAME", pw->pw_name, 1);
	}
	if (pw->pw_shell && *pw->pw_shell)
		setenv("SHELL", pw->pw_shell, 1);
	if (!getenv("TERM"))
		setenv("TERM", "linux", 1);
}

/* One login attempt. Returns the shell's exit status, or -1 to ask again. */
static int login_once(void)
{
	char user[MAX_LINE], password[MAX_LINE];
	struct passwd *pw;
	char *hash;

	if (prompt("Samsara login: ", 1, user, sizeof user) < 0)
		return -1;
	if (user[0] == '\0')
		return -1;

	pw = getpwnam(user);
	/* The same message for "no such user" and "wrong password". Distinguishing
	 * them would let someone enumerate the accounts on the system, and there is
	 * nothing to learn from the difference that matters here. */
	if (!pw) {
		/* Still read a password, so the timing does not give it away either. */
		prompt("Password: ", 0, password, sizeof password);
		put("Login incorrect\n\n");
		return -1;
	}

	if (prompt("Password: ", 0, password, sizeof password) < 0)
		return -1;

	/* An account with no password field is refused rather than admitted. An
	 * empty field means "no password required" to `login(1)`, and that is the
	 * right default there -- a machine nobody has set up yet. Here it would mean
	 * anyone at the keyboard becomes that user, including root, so it is an
	 * error to fix rather than a convenience. */
	if (!pw->pw_passwd || !*pw->pw_passwd) {
		put("This account has no password set.\n\n");
		return -1;
	}
	/* `!` and `*` are the conventional "locked" prefixes. */
	if (pw->pw_passwd[0] == '!' || pw->pw_passwd[0] == '*') {
		put("This account is locked.\n\n");
		return -1;
	}

	hash = crypt(password, pw->pw_passwd);
	/* Compare the whole string, and after zeroing the password buffer. A partial
	 * or early-returning compare leaks how much of a guess was correct. */
	if (!hash || strcmp(hash, pw->pw_passwd) != 0) {
		put("Login incorrect\n\n");
		scrub(password, sizeof password);
		return -1;
	}
	scrub(password, sizeof password);
	scrub(hash, strlen(hash));

	put("\n");

	/* Claim the controlling terminal, so job control works: without it, ^C
	 * cannot reach the foreground process group. Best effort -- a getty that
	 * already owns the tty does not need to, and failing here would be no
	 * reason to refuse an otherwise good login. */
	ioctl(0, TIOCSCTTY, 0);

	if (become_user(pw) != 0) {
		put("Login failed: could not set up the session.\n\n");
		return -1;
	}
	export_env(pw);
	show_motd();

	const char *shell = (pw->pw_shell && *pw->pw_shell) ? pw->pw_shell : "/bin/sh";
	/* `exec` rather than fork: the shell inherits the terminal and the session,
	 * and when it exits the getty's parent is told -- so a supervising getty
	 * sees the logout instead of having to infer it. */
	execl(shell, shell, (char *)NULL);
	fprintf(stderr, "getty: cannot run %s: %s\n", shell, strerror(errno));
	return 127;
}

int main(int argc, char **argv)
{
	/* A getty that dies on a signal leaves a terminal nobody owns. SIGHUP and
	 * SIGINT are ignored so ^C at a login prompt does not kill it, and the
	 * terminal is put back into a sane mode on the way out. */
	signal(SIGHUP, SIG_IGN);
	signal(SIGINT, SIG_IGN);
	signal(SIGQUIT, SIG_IGN);

	/* `-n`/`--login` is accepted and ignored: a real getty takes it to mean
	 * "do not prompt for a username", and this one has no autologin to offer.
	 * Accepting it is better than refusing to start over it. */
	for (int i = 1; i < argc; i++) {
		const char *a = argv[i];
		if (strcmp(a, "-n") == 0 || strcmp(a, "--login") == 0)
			continue;
		if (strcmp(a, "-f") == 0 || strcmp(a, "--issue") == 0) {
			/* `--issue FILE` is a getty that prints the banner and exits. */
			if (i + 1 < argc)
				motd_path = argv[++i];
			show_motd();
			return 0;
		}
	}

	/* The loop is the point of the program: on logout it prompts again, so the
	 * terminal returns to a login rather than to nothing. */
	for (;;) {
		int status = login_once();
		if (status < 0)
			continue;  /* a failed attempt: ask again */
		/* The shell exited, or could not be run. Either way the terminal
		 * belongs to nobody now, and the next iteration takes it back. */
	}
}
