// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! Preemptive Multi-Level Feedback Queue scheduler.
//!
//! Each thread owns a dedicated kernel stack (see [`super::Task`]) and the
//! scheduler hands the CPU between them with a raw stack switch
//! ([`super::context::switch_raw`]). Preemption is driven by the timer
//! interrupt: on every tick the scheduler ages the queues, wakes sleepers,
//! accounts the running thread's slice, and — when the slice is exhausted or
//! a higher-priority thread becomes runnable — switches to the best runnable
//! thread right from within the interrupt handler.
//!
//! * Four levels, doubling slices `[1, 2, 4, 8]` ticks (10 ms each).
//! * Full-slice consumers demote; early blockers keep their level.
//! * Aging promotes threads waiting >= 100 ticks, bounding starvation.
//!
//! The scheduler lock is never held across a context switch: the switch
//! itself only needs the collected `saved_rsp` values. Contended ticks (the
//! interrupted thread held the lock) are skipped via `try_lock`, matching the
//! previous cooperative design and avoiding IRQ-context deadlock.

use super::context;
use super::{State, Task, TaskId};
use crate::sync::Spinlock;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

/// Number of feedback levels (0 is highest priority).
pub const LEVELS: usize = 4;
/// Time slice per level, in timer ticks.
pub const SLICES: [usize; LEVELS] = [1, 2, 4, 8];
/// A ready thread older than this is promoted one level.
const AGING_TICKS: u64 = 100;

/// Sentinel value of [`CURRENT_ID`] meaning "the scheduler/boot context",
/// which has no [`Task`] entry of its own.
/// Task id of the scheduler's own context. Not a process: it has no address
/// space, and anything that attributes a fault to it is looking at the wrong
/// thing.
pub const SCHEDULER_ID: usize = 0;

struct Scheduler {
    tasks: BTreeMap<TaskId, Task>,
    /// FIFO run queues per level; index 0 = head (next to run).
    queues: [Vec<TaskId>; LEVELS],
    /// `(task, deadline tick)` pairs for sleeping threads.
    sleepers: Vec<(TaskId, u64)>,
    /// Dead children awaiting `waitpid`: `child -> (parent, status)`.
    zombies: BTreeMap<TaskId, (TaskId, i32)>,
    /// `(waiter, target)` pairs for processes blocked in `waitpid`.
    waiters: Vec<(TaskId, TaskId)>,
    current: Option<TaskId>,
    next_id: usize,
}

impl Scheduler {
    const fn new() -> Self {
        Self {
            tasks: BTreeMap::new(),
            queues: [Vec::new(), Vec::new(), Vec::new(), Vec::new()],
            sleepers: Vec::new(),
            zombies: BTreeMap::new(),
            waiters: Vec::new(),
            current: None,
            next_id: 1,
        }
    }

    fn alloc_id(&mut self) -> TaskId {
        let id = TaskId(self.next_id);
        self.next_id += 1;
        id
    }

    fn enqueue(&mut self, id: TaskId) {
        let lvl = match self.tasks.get_mut(&id) {
            Some(t) => {
                t.state = State::Ready;
                t.slice_used = 0;
                t.enqueued_at = crate::time::ticks();
                t.level.min(LEVELS - 1)
            }
            None => return,
        };
        self.queues[lvl].push(id);
    }

    fn dequeue_best(&mut self) -> Option<TaskId> {
        for q in self.queues.iter_mut() {
            while !q.is_empty() {
                let id = q.remove(0);
                if let Some(t) = self.tasks.get(&id) {
                    if matches!(t.state, State::Ready) {
                        return Some(id);
                    }
                }
            }
        }
        None
    }

    fn apply_aging(&mut self, now: u64) {
        for lvl in (1..LEVELS).rev() {
            let mut promoted = Vec::new();
            self.queues[lvl].retain(|&id| {
                let aged = self
                    .tasks
                    .get(&id)
                    .map(|t| now.saturating_sub(t.enqueued_at) >= AGING_TICKS)
                    .unwrap_or(false);
                if aged {
                    promoted.push(id);
                    false
                } else {
                    true
                }
            });
            for id in promoted.drain(..) {
                if let Some(t) = self.tasks.get_mut(&id) {
                    t.level -= 1;
                    t.enqueued_at = now;
                    self.queues[t.level].push(id);
                }
            }
        }
    }
}

static SCHED: Spinlock<Scheduler> = Spinlock::new(Scheduler::new());

/// ID of the thread physically executing on this (single) CPU.
static CURRENT_ID: AtomicUsize = AtomicUsize::new(SCHEDULER_ID);

/// Kernel stack top the `syscall` stub should use while servicing the
/// currently running thread. Updated on every context switch so each thread
/// syscalls onto its own kernel stack.
pub static CURRENT_KSTACK_TOP: AtomicUsize = AtomicUsize::new(0);

/// Kernel stack top for the currently running thread (see
/// [`CURRENT_KSTACK_TOP`]).
pub fn current_kstack_top() -> usize {
    CURRENT_KSTACK_TOP.load(Ordering::Relaxed)
}

/// CR3 value currently loaded in the (single) CPU's MMU, used to avoid
/// needless TLB flushes. Initialized from the kernel AS at scheduler start.
static ACTIVE_CR3: AtomicUsize = AtomicUsize::new(0);

/// Saved stack pointer of the scheduler/boot context, resumed whenever we
/// switch away from it and back. Written by `switch_raw` as it saves.
static mut SCHED_SAVED_RSP: usize = 0;

/// Set when a tick/wakeup requests a switch; consumed by cooperative paths.
static NEED_RESCHED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Flagging that a switch is desired at the next safe point.
pub fn request_resched() {
    NEED_RESCHED.store(true, Ordering::Release);
}

/// True (and clears) whether a switch was requested.
pub fn take_resched() -> bool {
    NEED_RESCHED.swap(false, Ordering::AcqRel)
}

