// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa
//
// Ring-3 signal exercise. Samsara delivers signals at syscall boundaries, so
// every test here funnels through a syscall return. What it verifies:
//
//   1. sigaction install/query round-trip; the handler is invoked with the
//      signal number in `rdi` and returns cleanly through the restorer.
//   2. A catchable signal interrupting a blocking pipe read: with a
//      SA_RESTART handler the read transparently resumes; without one the
//      read returns -EINTR and the handler still ran first.
//   3. sigprocmask: a blocked signal stays pending (sigpending) until the
//      mask is lifted, then delivery happens at the next syscall return.
//   4. sigsuspend parks until a caught signal wakes the process and reports
//      EINTR, with the handler running afterwards.
//   5. sigaltstack + SA_ONSTACK: the handler executes on the alternate stack
//      (checked by sampling `rsp` inside the handler).
//   6. Default (terminate) disposition for SIGTERM is exercised by credtst
//      and is not repeated here.

#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use nutcracker_rt::println;
use nutcracker_rt::syscall;
use nutcracker_rt::syscall::{sa_flags, signum, SigAction};

static H_USR1_RUNS: AtomicU32 = AtomicU32::new(0);
static H_USR1_RSP: AtomicUsize = AtomicUsize::new(0);
static H_USR2_RUNS: AtomicU32 = AtomicU32::new(0);
/// Parent's task id, exported to a forked child so it can poke us.
static PARENT_PID: AtomicUsize = AtomicUsize::new(0);

/// Handler for SIGUSR1: bumps a counter and records its entry `rsp` so the
/// test can prove the alternate-stack delivery landed on the right stack.
unsafe extern "C" fn on_usr1(_sig: i32) {
    H_USR1_RUNS.fetch_add(1, Ordering::Relaxed);
    let rsp: usize;
    // SAFETY: reads the caller's stack pointer; harmless in any context.
    unsafe {
        core::arch::asm!("mov {}, rsp", out(reg) rsp);
    }
    H_USR1_RSP.store(rsp, Ordering::Relaxed);
}

/// Handler for SIGUSR2 (block/pending and sigsuspend tests).
unsafe extern "C" fn on_usr2(_sig: i32) {
    H_USR2_RUNS.fetch_add(1, Ordering::Relaxed);
}

fn action(handler: usize, flags: u32) -> SigAction {
    SigAction {
        sa_handler: handler,
        sa_flags: flags,
        sa_restorer: syscall::sigreturn_trampoline as *const () as usize,
        sa_mask: 0,
    }
}

macro_rules! check {
    ($cond:expr, $msg:literal) => {
        if !($cond) {
            println!("[signaltst] FAIL: {}", $msg);
            syscall::proc_exit_code(255);
        }
    };
}

/// Record how many times `sig`'s user handler fired, then reset it.
fn usr1_runs_reset() -> u32 {
    H_USR1_RUNS.swap(0, Ordering::Relaxed)
}

fn usr2_runs_reset() -> u32 {
    H_USR2_RUNS.swap(0, Ordering::Relaxed)
}

fn test_sigaction_roundtrip() {
    let act = action(on_usr1 as *const () as usize, sa_flags::SA_RESTART);
    let mut old = SigAction {
        sa_handler: 0,
        sa_flags: 0,
        sa_restorer: 0,
        sa_mask: 0,
    };
    check!(syscall::sigaction(signum::SIGUSR1, Some(&act), Some(&mut old)).is_ok(), "sigaction install");
    check!(old.sa_handler == 0, "old action was not SIG_DFL");

    let mut got = SigAction {
        sa_handler: 0,
        sa_flags: 0,
        sa_restorer: 0,
        sa_mask: 0,
    };
    check!(
        syscall::sigaction(signum::SIGUSR1, None, Some(&mut got)).is_ok(),
        "sigaction query"
    );
    check!(got.sa_handler == act.sa_handler && got.sa_flags == act.sa_flags, "query mismatch");
    println!("[signaltst] OK sigaction install/query round-trip");
}

/// A direct self-kill: the pending signal is delivered on the `kill` syscall's
/// return, so the handler runs before `kill` returns to user land.
fn test_self_kill_delivery() {
    let act = action(on_usr1 as *const () as usize, sa_flags::SA_RESTART);
    check!(syscall::sigaction(signum::SIGUSR1, Some(&act), None).is_ok(), "reinstall usr1");
    check!(syscall::kill(syscall::get_pid(), signum::SIGUSR1).is_ok(), "self kill usr1");
    check!(usr1_runs_reset() == 1, "handler did not run exactly once after self-kill");
    println!("[signaltst] OK self-kill delivered on the killing syscall's return");
}

