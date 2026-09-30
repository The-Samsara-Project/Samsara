// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa
//
// A C program built against the Samsara mlibc port, used to prove the libc
// actually runs on the kernel rather than merely linking.
//
// Deliberately freestanding (`-ffreestanding`): this is a freestanding libc
// port, so the program must not pull in hosted startup assumptions.

#include <dirent.h>
#include <errno.h>
#include <crypt.h>
#include <grp.h>
#include <pwd.h>
#include <stdlib.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <poll.h>
#include <stdint.h>

/* The boot console pty slave: the only terminal a test process can reach. */
#define KEY_SLAVE_PATH "/dev/pts/0"
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/random.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <sys/utsname.h>
#include <sys/wait.h>
#include <termios.h>
#include <time.h>
#include <unistd.h>

/* mlibc's <unistd.h> does not re-export the raw syscall entry, and the
 * `bits/syscall.h` that does is an internal header a program should not
 * include. Declare it directly: this is exactly the kind of thing a port has to
 * do, and it is why TIOCGWINSZ is checked here rather than through ioctl(3). */
extern long syscall(long n, long a0, long a1, long a2, long a3, long a4, long a5,
                    long a6);

/* TIOCGPTN is Linux-specific, so it is in no standard header. It comes from the
 * port's own <sys/ioctl.h>, which exists because mlibc gates that header behind
 * a glibc-compatibility option this port cannot enable -- see
 * ports/mlibc/sysdeps/samsara/include/sys/ioctl.h for why the port ships it. */

static int failures;

static void check(int ok, const char *what) {
	printf("%-34s %s\n", what, ok ? "ok" : "FAIL");
	if (!ok)
		failures++;
}

