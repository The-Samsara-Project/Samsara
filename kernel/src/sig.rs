// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! Signal numbers, per-process signal state and the delivery machinery.
//!
//! Samsara's signals are delivered synchronously at **syscall boundaries**:
//! the assembly `syscall` stub calls [`samsara_post_syscall`] after the
//! dispatcher returns, and that hook decides whether to run a handler,
//! ignore a pending signal, or terminate the process. A blocked process (in a
//! pipe, `waitpid`, IPC or a sleep) is woken the moment its signals become
//! *deliverable* (`pending & !blocked & !deferred`), so its blocking syscall
//! can observe `EINTR` and let the handler run. A process spinning in user
//! land without issuing syscalls will not see signals until its next syscall
//! — a deliberate, documented simplification for this microkernel.
//!
//! ## Delivery
//!
//! To deliver a handler, `post_syscall` copies the interrupted trap frame into
//! a [`SigFrame`] on the target's stack (or its alternate stack when the
//! action requests `SA_ONSTACK`), places the restorer trampoline above it, and
//! rewrites the frame so the stub `sysretq`s straight into the handler with
//! the signal number in `rdi`. The handler returns through `sa_restorer`,
//! which issues `SIGRETURN`; the kernel then restores the saved frame and
//! blocked-signal mask from the `SigFrame`. Only one delivery is in flight per
//! process at a time (further catchable signals wait for the next `sigreturn`).
//!
//! ## Interrupted syscalls and `SA_RESTART`
//!
//! Blocking operations (pipes, `waitpid`, IPC, sleeps) check
//! [`deliverable_now`] before parking and after each wake. On a deliverable
//! signal a blocking syscall may be restarted ([`defer_pending`] marks the
//! deliverable set as deferred, so the syscall retries without popping
//! another `EINTR`) or it returns `-EINTR`, per the handler's `SA_RESTART`
//! flag. `nanosleep` and `sigsuspend` never restart.

use alloc::collections::BTreeMap;

use crate::sync::Spinlock;
use crate::task::TaskId;

// ---------------------------------------------------------------------------
// Signal numbers
// ---------------------------------------------------------------------------

/// Hangup.
pub const SIGHUP: u32 = 1;
/// Interrupt.
pub const SIGINT: u32 = 2;
/// Quit.
pub const SIGQUIT: u32 = 3;
/// Illegal instruction.
pub const SIGILL: u32 = 4;
/// Trace/breakpoint trap.
pub const SIGTRAP: u32 = 5;
/// Abort.
pub const SIGABRT: u32 = 6;
/// Bus error.
pub const SIGBUS: u32 = 7;
/// Floating point exception.
pub const SIGFPE: u32 = 8;
/// Kill (uncatchable: always terminates).
pub const SIGKILL: u32 = 9;
/// User-defined signal 1.
pub const SIGUSR1: u32 = 10;
/// Segmentation fault.
pub const SIGSEGV: u32 = 11;
/// User-defined signal 2.
pub const SIGUSR2: u32 = 12;
/// Write to a pipe with no reader.
pub const SIGPIPE: u32 = 13;
/// Alarm clock.
pub const SIGALRM: u32 = 14;
/// Termination.
pub const SIGTERM: u32 = 15;
/// Stack fault.
pub const SIGSTKFLT: u32 = 16;
/// Child stopped or terminated.
pub const SIGCHLD: u32 = 17;
/// Continue if stopped.
pub const SIGCONT: u32 = 18;
/// Stop (uncatchable).
pub const SIGSTOP: u32 = 19;
/// Keyboard stop.
pub const SIGTSTP: u32 = 20;
/// Terminal I/O for background job.
pub const SIGTTIN: u32 = 21;
/// Terminal output for background job.
pub const SIGTTOU: u32 = 22;
/// Urgent condition on a socket.
pub const SIGURG: u32 = 23;
/// CPU time limit exceeded.
pub const SIGXCPU: u32 = 24;
/// File size limit exceeded.
pub const SIGXFSZ: u32 = 25;
/// Virtual alarm clock.
pub const SIGVTALRM: u32 = 26;
/// Profiling timer expired.
pub const SIGPROF: u32 = 27;
/// Window size change.
pub const SIGWINCH: u32 = 28;
/// Asynchronous I/O available.
pub const SIGIO: u32 = 29;
/// Power failure.
pub const SIGPWR: u32 = 30;
/// Bad system call.
pub const SIGSYS: u32 = 31;