/// Switch the CPU to `next` (already `Running`), saving the current thread's
/// context onto its own kernel stack.
///
/// Must be called with interrupts disabled. Never returns until the current
/// thread is later switched back to; it resumes right after the internal
/// [`context::switch_raw`].
fn do_switch(next: TaskId) {
    let prev_id = CURRENT_ID.load(Ordering::Relaxed);
    if prev_id == next.0 {
        return;
    }

    // A raw stack swap is only safe with interrupts disabled: a timer IRQ
    // landing between `CURRENT_ID`/`RSP0` being armed for `next` and the
    // swap itself would nest a second switch on this stack and save a bogus
    // context for `next`. Most callers already run with IF=0 (timer handler,
    // syscalls), but cooperative paths (e.g. `idle_main` -> `yield_now`) may
    // not, so pin IF down for the whole switch and restore it on resume.
    let flags: usize;
    unsafe {
        core::arch::asm!("pushfq; pop {0}; cli", out(reg) flags, options(nomem));
    }

    let (
        next_rsp,
        first_run,
        next_kstack_top,
        prev_slot,
        next_as,
        next_user,
        next_first,
        next_io,
        next_fs,
    ) = {
        let mut g = SCHED.lock();
        // Park the outgoing thread's thread pointer before loading anyone
        // else's. `%fs` is per-CPU, so a process that installed a TCB and was
        // then preempted would otherwise resume holding whatever the next
        // process installed. Read it under the lock, alongside the rest of the
        // outgoing state, so the save and the switch cannot interleave.
        if prev_id != SCHEDULER_ID {
            if let Some(pt) = g.tasks.get_mut(&TaskId(prev_id)) {
                pt.fs_base = context::read_fs_base();
            }
        }
        let (rsp, first, top, first_ever) = {
            let nt = g.tasks.get_mut(&next).expect("switch to unknown task");
            let fe = !nt.started;
            nt.started = true;
            (nt.saved_rsp, nt.entry.take(), nt.kstack_top(), fe)
        };
        let (as_root, user, fs) = {
            let nt = g.tasks.get(&next).expect("switch to unknown task");
            (nt.as_root, nt.user, nt.fs_base)
        };
        let io = g
            .tasks
            .get(&next)
            .map(|t| t.io_allow)
            .unwrap_or([0xFFu8; 128]);
        let pslot = if prev_id == SCHEDULER_ID {
            // SAFETY: single CPU, interrupts off; only the scheduler writes
            // this slot.
            unsafe { &raw mut SCHED_SAVED_RSP as *mut usize }
        } else {
            let pt = g
                .tasks
                .get_mut(&TaskId(prev_id))
                .expect("unknown running task");
            &mut pt.saved_rsp as *mut usize
        };
        (rsp, first, top, pslot, as_root, user, first_ever, io, fs)
    };

    if let Some((entry, arg)) = first_run {
        context::stage_entry(entry, arg);
    }
    if next_first {
        // First time this thread runs: stage whichever trampoline it needs.
        if let Some(u) = next_user {
            crate::log::kdebug!(
                "sched: first-run user {} entry={:#x} stack={:#x} fs={:#x}",
                next.0,
                u.entry,
                u.stack_top,
                next_fs
            );
            context::stage_user_entry(u.entry, u.stack_top, next_fs);
        }
    }

    // Switch to the thread's address space (user threads have their own;
    // kernel threads use the kernel AS). Only write CR3 when it changes.
    let target_cr3 = next_as.unwrap_or_else(crate::memory::vmm::kernel_root);
    let active = ACTIVE_CR3.load(Ordering::Relaxed);
    if target_cr3 != active {
        unsafe {
            core::arch::asm!("mov cr3, {}", in(reg) target_cr3, options(nomem, nostack));
        }
        ACTIVE_CR3.store(target_cr3, Ordering::Release);
    }

    // ... and to its thread pointer, for the same reason and with the same
    // "only when it changes" shape. A thread that has never installed a TCB
    // legitimately wants base 0 ("no thread pointer"), so this is not
    // conditional on the value being non-zero.
    if next_fs != context::read_fs_base() {
        // SAFETY: ring 0, single CPU, interrupts off.
        unsafe { context::write_fs_base(next_fs) };
    }

    // Point the TSS at the next thread's kernel stack for any ring-0 entry,
    // and tell the syscall stub which stack to use. User tasks must use their
    // *own* kstack here, not the shared IST-0 stack: ring-3 interrupts land on
    // RSP0, and a shared stack lets the next task's interrupt frame overwrite
    // the preempted task's saved context.
    crate::interrupts::gdt::set_rsp0(next_kstack_top);
    crate::interrupts::gdt::set_io_bitmap(&next_io);
    CURRENT_KSTACK_TOP.store(next_kstack_top, Ordering::Release);
    CURRENT_ID.store(next.0, Ordering::Release);

    // SAFETY: interrupts are off and the collected pointers are stable (no
    // collection mutation happens while they are in use here).
    unsafe {
        context::switch_raw(prev_slot, next_rsp);
        // Resumed here when this thread is switched back to (first-run
        // threads resume in their trampoline and never reach this).
        core::arch::asm!("push {0}; popfq", in(reg) flags, options(nomem));
    }
}

/// Dequeue the best runnable thread, marking it `Running`, or `None` if the
/// queues are empty.
fn pick_next() -> Option<TaskId> {
    let mut g = SCHED.lock();
    match g.dequeue_best() {
        Some(id) => {
            let t = g.tasks.get_mut(&id).expect("dequeued unknown task");
            t.state = State::Running;
            g.current = Some(id);
            Some(id)
        }
        None => {
            g.current = None;
            None
        }
    }
}

/// Spawn a thread whose `entry(arg)` runs on its own dedicated stack.
pub fn spawn(name: &str, entry: fn(usize), arg: usize) -> TaskId {
    let mut s = SCHED.lock();
    let id = s.alloc_id();
    let mut task = Task::new(id, name, entry, arg);
    task.endpoint = crate::ipc::create_endpoint(id, name);
    s.tasks.insert(id, task);
    let lvl = 0;
    s.queues[lvl].push(id);
    crate::log::kdebug!("sched: spawned \"{}\" as {:?}", name, id);
    drop(s);
    request_resched();
    id
}

/// Spawn the idle thread at the lowest level.
pub fn spawn_idle() -> TaskId {
    let mut s = SCHED.lock();
    let id = s.alloc_id();
    let mut task = Task::new(id, "idle", idle_main, 0);
    task.level = LEVELS - 1;
    s.tasks.insert(id, task);
    let lvl = LEVELS - 1;
    s.queues[lvl].push(id);
    id
}

/// Spawn a user-mode thread running `entry` in address space `as_root` with
/// user stack top `stack_top`. Used by tests and one-off bring-up paths that
/// have no argv/envp of their own; the normal route is
/// [`crate::user::spawn_program_args`], which stages both on the initial stack.
pub fn spawn_user(name: &str, as_root: usize, entry: usize, stack_top: usize) -> TaskId {
    crate::task::sched::spawn_user_ep(
        name,
        as_root,
        entry,
        stack_top,
        None,
        None,
        alloc::vec::Vec::new(),
        alloc::vec::Vec::new(),
        alloc::string::String::from("/"),
    )
}

/// Spawn a user-mode thread, optionally pinning its IPC endpoint to the
/// well-known id `ep` (used for the fixed bootstrap servers).
///
/// `fs_base` is the address the newborn's thread pointer (`%fs`) takes, or 0 for
/// an image with no thread-local storage. It is threaded in rather than left to
/// be discovered because the loader is the only thing that knows the layout: it
/// put the thread-control block at that address and the image's thread-local
/// variables are addressed relative to it.
pub fn spawn_user_ep(
    name: &str,
    as_root: usize,
    entry: usize,
    stack_top: usize,
    fs_base: Option<usize>,
    ep: Option<crate::ipc::EndpointId>,
    args: Vec<String>,
    env: Vec<String>,
    cwd: String,
) -> TaskId {
    let mut s = SCHED.lock();
    let id = s.alloc_id();
    let mut task = Task::new_user(id, name, as_root, entry, stack_top);
    task.fs_base = fs_base.unwrap_or(0);
    task.args = args;
    task.env = env;
    task.cwd = cwd;
    // A newborn starts in its creator's process group and session, so terminal
    // signals reach it before it has had any chance to rearrange itself. A
    // task spawned from kernel boot has no creator and becomes its own group
    // and session leader.
    let (pgid, sid) = match CURRENT_ID.load(Ordering::Relaxed) {
        SCHEDULER_ID => (id.0 as u32, id.0 as u32),
        parent => match s.tasks.get(&TaskId(parent)) {
            Some(p) => (p.pgid, p.sid),
            None => (id.0 as u32, id.0 as u32),
        },
    };
    task.pgid = pgid;
    task.sid = sid;
    task.session_leader = sid == id.0 as u32;
    task.endpoint = match ep {
        Some(w) => {
            crate::ipc::register_well_known(w, name);
            // The task's endpoint id equals the well-known id.
            let _ = w;
            w
        }
        None => crate::ipc::create_endpoint(id, name),
    };
    let ep_id = task.endpoint;
    s.tasks.insert(id, task);
    let lvl = 0;
    s.queues[lvl].push(id);
    crate::log::kdebug!(
        "sched: spawned user \"{}\" as {:?} (cr3={:#x}, entry={:#x}, ep={})",
        name,
        id,
        as_root,
        entry,
        ep_id
    );
    drop(s);
    request_resched();
    id
}

