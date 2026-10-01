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
/// Index of the bounded exec target (reached via `exec()` from `forkx`).
pub const PROG_EXECTST: usize = 10;
/// Index of `chello`, the C program linked against the mlibc port. It is the
/// only non-Rust user image, so it is what proves the libc port runs rather
/// than merely links; the installer runs it as a boot self-test.
pub const PROG_CHELLO: usize = 11;
/// Index of `fbterm`, the ported Linux terminal emulator. Present in the table
/// unconditionally so the index is stable whether or not the port has been
/// built; see the table entry for how the image is gated.
pub const PROG_FBTERM: usize = 12;
/// Index of `busybox`, the ported multi-call binary and the system userland.
/// One image; the applet is chosen by `argv[0]`, which is why every name under
/// `/bin` is a symlink to it rather than a copy.
pub const PROG_BUSYBOX: usize = 15;
/// Index of `mkpasswd`, the helper that turns a password into a `crypt(3)` hash.
///
/// A separate program rather than part of the installer because the installer is
/// Rust and the Rust programs here do not link mlibc, so it cannot call `crypt(3)`
/// itself. Hashing stays in user space, in a program that can be checked against
/// a reference implementation, rather than in the kernel.
pub const PROG_MKPASSWD: usize = 13;
/// Index of `getty`, the program that fronts a terminal and runs a login.
pub const PROG_GETTY: usize = 14;

/// A boot-time user-space program.
pub(crate) struct Program {
    name: &'static str,
    /// Endpoint pinned for boot servers (spawned dynamically for the rest).
    endpoint: Option<crate::ipc::EndpointId>,
    /// Spawn with the root identity instead of the default user.
    root: bool,
    /// Embedded static-PIE ELF image.
    pub(crate) image: &'static [u8],
}

pub(crate) const PROGRAMS: [Program; 16] = [
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
        name: "exectst",
        endpoint: None,
        root: false,
        image: include_bytes!("../../target/user-exectst.elf"),
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
    // busybox, the ported multi-call binary from ports/busybox. This is the
    // system userland: `/bin/ls`, `/bin/cat` and the rest are all this one image,
    // reached by name through symlinks, and busybox selects the applet from
    // argv[0]. A copy per applet would be 106 copies of a 1 MB binary and a
    // package manager to keep them in step.
    //
    // Like fbterm it is not a cargo target, so it is built by its port's own
    // script and copied into place by the Makefile. The image is embedded
    // unconditionally, which is what makes `make` fail loudly rather than boot a
    // kernel whose userland is missing.
    Program {
        name: "mkpasswd",
        endpoint: None,
        // Run as root: it is fed a password and asked for a hash, and there is
        // no reason for it to hold the session user's identity while it does.
        root: true,
        image: include_bytes!("../../target/user-mkpasswd.elf"),
    },
    Program {
        name: "getty",
        endpoint: None,
        // Root, because a getty has to be able to become any user it admits:
        // it reads the passwd database, authenticates, and drops privileges with
        // setuid. A getty that started unprivileged could never log anyone in.
        root: true,
        image: include_bytes!("../../target/user-getty.elf"),
    },
    Program {
        name: "busybox",
        endpoint: None,
        root: false,
        image: include_bytes!("../../target/user-busybox.elf"),
    },
];

