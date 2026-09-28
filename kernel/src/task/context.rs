// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! Raw machine context switch for preemptive threads.
//!
//! Every thread owns a dedicated kernel stack. Whenever the scheduler hands
//! the CPU to another thread it calls [`switch_raw`], which saves the
//! currently active thread's callee-saved registers onto its own stack,
//! reloads the target thread's saved stack pointer, and resumes there. This
//! is the only place the stack pointer is swapped; all higher-level
//! scheduler state (run queues, blocking) lives above this primitive.
//!
//! Because the switch happens inside interrupt/scheduler context, a thread
//! that is preempted mid-interrupt simply resumes after the very same
//! `switch_raw` call when it is later switched back — the interrupt frame
//! stays on its kernel stack and unwinds naturally.

use core::arch::global_asm;

/// Number of 8-byte words a saved context occupies on the stack:
/// 6 callee-saved registers plus the return address into the trampoline.
const CTX_WORDS: usize = 7;

/// The entry point a brand-new thread resumes at (via `ret`) after its first
/// [`switch_raw`]. It reads the pending `(entry, arg)` from globals that the
/// scheduler sets immediately before the thread's first switch.
extern "C" {
    fn kernel_thread_trampoline();
    fn user_entry_trampoline();
    fn fork_return_trampoline();
}

/// `IA32_FS_BASE`, the MSR holding the x86-64 thread pointer (`%fs:0`).
pub const MSR_FS_BASE: u32 = 0xc000_0100;

/// Read the thread pointer the CPU is currently using.
///
/// Only meaningful at CPL 0, and only with interrupts off if the value is going
/// to be stored somewhere a context switch will later restore it from.
#[inline]
pub fn read_fs_base() -> usize {
    let lo: u32;
    let hi: u32;
    // SAFETY: `rdmsr` is unconditional at CPL 0; `0xc000_0100` is architecturally
    // `IA32_FS_BASE` on any x86-64 part.
    unsafe {
        core::arch::asm!("rdmsr", in("ecx") MSR_FS_BASE, out("eax") lo, out("edx") hi, options(nomem, nostack, preserves_flags));
    }
    ((hi as usize) << 32) | lo as usize
}

/// Install `base` as the CPU's thread pointer.
///
/// # Safety
/// CPL 0 only. With a single CPU this is process-global state, which is why the
/// scheduler brackets it with the same save/restore discipline it uses for CR3:
/// a libc that installs a TCB would otherwise leave every other process
/// pointing at it.
#[inline]
pub unsafe fn write_fs_base(base: usize) {
    // SAFETY: caller guarantees CPL 0.
    unsafe {
        core::arch::asm!("wrmsr", in("ecx") MSR_FS_BASE, in("eax") base as u32, in("edx") (base >> 32) as u32, options(nomem, nostack, preserves_flags));
    }
}

/// Saved outgoing stack pointer for a thread, to be resumed next time it is
/// switched back to.
pub type SavedRsp = usize;

global_asm!(
    ".section .text",
    ".align 16",
    ".globl switch_raw",
    ".type switch_raw, @function",
    // void switch_raw(usize *prev_rsp_out, usize next_rsp);
    //   rdi = store location for the current rsp
    //   rsi = target thread's saved_rsp
    "switch_raw:",
    "push rbp",
    "push rbx",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "mov [rdi], rsp",
    "mov rsp, rsi",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop rbx",
    "pop rbp",
    "ret",
    ".size switch_raw, . - switch_raw",
);

/// # Safety
/// Called only from the scheduler with interrupts disabled.
#[inline]
pub unsafe fn switch_raw(prev_rsp_out: *mut SavedRsp, next_rsp: SavedRsp) {
    // SAFETY: mirrors the `switch_raw` assembly contract above.
    unsafe {
        core::arch::asm!(
            "call switch_raw",
            in("rdi") prev_rsp_out,
            in("rsi") next_rsp,
            clobber_abi("C"),
        );
    }
}

global_asm!(
    ".section .text",
    ".align 16",
    ".globl switch_to_thread",
    ".type switch_to_thread, @function",
    // # Safety
    // The outgoing thread is being destroyed; nothing is saved back.
    // void switch_to_thread(usize next_rsp);
    //   rsi = target thread's saved_rsp
    "switch_to_thread:",
    "mov rsp, rsi",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop rbx",
    "pop rbp",
    "ret",
    ".size switch_to_thread, . - switch_to_thread",
);

