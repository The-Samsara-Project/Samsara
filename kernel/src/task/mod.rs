// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! Threads: control blocks, scheduling states and per-thread stacks.
//!
//! Samsara's threads are *preemptive*: each owns a dedicated kernel stack,
//! and the scheduler switches between them with a raw stack swap (see
//! [`context`]). When a user thread is later introduced, its user-mode
//! registers, address space and stack live alongside the kernel state here.

pub mod context;
pub mod sched;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// Unique thread identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TaskId(pub usize);

/// Lifecycle state of a thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Currently executing on a CPU.
    Running,
    /// In a scheduler run queue, waiting for the CPU.
    Ready,
    /// Waiting on I/O or a deadline; not scheduled until woken.
    Blocked,
}

/// Size of each thread's dedicated kernel stack.
pub const KSTACK_SIZE: usize = 32 * 1024;

/// Everything the scheduler needs to know about one thread.
pub struct Task {
    /// Stable identifier.
    pub id: TaskId,
    /// Human-readable name.
    pub name: String,
    /// Current lifecycle state.
    pub state: State,
    /// Set when [`crate::task::sched::wake`] runs before the target has parked
    /// itself in [`crate::task::sched::block_current`]. [`block_current`] then
    /// skips the sleep and returns so the caller re-checks its condition,
    /// closing the lost-wakeup window between registering as a waiter and
    /// actually blocking.
    pub wake_pending: bool,
    /// MLFQ priority level this thread queues at (0 = highest).
    pub level: usize,
    /// Ticks consumed of the current time slice.
    pub slice_used: usize,
    /// Tick at which the thread was last enqueued (for aging).
    pub enqueued_at: u64,
    /// The thread's dedicated kernel stack.
    pub kstack: Box<[u8]>,
    /// Stack pointer to resume at on the next switch (offset addressing into
    /// `kstack`). Initialized to the fabricated trampoline context.
    pub saved_rsp: usize,
    /// Entry point invoked when the thread first runs.
    pub entry: Option<(fn(usize), usize)>,
    /// Whether the thread has executed at least once (its `saved_rsp` is a
    /// live context rather than a fabricated first-run frame).
    pub started: bool,
    /// Physical page-table root (CR3) for a user thread's address space.
    pub as_root: Option<usize>,
    /// The thread's `IA32_FS_BASE`, saved and restored across every context
    /// switch alongside `as_root`.
    ///
    /// This is per-*thread* state that lives in a per-*CPU* register, which is
    /// the whole reason it has to be saved explicitly. The x86-64 ABI puts the
    /// thread pointer at `%fs:0`, and a libc reads it constantly (every
    /// `errno` store, every thread-local access, every mutex). Leaving the MSR
    /// as one global value means the most recent process to install a TCB owns
    /// it: with a single process nothing is ever wrong, and with two or more
    /// each one resumes holding somebody else's thread pointer and faults on
    /// the first `%fs:` dereference.
    pub fs_base: usize,
    /// Ring-3 entry point and stack top (user threads only).
    pub user: Option<UserState>,
    /// IPC endpoint the process answers on (see [`crate::ipc`]).
    pub endpoint: crate::ipc::EndpointId,
    /// Creator task, if any. Used to deliver exit status to `waitpid`.
    pub parent: Option<TaskId>,
    /// Permitted I/O port bitmap (bit `p` set means port `p` is allowed for
    /// ring-3 drivers; rolled into the TSS on every context switch).
    pub io_allow: [u8; 128],
    /// Process argument vector (argv), fixed at spawn/exec time and read back
    /// through the `GET_ARGS` syscall. `fork` copies it; `exec` replaces it.
    pub args: Vec<String>,
    /// Process environment (envp), staged on the initial stack and read back
    /// through the `GET_ENV` syscall. `fork` copies it; `exec` keeps it, since
    /// `EXEC` names a program but carries no new environment.
    pub env: Vec<String>,
    /// Working directory as a normalized absolute path, used to resolve
    /// relative paths in `open`/`stat`/`chdir` and friends.
    pub cwd: String,
    /// Process group id. Job control groups every process that should
    /// receive the same terminal-generated signals; a terminal delivers
    /// `SIGINT`/`SIGQUIT`/`SIGTSTP` to its *foreground* group, so `^C` only
    /// interrupts the foreground job.
    ///
    /// A process begins in its parent's group, which is what makes a shell's
    /// children interruptible by the terminal before it calls `setpgid`.
    pub pgid: u32,
    /// Session id. A session is created by `setsid` and owns at most one
    /// controlling terminal; the shell uses it to keep job control from
    /// leaking across terminals.
    pub sid: u32,
    /// Whether this task is a session leader (its `sid` equals its own pid).
    /// A session leader may not call `setsid` again, and its terminal signals
    /// go to the group rather than to itself directly.
    pub session_leader: bool,
}

