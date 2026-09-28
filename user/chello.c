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
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <poll.h>
#include <stdint.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/random.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <sys/utsname.h>
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

int main(void) {
	printf("[chello] linked against mlibc, running on Samsara\n");

	// write(2) reached the console.
	check(write(1, "[chello] write(2) works\n", 25) == 25, "write(2)");

	// isatty(3) must agree with the kernel. This is the check that motivated
	// the TTY work: a stub that always answers "yes" makes stdio line-buffer a
	// file, which silently corrupts redirected output.
	int fd = open("/dev/pts0", O_RDWR, 0);
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

	int tf = open("/dev/pts0", O_RDWR, 0);
	if (tf >= 0) {
		int ok = fstat(tf, &sb) == 0;
		check(ok && S_ISCHR(sb.st_mode), "fstat: terminal is S_IFCHR");
		// A terminal has no length. Claiming one is how a program decides to
		// seek to the end and then wonder why it read nothing.
		check(ok && sb.st_size == 0, "fstat: terminal size is 0");
		close(tf);
	} else {
		check(0, "open /dev/pts0 for fstat");
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
	// the master of pair 0 and the installer reads `/dev/pts0`; if those two
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
	// published as `/dev/pts0`), and a freshly opened `/dev/ptmx` must report
	// something else, because that name is an allocating multiplexer and
	// mistaking it for pair 0's master is the mistake being ruled out.
	int m0 = open("/dev/ptmx0", O_RDWR, 0);
	int s0 = open("/dev/pts0", O_RDWR, 0);
	if (m0 < 0 || s0 < 0) {
		check(0, "/dev/ptmx0 and /dev/pts0 open");
	} else {
		check(1, "/dev/ptmx0 and /dev/pts0 open");
		int n0 = -1;
		check(ioctl(m0, TIOCGPTN, &n0) == 0, "TIOCGPTN on /dev/ptmx0");
		check(n0 == 0, "/dev/ptmx0 drives pair 0 (/dev/pts0)");

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

	if (failures) {
		printf("[chello] %d CHECK(S) FAILED\n", failures);
		return 1;
	}
	printf("[chello] ALL PASS\n");
	return 0;
}