/// Largest accepted signal number.
pub const SIG_MAX: u32 = 31;

/// Bits for signals `1..=31` only (signal 0 is just an existence check).
const VALID_MASK: u64 = (1u64 << 32) - 2;

/// Bits for the uncatchable stop-class signals (no job control in Samsara:
/// they are accepted and dropped).
const STOP_PENDINGS: u64 =
    (1u64 << SIGSTOP) | (1u64 << SIGTSTP) | (1u64 << SIGTTIN) | (1u64 << SIGTTOU);

// ---------------------------------------------------------------------------
// ABI structs and constants shared with user space (see `kernel/src/abi/mod.rs`)
// ---------------------------------------------------------------------------

/// Default disposition: reset to the kernel's default (mostly terminate).
pub const SIG_DFL: usize = 0;
/// Ignore the signal.
pub const SIG_IGN: usize = 1;

/// `sigaction` flags.
pub const SA_ONSTACK: u32 = 1 << 0;
/// Restart an interrupted blocking syscall instead of returning `EINTR`.
pub const SA_RESTART: u32 = 1 << 1;
/// Do not block the signal while its own handler runs.
pub const SA_NODEFER: u32 = 1 << 2;

/// `sigaltstack` `ss_flags` values.
pub const SS_ONSTACK: u32 = 1 << 0;
/// The alternate stack is disabled.
pub const SS_DISABLE: u32 = 1 << 1;
/// Minimum acceptable alternate stack size.
pub const SIGSTKSZ_MIN: usize = 512;
/// Minimum stack region a single delivery needs (guard + frame + restorer).
pub const SIGFRAME_TOTAL: usize = 200 + 8;

/// `sigprocmask` `how` values.
pub const SIG_BLOCK: u64 = 0;
/// Unblock the listed signals.
pub const SIG_UNBLOCK: u64 = 1;
/// Replace the blocked mask outright (`set` may be `None` to query only).
pub const SIG_SETMASK: u64 = 2;

/// A signal action installed by `sigaction`. Layout must match the `SigAction`
/// in `user/rt/src/syscall.rs`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SigAction {
    /// Handler address, or [`SIG_DFL`]/[`SIG_IGN`].
    pub sa_handler: usize,
    /// `SA_*` bitfield.
    pub sa_flags: u32,
    /// Restorer trampoline address invoked when the handler returns.
    pub sa_restorer: usize,
    /// Signals additionally blocked while the handler runs.
    pub sa_mask: u64,
}

const DEFAULT_ACTION: SigAction = SigAction {
    sa_handler: 0,
    sa_flags: 0,
    sa_restorer: 0,
    sa_mask: 0,
};

/// Alternate signal stack descriptor (mirrors `StackBuf` in `rt`). Offsets are
/// fixed by the `sigaltstack` ABI.
#[repr(C)]
#[derive(Clone, Copy)]
#[allow(missing_docs)]
pub struct AltStack {
    pub ss_base: usize,
    pub ss_size: usize,
    pub ss_flags: u32,
}

