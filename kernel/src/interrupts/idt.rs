// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! Interrupt Descriptor Table with full exception coverage.

use crate::sync::OnceCell;
use core::arch::asm;

const ENTRY_COUNT: usize = 256;

#[repr(C, align(8))]
struct Entry {
    offset_low: u16,
    selector: u16,
    ist: u8,
    type_attr: u8,
    offset_mid: u16,
    offset_high: u32,
    reserved: u32,
}

impl Entry {
    const MISSING: Entry = Entry {
        offset_low: 0,
        selector: 0,
        ist: 0,
        type_attr: 0,
        offset_mid: 0,
        offset_high: 0,
        reserved: 0,
    };
}

#[repr(C)]
struct Idt([Entry; ENTRY_COUNT]);

/// The 64-bit IDT descriptor register image (`lidt` operand).
#[repr(C, packed(2))]
struct Pointer {
    limit: u16,
    base: usize,
}

static mut IDT: Idt = Idt([Entry::MISSING; ENTRY_COUNT]);
static LOADED: OnceCell<()> = OnceCell::new();

/// One thunk per vector, 0..=255, all pointing at [`unexpected_vector`].
///
/// The reason this exists: a gate whose present bit is clear is not a gate. If
/// the CPU is asked to deliver a vector the table does not define, it does not
/// jump to a handler and return -- it raises `#GP` as a *pseudo-fault*, without
/// pushing a frame. The `#GP` handler then reads whatever happened to be on the
/// stack, so the reported faulting `rip` is a stale value from unrelated code
/// and the error code is garbage. That is close to undiagnosable: it looks like
/// a jump to a wild address, and indeed the "faulting instruction" turns out to
/// be a `testb` in `kmain` that cannot possibly fault.
///
/// So every vector gets a real gate. The ones the kernel drives land in their
/// proper handlers; anything else arrives at [`unexpected_vector`], which says
/// which vector it was. A defined answer beats a garbage one.
extern "x86-interrupt" fn unexpected_vector(frame: InterruptFrame) {
    // The vector number is not in the frame, so identify it the way the
    // hardware does: ask the local APIC what it had pending and in service.
    crate::log::kerror!(
        "#GP rip={:#x} rsp={:#x} cs={:#x} -- unexpected vector, IDT gate missing?",
        frame.rip,
        frame.rsp,
        frame.cs
    );
    super::apic::diag_dump_vectors();
    super::ioapic::diag_dump_redirs();
    die("unexpected interrupt vector", 0);
}

/// Give every vector a defined gate before the specific ones are installed.
///
/// A gate whose present bit is clear is not a gate. If the CPU is asked to
/// deliver a vector the table does not define, it does not jump to a handler and
/// return -- it raises `#GP` as a *pseudo-fault*, without pushing a frame. The
/// `#GP` handler then reads whatever happened to be on the stack, so the
/// reported faulting `rip` is a stale value from unrelated code and the error
/// code is garbage. That is close to undiagnosable: it presents as a jump to a
/// wild address, and the "faulting instruction" turns out to be a `testb` in
/// `kmain` that cannot possibly fault.
///
/// This is what that intermittent boot-time `#GP` turned out to be. Filling the
/// table means a stray vector lands in [`unexpected_vector`], which says so
/// and dumps the APIC state, instead of corrupting the picture.
fn install_default_gates() {
    for v in 0..ENTRY_COUNT as u8 {
        set_entry_ist(v, unexpected_vector as usize, 0);
    }
}

/// Install a gate on the emergency stack (IST 1) for a CPU exception.
fn set_entry(vector: u8, handler: usize) {
    set_entry_ist(vector, handler, 1);
}

fn set_entry_ist(vector: u8, handler: usize, ist: u8) {
    unsafe {
        let e = &mut IDT.0[vector as usize];
        *e = Entry {
            offset_low: (handler & 0xFFFF) as u16,
            selector: super::gdt::KERNEL_CODE,
            ist,
            type_attr: 0x8E,
            offset_mid: ((handler >> 16) & 0xFFFF) as u16,
            offset_high: (handler >> 32) as u32,
            reserved: 0,
        };
    }
}