fn idle_main(_arg: usize) {
    loop {
        // Poll legacy devices whose IRQ lines may not be connected, then park
        // the CPU until the next interrupt. Skip any device whose IRQ is owned
        // by a user-space driver (inputd binds IRQ 1), otherwise this fallback
        // would steal scancodes out from under it.
        if !crate::interrupts::idt::irq_is_bound(1) {
            crate::devices::keyboard::poll();
        }
        if !crate::interrupts::idt::irq_is_bound(12) {
            crate::devices::mouse::poll();
        }
        unsafe {
            core::arch::asm!("sti", options(nomem, nostack));
            core::arch::asm!("hlt", options(nomem, nostack));
        }
        if take_resched() {
            yield_now();
        }
    }
}

/// Run the scheduler forever. Must be the last call from the bootstrap.
///
/// The boot context doubles as the scheduler: it repeatedly picks the best
/// runnable thread and switches to it, resuming whenever that thread is
/// preempted and yields the CPU back.
pub fn enter_scheduler() -> ! {
    // Record the kernel AS as the currently loaded MMU state before the
    // first context switch, so CR3 is only rewritten when it actually changes.
    ACTIVE_CR3.store(crate::memory::vmm::kernel_root(), Ordering::Release);
    loop {
        match pick_next() {
            Some(id) => do_switch(id),
            None => {
                // Nothing runnable (idle not yet spawned, or all blocked):
                // wait for an interrupt, then re-check.
                unsafe {
                    core::arch::asm!("sti", options(nomem, nostack));
                    core::arch::asm!("hlt", options(nomem, nostack));
                    core::arch::asm!("cli", options(nomem, nostack));
                }
            }
        }
    }
}

/// Cooperative yield: requeue the current thread to the back of its queue and
/// switch to the best runnable thread.
pub fn yield_now() {
    let cur = CURRENT_ID.load(Ordering::Relaxed);
    if cur == SCHEDULER_ID {
        return;
    }
    let next = {
        let mut g = SCHED.lock();
        let (lvl, id) = {
            let t = g.tasks.get_mut(&TaskId(cur)).expect("unknown task");
            t.state = State::Ready;
            t.slice_used = 0;
            t.enqueued_at = crate::time::ticks();
            (t.level.min(LEVELS - 1), TaskId(cur))
        };
        let _ = id;
        g.queues[lvl].push(TaskId(cur));
        g.current = None;
        g.dequeue_best()
    };
    if let Some(n) = next {
        {
            let mut g = SCHED.lock();
            let t = g.tasks.get_mut(&n).expect("unknown task");
            t.state = State::Running;
            g.current = Some(n);
        }
        do_switch(n);
    }
}

/// Mark the current thread blocked and switch to the best runnable thread.
pub fn block_current() {
    let cur = CURRENT_ID.load(Ordering::Relaxed);
    if cur == SCHEDULER_ID {
        return;
    }
    {
        let mut g = SCHED.lock();
        let t = g.tasks.get_mut(&TaskId(cur)).expect("unknown task");
        // A `wake` that raced ahead of this park already marked us runnable;
        // consume it and return so the caller re-checks its condition instead
        // of sleeping past the wakeup.
        if t.wake_pending {
            t.wake_pending = false;
            return;
        }
        t.state = State::Blocked;
    }
    if let Some(n) = pick_next() {
        do_switch(n);
    } else {
        // All threads blocked and no idle: wait for a wakeup.
        unsafe {
            core::arch::asm!("sti", options(nomem, nostack));
            core::arch::asm!("hlt", options(nomem, nostack));
            core::arch::asm!("cli", options(nomem, nostack));
        }
    }
    // We were resumed by a wake: drop the flag it set so a later park can
    // actually block.
    SCHED
        .lock()
        .tasks
        .get_mut(&TaskId(cur))
        .expect("unknown task")
        .wake_pending = false;
}

/// Legacy cooperative probe: with preemptive threads nothing needs to skip a
/// step, so this is retained only for API compatibility.
pub fn pending() -> bool {
    false
}

/// Park the current thread until [`wake`] is called on it, or — when `deadline`
/// is given — until the 100 Hz timer passes that absolute tick count. The
/// deadline sleeper entry is what a `poll` timeout relies on; the caller can
/// drop meeting the deadline early by clearing the entry, then re-checking its
/// condition (see [`clear_timeout`]), exactly like [`sleep_ticks_interruptible`].
pub fn block_until(deadline: Option<u64>) {
    let cur = CURRENT_ID.load(Ordering::Relaxed);
    if cur == SCHEDULER_ID {
        return;
    }
    {
        let mut g = SCHED.lock();
        let t = g.tasks.get_mut(&TaskId(cur)).expect("unknown task");
        // A wake that arrived between the caller's readiness check and this
        // moment already set `wake_pending`; consume it and return without
        // sleeping, so the caller re-checks and sees the data.
        //
        // `sleep_ticks` above has had this check since it was written, and
        // `block_until` did not -- which is why the pty waiter could hang
        // forever while everything built on `sleep_ticks` behaved. The sequence
        // is short and needs no unlucky interruption: a reader checks for data,
        // finds none, and is handed a keystroke in the window before it is
        // marked blocked. The wake fires, finds a task that is not blocked yet,
        // sets `wake_pending`, and the task then sleeps on a byte that arrived
        // before it slept.
        //
        // With no deadline there is nothing to re-probe on -- an infinite
        // `poll` or `select` passes `None` -- so the wake is simply lost and the
        // process stops responding for the rest of the session. Nothing on the
        // screen says why, which is why this presents as a terminal that
        // freezes rather than as a lost interrupt.
        if t.wake_pending {
            t.wake_pending = false;
            return;
        }
        t.state = State::Blocked;
        if let Some(at) = deadline {
            g.sleepers.push((TaskId(cur), at));
        }
    }
    if let Some(n) = pick_next() {
        do_switch(n);
    } else {
        unsafe {
            core::arch::asm!("sti", options(nomem, nostack));
            core::arch::asm!("hlt", options(nomem, nostack));
            core::arch::asm!("cli", options(nomem, nostack));
        }
    }
    // Resumed by a wake: drop the flag it set, so the next park in this loop
    // can actually block instead of returning immediately and spinning. Without
    // this a `poll` loop would re-check its condition without ever sleeping,
    // which is the same CPU burn as spinning -- just with more steps.
    SCHED
        .lock()
        .tasks
        .get_mut(&TaskId(cur))
        .expect("unknown task")
        .wake_pending = false;
}

/// Drop the current thread's pending timeout-sleeper entry (if any). Callers
/// that park with [`block_until`] and wake early must prune the entry so the
/// timer stops re-waking the thread across later iterations.
pub fn clear_timeout() {
    let cur = CURRENT_ID.load(Ordering::Relaxed);
    if cur == SCHEDULER_ID {
        return;
    }
    let mut g = SCHED.lock();
    g.sleepers.retain(|(t, _)| *t != TaskId(cur));
}

/// Park the current thread for at least `ticks` timer ticks.
pub fn sleep_ticks(ticks: u64) {
    let cur = CURRENT_ID.load(Ordering::Relaxed);
    if cur == SCHEDULER_ID {
        return;
    }
    let deadline = crate::time::ticks() + ticks.max(1);
    {
        let mut g = SCHED.lock();
        g.tasks.get_mut(&TaskId(cur)).expect("unknown task").state = State::Blocked;
        g.sleepers.push((TaskId(cur), deadline));
    }
    if let Some(n) = pick_next() {
        do_switch(n);
    }
}

