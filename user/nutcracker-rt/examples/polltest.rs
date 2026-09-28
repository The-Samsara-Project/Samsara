// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa
//
// Ring-3 exercise for the kernel `POLL` syscall:
//
//   1. non-blocking: an empty pipe read end (writer still open) reports no
//      readiness and a `timeout_ms = 0` poll returns immediately;
//   2. POLLIN + retransfer: writing to the pipe makes the read end ready, and
//      the data still round-trips afterward;
//   3. POLLOUT: an empty pipe write end is writable, and once every reader is
//      closed it flips to POLLERR (the pre-image of an EPIPE write);
//   4. POLLHUP: closing every writer makes the read end readable-EOF;
//   5. POLLNVAL: a closed descriptor reports POLLNVAL;
//   6. timeout: a blocking-poll on a silent pipe returns after roughly the
//      requested milliseconds with nothing ready;
//   7. device: /dev/kbd0 (when present) is at least writable, and poll does
//      not fault on a real driver-backed node.

#![no_std]
#![no_main]

use nutcracker_rt::println;
use nutcracker_rt::syscall::{self, poll_events, PollFd};

/// Abort the whole test with a hard failure.
fn fail(msg: &str) -> ! {
    println!("[polltest] FAIL: {}", msg);
    syscall::proc_exit_code(255);
}

/// Assert a condition or abort.
macro_rules! check {
    ($cond:expr, $msg:literal) => {
        if !($cond) {
            fail($msg);
        }
    };
}

/// Poll one descriptor for `events`, returning its `revents`.
fn poll_once(fd: usize, events: u16, timeout_ms: i64) -> Result<u16, i64> {
    let mut p = [PollFd {
        fd: fd as i32,
        events,
        revents: 0,
    }];
    let ready = syscall::poll(&mut p, timeout_ms)?;
    check!(ready <= 1, "poll reported more ready fds than supplied");
    Ok(p[0].revents)
}

/// Test 1+2+3: non-blocking probe, POLLIN on write, POLLOUT, round-trip.
fn test_readiness() -> Result<(), i64> {
    let (r, w) = syscall::pipe()?;

    // 1. Empty pipe, writer still open: not readable, poll(0) returns 0.
    let rev = poll_once(r, poll_events::POLLIN, 0)?;
    check!(rev == 0, "empty pipe reported readable");

    // 3. An empty pipe write end is immediately writable.
    let rev = poll_once(w, poll_events::POLLOUT, 0)?;
    check!(rev & poll_events::POLLOUT != 0, "empty pipe not writable");

    // 2. Writing makes the read end POLLIN.
    let n = syscall::write(w, b"poll!")?;
    check!(n == 5, "short write");
    let rev = poll_once(r, poll_events::POLLIN, 100)?;
    check!(rev & poll_events::POLLIN != 0, "POLLIN not reported after write");

    // Data still transfers normally after the poll worked.
    let mut buf = [0u8; 16];
    let n = syscall::read(r, &mut buf)?;
    check!(n == 5 && &buf[..5] == b"poll!", "read after poll mismatch");

    syscall::close(r)?;
    syscall::close(w)?;
    Ok(())
}

/// Test 3b: closing every reader flips the write end from POLLOUT to POLLERR.
fn test_epipe_readiness() -> Result<(), i64> {
    let (r, w) = syscall::pipe()?;
    println!("[polltest] epipe r={} w={}", r, w);

    let rev = poll_once(w, poll_events::POLLOUT, 0)?;
    println!("[polltest] epipe pre-close rev={:#06x}", rev);
    check!(rev & poll_events::POLLOUT != 0, "write end not writable at start");

    syscall::close(r)?;
    println!("[polltest] epipe reader closed");
    let rev = poll_once(w, poll_events::POLLOUT, 0)?;
    println!("[polltest] epipe post-close rev={:#06x}", rev);
    check!(
        rev & poll_events::POLLOUT == 0 && rev & poll_events::POLLERR != 0,
        "write end did not report POLLERR with readers gone"
    );

    syscall::close(w)?;
    Ok(())
}

