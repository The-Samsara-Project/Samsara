//! Boot-time coverage for the PTY terminal-control ABI.
//!
//! Opens both ends of the first pty pair so the test can drive the master and
//! observe what the line discipline does to the bytes on their way to the
//! slave. That is the only way to cover the parts of a terminal that are not
//! just getter/setter round-trips: canonical line editing, `^D` end-of-file,
//! literal-next, signal characters, and blocking versus non-blocking reads.

#![no_std]
#![no_main]

use nutcracker_rt::{println, syscall};
use core::result::Result::{Err, Ok};
use core::sync::atomic::Ordering;

const O_RDWR: u64 = 2;

// c_lflag bits, matching the kernel's termios.
const ISIG: u32 = 0x0000_0001;
const ICANON: u32 = 0x0000_0002;
const ECHO: u32 = 0x0000_0008;
const ECHOE: u32 = 0x0000_0010;
const IEXTEN: u32 = 0x0000_8000;

// c_iflag bits.
const ICRNL: u32 = 0x0000_0200;
const IXON: u32 = 0x0000_0800;

// c_cc indices.
const VEOF: usize = 4;
const VSTART: usize = 8;
const VSTOP: usize = 9;
const VWERASE: usize = 14;
const VLNEXT: usize = 15;

static mut FAILED: bool = false;

/// SIGINT (2).
const SIGINT: u32 = 2;

/// Counts SIGINT deliveries from the terminal's ISIG handling.
static INT_HITS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Handler for the terminal-generated SIGINT.
unsafe extern "C" fn on_int(_sig: i32) {
    INT_HITS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
}

fn check(ok: bool, message: &str) {
    if ok {
        println!("[termiostst] OK {message}");
    } else {
        println!("[termiostst] FAIL {message}");
        // SAFETY: single-threaded test body; no other thread touches this.
        unsafe { FAILED = true };
    }
}

fn termios_of(fd: usize) -> syscall::Termios {
    let mut t = unsafe { core::mem::zeroed::<syscall::Termios>() };
    let _ = syscall::ioctl(fd, syscall::TCGETS, &mut t);
    t
}

/// Feed `bytes` to the line discipline by writing them on the master, then
/// read the result back off the slave.
fn roundtrip(master: usize, slave: usize, bytes: &[u8], label: &str) -> Vec<u8> {
    if syscall::write(master, bytes).is_err() {
        check(false, "master write");
        return Vec::new();
    }
    let mut buf = [0u8; 128];
    match syscall::read(slave, &mut buf) {
        Ok(n) => buf[..n].to_vec(),
        Err(e) => {
            check(false, label);
            println!("[termiostst]   read error {e}");
            Vec::new()
        }
    }
}