/// Sleep like [`sleep_ticks`] but abort early (returning `true`) once a
/// deliverable signal is pending, pruning the sleeper entry so the timer
/// stops re-waking the task. Never restarts; the caller surfaces `EINTR`.
pub fn sleep_ticks_interruptible(ticks: u64) -> bool {
    let cur = CURRENT_ID.load(Ordering::Relaxed);
    if cur == SCHEDULER_ID {
        return false;
    }
    let deadline = crate::time::ticks() + ticks.max(1);
    loop {
        {
            let mut g = SCHED.lock();
            g.sleepers.retain(|(t, _)| *t != TaskId(cur));
            if crate::time::ticks() >= deadline {
                return false;
            }
            if crate::sig::deliverable_now() {
                return true;
            }
            g.tasks.get_mut(&TaskId(cur)).expect("unknown task").state = State::Blocked;
            g.sleepers.push((TaskId(cur), deadline));
        }
        if let Some(n) = pick_next() {
            do_switch(n);
        } else {
            // Nothing else runnable: park until the timer wakes us.
            unsafe {
                core::arch::asm!("sti", options(nomem, nostack));
                core::arch::asm!("hlt", options(nomem, nostack));
                core::arch::asm!("cli", options(nomem, nostack));
            }
        }
    }
}

/// Wake a blocked thread into its level's queue and request a reschedule.
pub fn wake(id: TaskId) {
    let mut s = SCHED.lock();
    if let Some(t) = s.tasks.get_mut(&id) {
        // Record the wake even if the target has not parked yet: the ensuing
        // `block_current` observes this and returns instead of sleeping.
        t.wake_pending = true;
        if t.state == State::Blocked {
            t.state = State::Ready;
            t.slice_used = 0;
            t.enqueued_at = crate::time::ticks();
            let lvl = t.level.min(LEVELS - 1);
            s.queues[lvl].push(id);
            request_resched();
        }
    }
}

/// Interrupt-context variant of [`wake`]: never spins on a contended
/// scheduler lock, so an IRQ can never deadlock a critical section. Returns
/// `false` if the wake was skipped.
pub fn try_wake(id: TaskId) -> bool {
    let mut s = match SCHED.try_lock() {
        Some(g) => g,
        None => return false,
    };
    if let Some(t) = s.tasks.get_mut(&id) {
        t.wake_pending = true;
        if t.state == State::Blocked {
            t.state = State::Ready;
            t.slice_used = 0;
            t.enqueued_at = crate::time::ticks();
            let lvl = t.level.min(LEVELS - 1);
            s.queues[lvl].push(id);
            request_resched();
            return true;
        }
    }
    false
}

/// Timer tick bookkeeping + preemption. Runs in interrupt context.
///
/// Wakes expired sleepers, ages starved threads, accounts the running
/// thread's slice, and — if the slice is exhausted or a better thread is
/// runnable — switches to it. Contended ticks are skipped to avoid
/// IRQ-context deadlock.
pub fn on_timer_tick() {
    let now = crate::time::ticks();

    // Wake expired sleepers (separate short critical sections).
    let due: Vec<TaskId> = {
        let mut s = match SCHED.try_lock() {
            Some(g) => g,
            None => return,
        };
        let due: Vec<TaskId> = s
            .sleepers
            .iter()
            .filter(|(_, at)| *at <= now)
            .map(|(id, _)| *id)
            .collect();
        if !due.is_empty() {
            s.sleepers.retain(|(_, at)| *at > now);
        }
        due
    };
    for id in due {
        wake(id);
    }

    // Slice accounting + preemption decision.
    let next = {
        let mut s = match SCHED.try_lock() {
            Some(g) => g,
            None => return,
        };
        s.apply_aging(now);

        if let Some(cur) = s.current {
            let t = s.tasks.get_mut(&cur).expect("running task missing");
            t.slice_used += 1;
            let exhausted = t.slice_used >= SLICES[t.level.min(LEVELS - 1)];
            if exhausted {
                if t.level < LEVELS - 1 {
                    t.level += 1;
                }
                t.state = State::Ready;
                t.enqueued_at = now;
                let lvl = t.level;
                s.queues[lvl].push(cur);
                s.current = None;
                match s.dequeue_best() {
                    Some(i) => {
                        s.tasks.get_mut(&i).expect("dequeued unknown").state = State::Running;
                        s.current = Some(i);
                    }
                    None => {}
                }
            }
        }
        s.current
    };

    if let Some(n) = next {
        let running = CURRENT_ID.load(Ordering::Relaxed);
        if running != n.0 {
            do_switch(n);
        }
    }
}

/// Snapshot `(id, name, state-name, level)` of all threads, by ID.
pub fn snapshot() -> Vec<(usize, String, &'static str, usize)> {
    let s = SCHED.lock();
    s.tasks
        .values()
        .map(|t| {
            (
                t.id.0,
                t.name.clone(),
                match t.state {
                    State::Running => "running",
                    State::Ready => "ready",
                    State::Blocked => "blocked",
                },
                t.level,
            )
        })
        .collect()
}

/// Identifier of the currently executing thread, if any.
pub fn current_task_id() -> Option<TaskId> {
    let id = CURRENT_ID.load(Ordering::Relaxed);
    if id == SCHEDULER_ID {
        None
    } else {
        Some(TaskId(id))
    }
}

/// True if `pid` is a live task in the scheduler.
pub fn task_alive(pid: usize) -> bool {
    SCHED.lock().tasks.contains_key(&TaskId(pid))
}

/// True if `pid` is a zombie awaiting `waitpid`.
pub fn is_zombie(pid: usize) -> bool {
    SCHED.lock().zombies.contains_key(&TaskId(pid))
}

/// Forcibly terminate the *non-current* process `pid`, recording its exit
/// status for any `waitpid` callers and waking waiters, exactly like
/// [`exit_current`] but without a context switch (the caller keeps the CPU).
///
/// The target may legitimately be blocked (in a pipe, `waitpid`, a sleep): it
/// is simply removed from every scheduler structure; its kernel stack is never
/// resumed. Its open file descriptions are released first, so blocked pipe
/// peers are woken by the resulting `on_close` (EOF / `EPIPE`) while the
/// scheduler lock is not held.
/// The CR3 root of task `pid`'s user address space, if it has one.
///
/// A separate function because both exit paths need it and neither holds the
/// scheduler lock at the point it does: `destroy_user_as` walks page tables, and
/// taking the scheduler lock around that would serialise every process exit
/// against every schedule decision -- and, worse, would invite a caller to
/// destroy an address space belonging to a task that had already been removed
/// from the table.
fn as_root_of(pid: usize) -> Option<usize> {
    SCHED
        .lock()
        .tasks
        .get(&TaskId(pid))
        .and_then(|t| t.as_root)
}

/// Terminate task `pid` with `status`, from another task.
///
/// The counterpart to [`exit_current`], for a process killed rather than exiting
/// on its own. Refuses when the target is the scheduler or the caller: both of
/// those have to go through the self-exit path, which runs on the dying task's own
/// stack and so cannot free that stack.
pub fn terminate(pid: usize, status: i32) {
    let id = TaskId(pid);
    let cur = CURRENT_ID.load(Ordering::Relaxed);
    if cur == SCHEDULER_ID || cur == pid {
        return; // self-termination uses exit_current
    }

    crate::vfs::fdtab::close_all(pid);
    crate::cred::drop_creds(pid);
    crate::sig::drop_state(pid);
    // The user address space goes back to the allocator here too. Without it a
    // process killed by another one -- rather than exiting on its own -- leaked
    // exactly as much as one that exited, which is the same bug on a path that is
    // easier to reach than it looks: any signal handler that calls `exit`, and
    // every `kill`.
    if let Some(root) = as_root_of(pid) {
        crate::memory::user_map::destroy_user_as(root);
    }

    let mut g = SCHED.lock();
    if !g.tasks.contains_key(&id) {
        return; // already gone (raced a concurrent exit or another kill)
    }
    let parent = g.tasks.get(&id).and_then(|t| t.parent);
    g.tasks.remove(&id);
    g.sleepers.retain(|(t, _)| *t != id);
    // The victim can no longer be waiting on anyone.
    g.waiters.retain(|(waiter, _)| *waiter != id);

    // Record the zombie so a (possibly later) waitpid can reap it.
    if let Some(p) = parent {
        g.zombies.insert(id, (p, status));
    }
    // Wake any process blocked waiting on this pid.
    let woken: Vec<TaskId> = {
        let mut w = Vec::new();
        g.waiters.retain(|&(waiter, target)| {
            if target == id {
                w.push(waiter);
                false
            } else {
                true
            }
        });
        w
    };
    for waiter in woken {
        wake_locked(&mut g, waiter);
    }
    // Drop any queued references to the removed task so no later dequeue can
    // observe it.
    for q in g.queues.iter_mut() {
        q.retain(|t| *t != id);
    }
}