/// # Safety
/// Must only run once, after the GDT is installed.
unsafe fn load() {
    let p = Pointer {
        limit: (core::mem::size_of::<Idt>() - 1) as u16,
        base: (&raw const IDT).addr(),
    };
    asm!("lidt [{}]", in(reg) &p, options(nostack));
}

#[repr(C)]
struct InterruptFrame {
    rip: usize,
    cs: usize,
    rflags: usize,
    rsp: usize,
    ss: usize,
}extern "x86-interrupt" fn divide_error(_: InterruptFrame) {
    die("divide error (#DE)", 0);
}
extern "x86-interrupt" fn debug_trap(_: InterruptFrame) {
    log_fault("debug trap");
}
extern "x86-interrupt" fn breakpoint(_: InterruptFrame) {
    log_fault("breakpoint (#BP)");
}
extern "x86-interrupt" fn overflow(_: InterruptFrame) {
    die("overflow (#OF)", 0);
}
extern "x86-interrupt" fn bound_range(_: InterruptFrame) {
    die("bound range exceeded (#BR)", 0);
}
extern "x86-interrupt" fn invalid_opcode(f: InterruptFrame) {
    // The faulting instruction pointer is the only clue about what executed
    // something illegal; without it a #UD from ring 3 is unactionable.
    crate::log::kerror!("#UD rip={:#x} rsp={:#x}", f.rip, f.rsp);
    die("invalid opcode (#UD)", 0);
}
extern "x86-interrupt" fn device_not_available(_: InterruptFrame) {
    die("device not available (#NM)", 0);
}
extern "x86-interrupt" fn double_fault(frame: InterruptFrame, error: u64) -> ! {
    raw_diag(b"DF", error as usize, frame.rip, 0);
    panic!("double fault (#DF), error={:#x}", error);
}
extern "x86-interrupt" fn invalid_tss(_: InterruptFrame, error: u64) {
    die("invalid TSS (#TS)", error);
}
extern "x86-interrupt" fn segment_not_present(_: InterruptFrame, error: u64) {
    die("segment not present (#NP)", error);
}
extern "x86-interrupt" fn stack_fault(_: InterruptFrame, error: u64) {
    raw_diag(b"[#SS]", error as usize, 0, 0);
    die("stack fault (#SS)", error);
}
extern "x86-interrupt" fn general_protection(frame: InterruptFrame, error: u64) {
    raw_diag(b"GP", frame.rip, error as usize, frame.rsp);
    crate::log::kerror!("#GP rip={:#x} err={:#x}", frame.rip, error);
    // Which stack are we on? An IST stack means this fault happened *inside*
    // an interrupt handler, which points at the handler; the boot or task
    // stack means it happened in ordinary kernel code. Cheap to print, and it
    // splits those two possibilities immediately.
    crate::log::kerror!(
        "    rsp={:#x} cs={:#x} on_ist0={} ist0={:#x}",
        frame.rsp,
        frame.cs,
        frame.rsp >= super::gdt::current_ist0(),
        super::gdt::current_ist0(),
    );
    // Panic-state: a sign-extended vector in the error code means an
    // interrupt with no IDT gate was delivered. Dump what the LAPIC and
    // IO-APIC believe is pending/in-service before we die.
    super::apic::diag_dump_vectors();
    super::ioapic::diag_dump_redirs();
    die("general protection fault (#GP)", error);
}
extern "x86-interrupt" fn alignment_check(_: InterruptFrame, error: u64) {
    die("alignment check (#AC)", error);
}
extern "x86-interrupt" fn simd_floating_point(_: InterruptFrame) {
    die("SIMD floating point exception (#XM)", 0);
}
extern "x86-interrupt" fn control_protection(_: InterruptFrame, error: u64) {
    die("control protection (#CP)", error);
}
extern "x86-interrupt" fn virtualization(_: InterruptFrame) {
    die("virtualization exception (#VE)", 0);
}
extern "x86-interrupt" fn security(_: InterruptFrame, error: u64) {
    die("security exception (#SX)", error);
}

