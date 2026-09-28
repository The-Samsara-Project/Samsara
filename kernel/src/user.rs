// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! User-space process loader.
//!
//! Every ring-3 server is a static-position-independent ELF64 binary produced
//! by the `user/` workspace and embedded here. Each runs in its own address
//! space; [`crate::elf`] maps the `PT_LOAD` segments, acts as the dynamic
//! linker (resolving `.rela.dyn`/`.rela.plt`) and stages the initial stack.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// High end of the user stack region (we map downward from here).
pub const USER_STACK_END: usize = 0x0000_0080_0000_0000;
/// Size of the user stack region.
pub const USER_STACK_SIZE: usize = 1 * 1024 * 1024;

/// Index of the hello-world example process.
pub const PROG_HELLO: usize = 0;
/// Index of the ring-3 console server (endpoint [`crate::ipc::EP_CONSOLED`]).
pub const PROG_CONSOLED: usize = 1;
/// Index of the ring-3 PS/2 keyboard driver (endpoint [`crate::ipc::EP_INPUTD`]).
pub const PROG_INPUTD: usize = 2;
/// Index of the fork/exec/waitpid example process.
pub const PROG_FORKX: usize = 3;
/// Index of the pipe exercise process.
pub const PROG_PIPETEST: usize = 4;
/// Index of the credential/permission exercise process (spawned as root).
pub const PROG_CREDTST: usize = 5;
/// Index of the signal-delivery exercise process.
pub const PROG_SIGNALTST: usize = 6;
/// Index of the PTY termios ABI exercise process.
pub const PROG_TERMIOS_TST: usize = 7;
/// Index of the `poll` multiplexing exercise process.
pub const PROG_POLLTEST: usize = 8;
/// Index of the standalone shell, run directly on the console PTY.
pub const PROG_SHELL: usize = 9;
/// Index of the interactive setup wizard (the boot-time user front end).
pub const PROG_INSTALLER: usize = 10;
/// Index of the bounded exec target (reached via `exec()` from `forkx`).
pub const PROG_EXECTST: usize = 11;
/// Index of the ring-3 utility multi-call binary (busybox-style `samutils`),
/// spawned with an argv by the shell and other users.
pub const PROG_SAMUTILS: usize = 12;
/// Index of `chello`, the C program linked against the mlibc port. It is the
/// only non-Rust user image, so it is what proves the libc port runs rather
/// than merely links; the installer runs it as a boot self-test.
pub const PROG_CHELLO: usize = 13;
/// Index of `fbterm`, the ported Linux terminal emulator. Present in the table
/// unconditionally so the index is stable whether or not the port has been
/// built; see the table entry for how the image is gated.
pub const PROG_FBTERM: usize = 14;

/// A boot-time user-space program.
struct Program {
    name: &'static str,
    /// Endpoint pinned for boot servers (spawned dynamically for the rest).
    endpoint: Option<crate::ipc::EndpointId>,
    /// Spawn with the root identity instead of the default user.
    root: bool,
    /// Embedded static-PIE ELF image.
    image: &'static [u8],
}