/// Name of the currently executing thread.
pub fn current_name() -> String {
    let id = CURRENT_ID.load(Ordering::Relaxed);
    if id == SCHEDULER_ID {
        return String::from("scheduler");
    }
    let s = SCHED.lock();
    match s.tasks.get(&TaskId(id)) {
        Some(t) => t.name.clone(),
        None => String::from("none"),
    }
}

/// IPC endpoint of the currently executing process, if any.
pub fn current_endpoint() -> Option<crate::ipc::EndpointId> {
    let id = CURRENT_ID.load(Ordering::Relaxed);
    if id == SCHEDULER_ID {
        return None;
    }
    let s = SCHED.lock();
    s.tasks.get(&TaskId(id)).map(|t| t.endpoint)
}

/// Record `parent` as the creator of `child` so it can reap the child's exit
/// status with `waitpid`. Ignored silently for unknown tasks.
pub fn set_parent(child: TaskId, parent: TaskId) {
    let mut s = SCHED.lock();
    if let Some(t) = s.tasks.get_mut(&child) {
        t.parent = Some(parent);
    }
}

/// Mutate the currently executing process's state.
pub fn with_current<F: FnOnce(&mut Task)>(f: F) {
    let id = CURRENT_ID.load(Ordering::Relaxed);
    if id == SCHEDULER_ID {
        return;
    }
    let mut s = SCHED.lock();
    if let Some(t) = s.tasks.get_mut(&TaskId(id)) {
        f(t);
    }
}

/// Physical root (CR3) of the currently executing process's address space,
/// if it is a user process.
pub fn current_as_root() -> Option<usize> {
    let id = CURRENT_ID.load(Ordering::Relaxed);
    if id == SCHEDULER_ID {
        return None;
    }
    let s = SCHED.lock();
    s.tasks.get(&TaskId(id)).and_then(|t| t.as_root)
}

/// Copy the current process's I/O permission bitmap into the TSS so ring-3
/// `in`/`out` reflects the latest grants immediately.
pub fn refresh_io_bitmap() {
    let id = CURRENT_ID.load(Ordering::Relaxed);
    if id == SCHEDULER_ID {
        return;
    }
    let s = SCHED.lock();
    if let Some(t) = s.tasks.get(&TaskId(id)) {
        crate::interrupts::gdt::set_io_bitmap(&t.io_allow);
    }
}

/// Destroy the currently running process, record its exit status for any
/// `waitpid` callers, and switch to the next best runnable thread. Never
/// returns.
pub fn exit_current(status: i32) -> ! {
    let cur = CURRENT_ID.load(Ordering::Relaxed);
    assert!(cur != SCHEDULER_ID, "scheduler context cannot exit");
    let id = TaskId(cur);

    // Release the process's open file descriptions before tearing down its
    // scheduler state: a pipe end's `on_close` may `wake()` peers blocked on
    // that pipe, and `wake()` takes the scheduler lock, so this must run
    // while the scheduler lock is not held below.
    crate::vfs::fdtab::close_all(cur);
    crate::cred::drop_creds(cur);
    crate::sig::drop_state(cur);

    // Free the user address space, while this process is still running in it.
    //
    // This is a leak fix, and it was a large one: nothing anywhere freed a user
    // address space on exit. `destroy_user_as` had exactly two callers -- a
    // failed `clone_user_as`, and the exec path's teardown of the *old* image --
    // so every program that ever ran and exited took its whole page set with it.
    // 130 MiB of physical memory, and after a handful of self-tests the frame
    // allocator reported 32639 of 32639 frames in use and a later `execve` failed
    // with ENOMEM part-way through mapping a user stack.
    //
    // Here, rather than in `terminate`, because ordering matters: the frames have
    // to go back before the scheduler lock is taken, and because this is the path
    // nearly every exit takes. `terminate` frees them too, for the `kill` case.
    //
    // Safe while running in the space being freed: the frames are returned to the
    // allocator, but this thread does not touch user memory again -- the kernel
    // half is shared and untouched, and the user stack it was using is on a
    // separate kernel mapping. The deferred-free comment below explains why the
    // *kernel* stack is leaked deliberately; the user half has no such problem.
    if let Some(root) = as_root_of(cur) {
        crate::memory::user_map::destroy_user_as(root);
    }

    // Remove the process from every scheduler structure, then pick the next
    // runnable thread (mirrors pick_next but without touching the dead task).
    let (next_rsp, first_run, next_kstack_top, next_as, next_user, next_first, next_io, next_id, next_fs) =
        {
            let mut g = SCHED.lock();
            let parent = g.tasks.get(&id).and_then(|t| t.parent);
            if let Some(t) = g.tasks.remove(&id) {
                // We are still executing on this very stack; freeing it here
                // would be a use-after-free for the remainder of this function
                // (and any allocation could reuse and overwrite it). Defer the
                // free by leaking until a stack-safe reclamation path exists.
                core::mem::forget(t.kstack);
            }
            g.sleepers.retain(|(t, _)| *t != id);

            // Record the zombie so a (possibly later) waitpid can reap it.
            if let Some(p) = parent {
                g.zombies.insert(id, (p, status));
            }
            // Wake any process blocked waiting on this pid.
            let woken: Vec<TaskId> = {
                let mut w = Vec::new();
                g.waiters.retain(|&(waiter, target)| {
                    if target == id {
                        w.push(waiter);
                        false
                    } else {
                        true
                    }
                });
                w
            };
            for waiter in woken {
                wake_locked(&mut g, waiter);
            }

            g.current = None;
            match g.dequeue_best() {
                Some(n) => {
                    let (rsp, first, top, first_ever) = {
                        let nt = g.tasks.get_mut(&n).unwrap();
                        let fe = !nt.started;
                        nt.started = true;
                        (nt.saved_rsp, nt.entry.take(), nt.kstack_top(), fe)
                    };
                    let (as_root, user, fs) = {
                        let nt = g.tasks.get(&n).unwrap();
                        (nt.as_root, nt.user, nt.fs_base)
                    };
                    let io = g.tasks.get(&n).map(|t| t.io_allow).unwrap_or([0xFF; 128]);
                    let nt = g.tasks.get_mut(&n).unwrap();
                    nt.state = State::Running;
                    g.current = Some(n);
                    (rsp, first, top, as_root, user, first_ever, io, n.0, fs)
                }
                None => {
                    // Nothing left runnable: park the CPU forever.
                    unsafe {
                        core::arch::asm!("sti", options(nomem, nostack));
                        loop {
                            core::arch::asm!("hlt", options(nomem, nostack));
                        }
                    }
                }
            }
        };

    if let Some((entry, arg)) = first_run {
        context::stage_entry(entry, arg);
    }
    if next_first {
        if let Some(u) = next_user {
            context::stage_user_entry(u.entry, u.stack_top, next_fs);
        }
    }

    let target_cr3 = next_as.unwrap_or_else(crate::memory::vmm::kernel_root);
    let active = ACTIVE_CR3.load(Ordering::Relaxed);
    if target_cr3 != active {
        unsafe {
            core::arch::asm!("mov cr3, {}", in(reg) target_cr3, options(nomem, nostack));
        }
        ACTIVE_CR3.store(target_cr3, Ordering::Release);
    }

    crate::interrupts::gdt::set_rsp0(next_kstack_top);
    crate::interrupts::gdt::set_io_bitmap(&next_io);
    CURRENT_KSTACK_TOP.store(next_kstack_top, Ordering::Release);
    CURRENT_ID.store(next_id, Ordering::Release);

    // The dead process's thread pointer dies with it; hand the CPU the next
    // task's. Same per-CPU-register problem as in `do_switch`.
    if next_fs != context::read_fs_base() {
        // SAFETY: ring 0, single CPU, interrupts off.
        unsafe { context::write_fs_base(next_fs) };
    }

    // SAFETY: interrupts are off, the dead task is gone, and the switch
    // target state has been fully armed.
    unsafe {
        context::switch_to_thread(next_rsp);
    }
    unreachable!()
}