/// Ring-3 execution state for a user thread.
#[derive(Clone, Copy)]
pub struct UserState {
    /// Virtual address the thread starts executing at.
    pub entry: usize,
    /// Top of the thread's user-mode stack.
    pub stack_top: usize,
}

impl Task {
    /// Create a preemptive thread with its own kernel stack.
    pub fn new(id: TaskId, name: &str, entry: fn(usize), arg: usize) -> Self {
        let kstack = vec![0u8; KSTACK_SIZE].into_boxed_slice();
        let stack_end = kstack.as_ptr().addr() + KSTACK_SIZE;
        let saved_rsp = context::initial_saved_rsp(stack_end);
        Task {
            id,
            name: String::from(name),
            state: State::Ready,
            wake_pending: false,
            level: 0,
            slice_used: 0,
            enqueued_at: 0,
            kstack,
            saved_rsp,
            entry: Some((entry, arg)),
            started: false,
            as_root: None,
            fs_base: 0,
            user: None,
            endpoint: 0,
            parent: None,
            io_allow: [0xFF; 128],
            args: Vec::new(),
            env: Vec::new(),
            pgid: 0,
            sid: 0,
            session_leader: false,
            cwd: String::from("/"),
        }
    }

    /// Create a user thread: a fresh kernel stack fabricated to transfer to
    /// ring 3 on first run, running in address space `as_root`.
    pub fn new_user(
        id: TaskId,
        name: &str,
        as_root: usize,
        entry: usize,
        stack_top: usize,
    ) -> Self {
        let kstack = vec![0u8; KSTACK_SIZE].into_boxed_slice();
        let stack_end = kstack.as_ptr().addr() + KSTACK_SIZE;
        let saved_rsp = context::initial_user_saved_rsp(stack_end);
        Task {
            id,
            name: String::from(name),
            state: State::Ready,
            wake_pending: false,
            level: 0,
            slice_used: 0,
            enqueued_at: 0,
            kstack,
            saved_rsp,
            entry: None,
            started: false,
            as_root: Some(as_root),
            fs_base: 0,
            user: Some(UserState {
                entry,
                stack_top,
            }),
            endpoint: 0,
            parent: None,
            io_allow: [0xFF; 128],
            args: Vec::new(),
            env: Vec::new(),
            pgid: 0,
            sid: 0,
            session_leader: false,
            cwd: String::from("/"),
        }
    }

    /// Create a forked user thread: the scheduler copies the parent's ring-3
    /// syscall frame to `kstack_top - 160` before the child is first switched
    /// to, so its first resume returns to user land as if `fork` returned 0.
    pub fn new_forked(id: TaskId, name: &str, as_root: usize) -> Self {
        let kstack = vec![0u8; KSTACK_SIZE].into_boxed_slice();
        let stack_end = kstack.as_ptr().addr() + KSTACK_SIZE;
        let saved_rsp = context::initial_fork_saved_rsp(stack_end);
        Task {
            id,
            name: String::from(name),
            state: State::Ready,
            wake_pending: false,
            level: 0,
            slice_used: 0,
            enqueued_at: 0,
            kstack,
            saved_rsp,
            entry: None,
            // Already true, and this is load-bearing rather than a detail.
            //
            // A forked child does not start at a program entry: it resumes by
            // replaying its own copy of the parent's syscall frame, and
            // `initial_fork_saved_rsp` above has already staged that context.
            // Leaving this false made the scheduler treat the child as a thread
            // that had never run and stage an entry for it instead, overwriting
            // the staged frame with a null user stack -- so every child faulted
            // on its first return to user land, before it could run a single
            // instruction. The parent saw a successful `fork`, the child died
            // silently, and a shell that forks to run a command got nothing at
            // all: no output, no error, and a prompt straight back.
            started: true,
            as_root: Some(as_root),
            fs_base: 0,
            user: None,
            endpoint: 0,
            parent: None,
            io_allow: [0xFF; 128],
            args: Vec::new(),
            env: Vec::new(),
            pgid: 0,
            sid: 0,
            session_leader: false,
            cwd: String::from("/"),
        }
    }

    /// Top-of-stack address of the kernel stack (for TSS RSP0).
    pub fn kstack_top(&self) -> usize {
        self.kstack.as_ptr().addr() + self.kstack.len()
    }

    /// Grant the process access to I/O ports `lo..=hi` (bitmap bit = allowed).
    pub fn grant_io_ports(&mut self, lo: u16, hi: u16) {
        for p in lo..=hi {
            let b = (p & 7) as u8;
            self.io_allow[(p as usize) >> 3] &= !(1u8 << b);
        }
    }

    /// Grant access to I/O port `p` only (single-port convenience).
    pub fn grant_io_port(&mut self, p: u16) {
        self.grant_io_ports(p, p);
    }
}