const PROGRAMS: [Program; 15] = [
    Program {
        name: "hello",
        endpoint: None,
        root: false,
        image: include_bytes!("../../target/user-hello.elf"),
    },
    Program {
        name: "consoled",
        endpoint: Some(crate::ipc::EP_CONSOLED),
        root: false,
        image: include_bytes!("../../target/user-consoled.elf"),
    },
    Program {
        name: "inputd",
        endpoint: Some(crate::ipc::EP_INPUTD),
        root: false,
        image: include_bytes!("../../target/user-inputd.elf"),
    },
    Program {
        name: "forkx",
        endpoint: None,
        root: false,
        image: include_bytes!("../../target/user-forkx.elf"),
    },
    Program {
        name: "pipetest",
        endpoint: None,
        root: false,
        image: include_bytes!("../../target/user-pipetest.elf"),
    },
    Program {
        name: "credtst",
        endpoint: None,
        root: true,
        image: include_bytes!("../../target/user-credtst.elf"),
    },
    Program {
        name: "signaltst",
        endpoint: None,
        root: false,
        image: include_bytes!("../../target/user-signaltst.elf"),
    },
    Program {
        name: "termiostst",
        endpoint: None,
        root: false,
        image: include_bytes!("../../target/user-termiostst.elf"),
    },
    Program {
        name: "polltest",
        endpoint: None,
        root: false,
        image: include_bytes!("../../target/user-polltest.elf"),
    },
    Program {
        name: "sh",
        endpoint: None,
        root: false,
        image: include_bytes!("../../target/user-sh.elf"),
    },
    Program {
        name: "installer",
        endpoint: None,
        // The setup wizard runs privileged: it must stat/open block devices
        // (mode-0 devfs nodes) and manage /etc without permission surprises.
        root: true,
        image: include_bytes!("../../target/user-installer.elf"),
    },
    Program {
        name: "exectst",
        endpoint: None,
        root: false,
        image: include_bytes!("../../target/user-exectst.elf"),
    },
    Program {
        name: "samutils",
        endpoint: None,
        root: false,
        image: include_bytes!("../../target/user-samutils.elf"),
    },
    // The C smoke test. Not a boot server and not spawned at boot: the
    // installer launches it with the rest of the self-tests, which is where a
    // pass/fail result is actually collected and shown.
    Program {
        name: "chello",
        endpoint: None,
        root: false,
        image: include_bytes!("../../target/user-chello.elf"),
    },
    // The ported Linux terminal emulator, from ports/fbterm. Feature-gated
    // because it is not a cargo target: it is built by ports/fbterm/build.sh
    // against the mlibc sysroot, and making `make` depend on that would force
    // a libc port on anyone building the tree. `make fbterm` builds it and
    // `make fbterm-run` rebuilds the kernel with the feature so the image is
    // actually present.
    //
    // Without the feature the image is empty rather than absent, which keeps
    // this array a fixed size and keeps every other index stable. Spawning an
    // empty image fails in the ELF loader, which is the honest outcome: the
    // program is not there, and nothing pretends otherwise.
    Program {
        name: "fbterm",
        endpoint: None,
        root: false,
        image: include_bytes!("../../target/user-fbterm.elf"),
    },
];

/// Default search path for spawned programs. `samutils` lives in `/bin`, and
/// a shell in `/bin` as well; `/usr/bin` is included so a
/// later port has somewhere obvious to install.
pub const DEFAULT_PATH: &str = "/bin:/usr/bin";

/// The environment every newly spawned process starts with, before any
/// inherited entries are layered on top.
///
/// `TERM` matters more than it looks: a libc-based program uses it to decide
/// whether to emit ANSI escapes, and getting it wrong makes output unreadable
/// in a terminal that could have rendered it. `PATH` is what lets a program
/// find its sibling applets by name instead of by absolute path.
///
/// The kernel has no filesystem-backed `/etc/environment` reader, so this is a
/// fixed base rather than a parsed file. A process may still override entries
/// for itself; nothing here is authoritative.
pub fn default_env(cwd: &str) -> Vec<String> {
    vec![
        String::from("PATH=") + DEFAULT_PATH,
        String::from("HOME=/root"),
        String::from("PWD=") + cwd,
        String::from("SHELL=/bin/sh"),
        String::from("TERM=xterm-256color"),
        String::from("USER=root"),
        String::from("LOGNAME=root"),
        String::from("LANG=C"),
        String::from("SHLVL=1"),
    ]
}

/// Build a fresh address space for embedded program `index`: the loader maps
/// and links the ELF, then maps a stack. Returns `(cr3, entry, rsp)`.
/// Shared with `exec`, which swaps a process onto this image in place.
pub(crate) fn build_image(
    index: usize,
    args: &[String],
    env: &[String],
) -> Result<(usize, usize, usize), i64> {
    let p = PROGRAMS
        .get(index)
        .ok_or(crate::abi::errno::ENOENT as i64)?;
    let stack_base = USER_STACK_END - USER_STACK_SIZE;
    let (cr3, entry, rsp) = crate::elf::load(p.image, stack_base, USER_STACK_END, args, env)?;
    crate::log::kdebug!(
        "user: staged \"{}\" ({} bytes) at {:#x}, cr3={:#x}",
        p.name,
        p.image.len(),
        entry,
        cr3
    );
    Ok((cr3, entry, rsp))
}

/// Spawn a process from the embedded program table. Returns its task id.
pub fn spawn_program(index: usize) -> Result<crate::task::TaskId, i32> {
    spawn_program_args(index, alloc::vec::Vec::new())
}

