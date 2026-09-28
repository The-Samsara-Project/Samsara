// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! Global Descriptor Table and Task State Segment.
//!
//! Selector layout (also mirrored into `STAR` by [`crate::abi`]):
//!
//! | Index | Selector | Purpose            |
//! |-------|----------|--------------------|
//! | 1     | 0x08     | kernel code (ring0)|
//! | 2     | 0x10     | kernel data        |
//! | 3     | 0x18     | user code (ring3)  |
//! | 4     | 0x20     | user data          |
//! | 5     | 0x28     | TSS                |

use crate::sync::OnceCell;

/// Kernel code selector.
pub const KERNEL_CODE: u16 = 0x08;
/// Kernel data selector.
pub const KERNEL_DATA: u16 = 0x10;
/// User data selector (ring 3). Placed just below [`USER_CODE`] to satisfy
/// the `sysret` selector arithmetic (it loads CS = base+16, SS = base+8, so
/// the data segment must sit at a lower index than the code segment).
pub const USER_DATA: u16 = 0x18;
/// User code selector (ring 3).
pub const USER_CODE: u16 = 0x20;
/// TSS selector.
pub const TSS_SELECTOR: u16 = 0x28;

/// Six visible selectors plus the two extra quadwords a 64-bit TSS
/// descriptor occupies (it is 16 bytes wide in long mode).
const GDT_ENTRIES: usize = 8;

#[repr(C, packed(4))]
#[derive(Clone, Copy)]
struct Entry(u64);

impl Entry {
    const NULL: Entry = Entry(0);
    /// Ring-0 64-bit code: P=1, DPL=0, S=1, exec/read, L=1, G=1.
    const KERNEL_CODE: Entry = Entry(0x00AF_9A00_0000_FFFF);
    /// Ring-0 data: P=1, DPL=0, S=1, read/write, big.
    const KERNEL_DATA: Entry = Entry(0x00CF_9200_0000_FFFF);
    /// Ring-3 64-bit code.
    const USER_CODE: Entry = Entry(0x00AF_FA00_0000_FFFF);
    /// Ring-3 data.
    const USER_DATA: Entry = Entry(0x00CF_F200_0000_FFFF);

    /// Build a TSS system descriptor from its linear address and size.
    ///
    /// Layout: limit[15:0] | base[23:0] @16 | type/access @40 |
    /// base[31:24] @56.
    const fn system(base: usize, limit: usize) -> Entry {
        let b = base as u64;
        Entry(
            (limit as u64 & 0xFFFF)
                | ((b & 0xFF_FFFF) << 16)
                | (0x89u64 << 40) // present | TSS available (64-bit)
                | (((b >> 24) & 0xFF) << 56),
        )
    }
}

#[repr(C, packed(2))]
struct Pointer {
    limit: u16,
    base: usize,
}

/// Task State Segment as defined by AMD64.
///
/// `packed(4)` keeps the fields at their architectural offsets: the AMD64 TSS
/// places `RSP0` at byte `0x04`, `RSP1`/`RSP2` at `0x0C`/`0x14`, `IST1..7` at
/// `0x24`, and the I/O-map base at `0x66`. A plain `repr(C)` struct would pad
/// `rsp` to byte `8` for `usize` alignment, as a result the CPU reads a zero
/// `RSP0`/`IST`, and the first ring-3->ring-0 interrupt faults on a
/// non-canonical stack push (`#GP`).
#[repr(C, packed(4))]
pub struct TaskStateSegment {
    reserved0: u32,
    /// Stack pointers used on privilege-level switches / IST activation.
    pub rsp: [usize; 3],
    reserved1: u64,
    /// Interrupt stack table pointers.
    pub ist: [usize; 7],
    reserved2: [u8; 10],
    io_map_base: u16,
    /// I/O permission bitmap. Bit `n` of byte `i` controls port `i*8+n`;
    /// a **cleared** bit permits ring-3 access. Trailing `0xFF` denies every
    /// port beyond the 128 covered bytes (ports 0..1023).
    pub io_bitmap: [u8; 129],
}

impl TaskStateSegment {
    const fn new() -> Self {
        Self {
            reserved0: 0,
            rsp: [0; 3],
            reserved1: 0,
            ist: [0; 7],
            reserved2: [0; 10],
            io_map_base: 104,
            io_bitmap: [0xFF; 129],
        }
    }
}

impl TaskStateSegment {
    /// Load a process's permitted I/O port bitmap into the TSS. Called on
    /// every context switch to a ring-3 process.
    pub fn set_io_bitmap(&mut self, allowed: &[u8; 128]) {
        self.io_bitmap[..128].copy_from_slice(allowed);
        self.io_bitmap[128] = 0xFF;
    }
}

// IST stack 0: double fault / NMI safety net.
static mut IST_STACKS: [[u8; 16384]; 2] = [[0; 16384]; 2];

static mut TSS: TaskStateSegment = TaskStateSegment::new();
static mut GDT_AREA: [Entry; GDT_ENTRIES] = [
    Entry::NULL,
    Entry::KERNEL_CODE, // 0x08
    Entry::KERNEL_DATA, // 0x10
    Entry::USER_DATA,   // 0x18 (data below code for sysret)
    Entry::USER_CODE,   // 0x20
    Entry(0),           // 0x28: TSS lower quadword, filled at init
    Entry(0),           // TSS upper quadword, filled at init
    Entry::NULL,
];