/// Check that every `PROG_*` constant is the index of the entry it names.
///
/// These constants are plain integers indexing a plain array, and nothing in the
/// type system connects an index to the entry it refers to. Two programs were
/// once added to the table ahead of `busybox` rather than after it, and every
/// index from there on silently pointed one or two entries off. The symptoms
/// were two unrelated-looking failures with nothing in common: `/bin/getty` was
/// seeded with the busybox image, so a terminal reported "applet not found", and
/// spawning `mkpasswd` ran the getty image instead, which read the keyboard
/// where the password should have been and so reported that no password could be
/// hashed at all. Neither pointed at an index.
///
/// This is the check that turns that class of mistake into a compile error.
const _: () = {
    // Compared as bytes rather than as `&str`: matching on a string is not
    // const-stable, and a const `PartialEq` on `str` is not either. The
    // table's names are literals, so this is comparing constant data to
    // constant data and the compiler folds all of it away.
    macro_rules! same {
        ($name:expr, $s:literal) => {{
            let a = $name.as_bytes();
            let b = $s.as_bytes();
            a.len() == b.len() && {
                let mut k = 0;
                let mut eq = true;
                while k < a.len() {
                    if a[k] != b[k] {
                        eq = false;
                    }
                    k += 1;
                }
                eq
            }
        }};
    }
    let mut i = 0;
    while i < PROGRAMS.len() {
        let n = PROGRAMS[i].name;
        let expected = if same!(n, "hello") { PROG_HELLO }
        else if same!(n, "consoled") { PROG_CONSOLED }
        else if same!(n, "inputd") { PROG_INPUTD }
        else if same!(n, "forkx") { PROG_FORKX }
        else if same!(n, "pipetest") { PROG_PIPETEST }
        else if same!(n, "credtst") { PROG_CREDTST }
        else if same!(n, "signaltst") { PROG_SIGNALTST }
        else if same!(n, "termiostst") { PROG_TERMIOS_TST }
        else if same!(n, "polltest") { PROG_POLLTEST }
        else if same!(n, "sh") { PROG_SHELL }
        else if same!(n, "exectst") { PROG_EXECTST }
        else if same!(n, "chello") { PROG_CHELLO }
        else if same!(n, "fbterm") { PROG_FBTERM }
        else if same!(n, "mkpasswd") { PROG_MKPASSWD }
        else if same!(n, "getty") { PROG_GETTY }
        else if same!(n, "busybox") { PROG_BUSYBOX }
        else { i };
        assert!(
            i == expected,
            "a PROG_* constant does not match its entry in PROGRAMS"
        );
        i += 1;
    }
};

/// Default search path for spawned programs. The userland and the shell both
/// live in `/bin`; `/usr/bin` is included so a later port has somewhere
/// obvious to install.
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
        String::from("PWD=") + cwd,
        String::from("TERM=xterm-256color"),
        String::from("LANG=C"),
        String::from("SHLVL=1"),
        // HOME, USER, LOGNAME and SHELL are deliberately absent.
        //
        // They were here, and set to root's values, which is three separate
        // problems. They are per-user facts: the kernel does not know who is
        // logging in, so it was asserting an identity for every process it
        // started. And `SHELL=/bin/sh` in particular was *load-bearing* in the
        // wrong direction -- a terminal emulator consults `$SHELL` before the
        // passwd database when it picks a login shell, so this one line is what
        // made every session a bare shell no matter what the passwd database
        // said, and the login prompt never appeared.
        //
        // Whoever knows the user sets them: the getty from the passwd entry it
        // authenticated, and fbterm likewise. A program that starts before
        // either of those has no user yet, and inventing one for it is how a
        // program ends up writing to root's home directory.
    ]
}

/// The raw image bytes of embedded program `index`.
///
/// Exists for the one caller that needs the program *as a file* rather than as
/// something to run: the boot-time userland seeding, which writes `/bin/busybox`
/// into the filesystem. A spawned program gets its image from the ELF loader and
/// never reads this.
///
/// The alternative -- spawning busybox and having it ask the kernel for its own
/// image -- would need a syscall to read a program's bytes, which is a way for a
/// process to reach kernel memory layout. The kernel already has the image; a
/// process has no business asking for it.
/// The name embedded program `index` is registered under.
///
/// Used as a substitute `argv[0]` for a program spawned without arguments, since
/// a process cannot have `argc == 0` and every C runtime expects at least
/// `argv[0]`.
pub fn program_name(index: usize) -> Option<&'static str> {
    PROGRAMS.get(index).map(|p| p.name)
}

/// The raw image bytes of embedded program `index`.
///
/// Exists for the one caller that needs the program *as a file* rather than as
/// something to run: the boot-time userland seeding, which writes `/bin/busybox`
/// into the filesystem. A spawned program gets its image from the ELF loader and
/// never reads this.
///
/// The alternative -- spawning busybox and having it ask the kernel for its own
/// image -- would need a syscall to read a program's bytes, which is a way for a
/// process to reach kernel memory layout. The kernel already has the image; a
/// process has no business asking for it.
pub fn image(index: usize) -> Option<&'static [u8]> {
    PROGRAMS.get(index).map(|p| &p.image[..])
}