/// Spawn a process from the embedded program table with an argument vector.
/// The newborn inherits the caller's working directory (or `/`, when spawned
/// from kernel boot where there is no current user task) and the caller's
/// environment; a boot-spawned process gets [`default_env`].
pub fn spawn_program_args(
    index: usize,
    args: Vec<String>,
) -> Result<crate::task::TaskId, i32> {
    // The env has to exist before `build_image`, because it goes onto the
    // newborn's initial stack. A parent's env is inherited verbatim; `PWD` is
    // refreshed to match the inherited cwd so the two never disagree.
    let (cwd, mut env) = match crate::task::sched::current_task_id() {
        Some(_) => (
            crate::task::sched::current_cwd(),
            crate::task::sched::current_env(),
        ),
        None => (alloc::string::String::from("/"), Vec::new()),
    };
    if env.is_empty() {
        env = default_env(&cwd);
    } else if let Some(pwd) = env.iter_mut().find(|e| e.starts_with("PWD=")) {
        *pwd = String::from("PWD=") + &cwd;
    }
    let (cr3, entry, rsp) = build_image(index, &args, &env).map_err(|e| e as i32)?;
    let p = &PROGRAMS[index];
    // Whether there is a parent decides the descriptor table, and it has to be
    // read *before* the spawn: `spawn_user_ep` copies the caller's table when
    // there is one, and the question of what to do about descriptors 0-2 is
    // answered differently in the two cases.
    let parent = crate::task::sched::current_task_id();
    let tid = crate::task::sched::spawn_user_ep(p.name, cr3, entry, rsp, p.endpoint, args, env, cwd);
    match parent {
        // A user-space spawn inherits the caller's descriptor table, including
        // anything the caller did to it.
        //
        // This has to be explicit. `spawn_user_ep` copies the process group,
        // session and endpoint from the parent but deliberately does not touch
        // descriptors, so without this the child starts with an empty table:
        // every read and write fails with EBADF, `isatty(0)` is false, and a
        // program that checks its standard descriptors before doing anything
        // else gives up silently. Installing /dev/console unconditionally, which
        // is what happened before, is the opposite mistake -- it threw away the
        // caller's arrangement, so a caller could not hand a child a terminal,
        // a file, or a deliberately closed descriptor.
        Some(parent) => crate::vfs::fdtab::copy_table(parent.0, tid.0),
        // Kernel boot: no caller to inherit from, so the newborn needs a set of
        // standard descriptors of its own or every write to fd 1 fails with
        // EBADF and the program's output is lost.
        None => install_standard_fds(tid.0),
    }
    // A process spawned from user space (via `PROC_SPAWN`) becomes a child of
    // its spawner so the caller can reap it with `waitpid` — the installer
    // relies on this to supervise the boot tests it launches. During kernel
    // boot there is no current user task, so no parent is recorded.
    if let Some(parent) = parent {
        crate::task::sched::set_parent(tid, parent);
    }
    // Give the newborn process its identity before it can ever run.
    if p.root {
        crate::cred::seed_root(tid.0);
    } else {
        crate::cred::seed(
            tid.0,
            crate::cred::Credentials::user(
                crate::cred::DEFAULT_UID,
                crate::cred::DEFAULT_GID,
            ),
            crate::cred::DEFAULT_UMASK,
        );
    }
    crate::sig::seed(tid.0);
    Ok(tid)
}

/// Give a freshly spawned process its three standard descriptors.
///
/// POSIX gives an inherited process whatever its parent had, but a process the
/// kernel starts has no parent to inherit from, so descriptors 0, 1 and 2 would
/// otherwise be invalid. A libc-based program's very first act is usually to
/// write to stdout, and `write(2)` on a bad descriptor fails with `EBADF` --
/// output is lost with no error anywhere the program can see.
///
/// All three point at `/dev/console`. A fresh descriptor table hands out the
/// lowest free slot, so installing three times yields 0, 1, 2 in order.
///
/// Only called for a process with no parent. A process spawned by another
/// inherits that process's descriptor table wholesale, because a caller that
/// has gone to the trouble of arranging a child's descriptors -- a terminal on
/// stdin, an output file on stdout -- means it. Overwriting them here is what
/// used to make that impossible.
fn install_standard_fds(task: usize) {
    let Ok(node) = crate::vfs::devfs::console_node() else {
        return;
    };
    for _ in 0..3 {
        crate::vfs::fdtab::install(task, node.clone());
    }
}

/// Spawn the boot-time user environment: the console server, the keyboard
/// driver, then the interactive installer. Everything else — the example
/// programs, and the terminal — is launched on demand by the
/// installer, which is the first thing the user actually talks to.
pub fn start_first_user() {
    let _ = spawn_program(PROG_CONSOLED);
    let _ = spawn_program(PROG_INPUTD);
    let _ = spawn_program(PROG_INSTALLER);
}
