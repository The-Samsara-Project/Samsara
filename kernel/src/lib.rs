// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! # Samsara
//!
//! A blazingly fast, fully independent 64-bit micro-kernel written in Rust.
//!
//! Samsara is *not* Linux-compatible and does not follow any external design
//! specification. It implements its own boot protocol surface (Multiboot2 via
//! GRUB), its own memory architecture, and its own system call ABI.
//!
//! Current scope:
//! * GRUB / Multiboot2 boot into 64-bit long mode (higher half)
//! * Core kernel infrastructure: console, logging, locking, panic handling
//! * Physical frame allocator, virtual memory manager and kernel heap
//! * CPU structure: GDT/TSS, IDT with exception handling, PIC + PIT timer
//! * The stable in-kernel system call ABI (`crate::abi`) and dispatch layer
//! * Felt like commenting alot today :D

#![no_std]
#![feature(abi_x86_interrupt)]
#![deny(missing_docs)]

extern crate alloc;

use alloc::boxed::Box;
use alloc::sync::Arc;
use driver_common::{BlockDevice, CharDevice, Vnode, VnodeRef};

pub mod abi;
pub mod console;
pub mod cred;
pub mod devices;
pub mod drivers;
pub mod elf;
pub mod entropy;
pub mod framebuffer;
pub mod interrupts;
pub mod ipc;
pub mod io;
pub mod log;
pub mod memory;
pub mod multiboot2;
pub mod pipe;
pub mod rtc;
pub mod sig;
pub mod sync;
pub mod task;
pub mod time;
pub mod user;
pub mod vfs;

use core::arch::asm;
use core::panic::PanicInfo;

/// Multiboot2 bootloader magic value found in `eax` at entry.
pub const MB2_BOOTLOADER_MAGIC: u32 = 0x36D76289;

/// Kernel entry point, called from the assembly stub running in the higher
/// half of the 64-bit address space.
///
/// * `magic`   - Multiboot2 magic (`MB2_BOOTLOADER_MAGIC`), passed in `edi`.
/// * `mbi_phys` - physical address of the Multiboot2 information structure.
#[no_mangle]
pub extern "C" fn kmain(magic: u32, mbi_phys: u64) -> ! {
    io::uart::init();
    log::init(log::Level::Debug);

    log::kinfo!("Samsara/Nutcracker booting");
    log::kinfo!("kernel: Samsara, userspace: Nutcracker; (C) 2026 Harsh Nikarsa - GPLv3+");

    if magic != MB2_BOOTLOADER_MAGIC {
        panic!(
            "invalid bootloader magic {:#x}, expected {:#x}",
            magic, MB2_BOOTLOADER_MAGIC
        );
    }
    log::kdebug!("multiboot2 magic verified ({:#x})", magic);

    let boot_info = multiboot2::parse(mbi_phys);
    if let Some(name) = boot_info.bootloader_name {
        log::kinfo!("booted by: {}", name);
    }
    if let Some(cmdline) = boot_info.command_line {
        log::kinfo!("cmdline: \"{}\"", cmdline);
    }

    // ---- memory management -------------------------------------------------
    memory::init(&boot_info);

    // ---- display -----------------------------------------------------------
    framebuffer::init(boot_info.framebuffer.as_ref());
    console::init();
    log_console();

    interrupts::init();

    // ---- PCI subsystem ------------------------------------------------------
    io::pci::init();

    // ---- framebuffer fallback ----------------------------------------------
    // GRUB under QEMU usually leaves the VGA in text mode (its VBE probe finds
    // no modes), so the boot tag is unusable: switch the card to a linear 32bpp
    // mode ourselves and bring the console up on it.
    io::bochs::init();
    if framebuffer::active() && !console::active() {
        console::init();
        log_console();
    }

    // ---- system call ABI ---------------------------------------------------
    abi::register_defaults();

    // ---- virtual filesystem ------------------------------------------------
    vfs::init(vfs::ramfs::new_root());
    rtc::diag_dump();
    entropy::diag();
    let _ = vfs::mount("/dev", vfs::devfs::new_root());
    // `/dev/fb0`, for programs that want the display as a device rather than
    // through the console. Registered here rather than where the framebuffer
    // comes up, because two things have to be true first: devfs must exist for
    // the name to resolve, and the framebuffer must already be live -- which is
    // not true until the linear-mode fallback above has run.
    if framebuffer::active() {
        let _ = vfs::devfs::register("fb0", alloc::sync::Arc::new(vfs::devfs::FbDevice));
    }
    let _ = vfs::mount("/proc", vfs::procfs::new_root());
    vfs::procfs::register_defaults();

    // /dev/console backs the standard descriptors of kernel-started processes.
    // It is registered after the devfs mount so a spawn can find it.
    let _ = vfs::devfs::register(
        "console",
        alloc::sync::Arc::new(vfs::devfs::ConsoleDevice) as alloc::sync::Arc<dyn driver_common::CharDevice>,
    );

    // Seed a small ramfs tree so the namespace is not empty.
    let _ = vfs::create("/etc", vfs::NodeKind::Dir);
    if let Ok(motd) = vfs::create("/etc/motd", vfs::NodeKind::File) {
        let _ = motd.write_at(
            0,
            b"Welcome to Samsara/Nutcracker.\nCopyright (C) 2026 Harsh Nikarsa. GPLv3+\n",
        );
    }
    // The active keyboard layout, which `inputd` reads at startup and the
    // installer rewrites when the user picks a different one.
    //
    // Shipped with a real value rather than left absent. Both readers already
    // fall back to the default layout when the file is missing, so an absent
    // file works -- but it makes "no configuration" indistinguishable from
    // "configuration that happens to be the default", and it means the
    // installer's read-modify-write cycle starts from an undefined state. A
    // system that has a settable keymap should have somewhere to keep it.
    if let Ok(km) = vfs::create("/etc/keymap.conf", vfs::NodeKind::File) {
        let _ = km.write_at(0, b"us\n");
    }
    // World-writable scratch directory (sticky, so children only create).
    let _ = vfs::create_as("/tmp", vfs::NodeKind::Dir, 0, 0, 0o1777);

    // ---- block devices (NVMe, AHCI/SATA) ------------------------------------
    drivers::nvme::init();
    drivers::storage::init();

    // ---- character devices (PTY) --------------------------------------------
    drivers::pty::init();

    // ---- persistent filesystem (ext2) ---------------------------------------
    if let Err(e) = drivers::ext2::try_mount_first() {
        log::kdebug!("ext2: no mountable filesystem found: {:?}", e);
    }

    // ---- input drivers ------------------------------------------------------
    devices::init_input();

    // Exercise the heap to prove the full stack works end-to-end.
    let mut probe = alloc::vec::Vec::<u64>::new();
    for i in 0..64u64 {
        probe.push(i * i);
    }
    log::kdebug!(
        "heap self-test: {} entries, checksum {:#x}",
        probe.len(),
        probe.iter().fold(0u64, |a, b| a ^ b)
    );
    drop(probe);

    log::kinfo!("Samsara initialized successfully");

    // ---- scheduler takes over ----------------------------------------------
    task::sched::spawn_idle();
    crate::user::start_first_user();
    log::kinfo!("scheduler online; handing over CPU");
    task::sched::enter_scheduler();
}