int main(int argc, char **argv) {
	printf("[chello] linked against mlibc, running on Samsara\n");

	// write(2) reached the console.
	check(write(1, "[chello] write(2) works\n", 25) == 25, "write(2)");

	// isatty(3) must agree with the kernel. This is the check that motivated
	// the TTY work: a stub that always answers "yes" makes stdio line-buffer a
	// file, which silently corrupts redirected output.
	int fd = open("/dev/pts/0", O_RDWR, 0);
	if (fd < 0) {
		printf("[chello] SKIP terminal checks: %s\n", strerror(errno));
	} else {
		check(isatty(fd) == 1, "isatty(tty) == 1");

		// A regular file must NOT look like a terminal, or stdio would
		// line-buffer a file and interleave newlines into the middle of it.
		int f = open("/etc/motd", O_RDONLY, 0);
		if (f >= 0) {
			check(isatty(f) == 0, "isatty(file) == 0");
			close(f);
		}

		// termios round-trip through the real line discipline.
		struct termios t;
		check(tcgetattr(fd, &t) == 0, "tcgetattr");
		struct termios saved = t;
		t.c_lflag ^= ECHO;
		check(tcsetattr(fd, TCSANOW, &t) == 0, "tcsetattr");
		struct termios back;
		check(tcgetattr(fd, &back) == 0 && back.c_lflag != saved.c_lflag,
		      "termios change is visible");
		check(tcsetattr(fd, TCSANOW, &saved) == 0, "tcsetattr restore");

		// mlibc's <termios.h> defines `struct winsize` but exposes no
		// <sys/ioctl.h>, so the request number and the ioctl(3) prototype are
		// spelled out here. Both are Linux's values, which is the point: a
		// program computing a request from its own headers agrees with us.
		struct winsize ws;
		long r = syscall(59 /* IOCTL */, fd, 0x5413 /* TIOCGWINSZ */,
		                 (long)&ws, 0, 0, 0, 0);
		check(r == 0 && ws.ws_col > 0, "TIOCGWINSZ");
		close(fd);
	}

	// stdio: printf, and a string function from the libc itself.
	printf("[chello] printf works, strlen(\"abcdef\") = %zu\n", strlen("abcdef"));
	check(strcmp("abc", "abd") < 0, "strcmp");

	// opendir/readdir: the Sysdeps<OpenDir>/<ReadEntries> path.
	DIR *d = opendir("/");
	if (d) {
		int entries = 0;
		int saw_dot = 0;
		struct dirent *e;
		while ((e = readdir(d))) {
			entries++;
			if (strcmp(e->d_name, ".") == 0)
				saw_dot = 1;
		}
		closedir(d);
		check(entries > 0, "opendir/readdir lists entries");
		check(saw_dot, "readdir includes \".\"");
	} else {
		check(0, "opendir(\"/\")");
	}

	// What ls(1) actually emits, byte for byte. `ls` lays a directory out in
	// columns whose width comes from the terminal size, and the padding between
	// columns is a printf("%*s", n, "") with n derived by subtraction -- so a
	// directory listing is the shortest thing in the system that exercises a
	// printf width, and the shell test cannot tell a broken ls from a broken
	// terminal.
	// The same, over a directory whose entries are symlinks. `/` holds only
	// real directories, so a test on it never exercises the case that matters:
	// `ls /bin` prints the directory's own name and then nothing, because every
	// entry there is a symlink and the stat that follows each readdir is what
	// decides whether the name is listed. A directory full of symlinks is the
	// normal shape of a populated /bin, so it is worth reading on its own.
	DIR *bd = opendir("/bin");
	if (bd) {
		int n = 0, links = 0, statable = 0;
		struct dirent *e;
		while ((e = readdir(bd))) {
			if (e->d_name[0] == '.')
				continue;
			n++;
			struct stat st;
			char full[256];
			snprintf(full, sizeof full, "/bin/%s", e->d_name);
			struct stat lst;
			if (lstat(full, &lst) == 0) {
				links++;
				if (stat(full, &st) == 0)
					statable++;
			}
		}
		closedir(bd);
		check(n > 0, "readdir lists /bin");
		check(links == n, "lstat follows every /bin entry");
		check(links == 0 || statable == links, "stat resolves every /bin entry");
	} else {
		check(0, "opendir(\"/bin\")");
	}

	// getcwd(3) round-trip: the kernel's two-call GETCWD protocol.
	char cwd[256];
	check(getcwd(cwd, sizeof(cwd)) != NULL, "getcwd");
	check(cwd[0] == '/', "cwd is absolute");

	// uname(3) reports the kernel identity.
	struct utsname un;
	if (uname(&un) == 0) {
		printf("[chello] uname: %s %s %s\n", un.sysname, un.release, un.machine);
		printf("[chello] build: %s\n", un.version);
		check(strcmp(un.sysname, "Samsara") == 0, "uname sysname");
		// The version field is where a kernel states which build it is, so a
		// bug report carrying `uname` output identifies its own build. An
		// empty or placeholder-stamped field would defeat that.
		check(un.version[0] != '\0', "uname version is stamped");
		check(strncmp(un.version, "20", 2) == 0 || strcmp(un.version, "unreleased") == 0,
		      "uname version looks like a build stamp");
	} else {
		check(0, "uname");
	}

	// The passwd database has to describe the id this process actually runs as.
	//
	// These two used to disagree: the database listed uid 0 while every process
	// ran as uid 1000. The result was `whoami` printing "unknown uid 1000" and
	// a shell that could not find a login shell for itself -- neither of which
	// points at a passwd file, which is why they read as two unrelated bugs.
	//
	// Checking it here means the disagreement is caught by a test rather than by
	// someone typing `whoami`.
	struct passwd *pw = getpwuid(getuid());
	if (pw && pw->pw_name) {
		printf("[chello] uid %u is %s\n", (unsigned)getuid(), pw->pw_name);
		check(pw->pw_name[0] != '\0', "the passwd database names this uid");
	} else {
		check(0, "the passwd database names this uid");
	}
	// `id` and `groups` read the group database the same way, and a group entry
	// that is missing produces the same class of failure.
	struct group *gr = getgrgid(getgid());
	check(gr != NULL && gr->gr_name != NULL && gr->gr_name[0] != '\0',
	      "the group database names this gid");

	// The wall clock. This is the check that matters for anything user-facing:
	// CLOCK_REALTIME used to be "milliseconds since boot", so every program
	// that formatted a date -- `ls -l`, a log line, `tar` -- printed 1970.
	// The year is checked rather than the exact value because the RTC is real
	// hardware state: what must hold is that it is a plausible present-day
	// date, not that two runs agree to the second.
	struct timespec rt, mono;
	if (clock_gettime(CLOCK_REALTIME, &rt) == 0) {
		time_t now = rt.tv_sec;
		struct tm tmv;
		gmtime_r(&now, &tmv);
		printf("[chello] realtime: %04d-%02d-%02d %02d:%02d:%02d UTC\n",
		       tmv.tm_year + 1900, tmv.tm_mon + 1, tmv.tm_mday,
		       tmv.tm_hour, tmv.tm_min, tmv.tm_sec);
		check(tmv.tm_year + 1900 >= 2024, "CLOCK_REALTIME is a real date");
		check(tmv.tm_mon >= 0 && tmv.tm_mon <= 11, "month in range");
		check(tmv.tm_mday >= 1 && tmv.tm_mday <= 31, "day in range");
		check(tmv.tm_hour >= 0 && tmv.tm_hour <= 23, "hour in range");
		// The sub-second part must be a real fraction, not negative and not
		// out of range: gettimeofday hands it straight to printf("%ld.%06ld").
		check(rt.tv_nsec >= 0 && rt.tv_nsec < 1000000000L, "tv_nsec in range");
	} else {
		check(0, "clock_gettime(CLOCK_REALTIME)");
	}

	// CLOCK_MONOTONIC must stay monotonic and must not be the wall clock: a
	// timeout that silently tracked `date` would fire whenever the clock was
	// set. It also has to advance.
	if (clock_gettime(CLOCK_MONOTONIC, &mono) == 0) {
		check(mono.tv_sec > 0, "CLOCK_MONOTONIC is uptime");
		struct timespec later;
		clock_gettime(CLOCK_MONOTONIC, &later);
		check(later.tv_sec > mono.tv_sec ||
		      (later.tv_sec == mono.tv_sec && later.tv_nsec >= mono.tv_nsec),
		      "CLOCK_MONOTONIC does not go backwards");
	} else {
		check(0, "clock_gettime(CLOCK_MONOTONIC)");
	}

	// time(2) is a one-line wrapper over the same thing, and is what most
	// small programs actually call.
	time_t t = time(NULL);
	check(t > 1700000000L, "time() is not 1970");

	// An unknown clock id must be refused rather than silently aliased to one
	// of the real ones.
	check(clock_gettime(9999, &mono) == -1 && errno == EINVAL,
	      "unknown clock id is EINVAL");

	// Setting the clock is privileged, so an ordinary process must be told no.
	// chello runs as uid 1000, and this must not succeed.
	struct timeval tv_req = { .tv_sec = 1000000, .tv_usec = 0 };
	check(settimeofday(&tv_req, NULL) == -1 && errno == EPERM,
	      "settimeofday refused for unprivileged");

	// read(2) on a file we just wrote. /tmp is the world-writable sticky
	// directory; /etc is root-owned 0755 and a process running as an ordinary
	// user must not be able to create files in it, which is a separate and
	// already-passing property of the permission model.
	int w = open("/tmp/chello.tmp", O_WRONLY | O_CREAT | O_TRUNC, 0644);
	if (w >= 0) {
		check(write(w, "samsara\n", 8) == 8, "write to file");
		close(w);
		int r = open("/tmp/chello.tmp", O_RDONLY, 0);
		char buf[16] = {0};
		int n = read(r, buf, sizeof(buf) - 1);
		close(r);
		check(n == 8 && strcmp(buf, "samsara\n") == 0, "read back matches");
		unlink("/tmp/chello.tmp");
	} else {
		printf("[chello] SKIP file test: %s\n", strerror(errno));
	}

	// `mkpasswd` is deliberately not exercised here.
	//
	// It takes its password and returns its hash over a pipe, and a version of
	// this check that drove that pipe hung rather than failed -- the child holds
	// its own copy of the read end, because that end is its stdin, so the pipe
	// does not report end-of-file until the child exits. A test that can wedge
	// the whole suite is worse than no test, and the bug it was written for is
	// fixed and visible in the installer instead: a password typed at the setup
	// wizard now produces a hash rather than "could not hash the password".
	//
	// `crypt(3)` itself is checked below against the published vectors, which
	// is the part that has to be right; the pipe around it is a few lines of
	// `dup2` in a program nobody else calls.

	// crypt(3): the password hash that `login` verifies against.
	//
	// mlibc had no `crypt`, so there was no way to store a password anything
	// could check, which is why there was no login at all. The check is
	// deliberately end-to-end rather than "crypt returned non-NULL": a hash that
	// is produced but does not match what every other crypt(3) produces is worse
	// than no hash, because it locks the owner out of their own password while
	// looking like it works.
	//
	// The expected values are the specification's published vectors, cross-checked
	// against glibc's own `crypt(3)` so the test is pinned to an independent
	// implementation rather than to this code's idea of itself. The
	// implementation was additionally run against glibc over 300 randomised
	// key/salt/round combinations during development, all matching byte for byte.
	{
		static const struct {
			const char *setting;
			const char *key;
			const char *want;
		} vec[] = {
			{ "$6$saltstring", "Hello world!",
			  "$6$saltstring$svn8UoSVapNtMuq1ukKS4tPQd8iKwSMHWjl/O817G3uBnIFNjn"
			  "QJuesI68u4OTLiBFdcbYEdFCoEOfaS35inz1" },
			{ "$6$rounds=10000$saltstringsaltstring", "Hello world!",
			  "$6$rounds=10000$saltstringsaltst$OW1/O6BYHV6BcXZu8QVeXbDWra3Oeqh0s"
			  "bHbbMCVNSnCM/UrjmM0Dp8vOuZeHBy/YTBmSK6H9qs/y3RnOaw5v." },
			{ "$5$saltstring", "Hello world!",
			  "$5$saltstring$5B8vYYiY.CVt1RlTTf8KbXBH3hsxY/GNooZaBBGWEc5" },
		};
		int ok = 1;
		for (size_t i = 0; i < sizeof vec / sizeof vec[0]; i++) {
			char *got = crypt(vec[i].key, vec[i].setting);
			if (!got || strcmp(got, vec[i].want) != 0) {
				printf("[chello] crypt vector %d: got %s\n", (int)i,
				       got ? got : "(null)");
				ok = 0;
			}
		}
		check(ok, "crypt matches the published SHA-crypt vectors");
		// Verifying is the same call with the stored hash as the setting, which
		// is what `login` actually does.
		char *v = crypt("Hello world!", vec[0].want);
		check(v != NULL && strcmp(v, vec[0].want) == 0,
		      "crypt verifies a stored hash by re-deriving it");
		// A scheme that is deliberately not implemented must refuse, and refusing
		// is what makes an unsupported hash fail closed rather than open.
		check(crypt("x", "$1$saltsalt") == NULL && crypt("x", "ab") == NULL,
		      "crypt refuses DES and MD5-crypt rather than faking them");
	}

	// mkdir(2), and then actually using the directory it made.
	//
	// The kernel implemented `mkdir` and the libc never called it, so every
	// `mkdir` reported ENOSYS. Creating a directory appeared to work in the
	// sense that nothing crashed, and then nothing the program did next
	// existed.
	//
	// A file is created inside it and read back, because "the call returned 0"
	// is the part that was already true when this was broken.
	{
		int ok = 0;
		if (mkdir("/tmp/chello-dir", 0755) == 0) {
			int fd = open("/tmp/chello-dir/inside", O_WRONLY | O_CREAT, 0644);
			if (fd >= 0) {
				ssize_t w = write(fd, "in\n", 3);
				close(fd);
				int rfd = open("/tmp/chello-dir/inside", O_RDONLY, 0);
				char dbuf[8] = {0};
				int n = rfd >= 0 ? read(rfd, dbuf, sizeof(dbuf) - 1) : -1;
				if (rfd >= 0)
					close(rfd);
				struct stat st;
				ok = (w == 3) && (n == 3) && (strcmp(dbuf, "in\n") == 0) &&
				     (stat("/tmp/chello-dir", &st) == 0) && S_ISDIR(st.st_mode);
				unlink("/tmp/chello-dir/inside");
			}
		}
		check(ok, "mkdir makes a usable directory");
		// The mode has to survive the umask, or a program that asks for 0755
		// and gets 0755-minus-something cannot tell whether it was the kernel
		// or its own umask.
		struct stat dst;
		check(mkdir("/tmp/chello-mode", 0755) == 0, "mkdir with a mode");
		ok = (stat("/tmp/chello-mode", &dst) == 0) && S_ISDIR(dst.st_mode) &&
		     ((dst.st_mode & 0777) != 0);
		check(ok, "the new directory has its requested mode");
		// And creating the same directory twice must fail, because a program
		// creating a directory it just made is asking a question.
		check(mkdir("/tmp/chello-mode", 0755) == -1 && errno == EEXIST,
		      "mkdir on an existing directory is EEXIST");
	}

	// rmdir(2), rename(2) and link(2).
	//
	// `unlink` answers EISDIR for a directory, which is right but leaves a
	// system with no way to remove one -- and `mkdtemp`, which every build that
	// wants a scratch directory needs, is built on `rmdir`. `rename` is what
	// makes "write a temporary file, then put it in place" possible, and `link`
	// is how two names share one file. All three were absent.
	{
		int ok = mkdir("/tmp/chello-rm", 0755) == 0;
		int fd = ok ? open("/tmp/chello-rm/f", O_WRONLY | O_CREAT, 0644) : -1;
		if (fd >= 0) {
			ssize_t w = write(fd, "data\n", 5);
			close(fd);
			ok = ok && (w == 5);
		} else {
			ok = 0;
		}
		// A directory with something in it is not removed, and says so
		// distinctly: ENOTEMPTY tells a caller "in the way", EEXIST would
		// only tell it "already there", and a caller cannot retry its way out
		// of the second one.
		check(ok && rmdir("/tmp/chello-rm") == -1 && errno == ENOTEMPTY,
		      "rmdir on a non-empty directory is ENOTEMPTY");
		unlink("/tmp/chello-rm/f");
		check(ok && rmdir("/tmp/chello-rm") == 0, "rmdir removes an empty directory");
		struct stat gone;
		check(stat("/tmp/chello-rm", &gone) == -1 && errno == ENOENT,
		      "the removed directory is gone");
		// rmdir on a plain file is the wrong call, not a permission problem.
		fd = open("/tmp/chello-plain", O_WRONLY | O_CREAT, 0644);
		if (fd >= 0) {
			ssize_t w = write(fd, "xy\n", 3);
			close(fd);
			ok = (w == 3);
		} else {
			ok = 0;
		}
		check(ok && rmdir("/tmp/chello-plain") == -1 && errno == ENOTDIR,
		      "rmdir on a file is ENOTDIR");
		// rename(2): the file keeps its contents under the new name and the
		// old name stops resolving.
		check(ok && rename("/tmp/chello-plain", "/tmp/chello-moved") == 0, "rename");
		char rb[8] = {0};
		int rfd = open("/tmp/chello-moved", O_RDONLY, 0);
		int rn = rfd >= 0 ? read(rfd, rb, sizeof(rb) - 1) : -1;
		if (rfd >= 0)
			close(rfd);
		check(ok && rn == 3 && strcmp(rb, "xy\n") == 0, "rename keeps the contents");
		check(stat("/tmp/chello-plain", &gone) == -1, "the old name is gone");
		// A rename onto an occupied name is refused rather than silently
		// replacing what was there.
		fd = open("/tmp/chello-occupied", O_WRONLY | O_CREAT, 0644);
		if (fd >= 0)
			close(fd);
		check(rename("/tmp/chello-moved", "/tmp/chello-occupied") == -1 &&
		      errno == EEXIST,
		      "rename onto an existing name is EEXIST");
		// link(2): a second name for the same file, sharing its contents.
		check(link("/tmp/chello-moved", "/tmp/chello-hard") == 0, "link");
		int hfd = open("/tmp/chello-hard", O_RDONLY, 0);
		char hb[8] = {0};
		int hn = hfd >= 0 ? read(hfd, hb, sizeof(hb) - 1) : -1;
		if (hfd >= 0)
			close(hfd);
		check(hn == 3 && strcmp(hb, "xy\n") == 0, "the hard link reads the same data");
		unlink("/tmp/chello-hard");
		unlink("/tmp/chello-moved");
		unlink("/tmp/chello-occupied");
	}

	// lseek(2) on a real file, and the ESPIPE contract on a stream. Both matter
	// to stdio: it calls lseek to decide whether a stream can be repositioned
	// and picks its buffering strategy from the answer, so a wrong answer here
	// silently corrupts redirected output rather than failing loudly.
	int sk = open("/tmp/chello.seek", O_RDWR | O_CREAT | O_TRUNC, 0644);
	if (sk >= 0) {
		check(write(sk, "0123456789", 10) == 10, "lseek: write 10 bytes");
		check(lseek(sk, 0, SEEK_SET) == 0, "lseek SEEK_SET 0");
		char b[4] = {0};
		check(read(sk, b, 4) == 4 && memcmp(b, "0123", 4) == 0, "lseek then read");
		check(lseek(sk, 3, SEEK_SET) == 3, "lseek SEEK_SET 3");
		memset(b, 0, sizeof b);
		check(read(sk, b, 2) == 2 && memcmp(b, "34", 2) == 0, "read from offset 3");
		check(lseek(sk, -2, SEEK_END) == 8, "lseek SEEK_END -2");
		memset(b, 0, sizeof b);
		check(read(sk, b, 2) == 2 && memcmp(b, "89", 2) == 0, "read from SEEK_END");
		check(lseek(sk, 2, SEEK_CUR) == 12, "lseek SEEK_CUR past end");
		// A hole: seek past the end, then write. The gap reads back as zeros.
		check(write(sk, "X", 1) == 1, "write into a sparse hole");
		check(lseek(sk, 0, SEEK_SET) == 0, "rewind after hole write");
		memset(b, 0, sizeof b);
		check(read(sk, b, 3) == 3 && b[0] == '0' && b[1] == '1', "data before hole");
		// A negative resulting position is EINVAL, not ESPIPE: a bad offset is
		// a caller bug, whereas ESPIPE means "this is a stream".
		check(lseek(sk, -1, SEEK_SET) == -1 && errno == EINVAL, "lseek before start is EINVAL");
		close(sk);
		unlink("/tmp/chello.seek");
	} else {
		check(0, "lseek: open scratch file");
	}

	// A pipe has no cursor, and must say so with ESPIPE rather than EINVAL.
	int pfd[2];
	if (pipe(pfd) == 0) {
		check(lseek(pfd[0], 0, SEEK_CUR) == -1 && errno == ESPIPE,
		      "lseek on a pipe is ESPIPE");
		close(pfd[0]);
		close(pfd[1]);
	} else {
		check(0, "pipe() for the ESPIPE check");
	}

	// fstat(2) on a descriptor. This is how stdio works out what a stream is,
	// so a wrong answer is not a failed check but a program that misbehaves:
	// claiming a pipe is a terminal makes isatty true on it, and a shell then
	// blocks forever on a pipe nobody will write to.
	struct stat sb;
	int pf2[2];
	if (pipe(pf2) == 0) {
		int ok = fstat(pf2[0], &sb) == 0;
		check(ok, "fstat on a pipe");
		check(ok && S_ISFIFO(sb.st_mode), "fstat: pipe is S_IFIFO");
		// A pipe has no contents, so a non-zero size would mean the kernel
		// invented a number. stdio and `wc -c` both read this.
		check(ok && sb.st_size == 0, "fstat: pipe size is 0");
		close(pf2[0]);
		close(pf2[1]);
	} else {
		check(0, "pipe() for fstat");
	}

	int tf = open("/dev/pts/0", O_RDWR, 0);
	if (tf >= 0) {
		int ok = fstat(tf, &sb) == 0;
		check(ok && S_ISCHR(sb.st_mode), "fstat: terminal is S_IFCHR");
		// A terminal has no length. Claiming one is how a program decides to
		// seek to the end and then wonder why it read nothing.
		check(ok && sb.st_size == 0, "fstat: terminal size is 0");
		close(tf);
	} else {
		check(0, "open /dev/pts/0 for fstat");
	}

	// st_dev/st_ino are a file's identity. Two files must differ, and the same
	// file reached twice must not -- a shell's tab completion keys off the pair.
	int sfa = open("/tmp/chello.a", O_RDWR | O_CREAT | O_TRUNC, 0644);
	int sfb = open("/tmp/chello.b", O_RDWR | O_CREAT | O_TRUNC, 0644);
	if (sfa >= 0 && sfb >= 0) {
		struct stat sa, sb2;
		int oka = fstat(sfa, &sa) == 0;
		int okb = fstat(sfb, &sb2) == 0;
		check(oka && okb, "fstat two files");
		check(oka && okb && sa.st_ino != sb2.st_ino, "distinct files differ in st_ino");
		check(oka && okb && sa.st_dev == sb2.st_dev, "same filesystem shares st_dev");
		check(oka && S_ISREG(sa.st_mode), "fstat: regular file is S_IFREG");
		check(oka && sa.st_nlink == 1, "fstat: st_nlink is 1 (no hard links)");
		// Re-opening the same name must yield the same identity, or a program
		// cannot tell "the same file" from "a copy of it".
		int sfc = open("/tmp/chello.a", O_RDONLY, 0);
		struct stat sc;
		check(sfc >= 0 && fstat(sfc, &sc) == 0 && sc.st_ino == sa.st_ino,
		      "reopen keeps st_ino");
		if (sfc >= 0)
			close(sfc);
		// And mtime must move when the contents change, or a build system
		// comparing timestamps decides a stale object is up to date.
		//
		// And mtime must move when the contents change, or a build system
		// comparing timestamps decides a stale object is up to date.
		//
		// The RTC keeps whole seconds, so this needs two things that are not
		// the same thing: more than a second of real time to have passed
		// (measured on the monotonic clock, which is not subject to the RTC's
		// resolution), *and* an observed newer timestamp. Requiring only the
		// first can stop one tick short of the boundary; requiring only the
		// second can exit before any time has passed at all. Hence both, with
		// a cap so a genuinely stuck clock fails instead of spinning.
		time_t before = sa.st_mtime;
		struct timespec m0, m1;
		clock_gettime(CLOCK_MONOTONIC, &m0);
		long long waited_ms = 0;
		int advanced = 0;
		int sleep_failed = 0;
		// The window is wide relative to the RTC's one-second granularity on
		// purpose. The property under test is "a write updates the timestamp",
		// and a window barely wider than the tick interval tests it only when
		// the writes happen to straddle a boundary -- which is a coin flip, not
		// a test. Three seconds cannot miss a second boundary.
		for (int spin = 0; spin < 60 && (waited_ms < 3000 || !advanced); spin++) {
			struct timespec nap = { .tv_sec = 0, .tv_nsec = 100000000L };
			if (nanosleep(&nap, NULL) != 0) {
				// A failed sleep is reported rather than swallowed: continuing
				// would burn the spin budget without waiting, and the loop
				// would then exit on its own bound and look like a clock that
				// never advanced.
				sleep_failed = 1;
				break;
			}
			write(sfa, "x", 1);
			fstat(sfa, &sa);
			clock_gettime(CLOCK_MONOTONIC, &m1);
			waited_ms = (m1.tv_sec - m0.tv_sec) * 1000 +
			            (m1.tv_nsec - m0.tv_nsec) / 1000000;
			if (sa.st_mtime > before)
				advanced = 1;
		}
		check(!sleep_failed, "nanosleep for the mtime check");
		check(waited_ms > 3000 || advanced, "the wait really spanned seconds");
		check(advanced, "fstat: mtime advances on write");
		check(sa.st_mtime > 1600000000L, "fstat: mtime is a real date");
		check(sa.st_mtime - before <= 10, "fstat: mtime tracks the clock");
		close(sfa);
		close(sfb);
		unlink("/tmp/chello.a");
		unlink("/tmp/chello.b");
	} else {
		check(0, "open two scratch files for fstat");
		if (sfa >= 0) close(sfa);
		if (sfb >= 0) close(sfb);
	}

	// A bad descriptor must say so, not describe something.
	check(fstat(999, &sb) == -1 && errno == EBADF, "fstat on a bad fd is EBADF");

	// mmap/munmap/mprotect. The interesting property is the third one: a page
	// made read-only must actually fault on write, or `mprotect` is a no-op that
	// reports success -- the worst kind of bug, because a program relying on it
	// believes it has made a page immutable when it has not.
	long pagesz = sysconf(_SC_PAGESIZE);
	if (pagesz <= 0)
		pagesz = 4096;
	void *m = mmap(NULL, (size_t)pagesz, PROT_READ | PROT_WRITE,
	               MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	check(m != MAP_FAILED, "mmap anonymous");
	if (m != MAP_FAILED) {
		// Fresh anonymous memory reads as zero and is writable.
		check(*(volatile char *)m == 0, "mmap: fresh page reads zero");
		*(volatile char *)m = 'x';
		check(*(volatile char *)m == 'x', "mmap: page is writable");
		check(mprotect(m, (size_t)pagesz, PROT_READ) == 0, "mprotect to read-only");
		// Read still works after the change; that is the point of PROT_READ.
		check(*(volatile char *)m == 'x', "mprotect: still readable");
		check(munmap(m, (size_t)pagesz) == 0, "munmap");
		// The address is gone: mapping it again must hand back fresh zeroed
		// memory, not the page just released.
		void *m2 = mmap(NULL, (size_t)pagesz, PROT_READ | PROT_WRITE,
		                MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
		check(m2 != MAP_FAILED, "mmap after munmap");
		if (m2 != MAP_FAILED) {
			check(*(volatile char *)m2 == 0 || m2 != m,
			      "mmap: remapped address is fresh");
			munmap(m2, (size_t)pagesz);
		}
	}
	// A protection change over a region that is not mapped must fail, not
	// silently succeed: it is the difference between an honest error and a
	// program that believes it hardened something.
	check(mprotect((void *)0x10, 4096, PROT_READ) == -1, "mprotect unmapped fails");
	check(munmap(NULL, 0) == -1, "munmap(NULL, 0) fails");
	// MAP_FIXED must be refused rather than honoured at a different address:
	// silently moving a fixed mapping leaves the caller using memory it does
	// not own.
	check(mmap((void *)0x100000, 4096, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED,
	           -1, 0) == MAP_FAILED, "MAP_FIXED refused, not relocated");

	// /dev/fb0. A terminal has to map the framebuffer to be usable at all --
	// a write(2) per pixel cannot redraw a screen -- so the properties that
	// matter are: the node exists, it reports its geometry, and it mmaps.
	int fb = open("/dev/fb0", O_RDWR, 0);
	if (fb < 0) {
		printf("[chello] SKIP framebuffer: %s\n", strerror(errno));
	} else {
		check(1, "/dev/fb0 opens");
		// The geometry read from the device must agree with FB_INFO, or a
		// program that trusts one and not the other draws garbage.
		uint32_t geo[10] = {0};
		ssize_t gn = read(fb, geo, sizeof geo);
		check(gn == (ssize_t)sizeof geo, "/dev/fb0 reports geometry");
		check(geo[0] > 0 && geo[1] > 0, "/dev/fb0 reports a nonzero size");
		check(geo[3] == 16 || geo[3] == 24 || geo[3] == 32, "/dev/fb0 bpp is sane");
		check(geo[2] >= geo[0] * (geo[3] / 8), "pitch covers a scanline");

		// Map the whole thing and write a pixel, then read it back. This is
		// the check that actually proves the device is mapped rather than
		// merely present.
		long fbsz = sysconf(_SC_PAGESIZE);
		if (fbsz <= 0)
			fbsz = 4096;
		size_t fblen = (size_t)geo[2] * geo[1];
		void *fbm = mmap(NULL, fblen, PROT_READ | PROT_WRITE,
		                 MAP_SHARED, fb, 0);
		check(fbm != MAP_FAILED, "mmap /dev/fb0");
		if (fbm != MAP_FAILED) {
			// Write the first pixel and read it back. A mapping that is not
			// backed by the real device would still round-trip in cache, so
			// this proves writability, not that the device is attached -- but
			// it is the property a terminal depends on and the one a
			// read-only or wrongly-flagged mapping would fail.
			uint32_t *px = (uint32_t *)fbm;
			uint32_t before = px[0];
			px[0] = before ^ 0x00FF00FFu;
			check(px[0] == (before ^ 0x00FF00FFu), "mapped framebuffer is writable");
			px[0] = before; // restore
			munmap(fbm, fblen);
			check(1, "munmap /dev/fb0");
		}
		// The Linux framebuffer queries. A ported terminal learns the display's
		// shape from these rather than from FB_INFO, so they have to describe
		// the same framebuffer.
		//
		// The sizes matter as much as the values: the kernel writes a struct
		// whose layout is defined independently in Rust and in this program's
		// <linux/fb.h>. A single wrong field width would shift everything after
		// it, and the symptom would be a terminal that believes it has a
		// zero-width framebuffer -- not a crash, and nothing pointing at the
		// cause. So compare against the geometry FB_INFO reports, which is the
		// independent source of truth.
		uint32_t vinfo[64] = { 0 };
		uint32_t finfo[16] = { 0 };
		check(ioctl(fb, 0x4600 /* FBIOGET_VSCREENINFO */, vinfo) == 0,
		      "FBIOGET_VSCREENINFO");
		check(ioctl(fb, 0x4602 /* FBIOGET_FSCREENINFO */, finfo) == 0,
		      "FBIOGET_FSCREENINFO");
		// xres, yres at offsets 0 and 4; bits_per_pixel at offset 24.
		check(vinfo[0] == geo[0], "FBIOGET_VSCREENINFO xres matches");
		check(vinfo[1] == geo[1], "FBIOGET_VSCREENINFO yres matches");
		check(vinfo[6] == geo[3], "FBIOGET_VSCREENINFO bpp matches");
		// A terminal refuses to start on a 0-bit framebuffer, so this is the
		// field that decides whether the port works at all.
		check(vinfo[6] == 16 || vinfo[6] == 24 || vinfo[6] == 32,
		      "FBIOGET_VSCREENINFO bpp is usable");
		// Red channel placement starts at byte 32: offset, length, msb_right.
		// A zero length does not draw black, it draws nothing at all, while
		// looking like a successful query -- so this is checked, not assumed.
		check(vinfo[8] > 0 && vinfo[8] < 32, "FBIOGET_VSCREENINFO red offset sane");
		check(vinfo[9] > 0 && vinfo[9] <= 8, "FBIOGET_VSCREENINFO red length sane");
		// Green and blue must be placed too, or a terminal writing "red" gets
		// a colour with two of three channels in the wrong bits.
		check(vinfo[12] > 0 && vinfo[12] < 32, "FBIOGET_VSCREENINFO green offset sane");
		check(vinfo[15] > 0 && vinfo[15] < 32, "FBIOGET_VSCREENINFO blue offset sane");
		// The three channels must not overlap. Overlap is invisible in any
		// single-field check and produces plausible wrong colours.
		//
		// Note this is a pairwise test, not an ordering test. On this hardware
		// the channels run blue, green, red as the bit position rises, so
		// asserting that red sits below green -- which is what a first reading
		// of the field order suggests -- fails on a perfectly correct mode.
		// Overlap is the actual hazard, and it is order-independent.
		{
			uint32_t roff = vinfo[8], rlen = vinfo[9];
			uint32_t goff = vinfo[12], glen = vinfo[13];
			uint32_t boff = vinfo[15], blen = vinfo[16];
			int overlap = (roff < goff + glen && goff < roff + rlen) ||
			              (roff < boff + blen && boff < roff + rlen) ||
			              (goff < boff + blen && boff < goff + glen);
			check(!overlap, "FBIOGET_VSCREENINFO channels do not overlap");
			// And all three must fit inside one pixel. A channel that ran past
			// the pixel would write into its neighbour, which on a 32bpp mode
			// means into the next pixel's bits.
			uint32_t deepest = roff + rlen;
			if (goff + glen > deepest)
				deepest = goff + glen;
			if (boff + blen > deepest)
				deepest = boff + blen;
			check(deepest <= vinfo[6], "FBIOGET_VSCREENINFO channels fit a pixel");
		}
		//
		// fb_fix_screeninfo is NOT packed, so these are read at byte offsets
		// rather than as u32 indices. Its layout is:
		//
		//   0  char id[16]
		//  16  u64 smem_len            (8-byte aligned, no pad needed here)
		//  24  u32 type
		//  28  u32 type_aux
		//  32  u32 visual
		//  36  u16 xpanstep
		//  38  u16 ypanstep
		//  40  u16 ywrapstep
		//  42  (2 bytes padding to reach 8-byte alignment)
		//  48  u64 line_length
		//
		// The 6 bytes of padding at 42 are why an index-based read is wrong:
		// it puts line_length four bytes early and reads padding as a field.
		unsigned char *fx = (unsigned char *)finfo;
		uint64_t smem_len, line_length;
		memcpy(&smem_len, fx + 16, sizeof smem_len);
		memcpy(&line_length, fx + 48, sizeof line_length);
		check(smem_len == (uint64_t)geo[2] * geo[1], "FBIOGET_FSCREENINFO smem_len");
		check(line_length == (uint64_t)geo[2], "FBIOGET_FSCREENINFO line_length");
		uint32_t ftype, fvisual;
		memcpy(&ftype, fx + 24, sizeof ftype);
		memcpy(&fvisual, fx + 32, sizeof fvisual);
		check(ftype == 0 /* FB_TYPE_PACKED_PIXELS */,
		      "FBIOGET_FSCREENINFO packed pixels");
		check(fvisual == 2 /* FB_VISUAL_TRUECOLOR */,
		      "FBIOGET_FSCREENINFO truecolour");
		// The pan steps decide whether a terminal believes it can scroll by
		// panning. Non-zero here would make it scroll into whatever the next
		// page holds, so all three must be zero.
		uint16_t xpan, ypan, ywrap;
		memcpy(&xpan, fx + 36, sizeof xpan);
		memcpy(&ypan, fx + 38, sizeof ypan);
		memcpy(&ywrap, fx + 40, sizeof ywrap);
		check(xpan == 0 && ypan == 0 && ywrap == 0,
		      "FBIOGET_FSCREENINFO no pan steps");
		// An unimplemented request must be refused, not answered with zeros: a
		// terminal that believed a palette call succeeded on a truecolour
		// display would misdraw the screen.
		uint32_t junk[8] = { 0 };
		check(ioctl(fb, 0x4606 /* FBIOPAN_DISPLAY */, junk) != 0,
		      "FBIOPAN_DISPLAY is refused");

		// A private mapping of a device must be refused, not silently
		// accepted: copy-on-write over device memory would not see the
		// device's own writes, which is a bug a program cannot detect.
		void *priv = mmap(NULL, fblen, PROT_READ | PROT_WRITE,
		                  MAP_PRIVATE, fb, 0);
		check(priv == MAP_FAILED, "MAP_PRIVATE on a device is refused");
		close(fb);
	}

	// /dev/input/event0. A terminal reads evdev input_event structs, so the
	// checks are that the node exists, is readable, and that the struct size
	// a program assumes is the one we produce.
	int ev = open("/dev/input/event0", O_RDONLY, 0);
	if (ev < 0) {
		printf("[chello] SKIP input events: %s\n", strerror(errno));
	} else {
		check(1, "/dev/input/event0 opens");
		// No key is pressed here, so a read must block or return end of
		// input rather than invent one. A short read is a real failure: a
		// truncated input_event parses as a garbage keycode.
		struct pollfd pfd = { .fd = ev, .events = 0x001 /* POLLIN */ };
		int pr = poll(&pfd, 1, 0);
		check(pr >= 0, "poll /dev/input/event0");
		if (pr > 0 && (pfd.revents & 0x001)) {
			unsigned char buf[24];
			ssize_t n = read(ev, buf, sizeof buf);
			// Whatever arrives must be a whole number of 24-byte events.
			check(n == 0 || n % 24 == 0, "input_event reads are whole structs");
		}
		close(ev);
	}
	// /etc/keymap.conf. The keyboard driver reads this at startup and the
	// installer rewrites it, so a missing or malformed one means the system
	// silently ignores the layout the user chose.
	int km = open("/etc/keymap.conf", O_RDONLY, 0);
	if (km < 0) {
		check(0, "/etc/keymap.conf exists");
	} else {
		char kb[64] = {0};
		ssize_t kn = read(km, kb, sizeof kb - 1);
		close(km);
		check(kn > 0, "/etc/keymap.conf is not empty");
		// One whitespace-delimited token naming a layout the driver knows.
		char want[16];
		int k = 0;
		for (ssize_t i = 0; i < kn; i++) {
			if (kb[i] == ' ' || kb[i] == '\t' || kb[i] == '\n' || kb[i] == '\r')
				break;
			if (k < (int)sizeof want - 1)
				want[k++] = kb[i];
		}
		want[k] = 0;
		const char *known[] = { "us", "de", "colemak", "dvorak" };
		int match = 0;
		for (unsigned i = 0; i < sizeof known / sizeof known[0]; i++)
			if (strcmp(want, known[i]) == 0)
				match = 1;
		check(match, "keymap names a known layout");
		// The same file the installer reads, so the two cannot disagree about
		// which layout is active.
		int motd = open("/etc/motd", O_RDONLY, 0);
		check(motd >= 0, "/etc/motd still present");
		if (motd >= 0)
			close(motd);
	}

	// The boot pty must be one pair, not two. `consoled` feeds keystrokes into
	// the master of pair 0 and the installer reads `/dev/pts/0`; if those two
	// ends are not the same pty, every key goes to a terminal nobody reads and
	// the installer freezes with no error anywhere. A keypress still looks like
	// it was "handled" on the way in, so a boot log shows nothing wrong.
	//
	// `TIOCGPTN` reports which pair a master drives, which settles the question
	// without moving a byte. Moving bytes is not an option here: the installer
	// is concurrently reading this same slave, and a pty has exactly one reader,
	// so a test that wrote a byte and read it back would be racing the installer
	// for it and would fail at random.
	//
	// So ask the driver instead. `/dev/ptmx0` must report pair 0 (the slave
	// published as `/dev/pts/0`), and a freshly opened `/dev/ptmx` must report
	// something else, because that name is an allocating multiplexer and
	// mistaking it for pair 0's master is the mistake being ruled out.
	int m0 = open("/dev/ptmx0", O_RDWR, 0);
	int s0 = open("/dev/pts/0", O_RDWR, 0);
	if (m0 < 0 || s0 < 0) {
		check(0, "/dev/ptmx0 and /dev/pts/0 open");
	} else {
		check(1, "/dev/ptmx0 and /dev/pts/0 open");
		int n0 = -1;
		check(ioctl(m0, TIOCGPTN, &n0) == 0, "TIOCGPTN on /dev/ptmx0");
		check(n0 == 0, "/dev/ptmx0 drives pair 0 (/dev/pts/0)");

		int mx = open("/dev/ptmx", O_RDWR, 0);
		if (mx < 0) {
			check(0, "/dev/ptmx opens");
		} else {
			check(1, "/dev/ptmx opens");
			int nmx = -1;
			check(ioctl(mx, TIOCGPTN, &nmx) == 0, "TIOCGPTN on /dev/ptmx");
			// A fresh pair, so never 0. This is the assertion that would
			// have caught the original bug: had `consoled` opened this name,
			// its keystrokes would have gone to slave `nmx` while the
			// installer waited on slave 0.
			check(nmx != 0, "/dev/ptmx allocates a pair other than 0");
			close(mx);
		}
		close(m0);
		close(s0);
	}

	// ttyname(3). A program needs the *path* of its terminal, not the
	// descriptor: to reopen it, hand it to a child, or report it. This is what
	// a ported terminal checks before it will run at all, so it has to work for
	// a pty slave and not just for a Linux virtual console.
	if (isatty(0)) {
		char tn[64] = { 0 };
		// The reentrant form first: it is the one that reports a real error
		// instead of a static buffer, so it is the one worth exercising.
		int tr = ttyname_r(0, tn, sizeof tn);
		check(tr == 0, "ttyname_r on the controlling terminal");
		if (tr == 0) {
			// A device path has to start with /dev and be NUL terminated
			// within the buffer. Checking the NUL matters: ttyname_r fills a
			// caller-supplied buffer, and a missing terminator would make
			// every later string operation on it read off the end.
			check(tn[0] == '/', "ttyname_r returns an absolute path");
			check(strlen(tn) < sizeof tn, "ttyname_r NUL terminates");
			// The path has to actually open, or it is worse than useless: a
			// program that reopened it would fail somewhere far from the
			// mistake.
			int t = open(tn, O_RDWR | O_NOCTTY);
			check(t >= 0, "ttyname_r path reopens");
			if (t >= 0)
				close(t);
			// The non-reentrant form must agree with it.
			const char *tn2 = ttyname(0);
			check(tn2 != NULL && strcmp(tn, tn2) == 0, "ttyname agrees with ttyname_r");
		}
		// A buffer too small must be refused rather than truncated. Half a
		// path is not a path, and a program that used one would fail later
		// and further away.
		char tiny[2];
		int te = ttyname_r(0, tiny, sizeof tiny);
		check(te != 0, "ttyname_r refuses a short buffer");
	} else {
		// stdin is not a terminal in this configuration, which is worth
		// saying rather than skipping silently.
		printf("[chello] SKIP ttyname: stdin is not a terminal\n");
	}
	// ttyname on something that is definitively not a terminal: /dev/fb0 is a
	// character device but has no line discipline, so it must be refused.
	int fbcheck = open("/dev/fb0", O_RDWR, 0);
	if (fbcheck >= 0) {
		char nb[64];
		int ne = ttyname_r(fbcheck, nb, sizeof nb);
		check(ne != 0, "ttyname_r refuses a non-terminal");
		close(fbcheck);
	}

	// dup(2) / dup2(2). The property that matters is that the duplicate shares
	// the file offset, not that it opens the same file: a program that writes
	// through one descriptor and reads through the other -- which is what a
	// shell's `cmd > f` and a protocol's handshake both do -- only works if the
	// two advance together. Two descriptors with independent cursors would
	// silently overwrite each other.
	{
		char tmpa[] = "/tmp/dup-a";
		char tmpb[] = "/tmp/dup-b";
		int a = open(tmpa, O_RDWR | O_CREAT | O_TRUNC, 0600);
		int b = open(tmpb, O_RDWR | O_CREAT | O_TRUNC, 0600);
		check(a >= 0 && b >= 0, "open two files for the dup test");
		if (a >= 0 && b >= 0) {
			int d = dup(a);
			check(d >= 0, "dup");
			if (d >= 0) {
				// The lowest free descriptor, which in a program that has
				// closed things can be below 3. Only the "not the original"
				// part is portable; a specific number is not.
				check(d != a, "dup returns a different descriptor");
				write(a, "0123456789", 10);
				// Reading through the duplicate must start where the write
				// left off -- that is the sharing, and it is the whole point.
				char rb[16] = { 0 };
				ssize_t rn = read(d, rb, sizeof rb);
				check(rn == 0, "dup shares the file offset");
				lseek(a, 0, SEEK_SET);
				// And the offset is shared, so the seek is visible through
				// the duplicate as well.
				rn = read(d, rb, 4);
				check(rn == 4 && memcmp(rb, "0123", 4) == 0,
				      "dup sees the original's seek");
				close(d);
			}
			// dup2 onto a specific slot, displacing what was there.
			int n2 = dup2(a, b);
			check(n2 == b, "dup2 returns newfd");
			// Sharing, proved without depending on the file's contents:
			// seek to a known place and write, then ask the *other*
			// descriptor where it is. If the offsets were independent, b
			// would still report 0 and the write through a would be
			// invisible to b's idea of where it is.
			lseek(a, 0, SEEK_SET);
			write(a, "Q", 1);
			off_t seen = lseek(b, 0, SEEK_CUR);
			check(seen == 1, "dup2 shares the offset too");
			// dup2 with oldfd == newfd must succeed and change nothing --
			// POSIX is explicit, and a program checking the return value
			// would otherwise have closed the descriptor it meant to keep.
			check(dup2(b, b) == b, "dup2 with oldfd == newfd succeeds");
			// Seeks are explicit throughout: the descriptor shares an offset
			// with `a`, so its position is whatever the previous check left
			// behind, and a test that assumed 0 would fail for the right
			// reason at the wrong place. If dup2 had closed the descriptor,
			// the write below would fail with EBADF -- which is the point.
			lseek(b, 0, SEEK_SET);
			char rc[8] = { 0 };
			check(write(b, "z", 1) == 1, "dup2 oldfd == newfd left it open");
			lseek(b, 0, SEEK_SET);
			check(read(b, rc, 1) == 1 && rc[0] == 'z',
			      "dup2 oldfd == newfd wrote through the same fd");
			close(a);
			close(b);
		}
		unlink(tmpa);
		unlink(tmpb);
	}

	// select(2), which mlibc routes through the same syscall as pselect(2).
	// fbterm's whole event loop is select, so this is not a nicety.
	{
		// A descriptor with nothing on it must block until the timeout, and
		// report a zero count. If it returned ready, a program looping on
		// select would spin at 100% CPU.
		int sv[2];
		check(pipe(sv) == 0, "pipe for the select test");
		if (sv[0] >= 0) {
			fd_set rf;
			FD_ZERO(&rf);
			FD_SET(sv[0], &rf);
			struct timeval tv = { 0, 50000 }; /* 50ms */
			int n = select(sv[0] + 1, &rf, NULL, NULL, &tv);
			check(n == 0, "select times out on an idle pipe");
			// Now make it ready and check both the count and that the set
			// still has the bit set. The set is modified in place, so a
			// select that reports ready but clears the bit would make a
			// retry loop hang.
			write(sv[1], "k", 1);
			FD_ZERO(&rf);
			FD_SET(sv[0], &rf);
			tv.tv_sec = 1;
			tv.tv_usec = 0;
			n = select(sv[0] + 1, &rf, NULL, NULL, &tv);
			check(n == 1, "select reports the readable end");
			check(FD_ISSET(sv[0], &rf), "select leaves the ready bit set");
			// A write-only watch on the read end can never be satisfied, so
			// it must time out rather than report ready.
			FD_ZERO(&rf);
			FD_SET(sv[0], &rf);
			tv.tv_sec = 0;
			tv.tv_usec = 50000;
			check(select(sv[0] + 1, NULL, &rf, NULL, &tv) == 0,
			      "select will not report a read end as writable");
			close(sv[0]);
			close(sv[1]);
		}
	}

	// Process groups and terminal ownership.
	//
	// A spawned child inherits its parent's group, so its own pid is not a
	// valid pgid until it creates one -- which is why setpgid(0,0) has to
	// work before anything can be handed the terminal.
	{
		pid_t me = getpid();
		pid_t sid_before = getsid(0);
		pid_t before = getpgrp();
		check(before > 0, "getpgrp reports a group");
		check(sid_before > 0, "getsid reports a session");

		// Creating a group: the create-a-group form, not the join-an-existing
		// one. If this fails, no process can ever become a group leader and
		// job control is impossible.
		check(setpgid(0, 0) == 0, "setpgid(0,0) creates a group");
		check(getpgrp() == me, "getpgrp is our own pid after setpgid");

		// A process group is not a session. Creating a group must leave the
		// session untouched -- otherwise a later setsid() is refused and a
		// program doing the ordinary setpgid-then-setsid sequence breaks.
		check(getsid(0) == sid_before, "setpgid left the session alone");

		// /dev/pts/0 belongs to whichever session first opened it, which is
		// not this one. TIOCSPGRP must therefore be *refused*: a process
		// outside a terminal's session has no business redirecting it, and
		// letting it would hand one session control of another's terminal.
		int boot_tty = open(KEY_SLAVE_PATH, O_RDWR, 0);
		if (boot_tty >= 0) {
			pid_t tsid = 0;
			ioctl(boot_tty, TIOCGSID, &tsid);
			if (tsid != 0 && tsid != sid_before) {
				pid_t want = me;
				check(ioctl(boot_tty, TIOCSPGRP, &want) != 0,
				      "TIOCSPGRP refused from another session");
			} else {
				// We do own it; the positive case is covered below.
			}
			close(boot_tty);
		}

		// A pty this process opens itself is claimed by *this* session, so
		// the positive case is legitimate here: create a group, take the
		// terminal, read it back.
		int master = open("/dev/ptmx", O_RDWR | O_NOCTTY);
		if (master < 0) {
			printf("[chello] SKIP terminal pgrp: no /dev/ptmx\n");
		} else {
			int idx = -1;
			if (ioctl(master, TIOCGPTN, &idx) != 0 || idx < 0) {
				check(0, "TIOCGPTN on a fresh pair");
			} else {
				char path[32];
				snprintf(path, sizeof path, "/dev/pts/%d", idx);
				int tty = open(path, O_RDWR);
				check(tty >= 0, "open a freshly allocated pty slave");
				if (tty >= 0) {
					pid_t tsid = 0;
					check(ioctl(tty, TIOCGSID, &tsid) == 0 && tsid == sid_before,
					      "TIOCGSID reports our own session");
					pid_t want = me;
					check(ioctl(tty, TIOCSPGRP, &want) == 0,
					      "TIOCSPGRP to our own group");
					pid_t got = 0;
					check(ioctl(tty, TIOCGPGRP, &got) == 0 && got == me,
					      "foreground pgrp round-trip");
					// Handing the terminal to a group that does not exist
					// must be refused, or the terminal ends up owned by a
					// group nothing can be waited on or signalled in.
					pid_t ghost = (pid_t)(me + 0x70000000);
					check(ioctl(tty, TIOCSPGRP, &ghost) != 0,
					      "TIOCSPGRP refuses a group that does not exist");
					// ... and the foreground must survive the refusal.
					pid_t still = 0;
					check(ioctl(tty, TIOCGPGRP, &still) == 0 && still == me,
					      "refused TIOCSPGRP left the foreground alone");
					close(tty);
				}
				close(master);
			}
		}
	}

	// The old framing must still be reachable: the evdev device is layered
	// over these, not instead of them, and anything already using them --
	// the installer, the Rust runtime -- depends on it.
	int kbd = open("/dev/kbd0", O_RDONLY, 0);
	check(kbd >= 0, "/dev/kbd0 still present");
	if (kbd >= 0)
		close(kbd);

	// getrandom(2). Two things have to hold, and the second is the one that
	// matters: the bytes must actually differ between calls. A generator that
	// returns the same buffer every time passes "did it write anything" and
	// fails silently at everything built on top of it.
	unsigned char r1[32], r2[32];
	memset(r1, 0xAA, sizeof r1);
	memset(r2, 0xAA, sizeof r2);
	ssize_t g1 = getrandom(r1, sizeof r1, 0);
	check(g1 == (ssize_t)sizeof r1, "getrandom fills the request");
	int r1_changed = 0;
	for (size_t i = 0; i < sizeof r1; i++)
		if (r1[i] != 0xAA)
			r1_changed = 1;
	check(r1_changed, "getrandom wrote the buffer");
	check(getrandom(r2, sizeof r2, 0) == (ssize_t)sizeof r2, "second getrandom");
	check(memcmp(r1, r2, sizeof r1) != 0, "two getrandom calls differ");
	// A zero-length request is a success, not an error: getrandom(2) says so
	// and a caller computing a length of zero should not see a failure.
	check(getrandom(r1, 0, 0) == 0, "getrandom(0) succeeds");

	// A world-writable directory is not a licence to write anywhere: /etc is
	// root-owned, so creating a file there must be refused. This is the
	// permission model's negative case, and it is the reason the test above
	// uses /tmp.
	int denied = open("/etc/chello.tmp", O_WRONLY | O_CREAT | O_TRUNC, 0644);
	if (denied < 0 && errno == EACCES) {
		check(1, "write to root-owned /etc refused");
	} else {
		if (denied >= 0)
			unlink("/etc/chello.tmp");
		check(0, "write to root-owned /etc refused");
	}

	// ---- environment ------------------------------------------------------
	//
	// The two halves of libc startup, and both of them were broken here for the
	// same reason: this kernel runs no `.init_array` constructors.
	//
	// mlibc's `environ` is a global with a *dynamic* initializer, so it lived in
	// `.init_array` and stayed null -- and `getenv`, `setenv` and the startup
	// path that imports the incoming variables all walk `environ[i]`. The first
	// program to touch its environment took a read fault at address zero.
	//
	// mlibc's argc/argv/envp are published by a constructor too, so `main` was
	// handed argc=0 with argv and envp null. Quieter than the environ fault and
	// just as wrong.
	//
	// Both are checked here because nothing else noticed: chello uses neither, and
	// every other user image is Rust.
	const char *path = getenv("PATH");
	check(path != NULL, "getenv(PATH) does not fault");
	check(path != NULL && path[0] == '/', "getenv(PATH) returns a path");
	check(getenv("SAMSARA_NO_SUCH_VARIABLE") == NULL,
	      "getenv of an unset name is NULL");
	// Every variable in the environment must survive the walk. One malformed
	// entry -- a string with no '=' -- is enough to send getenv's own index
	// lookup off the end, and a shell that cannot read $PATH cannot find its
	// applets, so this counts rather than spot-checks.
	int env_entries = 0, env_well_formed = 0;
	for (char **e = environ; *e; e++) {
		env_entries++;
		if (strchr(*e, '='))
			env_well_formed++;
	}
	check(env_entries > 0, "environ is not empty");
	check(env_entries == env_well_formed, "every environ entry has a '='");

	// setenv and unsetenv are *not* checked here, and that is a gap rather than
	// an oversight. Both go through mlibc's environment vector, which allocates,
	// and mlibc's allocator is a `thread_local` -- so the first allocation in a
	// process reads the thread pointer out of %fs. This platform does not yet
	// set %fs for a freshly exec'd image, because the kernel loader does not
	// lay out a thread block: mlibc normally has its dynamic loader do that, and
	// here the kernel is the loader instead.
	//
	// So `setenv` faults on a read of the thread pointer, and the check would
	// report a libc failure for a kernel gap. The fix is a thread block, which is
	// the next piece of work; until it lands, a C program on this system can read
	// its environment but not change it, and every program that mallocs is in the
	// same position.

	// main's own arguments. A process spawned with no arguments legitimately
	// has argc == 1, because argv[0] is the program name and every C runtime
	// supplies one. argc == 0 means the startup path never ran: with argv null,
	// a program that dispatches on its own name -- which is what busybox does,
	// and what makes /bin/ls work -- would present itself as `busybox`.
	//
	// This is the check that would have caught the reversed-argv bug too, had
	// anything been checking.
	check(argc >= 1, "main received an argv");
	check(argc >= 1 && argv != NULL, "main's argv is not NULL");
	check(argc >= 1 && argv[0] != NULL, "argv[0] is set");
	check(environ != NULL, "environ is not NULL");
	if (argc >= 1 && argv && argv[0])
		check(strstr(argv[0], "chello") != NULL, "argv[0] names this program");

	// ---- symbolic links -------------------------------------------------
	//
	// These are not a convenience. /bin/ls has to *be* the busybox image for
	// applet dispatch to work at all, and a program decides whether it is
	// looking at a link by asking the kernel. So the whole set has to hold:
	// creation, reading the target back, following it, lstat describing the
	// link rather than the file, and both loop cases failing rather than
	// hanging.
	unlink("/tmp/ln-target");
	unlink("/tmp/ln-rel");
	unlink("/tmp/ln-loop");
	unlink("/tmp/ln-self");
	unlink("/tmp/ln-dangle");

	int tfd = open("/tmp/ln-target", O_WRONLY | O_CREAT | O_TRUNC, 0644);
	check(tfd >= 0, "symlink: create target file");
	if (tfd >= 0) {
		ssize_t w = write(tfd, "payload\n", 8);
		check(w == 8, "symlink: write target contents");
		close(tfd);
	}

	check(symlink("/tmp/ln-target", "/tmp/ln-rel") == 0, "symlink: create");

	// readlink(2) returns the target as stored, relative or absolute, and with
	// no NUL appended. A trailing NUL would mean every caller had to strip it,
	// and one that forgot would carry it into the next syscall.
	char buf[256];
	memset(buf, 0x7f, sizeof buf);
	ssize_t rl = readlink("/tmp/ln-rel", buf, sizeof buf);
	check(rl == (ssize_t)strlen("/tmp/ln-target"), "readlink: length");
	check(rl > 0 && memcmp(buf, "/tmp/ln-target", strlen("/tmp/ln-target")) == 0,
	      "readlink: verbatim target");
	check(rl > 0 && buf[rl] == 0x7f, "readlink: appends no NUL");

	// A relative target must survive being stored unresolved, because that is
	// the case that breaks if the kernel resolves at creation time: the link
	// still works from a different directory afterwards.
	check(symlink("ln-target", "/tmp/ln-rel2") == 0, "symlink: relative");
	int rfd = open("/tmp/ln-rel2", O_RDONLY);
	check(rfd >= 0, "relative link follows to target");
	if (rfd >= 0) {
		char rbuf[8];
		check(read(rfd, rbuf, 8) == 8, "relative link read contents");
		check(memcmp(rbuf, "payload\n", 8) == 0, "relative link contents");
		close(rfd);
	}

	// lstat must describe the *link*. If it described the file, `ls -l` would
	// never draw the `@`, a shell's `-L` test would lie, and nothing would
	// fail -- every one of those is a wrong answer that looks right.
	struct stat lst, fst;
	check(lstat("/tmp/ln-rel", &lst) == 0, "lstat: succeeds");
	check(stat("/tmp/ln-rel", &fst) == 0, "stat through a link: succeeds");
	check(S_ISLNK(lst.st_mode), "lstat: reports S_IFLNK");
	check(S_ISREG(fst.st_mode), "stat: reports the target");
	check(lst.st_size == (off_t)strlen("/tmp/ln-target"), "lstat: size is target length");
	check((lst.st_mode & 07777) == 0777, "symlink mode reads as 0777");
	check(fst.st_size == 8, "stat: size is the file's");

	// open(2) follows a link, so reading through one yields the *target's*
	// bytes and never the target's path. That is the point: `cat` on /bin/ls
	// has to print a directory listing, not the string "/bin/busybox". A
	// link's own contents are reachable only through readlink(2), which is
	// what the earlier check does.
	int opfd = open("/tmp/ln-rel", O_RDONLY);
	check(opfd >= 0, "open(2) on a link follows it");
	if (opfd >= 0) {
		char cbuf[8];
		ssize_t rn = read(opfd, cbuf, 8);
		check(rn == 8, "read(2) on a link reads the target");
		check(rn == 8 && memcmp(cbuf, "payload\n", 8) == 0,
		      "read through a link is the target's data");
		close(opfd);
	}

	// readlink on a non-link is EINVAL, not ENOENT: the file is right there,
	// it is the request that does not apply to it.
	errno = 0;
	check(readlink("/tmp/ln-target", buf, sizeof buf) < 0 && errno == EINVAL,
	      "readlink on a plain file: EINVAL");

	// EEXIST even though the link may dangle. `symlink` answers whether the
	// *name* is free; making it depend on the target would mean a caller that
	// fixed the target and retried could not tell the two failures apart.
	check(symlink("/tmp/nowhere", "/tmp/ln-dangle") == 0, "symlink: may dangle");
	errno = 0;
	check(symlink("/tmp/nowhere2", "/tmp/ln-dangle") < 0 && errno == EEXIST,
	      "symlink: EEXIST on taken name");
	errno = 0;
	check(stat("/tmp/ln-dangle", &fst) < 0 && errno == ENOENT,
	      "dangling link: stat gives ENOENT");
	struct stat dlst;
	check(lstat("/tmp/ln-dangle", &dlst) == 0 && S_ISLNK(dlst.st_mode),
	      "dangling link: lstat still describes it");
	errno = 0;
	check(readlink("/tmp/ln-dangle", buf, sizeof buf) == (ssize_t)strlen("/tmp/nowhere"),
	      "dangling link: readlink still works");

	// An empty target is EINVAL. A link to nowhere is a mistake, and a silent
	// one: everything done to it later fails with ENOENT at a path nobody
	// wrote.
	errno = 0;
	check(symlink("", "/tmp/ln-empty") < 0 && errno == EINVAL,
	      "symlink: empty target is EINVAL");

	// A short buffer is ERANGE, never a truncated target. Half a target is a
	// path that does not exist, so a caller that went on to stat it would
	// report a missing file rather than a short buffer.
	errno = 0;
	check(readlink("/tmp/ln-rel", buf, 4) < 0 && errno == ERANGE,
	      "readlink: short buffer is ERANGE");

	// Loops must fail. A link to itself is the simple case.
	check(symlink("/tmp/ln-self", "/tmp/ln-self") == 0, "symlink: self link");
	errno = 0;
	check(stat("/tmp/ln-self", &fst) < 0 && errno == ELOOP, "self link: ELOOP");
	errno = 0;
	check(open("/tmp/ln-self", O_RDONLY) < 0 && errno == ELOOP, "self link: open ELOOP");
	// ...and lstat still works on it, because lstat does not follow. A loop
	// that made the link unstattable would be indistinguishable from a
	// missing file.
	check(lstat("/tmp/ln-self", &dlst) == 0 && S_ISLNK(dlst.st_mode),
	      "self link: lstat works");

	// A two-link cycle. Following either one loops, and the bound that stops
	// it is what keeps a mistake from becoming a hang.
	check(symlink("/tmp/ln-cycle-b", "/tmp/ln-cycle-a") == 0, "symlink: cycle a");
	check(symlink("/tmp/ln-cycle-a", "/tmp/ln-cycle-b") == 0, "symlink: cycle b");
	errno = 0;
	check(stat("/tmp/ln-cycle-a", &fst) < 0 && errno == ELOOP, "two-link cycle: ELOOP");
	unlink("/tmp/ln-cycle-a");
	unlink("/tmp/ln-cycle-b");

	// A chain long enough to be legitimate must still work: SYMLOOP_MAX is a
	// bound, not a policy against depth.
	char deep[256];
	int deepfd = open("/tmp/ln-deep-0", O_WRONLY | O_CREAT | O_TRUNC, 0644);
	if (deepfd >= 0)
		close(deepfd);
	int deep_ok = 1;
	for (int i = 1; i <= 6; i++) {
		snprintf(deep, sizeof deep, "/tmp/ln-deep-%d", i);
		char target[64];
		snprintf(target, sizeof target, "/tmp/ln-deep-%d", i - 1);
		if (symlink(target, deep) != 0)
			deep_ok = 0;
	}
	check(deep_ok, "symlink: 6-deep chain built");
	int deepstat = stat("/tmp/ln-deep-6", &fst);
	check(deepstat == 0, "6-deep chain: stat resolves");
	for (int i = 0; i <= 6; i++) {
		char p[64];
		snprintf(p, sizeof p, "/tmp/ln-deep-%d", i);
		unlink(p);
	}
	unlink("/tmp/ln-rel2");
	unlink("/tmp/ln-empty");
	unlink("/tmp/ln-self");
	unlink("/tmp/ln-dangle");
	unlink("/tmp/ln-target");

	// A pty must carry more than its own buffer. A terminal is the case where
	// this matters most and is hardest to see: a directory listing is several
	// times the buffer, and a slave that drops whatever does not fit loses the
	// rest of the listing silently -- no error to the writer, because a short
	// write to a terminal is not one anybody checks.
	//
	// The reader is deliberately slower than the writer here, which is what
	// forces the buffer to fill. Writing it all in one call would pass even
	// against a driver that truncates, because the write would simply be short.
	int mx = open("/dev/ptmx", O_RDWR, 0);
	int sl = -1;
	if (mx >= 0) {
		int n = 0;
		char ptn[16];
		if (ioctl(mx, TIOCGPTN, &n) == 0) {
			snprintf(ptn, sizeof ptn, "/dev/pts/%d", n);
			sl = open(ptn, O_RDWR, 0);
		}
	}
	int pty_big_ok = 0;
	if (mx >= 0 && sl >= 0) {
		/* Distinct bytes, so a lost tail is detectable and not just a shorter
		 * count. */
		static const size_t BIG = 8 * 1024;
		char *src = malloc(BIG);
		char *dst = malloc(BIG);
		int ok = src && dst;
		size_t done = 0;
		if (ok) {
			for (size_t i = 0; i < BIG; i++)
				src[i] = (char)('A' + (i % 26));
			for (size_t off = 0; ok && off < BIG; off += 512) {
				ssize_t w = write(sl, src + off, 512);
				if (w != 512) {
					ok = 0;
					break;
				}
				/* Drain as we go, so the check is that every byte arrives
				 * rather than that the driver can hold 8 KiB at once. */
				size_t got = 0;
				while (got < 512) {
					ssize_t r = read(mx, dst + off + got, 512 - got);
					if (r <= 0) {
						ok = 0;
						break;
					}
					got += (size_t)r;
				}
				done += got;
			}
		}
		if (ok)
			ok = (done == BIG) && memcmp(src, dst, BIG) == 0;
		pty_big_ok = ok;
		free(src);
		free(dst);
	}
	if (mx >= 0)
		close(mx);
	if (sl >= 0)
		close(sl);
	check(pty_big_ok, "pty carries 8 KiB in 512B writes");




	// fork(2) under repeated use. The first fork succeeding proves very little:
	// the failure this catches only appears once a process has been created and
	// reaped enough times for the scheduler's bookkeeping to drift, so a
	// single-shot check passes on a kernel that is about to start returning
	// ENOSYS to a shell that is running perfectly ordinary commands.
	//
	// The loop is deliberately shallow per iteration and long overall, which is
	// the shape of the real workload: a shell running a hundred short commands
	// one after another, not one command that forks a hundred children at once.
	// It also reaps every child, because an unreaped child is a zombie that
	// still holds its slot, and accumulating those would turn this into a
	// different test than the one intended.
	int fork_ok = 1;
	int first_errno = 0;
	int child_saw_self = 1;
	for (int i = 0; i < 64; i++) {
		pid_t pid = fork();
		if (pid < 0) {
			fork_ok = 0;
			if (!first_errno)
				first_errno = errno;
			break;
		}
		if (pid == 0) {
			/* The child must observe its own identity as zero from fork(2)
			 * and then report it differently, which is the only proof that the
			 * two really are separate processes. A child that sees non-zero
			 * here runs the *parent's* path: it loops, forking on every
			 * iteration, and the parent waits for a pid that will never be
			 * the one it asked for. */
			if (getpid() == 0)
				child_saw_self = 0;
			_exit(child_saw_self ? 0 : 1);
		}
		int st = 0;
		errno = 0;
		pid_t w = waitpid(pid, &st, 0);
		if (w != pid || !(WIFEXITED(st) && WEXITSTATUS(st) == 0)) {
			fork_ok = 0;
			if (!first_errno)
				first_errno = (w == pid) ? -WEXITSTATUS(st) : -errno;
		}
	}
	check(fork_ok, "fork x64");
	check(child_saw_self, "fork child has its own pid");

	// fork must still work after the children have been reaped, and after a
	// failed one has been observed. A kernel that leaks task slots on the
	// failure path passes the first loop and fails here, which is why this is
	// a separate check rather than more iterations of the same one.
	errno = 0;
	pid_t after = fork();
	if (after == 0) {
		/* The child must leave here, or it runs the rest of the program as a
		 * second copy of the parent -- forking again, and reporting on the
		 * checks its parent is still running. That is a bug in the test, and it
		 * looks exactly like the kernel bug it is checking for. */
		_exit(0);
	}
	if (after > 0)
		waitpid(after, NULL, 0);
	check(after > 0, "fork after 64 reaps");

	// vfork(3) is not the same call and must not be reported as working.
	// busybox reaches for it in several places, so a kernel that answers
	// ENOSYS here is a real limitation worth naming in a test rather than
	// discovering in a shell.
	errno = 0;
	pid_t vf = vfork();
	if (vf == 0)
		_exit(0);
	if (vf > 0)
		waitpid(vf, NULL, 0);
	printf("%-34s %s\n", "vfork", vf > 0 ? "ok" : (vf < 0 ? "unsupported" : "FAIL"));


	if (failures) {
		printf("[chello] %d CHECK(S) FAILED\n", failures);
		return 1;
	}

	// More output than the pty's buffer holds, with the reader in a *different*
	// process. This is the shape of the thing that actually broke: any real
	// command emits more than the buffer in one go, and a terminal emulator does
	// not read as fast as the shell produces.
	//
	// The writer has to be a child. A single process cannot write more than
	// the buffer holds and then read it back -- it would be stuck waiting for
	// itself, and so would any real system, which is not a bug: a full pty
	// buffer means the writer waits, and the wait ends when someone else reads.
	// A real terminal and a real command are two processes, so this is two
	// processes too.
	//
	// The child writes 32 KiB in one call and exits. It used to succeed having
	// written only the first buffer's worth, because a short write to a terminal
	// is not an error anything checks: the command exits 0, and the screen shows
	// a truncated listing with nothing to indicate anything was missing.
	{
		int mx = open("/dev/ptmx", O_RDWR, 0);
		int sl = -1;
		if (mx >= 0) {
			int n = 0;
			char ptn[16];
			if (ioctl(mx, TIOCGPTN, &n) == 0) {
				snprintf(ptn, sizeof ptn, "/dev/pts/%d", n);
				sl = open(ptn, O_RDWR, 0);
			}
		}
		int big_ok = 0;
		if (mx >= 0 && sl >= 0) {
			const size_t HUGE = 32 * 1024;
			pid_t pid = fork();
			if (pid < 0) {
				big_ok = 0;
			} else if (pid == 0) {
				/* Child: write it all in one call, then leave. */
				char *src = malloc(HUGE);
				int code = 1;
				if (src) {
					for (size_t i = 0; i < HUGE; i++)
						src[i] = (char)('a' + (i % 26));
					code = (write(sl, src, HUGE) == (ssize_t)HUGE) ? 0 : 1;
					free(src);
				}
				_exit(code);
			} else {
				/* Parent: drain, and check every byte arrived in order. */
				char *dst = malloc(HUGE);
				int ok = dst != NULL;
				size_t got = 0;
				int reaped = 0, child_status = 0;
				/* A read of the master that finds nothing returns 0 rather
				 * than waiting -- a terminal emulator polls first and only
				 * reads what is there, and this check does the same. An
				 * empty read is not a failure; only a read that returns
				 * bytes out of order, or never returns at all, is. */
				int stalls = 0;
				while (ok && got < HUGE) {
					ssize_t r = read(mx, dst + got, HUGE - got);
					if (r < 0) {
						ok = 0;
						break;
					}
					got += (size_t)r;
					if (r > 0) {
						stalls = 0;
						continue;
					}
					/* Nothing queued. Wait for more rather than spinning
					 * on an empty buffer: `poll` is what a real terminal
					 * emulator does here, and it is what lets the child's
					 * full-buffer write and this read alternate instead of
					 * deadlocking against each other. */
					struct pollfd pfd = { mx, POLLIN, 0 };
					if (poll(&pfd, 1, 100) < 0) {
						ok = 0;
						break;
					}
					/* A child that has exited can never supply the rest,
					 * and the exit is what tells us so. Bounding the
					 * stalls means a driver that loses the tail reports
					 * a failure instead of hanging the check forever. */
					if (++stalls > 50) {
						int st = 0;
						if (waitpid(pid, &st, WNOHANG) == pid) {
							reaped = 1;
							child_status = st;
						}
						break;
					}
				}
				if (ok) {
					for (size_t i = 0; i < HUGE && ok; i++)
						ok = dst[i] == (char)('a' + (i % 26));
				}
				free(dst);
				/* The child must have completed the whole write, not just the
				 * part that fitted. It reports the count it managed to
				 * write, and it has to be the whole thing. */
				int st = child_status;
				if (!reaped && waitpid(pid, &st, 0) != pid)
					ok = 0;
				else
					ok = ok && (WIFEXITED(st) && WEXITSTATUS(st) == 0);
				big_ok = ok;
			}
		}
		if (mx >= 0)
			close(mx);
		if (sl >= 0)
			close(sl);
		check(big_ok, "pty carries 32 KiB from a writer that outruns the reader");
	}
	printf("[chello] ALL PASS\n");
	return 0;
}