/// The trap frame the `syscall` stub builds on the kernel stack. Offsets are
/// fixed by the assembly in `abi/mod.rs` and must never change.
#[repr(C)]
#[derive(Clone, Copy)]
#[allow(missing_docs)]
pub struct TrapFrame {
    pub rax: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub rbx: u64,
    pub rbp: u64,
    pub nr: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

const TRAP_FRAME_SIZE: usize = core::mem::size_of::<TrapFrame>();

/// Magic identifying a [`SigFrame`] on a process stack.
const SIGFRAME_MAGIC: u64 = u64::from_le_bytes(*b"SAMS_SIG");

/// One delivered signal's saved state, written to the recipient's stack.
/// Layout is internal to the kernel (not exposed in the ABI doc).
#[repr(C)]
#[derive(Clone, Copy)]
struct SigFrame {
    /// [`SIGFRAME_MAGIC`].
    magic: u64,
    /// `SS_ONSTACK` when delivered on the alternate stack.
    flags: u64,
    /// Blocked mask to restore on `sigreturn`.
    mask: u64,
    /// The delivered signal number.
    sig: u32,
    /// Reserved.
    pad: u32,
    /// The pre-handler trap frame (`TRAP_FRAME_SIZE` bytes).
    ucontext: [u8; TRAP_FRAME_SIZE],
}

const _: () = assert!(
    core::mem::size_of::<SigFrame>() == 8 + 8 + 8 + 4 + 4 + 160,
    "SigFrame layout changed; delivery offsets depend on it"
);

/// Disposition of a signal.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Disposition {
    /// Default action applies (for the fatal class: terminate with 128+sig).
    Default,
    /// No action: accept and discard.
    Ignore,
    /// Run a user handler at the stored address.
    Handler(usize),
    /// Uncatchable stop (Samsara has no job control: dropped).
    Stop,
}

fn default_continues(sig: u32) -> bool {
    matches!(
        sig,
        SIGCHLD | SIGCONT | SIGSTOP | SIGTSTP | SIGTTIN | SIGTTOU | SIGURG | SIGWINCH
    )
}

fn disposition(st: &SigState, sig: u32) -> Disposition {
    if sig == SIGKILL {
        return Disposition::Default;
    }
    if sig == SIGSTOP {
        return Disposition::Stop;
    }
    match st.actions[sig as usize].sa_handler {
        SIG_DFL => {
            if default_continues(sig) {
                Disposition::Ignore
            } else {
                Disposition::Default
            }
        }
        SIG_IGN => Disposition::Ignore,
        h => Disposition::Handler(h),
    }
}

// ---------------------------------------------------------------------------
// Per-process signal state
// ---------------------------------------------------------------------------

/// One task's complete signal state.
#[derive(Clone)]
struct SigState {
    /// Handler table indexed by signal: `[SigAction; 32]` (index 0 unused).
    actions: [SigAction; 32],
    /// Signals pending delivery (one bit per signal).
    pending: u64,
    /// Mask of blocked signals.
    blocked: u64,
    /// Signals currently "restarted" inside a blocking syscall; they stay
    /// pending but do not pop a second `EINTR` while the syscall retries.
    deferred: u64,
    /// Configured alternate signal stack.
    alt_stack: Option<AltStack>,
    /// User address of the `SigFrame` awaiting a `SIGRETURN`, if a handler is
    /// currently in flight. Suppresses nested delivery.
    frame_base: Option<usize>,
    /// True while a `sigsuspend` call has replaced the blocked mask.
    suspended: bool,
    /// Blocked mask before the active `sigsuspend`, restored on resume.
    suspend_prev: u64,
}

fn default_state() -> SigState {
    SigState {
        actions: [DEFAULT_ACTION; 32],
        pending: 0,
        blocked: 0,
        deferred: 0,
        alt_stack: None,
        frame_base: None,
        suspended: false,
        suspend_prev: 0,
    }
}

/// Global per-task signal state registry (mirrors `cred`).
static STATES: Spinlock<BTreeMap<usize, SigState>> = Spinlock::new(BTreeMap::new());