static LOADED: OnceCell<()> = OnceCell::new();

/// Install the kernel GDT/TSS and reload all segment registers.
pub fn init() {
    // IST[0]: double-fault safety net; IST[1]: every other exception/IRQ
    // (the IDT routes these through IST index 1 so they never depend on a
    // possibly-corrupted RSP0).
    //
    unsafe {
        TSS.ist[0] = (&raw const IST_STACKS[0]).addr() + 16384;
        TSS.ist[1] = (&raw const IST_STACKS[1]).addr() + 16384;

        let tss_addr = (&raw const TSS).addr();
        GDT_AREA[5] = Entry::system(tss_addr, size_of::<TaskStateSegment>() - 1);
        // Upper half of the 16-byte TSS descriptor: base[63:32], rest zero.
        GDT_AREA[6] = Entry(((tss_addr as u64) >> 32) & 0xFFFF_FFFF);
        GDT_AREA[7] = Entry::NULL;

        let gp = Pointer {
            limit: (size_of::<[Entry; GDT_ENTRIES]>() - 1) as u16,
            base: (&raw const GDT_AREA).addr(),
        };

        let data_sel: u16 = KERNEL_DATA;
        let tss_sel: u16 = TSS_SELECTOR;

        core::arch::asm!(
            "cli",
            "lgdt [{gdt_ptr}]",
            "push {kcode}",
            "lea {tmp}, [rip + 3f]",
            "push {tmp}",
            "retfq",
            "3:",
            "mov ds, [{dptr}]",
            "mov es, [{dptr}]",
            "mov ss, [{dptr}]",
            "mov fs, [{dptr}]",
            "mov gs, [{dptr}]",
            gdt_ptr = in(reg) &gp,
            kcode = in(reg) KERNEL_CODE as usize,
            tmp = out(reg) _,
            dptr = in(reg) &data_sel,
        );
        unsafe {
            core::arch::asm!("ltr [{tptr}]", tptr = in(reg) &tss_sel);
        }

        // CR0/CR4 feature bits that user space depends on.
        //
        // `CR4.OSFXSR` (bit 9) is the one that matters most: without it, any SSE
        // instruction executed at CPL 3 raises #UD. An x86-64 C program emits SSE
        // by default (xmm0 zeroing, 16-byte moves in memcpy), so a libc-based
        // program dies on its first vector instruction before reaching `main`.
        // `OSXMMEXCPT` (bit 10) makes #XM unmasked rather than a double fault.
        //
        // `CR0.EM` (emulation) must be clear for any of this to work, and
        // `CR0.MP` (monitor coprocessor) is set alongside it as the manual
        // requires. The kernel itself is compiled for x86-64 and has always used
        // SSE, so the FPU was evidently already enabled in a way that left EM set
        // or OSFXSR clear for ring 3 specifically.
        let cr0: u64;
        core::arch::asm!("mov {}, cr0", out(reg) cr0);
        // Clear EM (bit 2), set MP (bit 1).
        let cr0 = (cr0 & !(1u64 << 2)) | (1u64 << 1);
        core::arch::asm!("mov cr0, {}", in(reg) cr0);

        let cr4: u64;
        core::arch::asm!("mov {}, cr4", out(reg) cr4);
        // OSFXSR | OSXMMEXCPT. Note that FSGSBASE is *not* a CR4 bit -- it lives
        // in IA32_CR4_FSGSBASE (MSR 0x10A) -- and bit 16 is reserved. Writing it
        // here hangs QEMU, which is how that mistake was found.
        let cr4 = cr4 | (1 << 9) | (1 << 10);
        core::arch::asm!("mov cr4, {}", in(reg) cr4);
    }
    LOADED.set(()).ok().unwrap_or_default();
}

/// Physical/linear address of the live TSS (for diagnostics).
pub fn tss_addr() -> &'static TaskStateSegment {
    // SAFETY: single-threaded bring-up; TSS is never mutated after init.
    unsafe { &*(&raw const TSS) }
}

/// Current IST-0 stack top recorded in the TSS.
pub fn current_ist0() -> usize {
    tss_addr().ist[0]
}

/// Set the kernel stack pointer (RSP0) the CPU uses on any ring-0 entry.
///
/// The scheduler updates this before every context switch so a user thread's
/// syscall/interrupt lands on *its own* kernel stack.
pub fn set_rsp0(rsp0: usize) {
    // SAFETY: the TSS is owned by this module and the write is a plain
    // ring-0 field update; the scheduler calls this once per switch.
    unsafe {
        (*(&raw mut TSS)).rsp[0] = rsp0;
    }
}

/// Install the I/O permission bitmap of the process about to be switched in.
///
/// Called by the scheduler for every user process; the bitmap is the kernel's
/// grant of ring-3 access to specific I/O ports (see [`PORT_ALLOW`]).
///
/// [`PORT_ALLOW`]: crate::abi::nr::PORT_ALLOW
pub fn set_io_bitmap(allowed: &[u8; 128]) {
    // SAFETY: single CPU; the scheduler calls this between switching away and
    // resuming a user process, when the TSS is not in use by hardware.
    unsafe {
        (*(&raw mut TSS)).set_io_bitmap(allowed);
    }
}