extern "x86-interrupt" fn page_fault(frame: InterruptFrame, error: u64) {
    let cr2: usize;
    unsafe { asm!("mov {}, cr2", out(reg) cr2, options(nomem, nostack)) };
    raw_diag(b"PF", cr2, error as usize, frame.rip);
    // Full register dump: the values below are the live GP registers at
    // fault time (x86-interrupt ABI keeps them in-place on the stack).
    let mut regs: [usize; 15] = [0; 15];
    unsafe {
        asm!(
            "mov [{out} + 0x00], rax",
            "mov [{out} + 0x08], rbx",
            "mov [{out} + 0x10], rcx",
            "mov [{out} + 0x18], rdx",
            "mov [{out} + 0x20], rsi",
            "mov [{out} + 0x28], rdi",
            "mov [{out} + 0x30], rbp",
            "mov [{out} + 0x38], r8",
            "mov [{out} + 0x40], r9",
            "mov [{out} + 0x48], r10",
            "mov [{out} + 0x50], r11",
            "mov [{out} + 0x58], r12",
            "mov [{out} + 0x60], r13",
            "mov [{out} + 0x68], r14",
            "mov [{out} + 0x70], r15",
            out = in(reg) (&regs as *const [usize;15]) as usize,
            options(nomem)
        );
        core::ptr::write_volatile(&raw mut REG_DUMP, regs);
    }
    crate::log::kerror!(
        "#PF rip={:#x} cr2={:#x} err={:#x} rsp={:#x} rax={:#x} rbx={:#x} rcx={:#x} rdx={:#x}",
        frame.rip, cr2, error, frame.rsp,
        unsafe{REG_DUMP[0]}, unsafe{REG_DUMP[1]}, unsafe{REG_DUMP[2]}, unsafe{REG_DUMP[3]},
    );
    // Context dump. A faulting register dump is much easier to read when it
    // says *which* task was running and whether the MMU held the address
    // space that task is supposed to have: a frame pointer holding a
    // user-space value, for instance, is either a corrupted context or the
    // wrong CR3, and the two need very different fixes.
    crate::log::kerror!(
        "    name={} cr3={:#x} want={:#x} rbp={:#x} r15={:#x} r14={:#x}",
        crate::task::sched::current_name(),
        read_cr3(),
        match crate::task::sched::current_as_root() {
            Some(a) => a,
            None => 0,
        },
        unsafe{REG_DUMP[6]}, unsafe{REG_DUMP[13]}, unsafe{REG_DUMP[12]},
    );
    die("unhandled page fault", error);
}

/// The CR3 currently loaded in the MMU. Read directly rather than trusting
/// the scheduler's shadow copy: the point of the dump is to catch the two
/// disagreeing.
fn read_cr3() -> usize {
    let v: usize;
    unsafe { asm!("mov {}, cr3", out(reg) v, options(nomem, nostack)) };
    v
}

static mut REG_DUMP: [usize; 15] = [0; 15];

extern "x86-interrupt" fn timer(_frame: InterruptFrame) {
    // EOI first: the scheduler may switch away mid-handler and the
    // interrupted context will not run its tail until far in the future.
    //
    // The Rust `x86-interrupt` ABI restores IF before the body runs, but this
    // handler context-switches tasks (`do_switch` requires interrupts off).
    // With IF live, a second timer IRQ lands on the same kernel stack while
    // the first handler is mid-switch and clobbers the interrupted task's
    // frame, so its eventual `iretq` faults. Pin IF down for the whole body;
    // the frame's saved RFLAGS restores it on return.
    unsafe {
        core::arch::asm!("cli", options(nostack));
    }
    super::eoi(0);
    super::on_timer_irq();
}

/// LAPIC spurious-interrupt vector (`SVR`): no source, nothing to EOI.
extern "x86-interrupt" fn spurious(_: InterruptFrame) {
    crate::log::kdebug!("apic: spurious interrupt");
}

/// Local APIC error interrupt: clear + re-arm the error status.
extern "x86-interrupt" fn lapic_error(_: InterruptFrame) {
    crate::log::kdebug!("apic: error interrupt");
    super::apic::clear_error();
}

// --- generic driver IRQ routing -----------------------------------------

/// Signature of driver-level IRQ bottom halves (interrupts still off).
pub type IrqFn = fn();

static IRQ_ROUTES: [core::sync::atomic::AtomicUsize; 16] =
    [const { core::sync::atomic::AtomicUsize::new(0) }; 16];