/// Enqueue a blocked thread under a held scheduler lock (see [`wake`]).
fn wake_locked(g: &mut Scheduler, id: TaskId) {
    if let Some(t) = g.tasks.get_mut(&id) {
        if t.state == State::Blocked {
            t.state = State::Ready;
            t.slice_used = 0;
            t.enqueued_at = crate::time::ticks();
            let lvl = t.level.min(LEVELS - 1);
            g.queues[lvl].push(id);
        }
    }
}

// ---------------------------------------------------------------------------
// fork / exec / waitpid
// ---------------------------------------------------------------------------

/// Duplicate the currently running process.
///
/// The child gets a deep copy of the parent's user address space (device and
/// DMA mappings stay shared), a copy of its file descriptor table, a fresh IPC
/// endpoint, and a copy of the parent's ring-0 syscall frame with the `rax`
/// slot zeroed — so the child resumes in user land right after the `fork`
/// syscall with a return value of 0. Returns the child's task id.
pub fn fork_current() -> Result<TaskId, i64> {
    let cur = CURRENT_ID.load(Ordering::Relaxed);
    if cur == SCHEDULER_ID {
        return Err(crate::abi::errno::EPERM);
    }

    let (name, as_root, io, args, env, cwd, pgid, sid, fs_base) = {
        let g = SCHED.lock();
        let t = g.tasks.get(&TaskId(cur)).expect("unknown running task");
        (
            t.name.clone(),
            t.as_root,
            t.io_allow,
            t.args.clone(),
            t.env.clone(),
            t.cwd.clone(),
            t.pgid,
            t.sid,
            t.fs_base,
        )
    };
    let cr3 = match as_root {
        Some(root) => crate::memory::user_map::clone_user_as(root).map_err(|e| e)?,
        None => {
            crate::log::kwarn!("fork: caller has no address space to clone");
            return Err(crate::abi::errno::EPERM);
        }
    };

    let mut g = SCHED.lock();
    let id = g.alloc_id();
    let mut task = Task::new_forked(id, &name, cr3);
    task.io_allow = io;
    task.args = args;
    task.env = env;
    task.cwd = cwd;
    // The child shares the parent's address space contents, and the thread
    // pointer is no exception: it points into a thread block that the child's
    // copy of the parent's pages still holds, so the same address is correct.
    // Without this the child would come up with `%fs` at zero and its very first
    // `errno` write -- on any failing call -- would fault. That is the whole of
    // `fork`'s cost here; nothing else about the block needs duplicating, because
    // there are no second threads to keep apart.
    task.fs_base = fs_base;
    // A child stays in the parent's process group and session, which is what
    // makes a foreground job — the shell plus everything it started —
    // interruptible as a unit by ^C.
    task.pgid = pgid;
    task.sid = sid;
    task.session_leader = false;
    task.parent = Some(TaskId(cur));
    task.endpoint = crate::ipc::create_endpoint(id, &name);

    // Copy the parent's 20-word syscall frame (still live on its kernel stack
    // while `fork` executes) into the child's kernel stack, zeroing `rax` so
    // the child observes `fork() == 0`.
    let frame = task.kstack_top() - context::SYSCALL_FRAME_SIZE;
    let src = current_kstack_top() - context::SYSCALL_FRAME_SIZE;
    // SAFETY: both kernel stacks are this kernel's own heap memory, and the
    // single CPU guarantees the parent's frame is stable throughout.
    unsafe {
        core::ptr::copy_nonoverlapping(src as *const u8, frame as *mut u8, context::SYSCALL_FRAME_SIZE);
        *(frame as *mut usize) = 0;
    }
    // Point the child at the frame just copied. The context slot is written
    // after the copy because the copy lands where the child will read from, and
    // the two must agree.
    //
    // This is the whole fix for "fork stops working after a while". The child
    // resumes by replaying this frame, and it used to find the base of that
    // frame in a single global that the *next* fork would overwrite -- the
    // parent does not yield here, so two forks are routinely in flight at once.
    // The earlier child then replayed the later one's frame, took its `rax`, and
    // so saw `fork()` return a pid instead of zero: it ran the parent's path,
    // forking again on every iteration, and the parent sat in `waitpid` for a
    // pid that was never coming.
    crate::log::kdebug!(
        "fork: child frame @{:#x} rip={:#x} rsp={:#x} rax={} r0..3={:x} {:x} {:x} {:x}",
        frame,
        unsafe { *((frame + 120) as *const usize) },
        unsafe { *((frame + 144) as *const usize) },
        unsafe { *(frame as *const usize) },
        unsafe { *((frame + 0) as *const usize) },
        unsafe { *((frame + 8) as *const usize) },
        unsafe { *((frame + 16) as *const usize) },
        unsafe { *((frame + 24) as *const usize) },
    );
    task.user = Some(crate::task::UserState {
        entry: frame,
        stack_top: 0,
    });

    // Copy the parent's open file descriptors now that the child has an id.
    crate::vfs::fdtab::copy_table(cur, id.0);
    // The child inherits the parent's identity and umask.
    crate::cred::fork_creds(cur, id.0);
    // ... and its signal actions / blocked mask / alternate stack.
    crate::sig::fork_state(cur, id.0);

    g.tasks.insert(id, task);
    g.queues[0].push(id);
    crate::log::kinfo!(
        "sched: fork \"{}\" {:?} -> child {:?} (cr3={:#x})",
        name,
        TaskId(cur),
        id,
        cr3
    );
    drop(g);
    request_resched();
    Ok(id)
}

/// Outcome of one `waitpid` step.
pub enum WaitResult {
    /// Target reaped; carries its exit status.
    Reaped(i32),
    /// Target is a live child; the caller was blocked and will retry.
    Wait,
    /// Errored with an errno.
    Err(i64),
}

/// Block until child `cpid` terminates, then reap it and return its status.
/// Re-states after each wake, so the caller loops until [`WaitResult::Wait`]
/// stops appearing.
pub fn waitpid(cpid: usize) -> WaitResult {
    let cur = CURRENT_ID.load(Ordering::Relaxed);
    if cur == SCHEDULER_ID {
        return WaitResult::Err(crate::abi::errno::EPERM);
    }
    let self_id = TaskId(cur);
    let target = TaskId(cpid);

    let mut g = SCHED.lock();
    if let Some(&(owner, status)) = g.zombies.get(&target) {
        if owner == self_id {
            g.zombies.remove(&target);
            return WaitResult::Reaped(status);
        }
        return WaitResult::Err(crate::abi::errno::ECHILD);
    }

    let wants_child = g
        .tasks
        .get(&target)
        .map(|t| t.parent == Some(self_id))
        .unwrap_or(false);
    if !wants_child {
        return if cpid == cur {
            WaitResult::Err(crate::abi::errno::ECHILD)
        } else {
            WaitResult::Err(crate::abi::errno::ESRCH)
        };
    }

    // Live child: block and hand the CPU back; the child's exit will wake us.
    g.tasks.get_mut(&self_id).expect("unknown task").state = State::Blocked;
    g.waiters.push((self_id, target));
    drop(g);
    crate::log::kdebug!(
        "sched: waitpid {:?} blocking on kernel task {:?}",
        self_id,
        target
    );
    if let Some(n) = pick_next() {
        crate::log::kdebug!("sched: waitpid picked {:?} for {:?}", n, self_id);
        do_switch(n);
    }
    WaitResult::Wait
}