/// Build a fresh address space for embedded program `index`: the loader maps
/// and links the ELF, then maps a stack and a thread block. Shared with `exec`,
/// which swaps a process onto this image in place.
pub(crate) fn build_image(
    index: usize,
    args: &[String],
    env: &[String],
) -> Result<crate::elf::Loaded, i64> {
    let p = PROGRAMS
        .get(index)
        .ok_or(crate::abi::errno::ENOENT as i64)?;
    let stack_base = USER_STACK_END - USER_STACK_SIZE;
    let loaded = crate::elf::load(p.image, stack_base, USER_STACK_END, args, env, p.name)?;
    crate::log::kdebug!(
        "user: staged \"{}\" ({} bytes) at {:#x}, cr3={:#x}, fs={:#x}",
        p.name,
        p.image.len(),
        loaded.entry,
        loaded.cr3,
        loaded.fs_base
    );
    Ok(loaded)
}

/// Build a fresh address space for an ELF image held in memory.
///
/// The counterpart to [`build_image`], for a program that came out of the
/// filesystem rather than out of this table. The split is deliberate: the boot
/// path names programs by index and must not be able to reach the filesystem,
/// while `execve` names one by path and must not be able to reach the table --
/// a program that ran "whatever the kernel happens to carry" would make the
/// filesystem a suggestion.
///
/// `name` is only for the log line. A path is already in the log from the
/// syscall that got here.
pub(crate) fn build_image_bytes(
    image: &[u8],
    name: &str,
    args: &[String],
    env: &[String],
) -> Result<crate::elf::Loaded, i64> {
    let stack_base = USER_STACK_END - USER_STACK_SIZE;
    let loaded = crate::elf::load(image, stack_base, USER_STACK_END, args, env, name)?;
    crate::log::kdebug!(
        "user: staged \"{}\" ({} bytes) at {:#x}, cr3={:#x}, fs={:#x}",
        name,
        image.len(),
        loaded.entry,
        loaded.cr3,
        loaded.fs_base
    );
    Ok(loaded)
}

/// Spawn a process from the embedded program table. Returns its task id.
pub fn spawn_program(index: usize) -> Result<crate::task::TaskId, i32> {
    spawn_program_args(index, alloc::vec::Vec::new(), None)
}

/// Spawn a program whose three standard descriptors are a terminal rather than
/// the console. For the login prompt, which has to be on the display a person is
/// looking at.
pub fn spawn_program_on_terminal(
    index: usize,
    tty: crate::vfs::VnodeRef,
) -> Result<crate::task::TaskId, i32> {
    spawn_program_args(index, alloc::vec::Vec::new(), Some(tty))
}