/// Signals that currently require action (handled or fatal), for `st`.
fn actionable(st: &SigState) -> u64 {
    let candidates = st.pending & !st.blocked & !st.deferred;
    let mut out = 0u64;
    let mut c = candidates;
    while c != 0 {
        let b = c & c.wrapping_neg();
        let sig = b.trailing_zeros() as u32;
        if sig > 0
            && sig <= SIG_MAX
            && !matches!(disposition(st, sig), Disposition::Ignore | Disposition::Stop)
        {
            out |= b;
        }
        c &= !b;
    }
    out
}

/// Bitmask of signals the *current* process would act on right now.
#[allow(clippy::needless_returns)]
pub fn deliverable_set() -> u64 {
    let id = match crate::task::sched::current_task_id() {
        Some(t) => t.0,
        None => return 0,
    };
    STATES
        .lock()
        .get(&id)
        .map(actionable)
        .unwrap_or_default()
}

/// True when the current process has a deliverable catchable/fatal signal.
pub fn deliverable_now() -> bool {
    deliverable_set() != 0
}

/// True when every currently-deliverable signal is handled with `SA_RESTART`
/// (so an interrupted blocking syscall should retry instead of returning
/// `EINTR`).
pub fn should_restart() -> bool {
    let id = match crate::task::sched::current_task_id() {
        Some(t) => t.0,
        None => return false,
    };
    let d = deliverable_set();
    if d == 0 {
        return false;
    }
    let st = match STATES.lock().get(&id) {
        Some(st) => st.clone(),
        None => return false,
    };
    let mut c = d;
    while c != 0 {
        let b = c & c.wrapping_neg();
        let sig = b.trailing_zeros() as u32;
        let a = st.actions[sig as usize];
        if a.sa_handler <= 1 || (a.sa_flags & SA_RESTART) == 0 {
            return false;
        }
        c &= !b;
    }
    true
}