/// Jump to `next_rsp` without saving the current context (used when the
/// running thread is destroyed by `EXIT`).
///
/// # Safety
/// The current thread's kernel stack must be abandoned deliberately.
#[inline]
pub unsafe fn switch_to_thread(next_rsp: SavedRsp) {
    // SAFETY: caller guarantees the outgoing thread never resumes.
    unsafe {
        core::arch::asm!(
            "call switch_to_thread",
            in("rsi") next_rsp,
            clobber_abi("C"),
        );
    }
}

/// Global entry/argument handed to a fresh thread on its very first switch.
/// Only meaningful during the brief window before a never-run thread is
/// switched to (single CPU, interrupts off).
static ENTRY_FN: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
static ENTRY_ARG: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

/// Stage the entry point + argument for a thread about to run for the first
/// time.
pub(crate) fn stage_entry(entry: fn(usize), arg: usize) {
    ENTRY_FN.store(entry as usize, core::sync::atomic::Ordering::Relaxed);
    ENTRY_ARG.store(arg, core::sync::atomic::Ordering::Relaxed);
}

global_asm!(
    ".section .text",
    ".align 16",
    ".globl kernel_thread_trampoline",
    ".type kernel_thread_trampoline, @function",
    "kernel_thread_trampoline:",
    "mov rax, [rip + {efn}]",
    "mov rdi, [rip + {earg}]",
    "call rax",
    // If the thread function ever returns, park this CPU forever.
    "1:",
    "cli",
    "hlt",
    "jmp 1b",
    ".size kernel_thread_trampoline, . - kernel_thread_trampoline",
    efn = sym ENTRY_FN,
    earg = sym ENTRY_ARG,
);

/// Build an initial saved-rsp for a brand-new thread whose kernel stack ends
/// at `stack_end` (exclusive). Returns the stack pointer the thread's first
/// [`switch_raw`] will resume from.
///
/// Layout on the (empty) kernel stack, from low to high address:
/// `rbp, rbx, r12, r13, r14, r15, trampoline`.
pub fn initial_saved_rsp(stack_end: usize) -> usize {
    let sp = stack_end - CTX_WORDS * 8;
    unsafe {
        // Place the trampoline return address at the top of the context.
        (sp as *mut usize)
            .add(CTX_WORDS - 1)
            .write(kernel_thread_trampoline as *const () as usize);
    }
    sp
}

// ---------------------------------------------------------------------------
// User threads
// ---------------------------------------------------------------------------

/// Global user entry / stack staged for a fresh user thread before its first
/// switch.
static USER_ENTRY: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
static USER_STACK: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

/// Stage the ring-3 entry point and stack top for a thread about to run for
/// the first time as a user process.
pub(crate) fn stage_user_entry(entry: usize, stack_top: usize) {
    USER_ENTRY.store(entry, core::sync::atomic::Ordering::Relaxed);
    USER_STACK.store(stack_top, core::sync::atomic::Ordering::Relaxed);
}

global_asm!(
    ".section .text",
    ".align 16",
    ".globl user_entry_trampoline",
    ".type user_entry_trampoline, @function",
    // Resumed by switch_raw for a thread that has never run: hand control to
    // ring 3 via an iretq frame built on this thread's kernel stack.
    "user_entry_trampoline:",
    "mov rax, [rip + {uentry}]",     // user rip
    "mov rbx, [rip + {ustack}]",     // user rsp
    "mov rdx, [rip + {ucs}]",
    "mov rcx, [rip + {uss}]",
    // Establish ring-3 data segments before dropping to CPL 3.
    "mov ds, rcx",
    "mov es, rcx",
    "mov fs, rcx",
    "mov gs, rcx",
    // Build the whole iretq frame first. iretq pops rip, cs, rflags, rsp, ss,
    // so the pushes happen in reverse; doing them here leaves every register
    // free for the FS-base load below, which would otherwise clobber the cs
    // value held in rdx.
    "push rcx",                      // ss
    "push rbx",                      // user rsp
    "push qword ptr 0x202",          // rflags: IF | reserved bit 1
    "push rdx",                      // cs
    "push rax",                      // rip
    // Load IA32_FS_BASE while still at CPL 0. A ring-3 `wrfsbase` needs
    // CR4.FSGSBASE (which this kernel does not set) and a ring-3 `wrmsr` is not
    // privileged, so a libc that wants a thread pointer has no way to install
    // one for itself. Zeroing it here means FS starts at a defined, harmless
    // value rather than whatever the previously-run task left behind.
    "mov ecx, 0xc0000100",           // IA32_FS_BASE
    "xor eax, eax",
    "wrmsr",
    "iretq",
    ".size user_entry_trampoline, . - user_entry_trampoline",
    uentry = sym USER_ENTRY,
    ustack = sym USER_STACK,
    ucs = sym USER_CS_VALUE,
    uss = sym USER_SS_VALUE,
);
global_asm!(
    ".section .text",
    ".align 16",
    ".globl user_entry_trampoline_noif",
    ".type user_entry_trampoline_noif, @function",
    "user_entry_trampoline_noif:",
    "mov rax, [rip + {uentry}]",
    "mov rbx, [rip + {ustack}]",
    "mov rdx, [rip + {ucs}]",
    "mov rcx, [rip + {uss}]",
    "mov ds, rcx",
    "mov es, rcx",
    "mov fs, rcx",
    "mov gs, rcx",
    // Frame first, then the FS base: see the comment in user_entry_trampoline.
    "push rcx",
    "push rbx",
    "push qword ptr 0x2",
    "push rdx",
    "push rax",
    "mov ecx, 0xc0000100",
    "xor eax, eax",
    "wrmsr",
    "iretq",
    ".size user_entry_trampoline_noif, . - user_entry_trampoline_noif",
    uentry = sym USER_ENTRY,
    ustack = sym USER_STACK,
    ucs = sym USER_CS_VALUE,
    uss = sym USER_SS_VALUE,
);