/// Log the active framebuffer console's grid size, if one is up.
fn log_console() {
    if let Some(fb) = framebuffer::get() {
        log::kinfo!(
            "console: framebuffer text mode active ({} cols x {} rows)",
            fb.width() / console::FONT_W,
            fb.height() / console::FONT_H
        );
    }
}

static REPORT_LAST: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Kernel panic handler: reports the panic on every console then halts.
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    log::kemerg!("KERNEL PANIC: {}", info);
    crate::console::freeze_output();
    loop {
        unsafe { asm!("cli; hlt", options(nomem, nostack)) };
    }
}

/// DMA memory allocator - implemented for driver-common
#[no_mangle]
pub extern "Rust" fn alloc_dma_pages(pages: usize) -> Option<u64> {
    crate::memory::alloc_dma_pages(pages)
}

/// TTY signal bridge used by the PTY line discipline (`drivers/pty/ldisc.rs`):
/// deliver `sig` to every member of the foreground process group `pgid`.
///
/// A terminal signals the *group*, not a single process: pressing ^C while a
/// pipeline runs must interrupt the whole job, not just whichever process
/// happens to own the terminal. A `pgid` of zero means no group was ever
/// claimed with `TIOCSPGRP`, so there is nothing to deliver to.
#[no_mangle]
pub extern "Rust" fn tty_signal(pgid: u32, sig: u32) {
    if pgid == 0 {
        return;
    }
    for task in crate::task::sched::tasks_in_group(pgid) {
        let _ = crate::sig::kill(task as u64, sig);
    }
}