/// Defer the currently-deliverable signals: a blocking syscall is about to be
/// restarted, so those signals must not produce another `EINTR` while it
/// retries. They stay pending and are delivered via
/// [`samsara_post_syscall`] once the syscall completes.
pub fn defer_pending() {
    let id = match crate::task::sched::current_task_id() {
        Some(t) => t.0,
        None => return,
    };
    let d = deliverable_set();
    if d != 0 {
        if let Some(st) = STATES.lock().get_mut(&id) {
            st.deferred |= d;
        }
    }
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

/// Install the default signal state for a newborn process.
pub fn seed(task: usize) {
    STATES.lock().insert(task, default_state());
}

/// Copy `src`'s signal state to `dst` (mirrors `fork`'s credential copy).
/// Handlers, the blocked mask and the alternate stack are inherited; pending
/// and in-flight deliveries are not.
pub fn fork_state(src: usize, dst: usize) {
    let mut s = STATES.lock();
    let mut st = s.get(&src).cloned().unwrap_or_else(default_state);
    st.pending = 0;
    st.deferred = 0;
    st.frame_base = None;
    st.suspended = false;
    st.suspend_prev = 0;
    s.insert(dst, st);
}

/// Reset the signal state after an `exec`: every action, mask and the
/// alternate stack are cleared (ignored dispositions are *not* preserved,
/// keeping the model deliberately small).
pub fn exec_reset(task: usize) {
    if let Some(st) = STATES.lock().get_mut(&task) {
        st.actions = [DEFAULT_ACTION; 32];
        st.pending = 0;
        st.blocked = 0;
        st.deferred = 0;
        st.alt_stack = None;
        st.frame_base = None;
        st.suspended = false;
    }
}

/// Drop a task's signal state (process teardown).
pub fn drop_state(task: usize) {
    STATES.lock().remove(&task);
}

// ---------------------------------------------------------------------------
// `kill`, `sigaction`, `sigprocmask`, `sigsuspend`, `sigaltstack`, `sigreturn`
// ---------------------------------------------------------------------------

/// Implement `kill(pid, sig)` from the calling process's context.
///
/// * `sig == 0` performs the existence + permission check only.
/// * A zombie target is a silent no-op.
/// * A signal whose default disposition terminates the target (i.e. no
///   handler covers it) kills it immediately — even one parked mid-syscall —
///   via the scheduler's teardown, exiting with `128 + sig`.
/// * A catchable signal is made *pending*; a blocked target is woken when the
///   signal becomes deliverable so its blocking syscall can `EINTR` and the
///   handler run at the next syscall return. `SIGKILL` and the stop-class
///   signals are uncatchable.
pub fn kill(pid: u64, sig: u32) -> i64 {
    if sig > SIG_MAX {
        return crate::abi::errno::EINVAL;
    }
    let sender = match crate::task::sched::current_task_id() {
        Some(t) => t.0,
        None => return crate::abi::errno::EPERM,
    };

    let alive = crate::task::sched::task_alive(pid as usize);
    let zombie = crate::task::sched::is_zombie(pid as usize);
    if !alive && !zombie {
        return crate::abi::errno::ESRCH;
    }

    // Permission gate. Zombies retain no credential state, so (documented
    // simplification) signaling one succeeds as long as it exists.
    if alive {
        let sender_cred = crate::cred::get(sender);
        let target_cred = crate::cred::get(pid as usize);
        if !crate::cred::may_signal(&sender_cred, target_cred.uid, target_cred.suid) {
            return crate::abi::errno::EPERM;
        }
    }

    if sig == 0 || zombie {
        return 0;
    }

    let mut kill_now = false;
    let mut wake = false;
    {
        let mut map = STATES.lock();
        let st = map.entry(pid as usize).or_insert_with(default_state);
        st.pending |= 1u64 << sig;
        if sig == SIGCONT {
            // A continue clears the (dropped) stop pendings.
            st.pending &= !STOP_PENDINGS;
        }
        let disp = disposition(st, sig);
        if matches!(disp, Disposition::Ignore | Disposition::Stop) {
            // Accepted-and-dropped: never even stays pending.
            st.pending &= !(1u64 << sig);
        }
        // Immediate termination only when the signal is unblocked *and* has no
        // handler: a blocked or caught signal must pend until it is due.
        let deliverable = (st.blocked & (1u64 << sig)) == 0;
        kill_now = deliverable && disp == Disposition::Default;
        wake = actionable(st) != 0;
    }
    if kill_now {
        let status = 128 + sig as i32;
        if pid as usize == sender {
            crate::task::sched::exit_current(status)
        }
        crate::task::sched::terminate(pid as usize, status);
        crate::log::kinfo!("sig: pid {} terminated by kill({}, {})", pid, pid, sig);
        return 0;
    }
    if wake {
        crate::task::sched::wake(TaskId(pid as usize));
    }
    0
}

/// Install or query a signal action. Sigs 9 and 19 are uncatchable.
pub fn sigaction(
    task: usize,
    sig: u32,
    act: Option<&SigAction>,
    oldact: Option<&mut SigAction>,
) -> i64 {
    if sig < 1 || sig > SIG_MAX || sig == SIGKILL || sig == SIGSTOP {
        return crate::abi::errno::EINVAL;
    }
    let idx = sig as usize;
    if let Some(old) = oldact {
        let cur = STATES.lock().get(&task).cloned().unwrap_or_else(default_state);
        *old = cur.actions[idx];
    }
    if let Some(a) = act {
        let mut s = STATES.lock();
        let st = s.entry(task).or_insert_with(default_state);
        st.actions[idx] = *a;
        if a.sa_handler == SIG_IGN {
            // POSIX: ignoring a signal discards its pending occurrences.
            st.pending &= !(1u64 << sig);
        }
    }
    0
}

/// Change the blocked-signal mask.
pub fn sigprocmask(task: usize, how: u64, set: Option<u64>, old: Option<&mut u64>) -> i64 {
    if how > SIG_SETMASK {
        return crate::abi::errno::EINVAL;
    }
    if set.is_none() && how != SIG_SETMASK {
        return crate::abi::errno::EINVAL;
    }
    let mut s = STATES.lock();
    let st = s.entry(task).or_insert_with(default_state);
    if let Some(o) = old {
        *o = st.blocked;
    }
    if let Some(v) = set {
        let v = v & VALID_MASK;
        st.blocked = match how {
            SIG_BLOCK => st.blocked | v,
            SIG_UNBLOCK => st.blocked & !v,
            _ => v,
        };
    }
    0
}

/// The currently pending signal set.
pub fn sigpending(task: usize) -> u64 {
    STATES
        .lock()
        .get(&task)
        .map(|s| s.pending & VALID_MASK)
        .unwrap_or(0)
}

/// Pause the process until a catchable or terminating, currently-unblocked
/// signal arrives. Atomically adopts `mask` as the blocked set for the
/// duration; on resume the previous mask is restored and the call reports
/// `EINTR` (delivery happens after it returns through `post_syscall`).
pub fn sigsuspend(mask: u64) -> i64 {
    let cur = match crate::task::sched::current_task_id() {
        Some(t) => t.0,
        None => return crate::abi::errno::EPERM,
    };
    {
        let mut s = STATES.lock();
        let st = s.entry(cur).or_insert_with(default_state);
        st.suspend_prev = st.blocked;
        st.blocked = mask & VALID_MASK;
        st.suspended = true;
    }
    loop {
        if deliverable_now() {
            break;
        }
        // Parked until `kill` wakes us (a deliverable signal makes a blocked
        // task wakable). Re-check after every wake.
        crate::task::sched::block_current();
    }
    {
        let mut s = STATES.lock();
        if let Some(st) = s.get_mut(&cur) {
            st.blocked = st.suspend_prev;
            st.suspended = false;
        }
    }
    crate::abi::errno::EINTR
}

/// Install or query the alternate signal stack.
pub fn sigaltstack(task: usize, ss: Option<&AltStack>, old: Option<&mut AltStack>) -> i64 {
    if let Some(o) = old {
        let cur = STATES.lock().get(&task).cloned().unwrap_or_else(default_state);
        *o = cur.alt_stack.unwrap_or(AltStack {
            ss_base: 0,
            ss_size: 0,
            ss_flags: SS_DISABLE,
        });
    }
    if let Some(a) = ss {
        if a.ss_flags & !(SS_DISABLE) != 0 {
            return crate::abi::errno::EINVAL;
        }
        if (a.ss_flags & SS_DISABLE) == 0 && a.ss_size < SIGSTKSZ_MIN {
            return crate::abi::errno::EINVAL;
        }
        let mut s = STATES.lock();
        let st = s.entry(task).or_insert_with(default_state);
        st.alt_stack = if (a.ss_flags & SS_DISABLE) != 0 {
            None
        } else {
            Some(*a)
        };
    }
    0
}

/// Return from a signal handler: copy the saved `SigFrame.ucontext` back into
/// the current kernel trap frame and restore the pre-delivery blocked mask.
///
/// Always invoked from the restorer trampoline, whose own `SIGRETURN` syscall
/// has the current frame at `kstack_top - SYSCALL_FRAME_SIZE`.
pub fn sigreturn() -> i64 {
    let cur = match crate::task::sched::current_task_id() {
        Some(t) => t.0,
        None => return crate::abi::errno::EPERM,
    };
    let fb = match STATES.lock().get(&cur).and_then(|st| st.frame_base) {
        Some(fb) => fb,
        None => return crate::abi::errno::EINVAL,
    };
    if !region_mapped(fb, core::mem::size_of::<SigFrame>()) {
        return crate::abi::errno::EINVAL;
    }
    let sf = unsafe { (fb as *const SigFrame).read() };
    if sf.magic != SIGFRAME_MAGIC {
        return crate::abi::errno::EINVAL;
    }
    // Overwrite the current (trampoline) trap frame with the saved context;
    // the stub then pops the registers and returns the process to its
    // pre-signal point. The frame lives at the very top of the kernel stack,
    // above this function's own frames, so the copy is safe.
    let kframe =
        crate::task::sched::current_kstack_top() - crate::task::context::SYSCALL_FRAME_SIZE;
    unsafe {
        core::ptr::copy_nonoverlapping(sf.ucontext.as_ptr(), kframe as *mut u8, TRAP_FRAME_SIZE);
    }
    // Hand the restored `rax` back to the syscall stub: it stashes the syscall
    // return value into the frame's rax slot *after* we have restored it, so
    // returning the saved value here preserves exactly what the user context
    // held (e.g. `-EINTR`), instead of clobbering it with `0`.
    let saved_rax = u64::from_le_bytes(sf.ucontext[..8].try_into().unwrap());
    {
        let mut s = STATES.lock();
        if let Some(st) = s.get_mut(&cur) {
            st.blocked = sf.mask & VALID_MASK;
            st.deferred = 0;
            st.frame_base = None;
            st.suspended = false;
        }
    }
    crate::log::kdebug!("sig: sigreturn restored pid {}", cur);
    saved_rax as i64
}

// ---------------------------------------------------------------------------
// Delivery
// ---------------------------------------------------------------------------

/// True when the whole `[addr, addr + len)` range is mapped in the *current*
/// (i.e. the calling process's) address space.
fn region_mapped(addr: usize, len: usize) -> bool {
    if len == 0 {
        return true;
    }
    let first = addr & !0xFFF;
    let last = (addr + len - 1) & !0xFFF;
    let mut page = first;
    while page <= last {
        if crate::memory::vmm::translate_current(page).is_none() {
            return false;
        }
        page += 0x1000;
    }
    true
}

/// Choose the handler stack for a delivery: the alternate stack when asked
/// for (and configured), otherwise the process's normal stack. Returns the
/// handler's `rsp` and whether delivery happened on the alternate stack.
fn stack_slot(st: &SigState, frame: &TrapFrame, want_alt: bool) -> Option<(usize, bool)> {
    let try_alt = |st: &SigState| -> Option<usize> {
        let alt = st.alt_stack?;
        if (alt.ss_flags & SS_DISABLE) != 0 || alt.ss_size < SIGFRAME_TOTAL {
            return None;
        }
        let x = (alt.ss_base + alt.ss_size - SIGFRAME_TOTAL) & !7;
        if region_mapped(x - 200, SIGFRAME_TOTAL) {
            Some(x)
        } else {
            None
        }
    };
    if want_alt {
        if let Some(x) = try_alt(st) {
            return Some((x, true));
        }
    }
    let x = (frame.rsp as usize - SIGFRAME_TOTAL) & !7;
    if region_mapped(x - 200, SIGFRAME_TOTAL) {
        return Some((x, false));
    }
    if let Some(x) = try_alt(st) {
        return Some((x, true));
    }
    None
}

/// Build a [`SigFrame`] under the handler stack and rewrite `frame` so the
/// stub returns into the handler. Returns false (leaving the signal pending
/// caller-side) when no usable stack exists.
fn deliver(st: &mut SigState, frame: &mut TrapFrame, sig: u32, handler: usize) -> bool {
    let action = st.actions[sig as usize];
    let (x, on_alt) = match stack_slot(st, frame, (action.sa_flags & SA_ONSTACK) != 0) {
        Some(v) => v,
        None => {
            crate::log::kwarn!(
                "sig: no stack to deliver sig {} to pid {}",
                sig,
                current_id_msg()
            );
            return false;
        }
    };
    let old_blocked = st.blocked;
    let handler_mask = if (action.sa_flags & SA_NODEFER) != 0 {
        old_blocked
    } else {
        old_blocked | (1u64 << sig)
    };
    st.blocked = handler_mask;
    st.frame_base = Some(x - 200); // SigFrame starts 200 bytes below the rsp

    let saved: &[u8] = unsafe { core::slice::from_raw_parts(frame as *const TrapFrame as *const u8, TRAP_FRAME_SIZE) };
    let ub = x - 200;
    unsafe {
        let f = ub as *mut SigFrame;
        core::ptr::addr_of_mut!((*f).magic).write(SIGFRAME_MAGIC);
        core::ptr::addr_of_mut!((*f).flags).write(if on_alt { SS_ONSTACK as u64 } else { 0 });
        core::ptr::addr_of_mut!((*f).mask).write(old_blocked);
        core::ptr::addr_of_mut!((*f).sig).write(sig);
        core::ptr::addr_of_mut!((*f).pad).write(0);
        let uc = core::ptr::addr_of_mut!((*f).ucontext) as *mut u8;
        core::ptr::copy_nonoverlapping(saved.as_ptr(), uc, TRAP_FRAME_SIZE);
        *(x as *mut u64) = action.sa_restorer as u64;
    }
    // Land in the handler: rdi = signal number, rsp = the configured stack
    // with the restorer sitting on top of it.
    frame.rax = 0;
    frame.rdi = sig as u64;
    frame.rip = handler as u64;
    frame.rsp = x as u64;
    crate::log::kdebug!(
        "sig: delivering sig {} to pid {} (handler {:#x}, rsp {:#x})",
        sig,
        current_id_msg(),
        handler,
        x
    );
    true
}

fn current_id_msg() -> usize {
    crate::task::sched::current_task_id()
        .map(|t| t.0)
        .unwrap_or(0)
}

/// Post-syscall hook called by the `syscall` assembly stub after a handler has
/// produced a result. Delivers pending catchable signals, drops ignored ones
/// and terminates the process for fatal defaults. Returns the value the stub
/// should pop into `rax` (normally the syscall result).
///
/// # Safety
/// Called only from `syscall_entry` with `frame` pointing at the live trap
/// frame on the current thread's kernel stack; interrupts are masked.
#[no_mangle]
pub unsafe extern "C" fn samsara_post_syscall(frame: *mut TrapFrame) -> i64 {
    let cur = match crate::task::sched::current_task_id() {
        Some(t) => t.0,
        None => return unsafe { (*frame).rax as i64 },
    };
    let mut st = STATES.lock().get(&cur).cloned().unwrap_or_else(default_state);
    // The interrupted (or completed) syscall is over: "deferred" signals from
    // an SA_RESTART retry are deliverable again.
    st.deferred = 0;

    loop {
        let act = actionable(&st);
        if act == 0 {
            break;
        }
        let sig = act.trailing_zeros() as u32;
        let bit = 1u64 << sig;
        match disposition(&st, sig) {
            Disposition::Default => {
                // Fatal default: the process dies here (never returns).
                let status = 128 + sig as i32;
                crate::log::kinfo!(
                    "sig: pid {} killed by sig {} (status {})",
                    cur,
                    sig,
                    status
                );
                crate::task::sched::exit_current(status)
            }
            Disposition::Ignore | Disposition::Stop => {
                st.pending &= !bit;
            }
            Disposition::Handler(addr) => {
                if st.frame_base.is_some() {
                    // A handler is already in flight: nest nothing; deliver
                    // after its `sigreturn`.
                    break;
                }
                st.pending &= !bit;
                if deliver(&mut st, unsafe { &mut *frame }, sig, addr) {
                    break;
                }
                // No stack: the bit was cleared, so the signal is dropped.
            }
        }
    }

    if let Some(s) = STATES.lock().get_mut(&cur) {
        *s = st;
    }
    unsafe { (*frame).rax as i64 }
}