/// Point an ISA IRQ line at a handler function. Multiple registrations
/// overwrite the previous one; passing `None`-equivalent is not supported
/// in this minimal router. Unmasks the IO-APIC line so the edge is
/// delivered.
pub fn route_irq(irq: u8, f: IrqFn) {
    use core::sync::atomic::Ordering;
    if (irq as usize) < IRQ_ROUTES.len() {
        IRQ_ROUTES[irq as usize].store(f as usize, Ordering::Release);
        super::ioapic::unmask(irq);
    }
}

fn dispatch_irq(irq: usize) {
    use core::sync::atomic::Ordering;
    // EOI before the bottom half: handlers may block/switch.
    super::eoi(irq as u8);
    // On the microkernel IRQ-as-IPC path, a bound line belongs to the
    // owning user-space driver; skip any legacy kernel handler so the
    // device byte is never double-consumed.
    if IRQ_BINDINGS[irq].load(Ordering::Acquire) != 0 {
        deliver_bound_irq(irq);
        return;
    }
    let f = IRQ_ROUTES[irq].load(Ordering::Acquire);
    if f != 0 {
        // SAFETY: only set via route_irq with a valid `fn()` pointer.
        let g: IrqFn = unsafe { core::mem::transmute(f) };
        g();
    }
}

// --- IRQ-as-IPC ----------------------------------------------------------

/// Per-IRQ endpoints that receive interrupt messages (`0` = kernel-owned).
static IRQ_BINDINGS: [core::sync::atomic::AtomicU64; 16] =
    [const { core::sync::atomic::AtomicU64::new(0) }; 16];

/// Route future IRQ `irq` lines as IPC messages to the process owning
/// `ep`. Fails if the line is already bound or is the timer.
pub fn bind_irq(irq: u8, ep: crate::ipc::EndpointId) -> Result<(), ()> {
    use core::sync::atomic::Ordering;
    if irq == 0 || irq as usize >= IRQ_BINDINGS.len() || !crate::ipc::endpoint_alive(ep) {
        return Err(());
    }
    let prev = IRQ_BINDINGS[irq as usize].swap(ep, Ordering::AcqRel);
    if prev != 0 {
        // Someone else held it; restore and refuse.
        IRQ_BINDINGS[irq as usize].store(prev, Ordering::Release);
        return Err(());
    }
    // The PS/2 probe can leave a stray byte in the i8042 output buffer whose
    // IRQ edge was dropped while the line was still masked; with the line now
    // edge-triggered that byte would hold the line asserted and swallow every
    // later key edge. Drain the controller before unmasking.
    if irq == 1 {
        crate::devices::ps2::flush_input();
    }
    crate::log::kinfo!("irq: line {} bound to endpoint {}", irq, ep);
    super::ioapic::unmask(irq);
    Ok(())
}

/// Whether `irq` is currently routed to a user-space endpoint. The idle
/// polling fallback uses this to stay out of a driver-owned device's way.
pub fn irq_is_bound(irq: u8) -> bool {
    use core::sync::atomic::Ordering;
    (irq as usize) < IRQ_BINDINGS.len()
        && IRQ_BINDINGS[irq as usize].load(Ordering::Acquire) != 0
}

/// Release an IRQ line back to kernel ownership.
pub fn unbind_irq(irq: u8) {
    use core::sync::atomic::Ordering;
    if (irq as usize) < IRQ_BINDINGS.len() {
        IRQ_BINDINGS[irq as usize].store(0, Ordering::Release);
        super::ioapic::mask(irq);
    }
}

/// Delivery path called from interrupt context. Uses non-blocking locks so a
/// contended endpoint table or scheduler never deadlocks an IRQ; a dropped
/// interrupt is simply re-polled by the driver.
fn deliver_bound_irq(irq: usize) {
    use core::sync::atomic::Ordering;
    let ep = IRQ_BINDINGS[irq].load(Ordering::Acquire);
    if ep == 0 {
        return;
    }
    crate::ipc::deliver_irq(ep, irq as u8);
}