/// Ring-3 code selector (with RPL bits).
static USER_CS_VALUE: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(crate::interrupts::gdt::USER_CODE as usize | 3);
/// Ring-3 data selector (with RPL bits).
static USER_SS_VALUE: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(crate::interrupts::gdt::USER_DATA as usize | 3);

/// Build an initial saved-rsp for a brand-new *user* thread. The fabricated
/// context resumes into [`user_entry_trampoline`], which transfers to ring 3.
pub fn initial_user_saved_rsp(stack_end: usize) -> usize {
    let sp = stack_end - CTX_WORDS * 8;
    unsafe {
        (sp as *mut usize)
            .add(CTX_WORDS - 1)
            .write(user_entry_trampoline as *const () as usize);
    }
    sp
}

// ---------------------------------------------------------------------------
// Forked processes
// ---------------------------------------------------------------------------

/// Size in bytes of the ring-0 syscall trap frame built by `syscall_entry`
/// (20 quadwords, see `abi/mod.rs`). Fork copies the parent's frame verbatim
/// into the child's kernel stack so the child resumes user land right after
/// the `syscall` instruction, exactly as the parent would have.
pub const SYSCALL_FRAME_SIZE: usize = 20 * 8;

/// The child resumes by replaying `syscall_entry`'s frame-restore tail. The
/// staged frame base pointer is carried in [`USER_ENTRY`]; [`USER_STACK`] is
/// unused by this trampoline (kept for the shared [`stage_user_entry`] path).
global_asm!(
    ".section .text",
    ".align 16",
    ".globl fork_return_trampoline",
    ".type fork_return_trampoline, @function",
    "fork_return_trampoline:",
    // rsp = copy of the parent's syscall frame (rax slot already zeroed).
    "mov rsp, [rip + {fentry}]",
    "pop rax",       // result: 0 in the child
    "pop rcx",
    "pop rdx",
    "pop rsi",
    "pop rdi",
    "pop r8",
    "pop r9",
    "pop r10",
    "pop r12",
    "pop r13",
    "pop r14",
    "pop r15",
    "pop rbx",
    "pop rbp",
    "add rsp, 8",    // discard syscall nr
    "pop rcx",       // user rip
    "add rsp, 8",    // discard user cs placeholder
    "pop r11",       // user rflags
    "pop rsp",       // back to the user stack
    "sysretq",
    ".size fork_return_trampoline, . - fork_return_trampoline",
    fentry = sym USER_ENTRY,
);

/// Build an initial saved-rsp for a freshly forked child thread. Above the
/// 7-word switch context the caller copy the 160-byte parent syscall frame at
/// `stack_end - 160`; the child's first resume returns to ring 3 through that
/// frame with `rax = 0`.
pub fn initial_fork_saved_rsp(stack_end: usize) -> usize {
    let sp = stack_end - SYSCALL_FRAME_SIZE - CTX_WORDS * 8;
    unsafe {
        (sp as *mut usize)
            .add(CTX_WORDS - 1)
            .write(fork_return_trampoline as *const () as usize);
    }
    sp
}