/// TTY job-control bridge: may the calling task hand a terminal's foreground
/// role to process group `pgid`?
///
/// POSIX requires the caller to be in the terminal's session and the group to
/// be in that same session. Both halves matter: without the first, a process
/// outside the session could seize the terminal; without the second, a shell
/// could point its terminal at a group it does not own and lose track of its
/// own jobs.
#[no_mangle]
pub extern "Rust" fn tty_check_pgrp(pgid: u64, tty_sid: u64) -> bool {
    // A terminal may only be manipulated by a member of *its own* session, and
    // only to hand the foreground to a group in that same session.
    //
    // The previous version compared the target group against the *caller's*
    // session and never looked at the terminal's, so a process in session A
    // could set the foreground of a terminal belonging to session B to a group
    // in A. That hands control of B's terminal to A: B's shell loses the
    // terminal it is supposed to own, and signals meant for B's foreground job
    // go to A's. The driver knows the terminal's session and simply was not
    // being told, so the check could not be made correctly from where it was.
    if tty_sid == 0 {
        // The terminal has no session, so there is nothing to be a member of.
        return false;
    }
    if crate::task::sched::current_sid() != tty_sid as u32 {
        return false;
    }
    // The target must be a live process group in the terminal's session. An
    // empty group means the id names nothing, which is the case that matters:
    // a process that is not a group leader has a pid that is not a valid pgid,
    // and setting the foreground to it would record a group that can never be
    // waited on or signalled.
    crate::task::sched::tasks_in_group(pgid as u32)
        .first()
        .map(|t| crate::task::sched::task_groups(*t).sid == tty_sid as u32)
        .unwrap_or(false)
}

/// TTY bridge: the calling task's id, or 0 when there is no current user task.
///
/// A boot-time context has no task to register as a waiter, so a device that
/// asks during early init must be told "nobody" rather than given a bogus id.
#[no_mangle]
pub extern "Rust" fn tty_current_task() -> u32 {
    match crate::task::sched::current_task_id() {
        Some(id) => id.0 as u32,
        None => 0,
    }
}

/// TTY bridge: make a parked task runnable again.
#[no_mangle]
pub extern "Rust" fn tty_wake(task: u32) {
    if task != 0 {
        crate::task::sched::wake(crate::task::TaskId(task as usize));
    }
}

/// Free DMA pages previously allocated with [`alloc_dma_pages`].
#[no_mangle]
pub extern "Rust" fn free_dma_pages(phys: u64, pages: usize) {
    crate::memory::free_dma_pages(phys, pages)
}

/// Translate a physical address to its kernel virtual alias.
#[no_mangle]
pub extern "Rust" fn phys_to_virt(phys: u64) -> u64 {
    crate::memory::phys_to_virt(phys as usize) as u64
}

/// Translate a physical-map kernel virtual address back to physical.
#[no_mangle]
pub extern "Rust" fn virt_to_phys(virt: u64) -> u64 {
    crate::memory::virt_to_phys(virt as usize) as u64
}

/// PCI config space access
#[no_mangle]
pub extern "Rust" fn pci_read_config32(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    crate::io::pci::pci_read_config32(bus, device, function, offset)
}

/// Write a 32-bit value to PCI config space for a given PCI address.
#[no_mangle]
pub extern "Rust" fn pci_write_config32(bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    crate::io::pci::pci_write_config32(bus, device, function, offset, value)
}

/// Enumerate the PCI bus for devices of a given class, and return their info.
#[no_mangle]
pub extern "Rust" fn pci_find_class(class_code: u16) -> alloc::vec::Vec<driver_common::PciDeviceInfo> {
    let bodies = crate::io::pci::pci_find_class(class_code);
    // Decisive kernel-side logging (guaranteed serial): this is the exact bridge
    // the AHCI storage driver uses to reach PCI.
    crate::log::kinfo!(
        "kern pci_find_class({:#06x}) -> {} device(s)",
        class_code,
        bodies.len()
    );
    for d in &bodies {
        crate::log::kdebug!(
            "  {:02x}:{:02x}.{:x} [{:04x}:{:04x}] class={:04x} subclass={:02x} prog_if={:02x} rev={:02x}",
            d.addr.0, d.addr.1, d.addr.2,
            d.vendor_id, d.device_id,
            d.class_code, d.subclass, d.prog_if, d.revision
        );
        for (i, b) in d.bar.iter().enumerate() {
            crate::log::kdebug!("    bar[{}] = {:#018x}", i, b);
        }
        for (i, b) in d.bar_size.iter().enumerate() {
            crate::log::kdebug!("    bar_size[{}] = {:#x}", i, b);
        }
    }
    bodies
}

/// Log a debug-level message from a driver.
#[no_mangle]
pub extern "Rust" fn log_debug(msg: &str) {
    crate::log::kdebug!("{}", msg);
}

/// Log an info-level message from a driver.
#[no_mangle]
pub extern "Rust" fn log_info(msg: &str) {
    crate::log::kinfo!("{}", msg);
}

/// Log a warning-level message from a driver.
#[no_mangle]
pub extern "Rust" fn log_warn(msg: &str) {
    crate::log::kwarn!("{}", msg);
}