extern crate alloc;
use alloc::vec::Vec;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    // Allocate a private pair rather than using the boot console's `/dev/pts/0`.
    // The installer is itself blocked reading that terminal, so a shared pair
    // would let it consume this test's keystrokes — whichever reader won the
    // race would take the data, and the other would wait forever.
    let master = match syscall::open("/dev/ptmx", O_RDWR, 0) {
        Ok(fd) => fd,
        Err(e) => {
            println!("[termiostst] FAIL open /dev/ptmx: {e}");
            syscall::proc_exit_code(1);
        }
    };
    // Linux reports the pair index with TIOCGPTN; the slave lives at
    // /dev/pts/<index> -- in a subdirectory, as Linux lays pty slaves out. That
    // is also the path `ptsname(3)` reports, so building it here exercises the
    // same thing a real program would rely on.
    let mut index = 0i32;
    check(
        syscall::ioctl(master, syscall::TIOCGPTN, &mut index).is_ok() && index > 0,
        "TIOCGPTN reports a freshly allocated pair",
    );
    let slave_path = [b'/'; 64];
    let mut path = [0u8; 32];
    let text = b"/dev/pts/";
    path[..text.len()].copy_from_slice(text);
    let mut digits = [0u8; 8];
    let mut n = index as u32;
    let mut len = 0;
    if n == 0 {
        digits[0] = b'0';
        len = 1;
    }
    while n > 0 {
        digits[len] = b'0' + (n % 10) as u8;
        n /= 10;
        len += 1;
    }
    let mut off = text.len();
    for i in (0..len).rev() {
        path[off] = digits[i];
        off += 1;
    }
    let _ = slave_path;
    let fd = match syscall::open(core::str::from_utf8(&path[..off]).unwrap_or("/dev/pts/0"), O_RDWR, 0) {
        Ok(fd) => fd,
        Err(e) => {
            println!("[termiostst] FAIL open allocated slave: {e}");
            syscall::proc_exit_code(1);
        }
    };
    check(fd != usize::MAX, "allocated slave opened");

    // --- termios round-trip ---------------------------------------------
    let mut original = termios_of(fd);
    check(original.c_lflag != 0, "TCGETS returns defaults");

    let mut changed = original;
    changed.c_lflag ^= ECHOE;
    check(syscall::ioctl(fd, syscall::TCSETS, &mut changed).is_ok(), "TCSETS");
    let read_back = termios_of(fd);
    check(read_back.c_lflag == changed.c_lflag, "termios round-trip");
    check(
        syscall::ioctl(fd, syscall::TCGETA, &mut changed).is_ok(),
        "TCGETA (BSD spelling)",
    );
    check(
        syscall::ioctl(fd, syscall::TCSETA, &mut original).is_ok(),
        "TCSETA (BSD spelling)",
    );

    // --- isatty ----------------------------------------------------------
    // A terminal answers TCGETS; anything else must answer ENOTTY, which is
    // exactly how a libc decides whether a descriptor is a terminal.
    let mut probe = unsafe { core::mem::zeroed::<syscall::Termios>() };
    check(syscall::ioctl(fd, syscall::TCGETS, &mut probe).is_ok(), "isatty(tty) via TCGETS");
    let notty = match syscall::open("/dev/kbd0", O_RDWR, 0) {
        Ok(f) => f,
        Err(_) => {
            println!("[termiostst] SKIP isatty(non-tty): /dev/kbd0 unavailable");
            usize::MAX
        }
    };
    if notty != usize::MAX {
        let mut p2 = unsafe { core::mem::zeroed::<syscall::Termios>() };
        let r = syscall::ioctl(notty, syscall::TCGETS, &mut p2);
        // ENOTTY is what makes `isatty` work: a libc probes with TCGETS and a
        // non-terminal refuses.
        check(r == Err(-25), "isatty(non-tty) answers ENOTTY");
        let _ = syscall::close(notty);
    }

    // --- window size -----------------------------------------------------
    let mut size = syscall::Winsize { ws_row: 0, ws_col: 0, ws_xpixel: 0, ws_ypixel: 0 };
    check(syscall::ioctl(fd, syscall::TIOCGWINSZ, &mut size).is_ok(), "TIOCGWINSZ");
    let mut set_size = syscall::Winsize { ws_row: 42, ws_col: 120, ws_xpixel: 0, ws_ypixel: 0 };
    check(syscall::ioctl(fd, syscall::TIOCSWINSZ, &mut set_size).is_ok(), "TIOCSWINSZ");
    let mut size_back = syscall::Winsize { ws_row: 0, ws_col: 0, ws_xpixel: 0, ws_ypixel: 0 };
    check(
        syscall::ioctl(fd, syscall::TIOCGWINSZ, &mut size_back).is_ok()
            && size_back.ws_row == 42
            && size_back.ws_col == 120,
        "winsize round-trip",
    );

    // --- process groups --------------------------------------------------
    //
    // The order here is the order a real program uses, and it matters: a spawned
    // child inherits its parent's process group, so this process's own pid is
    // not a process group id until it creates one. Handing the terminal to that
    // pid first would be asking to set the foreground to a group that does not
    // exist, and a correct kernel refuses that -- which is what it did.
    let pid = syscall::get_pid() as u32;

    // Before: this process is a member of some inherited group, not its own.
    let inherited = syscall::getpgrp();
    check(inherited != 0, "getpgrp reports a group");
    check(
        inherited != pid || syscall::setsid().is_ok(),
        "a non-leader does not claim to lead a group",
    );

    // Become a group leader. This is the step that makes `pid` a valid pgid.
    check(syscall::setpgid(0, 0).is_ok(), "setpgid(0,0) creates a group");
    check(syscall::getpgrp() == pid, "getpgrp is our own pid after setpgid");
    // The session is unchanged by creating a process group: those are separate
    // things, and conflating them is exactly the bug this ordering would hide.
    check(
        syscall::getsid(0) != 0 && syscall::getsid(0) as u32 != pid,
        "setpgid did not make us a session leader",
    );

    // Now the terminal can be handed to that group.
    let mut pgid = pid;
    check(
        syscall::ioctl(fd, syscall::TIOCSPGRP, &mut pgid).is_ok(),
        "TIOCSPGRP to our own group",
    );
    let mut pgid_back = 0u32;
    check(
        syscall::ioctl(fd, syscall::TIOCGPGRP, &mut pgid_back).is_ok() && pgid_back == pid,
        "foreground pgrp round-trip",
    );
    // Setting the foreground to a group that does not exist must be refused, or
    // the terminal ends up owned by a group nothing can be waited on or
    // signalled in.
    let mut ghost = pid.wrapping_add(0x7000_0000);
    check(
        syscall::ioctl(fd, syscall::TIOCSPGRP, &mut ghost).is_err(),
        "TIOCSPGRP refuses a group that does not exist",
    );
    let mut sid = 0u32;
    check(
        syscall::ioctl(fd, syscall::TIOCGSID, &mut sid).is_ok() && sid == syscall::getsid(0) as u32,
        "TIOCGSID reports our session",
    );
    check(
        syscall::setpgid(0, syscall::get_pid() as u32).is_ok(),
        "setpgid(self) is allowed",
    );
    // A group in another session must be refused; a pid that does not exist is
    // ESRCH rather than a silent success.
    check(
        syscall::setpgid(0, 999_999).is_err(),
        "setpgid to a nonexistent group fails",
    );

    // --- canonical input: a whole line is withheld until it is terminated -
    check(syscall::write(master, b"par").is_ok(), "write partial line");
    let mut avail = 0u32;
    check(
        syscall::ioctl(fd, syscall::FIONREAD, &mut avail).is_ok() && avail == 0,
        "FIONREAD: partial line is not readable yet",
    );
    check(syscall::write(master, b"tial\n").is_ok(), "terminate line");
    let mut buf = [0u8; 128];
    let n = syscall::read(fd, &mut buf).unwrap_or(0);
    check(&buf[..n] == b"partial\n", "canonical line delivered on newline");
    check(
        syscall::ioctl(fd, syscall::FIONREAD, &mut avail).is_ok() && avail == 0,
        "FIONREAD drains after read",
    );

    // --- ^D on an empty line is end-of-file ------------------------------
    check(syscall::write(master, b"\x04").is_ok(), "write ^D");
    let n = syscall::read(fd, &mut buf).unwrap_or(usize::MAX);
    check(n == 0, "^D on an empty line reads as EOF");

    // EOF is consumed, not sticky: the terminal still works afterwards.
    check(syscall::write(master, b"after\n").is_ok(), "write after EOF");
    let n = syscall::read(fd, &mut buf).unwrap_or(0);
    check(&buf[..n] == b"after\n", "terminal usable after EOF");

    // --- ^D with pending text delivers it without waiting for a newline ---
    check(syscall::write(master, b"abc\x04").is_ok(), "write text then ^D");
    let n = syscall::read(fd, &mut buf).unwrap_or(0);
    check(&buf[..n] == b"abc", "^D delivers pending text, no newline needed");

    // --- erase and word-erase -------------------------------------------
    let out = roundtrip(master, fd, b"abcd\x7f\n", "VERASE");
    check(out == b"abc\n", "VERASE removes one character");
    let out = roundtrip(master, fd, b"foo bar\x17\n", "VWERASE");
    check(out == b"foo \n", "VWERASE removes the last word");

    // --- VLNEXT takes the next byte literally -----------------------------
    // With ISIG on, a bare ^C would raise SIGINT and deliver nothing. Quoting
    // it with ^V must put a literal 0x03 in the read buffer instead. The line
    // needs a terminator: canonical mode correctly withholds a partial line,
    // so a read here would (rightly) block forever.
    let out = roundtrip(master, fd, b"a\x16\x03\n", "VLNEXT");
    check(
        out.len() == 3 && out[1] == 0x03 && out[2] == b'\n',
        "VLNEXT passes ^C through literally",
    );

    // --- ISIG delivers to the foreground process group --------------------
    // A real handler is required: the test is itself the foreground group, so
    // an unhandled SIGINT would simply kill it — which is the correct behavior
    // and is exactly why the handler has to be installed first.
    let mut t = termios_of(fd);
    check(t.c_lflag & ISIG != 0, "ISIG is on by default");
    t.c_cc[VEOF] = 4;
    let _ = syscall::ioctl(fd, syscall::TCSETS, &mut t);

    let act = syscall::SigAction {
        sa_handler: on_int as *const () as usize,
        sa_flags: 0,
        sa_restorer: syscall::sigreturn_trampoline as *const () as *const () as usize,
        sa_mask: 0,
    };
    check(
        syscall::sigaction(SIGINT, Some(&act), None).is_ok(),
        "install SIGINT handler",
    );
    INT_HITS.store(0, Ordering::Relaxed);

    // ^C must not reach the reader as data...
    let _ = syscall::write(master, b"\x03");
    // ...it must interrupt this process, because we are the foreground group.
    let mut spins = 0;
    while INT_HITS.load(Ordering::Relaxed) == 0 && spins < 200 {
        syscall::yield_now();
        spins += 1;
    }
    check(
        INT_HITS.load(Ordering::Relaxed) > 0,
        "ISIG: ^C signals the foreground process group",
    );
    let mut buf2 = [0u8; 64];
    let mut pf_int = [syscall::PollFd { fd: fd as i32, events: 0x001, revents: 0 }];
    let ready = syscall::poll(&mut pf_int, 20).unwrap_or(0);
    if ready > 0 {
        let _ = syscall::read(fd, &mut buf2);
        check(false, "ISIG: ^C is not delivered to the reader");
    } else {
        check(true, "ISIG: ^C is not delivered to the reader");
    }

    // --- XON/XOFF flow control -------------------------------------------
    // XON/XOFF stops *output*, not input. Keystrokes are still delivered to the
    // reader; what stops is the echo the terminal would draw. Asserting the
    // opposite would pin the terminal to the wrong behavior.
    let mut t = termios_of(fd);
    t.c_iflag |= IXON;
    t.c_cc[VSTOP] = 0x13; // ^S
    t.c_cc[VSTART] = 0x11; // ^Q
    let _ = syscall::ioctl(fd, syscall::TCSETS, &mut t);
    let _ = syscall::ioctl(fd, syscall::TCXONC, &mut syscall::TCOOFF);
    // Drain echoes left by the earlier checks; only what arrives *after* the
    // stop is meaningful here.
    let mut echo = [0u8; 64];
    while syscall::read(master, &mut echo).unwrap_or(0) > 0 {}
    let _ = syscall::write(master, b"x\n");
    // No echo reaches the master while output is stopped.
    let mut pfm = [syscall::PollFd { fd: master as i32, events: 0x001, revents: 0 }];
    check(
        syscall::poll(&mut pfm, 20).unwrap_or(0) == 0,
        "TCOOFF suppresses echo on the master",
    );
    // Input itself is still delivered.
    let n = syscall::read(fd, &mut buf).unwrap_or(0);
    check(&buf[..n] == b"x\n", "TCOOFF does not withhold input from the reader");

    // ^Q itself must be seen by the discipline to lift the stop, so it has to
    // arrive as input rather than as an echo.
    let _ = syscall::write(master, b"\x11");
    let _ = syscall::write(master, b"y\n");
    let mut pfm2 = [syscall::PollFd { fd: master as i32, events: 0x001, revents: 0 }];
    check(
        syscall::poll(&mut pfm2, 50).unwrap_or(0) > 0,
        "^Q resumes output and echo",
    );
    let n = syscall::read(fd, &mut buf).unwrap_or(0);
    check(&buf[..n] == b"y\n", "input after ^Q is delivered");
    // Drain the echo the master accumulated so later checks see a clean slate.
    while syscall::read(master, &mut echo).unwrap_or(0) > 0 {}

    // --- tcflush discards buffered input ---------------------------------
    let _ = syscall::write(master, b"discard me\n");
    check(syscall::ioctl(fd, syscall::TCFLSH, &mut syscall::TCIFLUSH).is_ok(), "TCFLSH(TCIFLUSH)");
    let mut pf4 = [syscall::PollFd { fd: fd as i32, events: 0x001, revents: 0 }];
    check(syscall::poll(&mut pf4, 20).unwrap_or(0) == 0, "TCIFLUSH discarded the buffered line");

    // --- O_NONBLOCK returns EAGAIN instead of waiting --------------------
    let nb = match syscall::open(core::str::from_utf8(&path[..off]).unwrap_or("/dev/pts/0"), O_RDWR | syscall::O_NONBLOCK, 0) {
        Ok(f) => f,
        Err(e) => {
            println!("[termiostst] FAIL reopen O_NONBLOCK: {e}");
            syscall::proc_exit_code(1);
        }
    };
    let mut nbuf = [0u8; 32];
    check(
        syscall::read(nb, &mut nbuf) == Err(-11),
        "O_NONBLOCK read with no data returns EAGAIN",
    );
    let _ = syscall::close(nb);

    // --- raw mode passes bytes straight through --------------------------
    // Clear ISIG as `cfmakeraw` does: with it set, 0x03 is VINTR and would be
    // consumed as a signal instead of delivered, which is correct but is not
    // what this check is about.
    let mut raw = termios_of(fd);
    raw.c_lflag &= !(ICANON | ECHO | ECHOE | ISIG | IEXTEN);
    raw.c_iflag &= !(ICRNL | IXON);
    raw.c_oflag = 0;
    let _ = syscall::ioctl(fd, syscall::TCSETS, &mut raw);
    let out = roundtrip(master, fd, b"\x01\x02\x03\x04", "raw read");
    check(out == b"\x01\x02\x03\x04", "raw mode delivers bytes with no editing");
    // Leaving canonical mode must release a partially typed line rather than
    // discarding it.
    let _ = syscall::write(master, b"half");
    let mut back = termios_of(fd);
    back.c_lflag |= ICANON | ECHO | IEXTEN | ISIG;
    let _ = syscall::ioctl(fd, syscall::TCSETS, &mut back);
    let n = syscall::read(fd, &mut buf).unwrap_or(0);
    check(&buf[..n] == b"half", "leaving raw mode flushes the pending line");

    let _ = syscall::close(fd);
    let _ = syscall::close(master);

    // SAFETY: single-threaded test body; no other thread touches this.
    if unsafe { FAILED } {
        println!("[termiostst] SOME CHECKS FAILED");
        syscall::proc_exit_code(1);
    }
    println!("[termiostst] ALL PASS");
    syscall::proc_exit_code(0);
}