/// Spawn a process from the embedded program table with an argument vector.
/// The newborn inherits the caller's working directory (or `/`, when spawned
/// from kernel boot where there is no current user task) and the caller's
/// environment; a boot-spawned process gets [`default_env`].
pub fn spawn_program_args(
    index: usize,
    args: Vec<String>,
    stdio: Option<crate::vfs::VnodeRef>,
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
    // A process cannot have argc == 0; see `sched::with_program_name`. Substituted
    // here as well as in `exec_current` because this is the path the boot servers
    // and the self-tests take, and they are C-free only by accident of what they
    // happen to use.
    let args = if args.is_empty() {
        match program_name(index) {
            Some(n) => vec![String::from(n)],
            None => args,
        }
    } else {
        args
    };
    let loaded = build_image(index, &args, &env).map_err(|e| e as i32)?;
    let p = &PROGRAMS[index];
    // Whether there is a parent decides the descriptor table, and it has to be
    // read *before* the spawn: `spawn_user_ep` copies the caller's table when
    // there is one, and the question of what to do about descriptors 0-2 is
    // answered differently in the two cases.
    let parent = crate::task::sched::current_task_id();
    let tid = crate::task::sched::spawn_user_ep(
        p.name,
        loaded.cr3,
        loaded.entry,
        loaded.rsp,
        Some(loaded.fs_base),
        p.endpoint,
        args,
        env,
        cwd,
    );
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
        //
        // `stdio` is what it should be. It is set when the program belongs on
        // the terminal rather than the console -- the login prompt, which has to
        // read keystrokes and write its prompt where the terminal draws them.
        // Changing the descriptors afterwards does not work: the task has
        // already been created and may already have run, and displacing
        // `/dev/console` takes away the very thing every diagnostic is printed
        // through, so the failure is one that cannot be reported.
        None => {
            let node = match stdio {
                Some(n) => n,
                None => match crate::vfs::devfs::console_node() {
                    Ok(n) => n,
                    Err(_) => return Err(-9),
                },
            };
            install_standard_fds(tid.0, node);
        }
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
fn install_standard_fds(task: usize, node: crate::vfs::VnodeRef) {
    for _ in 0..3 {
        crate::vfs::fdtab::install(task, node.clone());
    }
}

/// Spawn the boot-time user environment: the console server, the keyboard
/// driver, then a login prompt and the terminal that draws it.
///
/// There is no installer between the keyboard and the login prompt. It was a
/// wizard for setting a machine up, and on a system that boots into ramfs there
/// is nothing to set up: no disk was found, so there is no filesystem to write,
/// no account to create, and no configuration to collect. The only thing a
/// person at the keyboard wants is to be let in.
///
/// The order matters and is not interchangeable. `consoled` owns the master of
/// pair 0 and copies keystrokes into it, so it has to be running before
/// anything reads that end. `inputd` is what feeds it, and has to be running
/// before anything types. The getty is started before the terminal because it
/// must be privileged -- it becomes whoever logs in, and nothing unprivileged
/// can start one -- while the terminal is an unprivileged drawing program that
/// has no business holding that privilege even briefly.
pub fn start_first_user() {
    let _ = spawn_program(PROG_CONSOLED);
    let _ = spawn_program(PROG_INPUTD);

    // The console pty's two ends. `consoled` writes keystrokes into the master
    // and the terminal reads the master's output; the getty reads and writes the
    // slave.
    //
    // Both nodes are the ones already published as `/dev/pts/0` and
    // `/dev/ptmx0`. Resolving the registered names rather than building fresh
    // wrappers is what makes this the same pair the console server is using:
    // two nodes for one pair would be two sets of buffers, and a login prompt on
    // one of them would show nothing on the other.
    let slave = match crate::vfs::resolve("/dev/pts/0") {
        Ok(n) => n,
        Err(e) => {
            crate::log::kwarn!("user: no console pty ({:?}); there will be no login", e);
            return;
        }
    };
    let master = match crate::vfs::resolve("/dev/ptmx0") {
        Ok(n) => n,
        Err(e) => {
            crate::log::kwarn!("user: no console pty master ({:?}); no terminal", e);
            return;
        }
    };

    // Stop the kernel console painting over the display.
    //
    // The installer used to do this as it handed the framebuffer over. Without
    // it the text console keeps drawing the boot log over the whole screen for
    // as long as the kernel is up, and a terminal emulator painting the same
    // pixels has it erased underneath itself a line at a time. What a person
    // sees is the boot log, which reads exactly like a terminal that never
    // started.
    //
    // It happens after the console and keyboard drivers are up, because both
    // report through the console, and their startup messages are the last thing
    // worth seeing there.
    crate::console::detach();

    // The login prompt, started before the terminal and privileged.
    //
    // It has to be privileged: it becomes whoever logs in, and nothing
    // unprivileged can start one. That is also why the kernel starts it rather
    // than the terminal, which is an unprivileged drawing program.
    let _ = spawn_program_on_terminal(PROG_GETTY, slave);

    // The terminal, drawing the master.
    //
    // `argv[0]` is the program's own name and the option parser starts after it,
    // so a vector of just the option would make that string the program name and
    // leave the parser with no arguments at all -- and the terminal then quietly
    // falls back to making its own pty, which is the arrangement that cannot
    // work.
    let args = alloc::vec![
        alloc::string::String::from("fbterm"),
        alloc::string::String::from("--attach-fd=0"),
    ];
    let _ = spawn_program_args(PROG_FBTERM, args, Some(master));
}