/// Substitute a program name for an empty argument vector.
///
/// A process cannot have `argc == 0`. Every C runtime expects at least `argv[0]`,
/// and a great deal turns on it: `getopt` reports the program name in its
/// diagnostics, a program decides what it is by what it was called, and a
/// multi-call binary dispatches on it -- busybox selecting `ls` from `/bin/ls` is
/// *entirely* `argv[0]`. With no `argv[0]` there is nothing for any of that to
/// read.
///
/// Only an empty vector is filled in. A caller that passes arguments has already
/// said what the program is called, and second-guessing it would be wrong --
/// `busybox --list` must still see an empty argv after `busybox`, or it would
/// look for an applet called `--list`.
///
/// The name comes from wherever the program was reached by: a path for `execve`,
/// the program's registered name for a table entry. Inventing a path for the
/// latter would put a name in `argv[0]` that does not resolve.
fn with_program_name(name: &str, args: Vec<String>) -> Vec<String> {
    if args.is_empty() {
        alloc::vec![String::from(name)]
    } else {
        args
    }
}

/// The name embedded program `prog` is registered under, or `"?"` if the index is
/// out of range. `exec_current` has already validated the index, so the fallback
/// is unreachable; it exists because this returns a `&str` rather than an
/// `Option` and a panic on a validated input would be worse than a wrong name.
fn registered_name(prog: usize) -> &'static str {
    crate::user::program_name(prog).unwrap_or("?")
}

/// Replace the current process image in place: swap in a freshly built address
/// space for embedded program `prog`, restage this task as a first-run user
/// process, and drop to ring 3. The syscall frame and the old address space
/// are abandoned. Never returns on success.
///
/// The index-based exec: the program is one the kernel already carries, named by
/// its slot in the program table. For a program named by *path* see
/// [`exec_path`], which is the one a shell needs.
pub fn exec_current(prog: usize, args: Vec<String>) -> Result<(), i64> {
    let cur = CURRENT_ID.load(Ordering::Relaxed);
    if cur == SCHEDULER_ID {
        return Err(crate::abi::errno::EPERM);
    }
    let args = with_program_name(registered_name(prog), args);
    // `EXEC` names a program but carries no new environment, so the exec'd
    // image inherits the caller's envp the way it inherits its credentials.
    let env = task_env(cur);
    if env.is_empty() {
        let cwd = current_cwd();
        let loaded = crate::user::build_image(prog, &args, &crate::user::default_env(&cwd))?;
        return exec_finish(cur, loaded, args, Vec::new());
    }
    let loaded = crate::user::build_image(prog, &args, &env)?;
    exec_finish(cur, loaded, args, env)
}

/// Replace the calling process with the ELF in `image`.
///
/// The path-based counterpart to [`exec_current`], and the reason a file in the
/// filesystem can be run at all. Everything about the swap -- the signal reset,
/// the address-space teardown, the ring-3 drop -- is [`exec_finish`], so there is
/// one implementation of "become a different program" rather than two.
///
/// `name` is for the log line only. Environment is inherited, as with
/// [`exec_current`]: an exec'd image gets the caller's envp, which is what makes
/// the two calls consistent with each other.
pub fn exec_path(image: Vec<u8>, args: Vec<String>, name: &str) -> Result<(), i64> {
    let cur = CURRENT_ID.load(Ordering::Relaxed);
    if cur == SCHEDULER_ID {
        return Err(crate::abi::errno::EPERM);
    }
    let args = with_program_name(name, args);
    let env = task_env(cur);
    let cwd = current_cwd();
    let env = if env.is_empty() {
        crate::user::default_env(&cwd)
    } else {
        env
    };
    let loaded = crate::user::build_image_bytes(&image, name, &args, &env)?;
    exec_finish(cur, loaded, args, env)
}

/// Swap task `cur` onto a freshly loaded image and drop to ring 3. Split out of
/// [`exec_current`] so the envp-staging decision stays with its caller.
fn exec_finish(
    cur: usize,
    loaded: crate::elf::Loaded,
    args: Vec<String>,
    env: Vec<String>,
) -> Result<(), i64> {
    let (cr3, entry, rsp) = (loaded.cr3, loaded.entry, loaded.rsp);
    // The new image starts with a clean signal state.
    crate::sig::exec_reset(cur);

    // Swap the task's image state; capture the old address space for teardown.
    let (old_root, new_saved) = {
        let mut g = SCHED.lock();
        let t = g.tasks.get_mut(&TaskId(cur)).expect("unknown running task");
        let old = t.as_root;
        let new_saved = context::initial_user_saved_rsp(t.kstack_top());
        t.as_root = Some(cr3);
        // The thread pointer belongs to the new image's thread block, not to the
        // process: an exec replaces the address space, and the old block went
        // with it. Carrying the parent's `%fs` across would leave the new libc
        // reading thread-local storage out of a mapping that no longer exists.
        t.fs_base = loaded.fs_base;
        // The exec'd image receives the new argv; cwd and envp carry across.
        t.args = args;
        if !env.is_empty() {
            t.env = env;
        }
        t.user = Some(crate::task::UserState {
            entry,
            stack_top: rsp,
        });
        t.saved_rsp = new_saved;
        t.started = false;
        (old, new_saved)
    };

    let ktop = current_kstack_top();
    if let Some(old) = old_root {
        if old != cr3 {
            crate::memory::user_map::destroy_user_as(old);
        }
    }

    // From here on the old image can never run again: re-point the TSS, switch
    // page tables, stage a fresh ring-3 entry and take off without returning.
    crate::interrupts::gdt::set_rsp0(ktop);
    let active = ACTIVE_CR3.load(Ordering::Relaxed);
    if cr3 != active {
        unsafe {
            core::arch::asm!("mov cr3, {}", in(reg) cr3, options(nomem, nostack));
        }
        ACTIVE_CR3.store(cr3, Ordering::Release);
    }
    context::stage_user_entry(entry, rsp, loaded.fs_base);
    unsafe {
        context::switch_to_thread(new_saved);
    }
    unreachable!()
}

/// Read the current thread's working directory (a normalized absolute path).
pub fn current_cwd() -> String {
    let cur = CURRENT_ID.load(Ordering::Relaxed);
    SCHED
        .lock()
        .tasks
        .get(&TaskId(cur))
        .map(|t| t.cwd.clone())
        .unwrap_or_else(|| alloc::string::String::from("/"))
}

/// Read the argument vector recorded on kernel task `task`.
pub fn task_args(task: usize) -> alloc::vec::Vec<String> {
    SCHED
        .lock()
        .tasks
        .get(&TaskId(task))
        .map(|t| t.args.clone())
        .unwrap_or_default()
}

/// Read the environment recorded on kernel task `task`.
pub fn task_env(task: usize) -> alloc::vec::Vec<String> {
    SCHED
        .lock()
        .tasks
        .get(&TaskId(task))
        .map(|t| t.env.clone())
        .unwrap_or_default()
}

/// Read the working directory recorded on kernel task `task`.
pub fn task_cwd(task: usize) -> String {
    SCHED
        .lock()
        .tasks
        .get(&TaskId(task))
        .map(|t| t.cwd.clone())
        .unwrap_or_else(|| alloc::string::String::from("/"))
}

/// Read the calling process's environment, for `GET_ENV` and for seeding a
/// newborn. Empty when there is no current user task.
pub fn current_env() -> alloc::vec::Vec<String> {
    let cur = CURRENT_ID.load(Ordering::Relaxed);
    if cur == SCHEDULER_ID {
        return alloc::vec::Vec::new();
    }
    task_env(cur)
}