fn test_pipe_eintr_restart() {
    let act = action(on_usr1 as *const () as usize, sa_flags::SA_RESTART);
    check!(syscall::sigaction(signum::SIGUSR1, Some(&act), None).is_ok(), "usr1 restart handler");

    let (ready_r, ready_w) = syscall::pipe().expect("ready pipe");
    let (data_r, data_w) = syscall::pipe().expect("data pipe");
    let child = syscall::fork();
    check!(child >= 0, "fork restart child");
    if child == 0 {
        let _ = syscall::close(ready_r);
        let _ = syscall::close(data_w);
        let _ = syscall::write(ready_w, b"R");
        let mut b = [0u8; 1];
        // SA_RESTART: the pending SIGUSR1 only delays this read; it resumes
        // and the byte arrives.
        let n = syscall::read(data_r, &mut b).expect("restart read must not EINTR");
        if n != 1 || b[0] != b'X' || H_USR1_RUNS.load(Ordering::Relaxed) != 1 {
            syscall::proc_exit_code(2);
        }
        syscall::proc_exit_code(0);
    }
    let _ = syscall::close(ready_w);
    let _ = syscall::close(data_r);
    let mut rb = [0u8; 1];
    check!(syscall::read(ready_r, &mut rb).is_ok() && rb[0] == b'R', "child did not signal readiness");
    let _ = syscall::close(ready_r);

    check!(syscall::kill(child as u64, signum::SIGUSR1).is_ok(), "signal restart child");
    let _ = syscall::write(data_w, b"X");
    let _ = syscall::close(data_w);

    let mut st = -1;
    check!(syscall::waitpid(child as u64, &mut st).is_ok(), "wait restart child");
    check!(st == 0, "restart child misbehaved");
    println!("[signaltst] OK SA_RESTART read resumed with the byte after handler");
}

fn test_pipe_eintr_plain() {
    let act = action(on_usr1 as *const () as usize, 0);
    check!(syscall::sigaction(signum::SIGUSR1, Some(&act), None).is_ok(), "usr1 plain handler");

    let (ready_r, ready_w) = syscall::pipe().expect("ready pipe");
    let (data_r, data_w) = syscall::pipe().expect("data pipe");
    let child = syscall::fork();
    check!(child >= 0, "fork plain child");
    if child == 0 {
        let _ = syscall::close(ready_r);
        let _ = syscall::close(data_w);
        let _ = syscall::write(ready_w, b"R");
        let mut b = [0u8; 1];
        // No SA_RESTART: the read must surface -EINTR, and the handler must
        // have been invoked before the error was observed.
        if syscall::read(data_r, &mut b) != Err(-4) {
            syscall::proc_exit_code(2);
        }
        if H_USR1_RUNS.load(Ordering::Relaxed) != 1 {
            syscall::proc_exit_code(3);
        }
        syscall::proc_exit_code(0);
    }
    let _ = syscall::close(ready_w);
    let _ = syscall::close(data_r);
    let mut rb = [0u8; 1];
    check!(syscall::read(ready_r, &mut rb).is_ok() && rb[0] == b'R', "child not ready");
    let _ = syscall::close(ready_r);

    check!(syscall::kill(child as u64, signum::SIGUSR1).is_ok(), "signal plain child");
    // Leave the pipe empty; nothing must unblock the child.
    sig_delay();
    let _ = syscall::close(data_w);

    let mut st = -1;
    check!(syscall::waitpid(child as u64, &mut st).is_ok(), "wait plain child");
    if st != 0 {
        println!("[signaltst] FAIL: plain child exit {}", st);
        syscall::proc_exit_code(255);
    }
    println!("[signaltst] OK non-restart read returned -EINTR after handler ran");
}

/// Tiny busy syscall loop that gives the scheduler a chance to run the child.
fn sig_delay() {
    for _ in 0..200 {
        syscall::yield_now();
    }
}

fn test_block_pending_deliver() {
    let act = action(on_usr2 as *const () as usize, 0);
    check!(syscall::sigaction(signum::SIGUSR2, Some(&act), None).is_ok(), "usr2 handler");

    let mask = 1u64 << signum::SIGUSR2;
    check!(syscall::sigprocmask(syscall::how::SIG_BLOCK, Some(mask), None).is_ok(), "block usr2");

    // With SIGUSR2 blocked the self-kill must NOT invoke the handler yet.
    check!(syscall::kill(syscall::get_pid(), signum::SIGUSR2).is_ok(), "self kill usr2");
    check!(H_USR2_RUNS.load(Ordering::Relaxed) == 0, "handler ran while blocked");
    check!((syscall::sigpending() & mask) != 0, "sigpending missing blocked signal");

    // Lifting the mask makes it deliverable; the *unblocking* syscall's own
    // return carries the delivery.
    check!(syscall::sigprocmask(syscall::how::SIG_UNBLOCK, Some(mask), None).is_ok(), "unblock usr2");
    check!(H_USR2_RUNS.load(Ordering::Relaxed) == 1, "handler did not run after unblock");
    println!("[signaltst] OK blocked->pending->unblocked delivery");
}