/// Log an error-level message from a driver.
#[no_mangle]
pub extern "Rust" fn log_error(msg: &str) {
    crate::log::kerror!("{}", msg);
}

/// Log a critical-level message from a driver.
#[no_mangle]
pub extern "Rust" fn log_critical(msg: &str) {
    crate::log::kemerg!("{}", msg);
}

/// VFS functions for drivers
#[no_mangle]
pub extern "Rust" fn vfs_resolve(path: &str) -> Result<VnodeRef, i32> {
    crate::vfs::resolve(path).map_err(|e| e.into())
}

/// Mount a driver-provided filesystem root at `path`.
///
/// `root` must be a `Box::into_raw`'d `Arc<dyn Vnode>` produced by the driver.
#[no_mangle]
pub extern "Rust" fn vfs_mount(path: &str, root: *mut ()) -> Result<(), i32> {
    // SAFETY: `root` must be a `Box<Arc<dyn Vnode>>` produced by the driver side
    // via `Box::into_raw`. The box is consumed here and the `Arc` adopted.
    let boxed = unsafe { Box::from_raw(root as *mut Arc<dyn Vnode>) };
    crate::vfs::mount(path, *boxed).map_err(|e| e.into())
}

/// Create a node at `path`; `kind` is 0=file, 1=dir, 2=char device.
///
/// Returns a raw pointer to the adopted `Arc<dyn Vnode>`, or NULL on error.
#[no_mangle]
pub extern "Rust" fn vfs_create(path: &str, kind: u8) -> Result<*mut (), i32> {
    use crate::vfs::NodeKind;
    let kind = match kind {
        0 => NodeKind::File,
        1 => NodeKind::Dir,
        2 => NodeKind::CharDevice,
        _ => return Err(-1),
    };
    crate::vfs::create(path, kind)
        .map(|v| Arc::into_raw(v) as *mut ())
        .map_err(|e| e.into())
}

/// Register a driver-provided character device into devfs at `name`.
///
/// `dev` must be a `Box::into_raw`'d `Arc<dyn CharDevice>` produced by the
/// driver.
#[no_mangle]
pub extern "Rust" fn vfs_devfs_register(name: &str, dev: *mut ()) -> Result<(), i32> {
    // SAFETY: `dev` must be a `Box<Arc<dyn CharDevice>>` produced by the driver
    // via `Box::into_raw`. The box is consumed and the `Arc` adopted.
    let boxed = unsafe { Box::from_raw(dev as *mut Arc<dyn CharDevice>) };
    crate::vfs::devfs::register(name, *boxed).map_err(|e| e.into())
}

/// Register a driver-provided character device into `/dev/<dir>/<name>`.
///
/// Used for the pty slaves, which Linux keeps in `/dev/pts/` rather than flat in
/// `/dev`. The directory is created on demand.
///
/// `dev` must be a `Box::into_raw`'d `Arc<dyn CharDevice>` produced by the
/// driver.
#[no_mangle]
pub extern "Rust" fn vfs_devfs_register_in_dir(
    dir: &str,
    name: &str,
    dev: *mut (),
) -> Result<(), i32> {
    // SAFETY: `dev` must be a `Box<Arc<dyn CharDevice>>` produced by the driver
    // via `Box::into_raw`. The box is consumed and the `Arc` adopted.
    let boxed = unsafe { Box::from_raw(dev as *mut Arc<dyn CharDevice>) };
    crate::vfs::devfs::register_in_dir(
        dir,
        name,
        crate::vfs::devfs::anonymous((*boxed).clone()),
    )
    .map_err(|e| e.into())
}

/// Register a driver-provided block device into devfs at `name`.
///
/// `dev` must be a `Box::into_raw`'d `Arc<dyn BlockDevice>` produced by the
/// driver.
#[no_mangle]
pub extern "Rust" fn vfs_devfs_register_block(name: &str, dev: *mut ()) -> Result<(), i32> {
    // SAFETY: `dev` must be a `Box<Arc<dyn BlockDevice>>` produced by the driver
    // via `Box::into_raw`. The box is consumed and the `Arc` adopted.
    let boxed = unsafe { Box::from_raw(dev as *mut Arc<dyn BlockDevice>) };
    crate::vfs::devfs::register_block(name, *boxed).map_err(|e| e.into())
}

/// Time functions for drivers
#[no_mangle]
pub extern "Rust" fn time_ticks() -> u64 {
    crate::time::ticks()
}

/// Return the timer's configured frequency (ticks per second).
#[no_mangle]
pub extern "Rust" fn time_timer_hz() -> u64 {
    crate::time::TIMER_HZ
}
