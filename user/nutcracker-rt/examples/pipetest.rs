// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa
//
// Ring-3 exercise for the kernel pipe implementation:
//
//   1. same-process echo: write bytes in, read the same bytes back out, then
//      verify that closing the write end produces EOF on the read end;
//   2. fork + gate: a child streams 73,728 bytes (more than the 64 KiB pipe
//      buffer) to its parent, which verifies every byte and sees EOF once the
//      child closes. The payload deliberately exceeds the buffer so *both*
//      write-blocking (child waits for room) and read-blocking (parent waits
//      for data) are exercised, regardless of scheduling order. A second
//      "gate" pipe sequences the two ends deterministically;
//   3. EPIPE: after every reader end is closed, a write must fail with -32
//      rather than block or succeed.

#![no_std]
#![no_main]

use nutcracker_rt::println;
use nutcracker_rt::syscall;

/// Deterministic byte for absolute stream position `i`.
fn span_byte(i: usize) -> u8 {
    (i.wrapping_mul(37) as u8).wrapping_add(0x5a)
}

/// Fill `buf` with the byte pattern for absolute offsets `base..base+len`.
fn fill(buf: &mut [u8], base: usize) {
    for (i, b) in buf.iter_mut().enumerate() {
        *b = span_byte(base + i);
    }
}

/// Verify `buf` holds the pattern for absolute offsets `base..base+len`.
fn matches(buf: &[u8], base: usize) -> bool {
    buf.iter().enumerate().all(|(i, &b)| b == span_byte(base + i))
}

