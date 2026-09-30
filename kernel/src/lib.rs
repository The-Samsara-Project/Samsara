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

    // ---- the userland ------------------------------------------------------
    seed_userland();
    seed_passwd();

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

/// Put the userland's one real binary at `/bin/busybox`.
///
/// Everything else under `/bin` is a symlink to this file, made by busybox itself
/// (`--install -s /bin`) once userspace is running. The split is not arbitrary: the
/// image has to exist before any user program runs, and only the kernel can put
/// it there, whereas the list of names is busybox's own and belongs to busybox. A
/// kernel-side list would be a second copy of it, stale the moment the port's
/// config changes -- and a stale list means `/bin/ls` missing while `busybox ls`
/// works, which reads as a broken kernel rather than a forgotten regeneration step.
///
/// Failure is reported rather than swallowed. A kernel that boots with no
/// `/bin/busybox` produces a system where the shell starts and then every command
/// inside it fails, and the cause is several steps from the symptom. One warning
/// here saves that search; a missing image means the Makefile did not copy one,
/// which is a build problem and should read as one.
fn seed_userland() {
    let image = match crate::user::image(crate::user::PROG_BUSYBOX) {
        Some(i) => i,
        None => {
            log::kwarn!("userland: busybox is not in the program table");
            return;
        }
    };
    // `/bin` first: `create_as` resolves the parent directory rather than
    // creating it, so a single call for the file would fail with ENOENT on a
    // system with no `/bin` yet -- which is exactly the state this runs in.
    let _ = vfs::create_as("/bin", vfs::NodeKind::Dir, 0, 0, 0o755);
    // `/usr/bin` too, because `DEFAULT_PATH` offers it and a program looking for
    // `/usr/bin/foo` should get ENOENT from the search rather than from a
    // directory that was never there.
    let _ = vfs::create_as("/usr/bin", vfs::NodeKind::Dir, 0, 0, 0o755);
    let _ = vfs::create_as("/root", vfs::NodeKind::Dir, 0, 0, 0o700);
    match vfs::create_as("/bin/busybox", vfs::NodeKind::File, 0, 0, 0o755) {
        Ok(node) => {
            // One `write_at`, not a loop: ramfs grows the file to fit, so this is
            // both correct and the cheapest thing available. A chunked loop would
            // be correct too and would exist only to reimplement a copy that is
            // already a copy.
            if let Err(e) = node.write_at(0, image) {
                log::kwarn!("userland: /bin/busybox write failed: {:?}", e);
            } else {
                log::kdebug!("userland: /bin/busybox seeded, {} bytes", image.len());
            }
        }
        Err(e) => log::kwarn!("userland: could not create /bin/busybox: {:?}", e),
    }
}