macro_rules! irq_router {
    ($name:ident, $n:expr) => {
        extern "x86-interrupt" fn $name(_: InterruptFrame) {
            dispatch_irq($n);
        }
    };
}
irq_router!(irq1_router, 1);
irq_router!(irq3_router, 3);
irq_router!(irq4_router, 4);
irq_router!(irq5_router, 5);
irq_router!(irq6_router, 6);
irq_router!(irq7_router, 7);
irq_router!(irq8_router, 8);
irq_router!(irq9_router, 9);
irq_router!(irq10_router, 10);
irq_router!(irq11_router, 11);
irq_router!(irq12_router, 12);
irq_router!(irq13_router, 13);
irq_router!(irq14_router, 14);
irq_router!(irq15_router, 15);

fn log_fault(name: &str) {
    crate::log::kdebug!("trap: {}", name);
}

/// Raw single-port diagnostic that works even when the logging/serial stack is
/// broken. Dumps three values in hex straight to COM1.
fn raw_diag(tag: &[u8], a: usize, b: usize, c: usize) {
    fn hex(v: usize) {
        let mut d = [0u8; 18];
        d[0] = b'0';
        d[1] = b'x';
        for i in 0..16 {
            let sh = (15 - i) * 4;
            let n = ((v >> sh) & 0xF) as u8;
            d[2 + i] = if n < 10 { b'0' + n } else { b'a' + (n - 10) };
        }
        for &b in &d {
            crate::io::uart::send(b);
        }
    }
    crate::io::uart::send(b'[');
    for &b in tag {
        crate::io::uart::send(b);
    }
    crate::io::uart::send(b']');
    hex(a);
    crate::io::uart::send(b' ');
    hex(b);
    crate::io::uart::send(b' ');
    hex(c);
    crate::io::uart::send(b'\n');
}

/// Report a fatal fault and halt the machine.
fn die(name: &str, error: u64) -> ! {
    panic!("{} (error={:#x})", name, error)
}

/// Populate and activate the IDT.
pub fn init() {
    assert!(LOADED.get().is_none(), "IDT initialized twice");

    // Fill every vector first, so no interrupt can ever find the table
    // undefined. The specific gates below overwrite the ones that matter.
    install_default_gates();

    set_entry(0, divide_error as usize);
    set_entry(1, debug_trap as usize);
    set_entry(3, breakpoint as usize);
    set_entry(4, overflow as usize);
    set_entry(5, bound_range as usize);
    set_entry(6, invalid_opcode as usize);
    set_entry(7, device_not_available as usize);
    set_entry(8, double_fault as usize);
    set_entry(10, invalid_tss as usize);
    set_entry(11, segment_not_present as usize);
    set_entry(12, stack_fault as usize);
    set_entry(13, general_protection as usize);
    set_entry(14, page_fault as usize);
    set_entry(17, alignment_check as usize);
    set_entry(19, simd_floating_point as usize);
    set_entry(20, virtualization as usize);
    set_entry(21, control_protection as usize);
    set_entry(30, security as usize);

    // Local APIC support vectors (Vectors 0xFE/0xFF) — always installed,
    // harmless while the PIC-only path keeps them masked.
    set_entry_ist(0xFE, lapic_error as usize, 0);
    set_entry_ist(0xFF, spurious as usize, 0);

    // Hardware IRQ vectors live at 0x20.. after the PIC remap.
    set_entry_ist(32 + 0, timer as usize, 0);

    // Generic routable stubs for every driver IRQ line; endpoints bind to
    // these at runtime via `bind_irq`.
    set_entry_ist(32 + 1, irq1_router as usize, 0);
    set_entry_ist(32 + 3, irq3_router as usize, 0);
    set_entry_ist(32 + 4, irq4_router as usize, 0);
    set_entry_ist(32 + 5, irq5_router as usize, 0);
    set_entry_ist(32 + 6, irq6_router as usize, 0);
    set_entry_ist(32 + 7, irq7_router as usize, 0);
    set_entry_ist(32 + 8, irq8_router as usize, 0);
    set_entry_ist(32 + 9, irq9_router as usize, 0);
    set_entry_ist(32 + 10, irq10_router as usize, 0);
    set_entry_ist(32 + 11, irq11_router as usize, 0);
    set_entry_ist(32 + 12, irq12_router as usize, 0);
    set_entry_ist(32 + 13, irq13_router as usize, 0);
    set_entry_ist(32 + 14, irq14_router as usize, 0);
    set_entry_ist(32 + 15, irq15_router as usize, 0);

    unsafe { load() };
    LOADED.set(()).ok().unwrap_or_default();
}