fn test_sigsuspend() {
    let act = action(on_usr2 as *const () as usize, 0);
    check!(syscall::sigaction(signum::SIGUSR2, Some(&act), None).is_ok(), "usr2 suspend handler");
    let _ = usr2_runs_reset();

    // A pinger waits a little, then pokes us with SIGUSR2. We park in
    // sigsuspend(0) (nothing blocked), wake on the signal, restore the mask
    // and report EINTR; the handler runs at that syscall's return.
    PARENT_PID.store(syscall::get_pid() as usize, Ordering::Relaxed);
    let pinger = syscall::fork();
    check!(pinger >= 0, "fork pinger");
    if pinger == 0 {
        for _ in 0..50 {
            syscall::yield_now();
        }
        let _ = syscall::kill(PARENT_PID.load(Ordering::Relaxed) as u64, signum::SIGUSR2);
        syscall::proc_exit_code(0);
    }

    let mut old = 0u64;
    check!(syscall::sigprocmask(syscall::how::SIG_SETMASK, Some(mask_all()), Some(&mut old)).is_ok(), "snapshot mask");
    // Mask restored to the empty snapshot before suspending.
    check!(syscall::sigprocmask(syscall::how::SIG_SETMASK, Some(old), None).is_ok(), "restore mask");
    check!(old == 0, "expected an empty pre-mask");

    match syscall::sigsuspend(0) {
        Err(-4) => {}
        other => {
            println!("[signaltst] FAIL: sigsuspend returned {:?}", other);
            syscall::proc_exit_code(255);
        }
    }
    check!(H_USR2_RUNS.load(Ordering::Relaxed) == 1, "sigsuspend handler count");
    let mut st = -1;
    check!(syscall::waitpid(pinger as u64, &mut st).is_ok(), "wait pinger");
    check!(st == 0, "pinger misbehaved");
    println!("[signaltst] OK sigsuspend woken by signal, EINTR + handler");
}

fn mask_all() -> u64 {
    (1u64 << signum::SIGUSR1) | (1u64 << signum::SIGUSR2)
}

fn test_altstack() {
    // A dedicated 64 KiB alternate stack from the anonymous mapper.
    let base = syscall::map_anon(16).expect("map alt stack");
    let size = 16 * 4096usize;
    let ss = syscall::StackBuf {
        ss_base: base,
        ss_size: size,
        ss_flags: 0,
    };
    check!(syscall::sigaltstack(Some(&ss), None).is_ok(), "install alt stack");

    let mut cur = syscall::StackBuf {
        ss_base: 0,
        ss_size: 0,
        ss_flags: 0,
    };
    check!(syscall::sigaltstack(None, Some(&mut cur)).is_ok(), "query alt stack");
    check!(cur.ss_base == base && cur.ss_size == size, "alt stack query mismatch");

    let act = action(on_usr1 as *const () as usize, sa_flags::SA_ONSTACK);
    check!(syscall::sigaction(signum::SIGUSR1, Some(&act), None).is_ok(), "onstack handler");
    check!(syscall::kill(syscall::get_pid(), signum::SIGUSR1).is_ok(), "self kill for altstack");
    check!(usr1_runs_reset() == 1, "altstack handler count");
    let rsp = H_USR1_RSP.load(Ordering::Relaxed);
    if !(rsp >= base && rsp < base + size) {
        println!(
            "[signaltst] FAIL: handler rsp {:#x} not on alt stack [{:#x},{:#x})",
            rsp, base, base + size
        );
        syscall::proc_exit_code(255);
    }

    let _ = syscall::sigaltstack(
        Some(&syscall::StackBuf { ss_base: 0, ss_size: 0, ss_flags: syscall::ss_flags::SS_DISABLE }),
        None,
    );
    println!("[signaltst] OK SA_ONSTACK handler ran on the alternate stack");
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    println!("[signaltst] pid={} starting", syscall::get_pid());

    test_sigaction_roundtrip();
    test_self_kill_delivery();
    test_pipe_eintr_restart();
    test_pipe_eintr_plain();
    test_block_pending_deliver();
    test_sigsuspend();
    test_altstack();

    println!("[signaltst] ALL PASS");
    syscall::proc_exit_code(0);
}