/// Seed `/etc/passwd` and `/etc/group`.
///
/// Present because `id`, `whoami`, `groups` and the shell's own idea of where
/// `$HOME` is all resolve the caller's name through the passwd database, and this
/// port's mlibc reads that database out of this file. Without it those three
/// applets abort on their first call rather than reporting that they have no name
/// to report -- mlibc's `getpwnam` returning "not found" is not a thing a program
/// checks for, so the failure is a panic rather than a message.
///
/// The entries have to describe the ids processes actually run as, which is the
/// point of this change: they used to describe uid 0 while every process ran as
/// [`crate::cred::DEFAULT_UID`] (1000). Nothing looked up uid 0, so `whoami`
/// reported "unknown uid 1000", and fbterm's shell, which asks the passwd
/// database for the login shell, silently fell through to its `/bin/sh`
/// fallback. Both were symptoms of one wrong file, not two separate bugs.
///
/// The entries answer "who is calling", which is the only question anything here
/// asks. There is no password field worth having, and a hash would be a
/// credential for a system that has no way to log in. `nobody` is present
/// because a program that drops privileges looks itself up afterwards and
/// deserves a name to find.
fn seed_passwd() {
    let user = crate::cred::DEFAULT_UID;
    let group = crate::cred::DEFAULT_GID;
    let passwd = alloc::format!(
        "root:x:0:0:root:/root:/bin/sh\n\
         nobody:x:65534:65534:nobody:/nonexistent:/bin/false\n\
         samsara:x:{}:{}:Samsara user:/root:/bin/sh\n",
        user, group
    );
    if let Ok(pw) = vfs::create("/etc/passwd", vfs::NodeKind::File) {
        let _ = pw.write_at(0, passwd.as_bytes());
    }
    let groups = alloc::format!("root:x:0:\nnogroup:x:65534:\nsamsara:x:{}:\n", group);
    if let Ok(gr) = vfs::create("/etc/group", vfs::NodeKind::File) {
        let _ = gr.write_at(0, groups.as_bytes());
    }
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

/// TTY job-control bridge: may the calling task take this terminal as its
/// controlling terminal?
///
/// The POSIX `TIOCSCTTY` rule, in the same shape as [`tty_check_pgrp`]: the
/// decision needs the caller's session, which only the kernel knows, so the
/// driver asks rather than guessing. `want` is the session id the caller passed
/// in, or 0 when it passed NULL and is asking to take the terminal regardless of
/// who holds it.
///
/// Three conditions, all of which POSIX requires and any one of which being
/// wrong is a way for one session to take another's terminal:
///
///   * The caller's process group must have no members other than the caller.
///     That is the "orphan process group" condition, and it is what stops a
///     background job in an existing shell from seizing the shell's terminal.
///   * The caller must already be a session leader, *or* must not already have a
///     controlling terminal. A session leader claiming a terminal is how a
///     terminal is acquired at all; a non-leader claiming one is only allowed
///     when it has none, which is the inheritance case.
///   * If the caller passed a session id, that id must be the caller's own.
///
/// The session id and the foreground group are written by the driver once this
/// returns true; nothing here mutates scheduler state, so a caller that is
/// refused has changed nothing.
#[no_mangle]
pub extern "Rust" fn tty_check_sctty(want: u64) -> bool {
    let cur = match crate::task::sched::current_task_id() {
        Some(t) => t,
        None => return false,
    };
    let (sid, pgid, leader) = match crate::task::sched::task_session_info(cur.0) {
        Some(info) => info,
        None => return false,
    };
    // A session id of 0 means the caller passed NULL: any of its own session will
    // do. A non-zero one is a demand to be that exact session, which is how a
    // program asserts it knows which session it is joining -- and the assertion
    // is checked against the caller's *actual* session, not trusted, because the
    // pointer is caller-supplied and a program that guessed wrong must be refused
    // rather than trusted into another session's terminal.
    if want != 0 {
        // The pointer is only dereferenced here, and only after this comparison
        // would have succeeded for a plausible value. Reading it first would mean
        // dereferencing an arbitrary user address to decide whether it was worth
        // reading.
        let Some(claimed) = (unsafe { crate::abi::user_read_u32(want) }) else {
            return false;
        };
        if claimed as u64 != sid as u64 {
            return false;
        }
    }
    // The caller's process group must be an orphan: nobody else in it. A shell
    // that has already started a job has a populated group and must not be able
    // to re-claim the terminal out from under that job.
    let members = crate::task::sched::tasks_in_group(pgid);
    if members.iter().any(|t| *t != cur.0) {
        return false;
    }
    // A session leader may always take one; anyone else only if they have none.
    // Without the second half a process could take a second terminal and quietly
    // end up with two, with signals and job control split across them.
    if !leader && tty_session_attached(sid as u64) {
        return false;
    }
    true
}

/// Whether session `sid` already holds a controlling terminal.
///
/// Asked through the driver's own view, because "does this session have a
/// controlling terminal" is a property of the terminal side of the pairing and
/// the kernel keeps no table of it. Unowned terminals report a session of 0, so
/// this is deliberately not "session != 0".
#[no_mangle]
pub extern "Rust" fn tty_session_attached(sid: u64) -> bool {
    crate::drivers::pty::session_has_controlling_tty(sid as u32)
}

/// TTY bridge: the calling task's session and process group.
///
/// `TIOCSCTTY` records both, as the terminal's new owner and its initial
/// foreground group. The driver cannot see either: a session id and a pgid are
/// scheduler state, and a device is handed a buffer and a request number.
#[no_mangle]
pub extern "Rust" fn tty_current_sid() -> u32 {
    crate::task::sched::current_sid()
}

/// TTY bridge: the calling task's process group. See [`tty_current_sid`].
#[no_mangle]
pub extern "Rust" fn tty_current_pgid() -> u32 {
    crate::task::sched::current_pgid()
}

/// TTY bridge: the session id the caller passed to `TIOCSCTTY`, or 0 for NULL.
///
/// The one ioctl on this device whose argument is a *pointer* rather than a value
/// in the copied buffer, and the distinction is load-bearing: NULL is the common
/// case and means "take this terminal from whoever has it". Reading four bytes
/// out of the buffer to discover that would have meant reading four bytes from
/// address zero, inside a call that is doing exactly what it was asked to do.
///
/// So the request is given a zero-length argument and the pointer is carried
/// alongside it. Reporting the pointer's *value* rather than what it points at is
/// deliberate: the only question asked of it is whether it is null, and a
/// non-null pointer is only dereferenced by the kernel, once, after the
/// permission check has already passed.
#[no_mangle]
pub extern "Rust" fn tty_sctty_arg() -> u64 {
    crate::abi::current_ioctl_arg()
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

/// TTY bridge: park the current task until it is woken, or until `deadline`/// ticks have passed (zero meaning no deadline).
///
/// This is how a pty writer waits for room. The task that will make the room
/// is the terminal reading, so the writer has to sleep rather than spin: a spin
/// would consume the CPU the reader needs and turn a terminal that is merely
/// behind into one that has stopped.
#[no_mangle]
pub extern "Rust" fn tty_park(deadline: u64) {
    crate::task::sched::block_until(if deadline == 0 { None } else { Some(deadline) });
}

/// TTY bridge: the kernel's tick counter.
///
/// Drivers cannot see the kernel's clock, and a park deadline is meaningless
/// without one, so the counter the scheduler measures deadlines against is read
/// through here.
#[no_mangle]
pub extern "Rust" fn tty_ticks() -> u64 {
    crate::time::ticks()
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