/// Test 4: closing every writer turns the read end into readable-EOF (POLLHUP).
fn test_eof_readiness() -> Result<(), i64> {
    println!("[polltest] eof test enter");
    let (r, w) = syscall::pipe()?;
    println!("[polltest] eof pipe made");

    let rev = poll_once(r, poll_events::POLLIN, 0)?;
    println!("[polltest] eof fresh rev={:#06x}", rev);
    check!(rev == 0, "fresh pipe read end reported readable");

    syscall::close(w)?;
    println!("[polltest] eof writer closed");
    let rev = poll_once(r, poll_events::POLLIN, 100)?;
    println!("[polltest] eof after-close rev={:#06x}", rev);
    check!(
        rev & poll_events::POLLIN != 0 && rev & poll_events::POLLHUP != 0,
        "EOF pipe read end did not report POLLIN|POLLHUP"
    );

    let mut buf = [0u8; 8];
    let n = syscall::read(r, &mut buf)?;
    check!(n == 0, "EOF pipe read did not return 0");

    syscall::close(r)?;
    Ok(())
}

/// Test 5: a closed descriptor reports POLLNVAL.
fn test_nval() -> Result<(), i64> {
    let rev = poll_once(0xfff, poll_events::POLLIN, 0)?;
    check!(rev & poll_events::POLLNVAL != 0, "bad fd did not report POLLNVAL");
    Ok(())
}

/// Test 6: a blocking poll on a silent pipe honors its ~30 ms timeout.
fn test_timeout() -> Result<(), i64> {
    let (r, w) = syscall::pipe()?;

    let start = syscall::uptime_ms();
    let rev = poll_once(r, poll_events::POLLIN, 30)?;
    let elapsed = syscall::uptime_ms().saturating_sub(start);

    check!(rev == 0, "silent pipe became ready while polling");
    check!(
        elapsed >= 20 && elapsed < 2000,
        "timeout poll returned outside expected duration"
    );

    syscall::close(r)?;
    syscall::close(w)?;
    Ok(())
}

/// Test 7: poll on a driver-backed device node (/dev/kbd0) for POLLIN|POLLOUT.
fn test_device() -> Result<(), i64> {
    let fd = match syscall::open("/dev/kbd0", syscall::O_RDWR, 0) {
        Ok(fd) => fd,
        Err(_) => {
            println!("[polltest] no /dev/kbd0; device test skipped");
            return Ok(());
        }
    };

    // Writable always; POLLIN only if scancodes are already queued.
    let rev = poll_once(fd, poll_events::POLLIN | poll_events::POLLOUT, 0)?;
    check!(rev == 0 || rev & poll_events::POLLOUT != 0, "kbd0 poll bogus revents");

    // A blocking poll on the device must not spin or fault (returns quickly
    // with just POLLOUT unless a key is pending).
    let rev = poll_once(fd, poll_events::POLLOUT, 50)?;
    check!(rev & poll_events::POLLOUT != 0, "kbd0 not considered writable");

    syscall::close(fd)?;
    Ok(())
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    println!("[polltest] pid={} starting", syscall::get_epid());
    let mut ok = true;
    ok &= match test_readiness() {
        Ok(()) => {
            println!("[polltest] PASS: non-blocking / POLLIN / POLLOUT / round-trip");
            true
        }
        Err(e) => {
            println!("[polltest] FAIL: readiness (errno {})", e);
            false
        }
    };
    ok &= match test_epipe_readiness() {
        Ok(()) => {
            println!("[polltest] PASS: POLLERR on EPIPE");
            true
        }
        Err(e) => {
            println!("[polltest] FAIL: POLLERR (errno {})", e);
            false
        }
    };
    ok &= match test_eof_readiness() {
        Ok(()) => {
            println!("[polltest] PASS: POLLIN|POLLHUP on EOF");
            true
        }
        Err(e) => {
            println!("[polltest] FAIL: POLLHUP (errno {})", e);
            false
        }
    };
    ok &= match test_nval() {
        Ok(()) => {
            println!("[polltest] PASS: POLLNVAL");
            true
        }
        Err(e) => {
            println!("[polltest] FAIL: POLLNVAL (errno {})", e);
            false
        }
    };
    ok &= match test_timeout() {
        Ok(()) => {
            println!("[polltest] PASS: timeout");
            true
        }
        Err(e) => {
            println!("[polltest] FAIL: timeout (errno {})", e);
            false
        }
    };
    ok &= match test_device() {
        Ok(()) => {
            println!("[polltest] PASS: device poll");
            true
        }
        Err(e) => {
            println!("[polltest] FAIL: device poll (errno {})", e);
            false
        }
    };

    if ok {
        println!("[polltest] ALL PASS");
        syscall::proc_exit_code(0);
    } else {
        println!("[polltest] SOME TESTS FAILED");
        syscall::proc_exit_code(1);
    }
}