/// Abort the whole test with a hard failure.
fn fail(msg: &str) -> ! {
    println!("[pipetest] FAIL: {}", msg);
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

/// Test 1: same-process echo + EOF on empty pipe with no writers.
fn test_echo() -> Result<(), i64> {
    let (r, w) = syscall::pipe()?;
    let msg = b"hello, pipes!";
    let n = syscall::write(w, msg)?;
    check!(n == msg.len(), "echo: short write");

    let mut buf = [0u8; 64];
    let n = syscall::read(r, &mut buf)?;
    check!(n == msg.len() && &buf[..n] == msg, "echo: round-trip mismatch");
    println!("[pipetest] echo ok: {} bytes", n);

    // No more data and the writer is still open: a read must not return.
    // We cannot observe "did not return" directly (no threads), so close the
    // writer and assert the *next* read yields EOF instead of blocking.
    syscall::close(w)?;
    let n = syscall::read(r, &mut buf)?;
    check!(n == 0, "echo: expected EOF after closing writer");
    println!("[pipetest] EOF on empty pipe with no writers ok");
    syscall::close(r)?;
    Ok(())
}

/// Test 2: fork a child that streams more than the buffer size through the
/// pipe; the parent verifies the stream and sees EOF when the child closes.
fn test_fork_stream() -> Result<(), i64> {
    const CHUNK: usize = 2048;
    const CHUNKS: usize = 36;
    const TOTAL: usize = CHUNK * CHUNKS; // 73,728 B > 64 KiB buffer

    let (data_r, data_w) = syscall::pipe()?;
    let (gate_r, gate_w) = syscall::pipe()?;

    let pid = syscall::fork();
    check!(pid >= 0, "fork failed");

    if pid == 0 {
        // Child keeps only the data write end and the gate read end.
        let _ = syscall::close(data_r);
        let _ = syscall::close(gate_w);

        // Wait for the parent to start reading before sending anything.
        let mut b = [0u8; 1];
        match syscall::read(gate_r, &mut b) {
            Ok(1) => {}
            Ok(n) => {
                println!("[pipetest] child gate got {} bytes", n);
                syscall::proc_exit_code(3);
            }
            Err(e) => {
                println!("[pipetest] child gate read error {}", e);
                syscall::proc_exit_code(3);
            }
        }
        let _ = syscall::close(gate_r);

        let mut chunk = [0u8; CHUNK];
        for c in 0..CHUNKS {
            fill(&mut chunk, c * CHUNK);
            // Keep trying until the whole chunk lands; large totals and a
            // 64 KiB buffer force the kernel to block us for room somewhere.
            loop {
                match syscall::write(data_w, &chunk) {
                    Ok(n) if n == chunk.len() => break,
                    Ok(n) => println!("[pipetest] child partial write {}", n),
                    Err(e) => {
                        println!("[pipetest] child data write error {}", e);
                        syscall::proc_exit_code(3);
                    }
                }
                syscall::yield_now();
            }
            if (c + 1) % 12 == 0 {
                println!("[pipetest] child wrote through byte {}", (c + 1) * CHUNK);
            }
        }
        let _ = syscall::close(data_w);
        println!("[pipetest] child wrote {} bytes and closed the write end", TOTAL);
        syscall::proc_exit_code(7);
    }

    // Parent keeps only the data read end and the gate write end.
    let _ = syscall::close(data_w);
    let _ = syscall::close(gate_r);

    // Release the child, then drain. The child cannot send until it has the
    // gate byte, so the parent's first read here runs before any data exists
    // and blocks until the child's writes wake it.
    syscall::write(gate_w, b"g")?;
    println!("[pipetest] parent: blocking read until the child writes");

    let mut total = 0usize;
    let mut buf = [0u8; CHUNK];
    loop {
        let n = syscall::read(data_r, &mut buf)?;
        if n == 0 {
            break;
        }
        check!(matches(&buf[..n], total), "stream data mismatch");
        total += n;
        if total % (CHUNK * 12) == 0 {
            println!("[pipetest] parent has read {} bytes", total);
        }
    }
    check!(total == TOTAL, "stream shorter than expected (EOF too early)");
    println!("[pipetest] parent verified all {} bytes and saw EOF", total);

    let _ = syscall::close(data_r);
    let _ = syscall::close(gate_w);

    let mut st = -1;
    let _ = syscall::waitpid(pid as u64, &mut st);
    check!(st == 7, "child exit status mismatch");
    Ok(())
}

/// Test 3: writing when every reader end is closed returns EPIPE (-32).
fn test_epipe() -> Result<(), i64> {
    let (data_r, data_w) = syscall::pipe()?;
    let (gate_r, gate_w) = syscall::pipe()?;

    let pid = syscall::fork();
    check!(pid >= 0, "fork failed");

    if pid == 0 {
        // Child drops its own reader copy, then waits for the parent to drop
        // the last one before attempting its write.
        let _ = syscall::close(data_r);
        let _ = syscall::close(gate_w);
        let mut b = [0u8; 1];
        let _ = syscall::read(gate_r, &mut b);
        let _ = syscall::close(gate_r);

        let r = syscall::write(data_w, b"x");
        match r {
            Err(-32) => println!("[pipetest] EPIPE delivered: write failed with -32"),
            Err(e) => {
                println!("[pipetest] expected EPIPE (-32), got {}", e);
                syscall::proc_exit_code(3);
            }
            Ok(_) => {
                println!("[pipetest] write succeeded with no readers!");
                syscall::proc_exit_code(3);
            }
        }
        let _ = syscall::close(data_w);
        syscall::proc_exit_code(9);
    }

    // Parent drops the last reader end, then releases the child.
    let _ = syscall::close(data_r);
    let _ = syscall::close(gate_r);
    syscall::write(gate_w, b"g")?;
    let _ = syscall::close(gate_w);

    let mut st = -1;
    let _ = syscall::waitpid(pid as u64, &mut st);
    check!(st == 9, "child exit status mismatch");
    println!("[pipetest] EPIPE test ok");
    Ok(())
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    println!("[pipetest] pid={} starting", syscall::get_epid());
    let mut ok = true;
    ok &= match test_echo() {
        Ok(()) => {
            println!("[pipetest] PASS: echo");
            true
        }
        Err(e) => {
            println!("[pipetest] FAIL: echo (errno {})", e);
            false
        }
    };
    ok &= match test_fork_stream() {
        Ok(()) => {
            println!("[pipetest] PASS: fork/blocking/EOF");
            true
        }
        Err(e) => {
            println!("[pipetest] FAIL: fork/blocking/EOF (errno {})", e);
            false
        }
    };
    ok &= match test_epipe() {
        Ok(()) => {
            println!("[pipetest] PASS: EPIPE");
            true
        }
        Err(e) => {
            println!("[pipetest] FAIL: EPIPE (errno {})", e);
            false
        }
    };

    if ok {
        println!("[pipetest] ALL PASS");
        syscall::proc_exit_code(0);
    } else {
        println!("[pipetest] SOME TESTS FAILED");
        syscall::proc_exit_code(1);
    }
}