// --- process groups and sessions ------------------------------------------
//
// Job control needs three things a terminal can ask for: put a process in a
// group, find out which group a process is in, and signal a whole group at
// once. Sessions add the layer above that lets a shell keep job control
// scoped to one terminal.

/// The calling process's group id. Equals its pid when it leads its group.
pub fn current_pgid() -> u32 {
    let cur = CURRENT_ID.load(Ordering::Relaxed);
    SCHED
        .lock()
        .tasks
        .get(&TaskId(cur))
        .map(|t| t.pgid)
        .unwrap_or(0)
}

/// Task `id`'s session, process group, and whether it leads its session.
///
/// One read under one lock, because the `TIOCSCTTY` rule consults all three
/// together and three separate lookups would let a scheduling decision land
/// between them -- which is exactly the window in which a shell's group could
/// change from orphaned to populated and have the check answer about a state
/// that never existed. Returns `None` for an id that names no live task.
pub fn task_session_info(id: usize) -> Option<(u32, u32, bool)> {
    let g = SCHED.lock();
    let t = g.tasks.get(&TaskId(id))?;
    Some((t.sid, t.pgid, t.session_leader))
}

/// The calling process's session id.
pub fn current_sid() -> u32 {
    let cur = CURRENT_ID.load(Ordering::Relaxed);
    SCHED
        .lock()
        .tasks
        .get(&TaskId(cur))
        .map(|t| t.sid)
        .unwrap_or(0)
}

/// Group and session of kernel task `task`.
/// A task's process group and session, by name.
///
/// This was a bare `(u32, u32)` and every caller had to remember which field
/// was which. That is not a hypothetical mistake: the terminal's foreground
/// check read `.0` believing it was the session, and it is the process group --
/// so it compared a group against a session, which is a comparison that is
/// almost never true and whose failure looks like an unrelated permission
/// error. Two of the four call sites had resorted to destructuring with a
/// leading underscore, which is the shape of code written by someone who does
/// not trust the tuple either.
///
/// Named fields make the mistake impossible to compile.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Groups {
    /// Process group the task belongs to.
    pub pgid: u32,
    /// Session the task belongs to.
    pub sid: u32,
}

/// Group and session of kernel task `task`.
pub fn task_groups(task: usize) -> Groups {
    SCHED
        .lock()
        .tasks
        .get(&TaskId(task))
        .map(|t| Groups {
            pgid: t.pgid,
            sid: t.sid,
        })
        .unwrap_or(Groups { pgid: 0, sid: 0 })
}

/// Move `target` into process group `pgid`.
///
/// POSIX restricts this to the target itself or its parent, and forbids moving
/// a process into a group belonging to a different session — otherwise a
/// process could escape the terminal whose signals it is supposed to receive.
/// Returns `false` when the move is not permitted.
pub fn setpgid(target: usize, pgid: u32) -> bool {
    let cur = CURRENT_ID.load(Ordering::Relaxed);
    if cur == SCHEDULER_ID || target == SCHEDULER_ID {
        return false;
    }
    let mut g = SCHED.lock();
    let (t_sid, t_pgid) = match g.tasks.get(&TaskId(target)) {
        Some(t) => (t.sid, t.pgid),
        None => return false,
    };
    // Only the process itself or its parent may move it.
    if target != cur {
        let parent = g
            .tasks
            .get(&TaskId(cur))
            .map(|p| p.parent == Some(TaskId(target)))
            .unwrap_or(false);
        if !parent {
            return false;
        }
    }
    // The destination group must be in the same session as the target.
    //
    // The subtlety is that `setpgid(0, 0)` *creates* a group rather than
    // joining one, so the group does not exist yet and cannot be found. The
    // previous code required it to exist already, which meant the
    // create-a-group form could never succeed for anything: `find` looked for a
    // task whose pgid equals the caller's own pid, and a spawned child inherits
    // its parent's group, so there was never one. No process could become a
    // group leader at all. Job control is therefore impossible, and a program
    // following the ordinary sequence -- become a group leader, then hand the
    // terminal to it -- cannot work no matter what it does.
    //
    // So there are two cases rather than one:
    //
    //   - `pgid == target`: the caller is creating a group led by itself. Valid
    //     unless a group in *another* session already holds that id, which
    //     would mean two unrelated groups sharing an id.
    //   - otherwise: joining an existing group, which must already be in this
    //     session.
    if pgid == target as u32 {
        if g
            .tasks
            .values()
            .any(|t| t.pgid == pgid && t.sid != t_sid)
        {
            return false;
        }
    } else {
        let dest_sid = g
            .tasks
            .values()
            .find(|t| t.pgid == pgid)
            .map(|t| t.sid);
        match dest_sid {
            Some(sid) if sid == t_sid => {}
            _ => return false,
        }
    }

    // POSIX: a process group leader may not be moved into another group, which
    // is what stops a group id being reused while it still has a leader. Only
    // relevant when the target is not the caller, since a caller moving itself
    // is giving up leadership rather than taking it away from anyone.
    if target != cur && t_pgid == target as u32 {
        return false;
    }

    if let Some(t) = g.tasks.get_mut(&TaskId(target)) {
        t.pgid = pgid;
        // Deliberately *not* touching `session_leader`. That flag means "this
        // task's sid equals its own pid" and is what `setsid` consults to refuse
        // a second call. Deriving it from group leadership instead -- as this
        // did -- meant an ordinary `setpgid(0, 0)` marked the process a session
        // leader, so the `setsid()` a program might well call next failed with
        // EPERM. Owning a process group and owning a session are different
        // things, and conflating them broke a completely ordinary sequence.
        let _ = t_pgid;
    }
    true
}

/// Create a new session with the calling process as its leader, detached from
/// any controlling terminal. This is how a shell starts a job with job control
/// disabled, and how `daemon`-style programs escape the terminal's signals.
pub fn setsid() -> Result<u32, i64> {
    let cur = CURRENT_ID.load(Ordering::Relaxed);
    if cur == SCHEDULER_ID {
        return Err(crate::abi::errno::EPERM);
    }
    let mut g = SCHED.lock();
    let already_leader = match g.tasks.get(&TaskId(cur)) {
        Some(t) => t.session_leader,
        None => return Err(crate::abi::errno::EPERM),
    };
    // POSIX: a process group leader may not call setsid.
    if already_leader {
        return Err(crate::abi::errno::EPERM);
    }
    let pid = cur as u32;
    if let Some(t) = g.tasks.get_mut(&TaskId(cur)) {
        t.sid = pid;
        t.pgid = pid;
        t.session_leader = true;
    }
    Ok(pid)
}

/// Task ids belonging to process group `pgid`.
///
/// Used by the terminal to deliver a signal to a whole foreground job. The
/// caller holds no lock across the return, so ids may already have exited by
/// the time they are signalled; [`crate::sig::kill`] tolerates a stale id.
pub fn tasks_in_group(pgid: u32) -> alloc::vec::Vec<usize> {
    SCHED
        .lock()
        .tasks
        .values()
        .filter(|t| t.pgid == pgid)
        .map(|t| t.id.0)
        .collect()
}

/// Does any process in `pgid` still exist? A terminal uses this to stop
/// reporting a dead job as the foreground group.
pub fn group_alive(pgid: u32) -> bool {
    if pgid == 0 {
        return false;
    }
    SCHED.lock().tasks.values().any(|t| t.pgid == pgid)
}

/// Replace the current thread's working directory. The caller passes an
/// absolute, already-normalized path that has been validated as a directory.
pub fn set_current_cwd(cwd: String) {
    let cur = CURRENT_ID.load(Ordering::Relaxed);
    SCHED
        .lock()
        .tasks
        .get_mut(&TaskId(cur))
        .expect("unknown running task")
        .cwd = cwd;
}
