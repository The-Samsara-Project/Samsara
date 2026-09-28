// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa
//
// Ring-3 credential exercise booted with the *root* identity. It verifies the
// kernel's identity/credential API while root, then permanently drops to
// uid 1000 and verifies the unprivileged denials and permission gates:
//
//   1. identity: real/effective/saved agree and are root;
//   2. umask: default 0o022, round-trips;
//   3. open(O_CREAT): mode honours umask; O_CREAT|O_EXCL -> EEXIST; writes;
//   4. chmod own file; chown group as root (gid 1000);
//   5. setgroups([117]) + getgroups, then reset;
//   6. setfsuid returns the previous value and ignores denied values;
//   7. a root-owned victim child is forked, then we drop to uid 1000;
//   8. EPERM kill of the root-owned victim, then EOF-unblock + reap(3);
//   9. set*id denials as non-root + harmless no-op reuid;
//  10. non-owner chmod/chown denials, owner-ok chown into own gid;
//  11. read on a write-only fd -> EBADF; O_TRUNC empties a file;
//  12. kill(): ESRCH/EINVAL, SIGCHLD non-fatal, SIGKILL/SIGTERM statuses.

#![no_std]
#![no_main]

use nutcracker_rt::println;
use nutcracker_rt::syscall;
use nutcracker_rt::syscall::signum;

/// Halt the test without a specific message.
fn fail() -> ! {
    println!("[credtst] FAIL!");
    syscall::proc_exit_code(255);
}

macro_rules! check {
    ($cond:expr, $msg:literal) => {
        if !($cond) {
            println!("[credtst] FAIL: {}", $msg);
            syscall::proc_exit_code(255);
        }
    };
}

macro_rules! check_err {
    ($res:expr, $want:expr, $msg:literal) => {
        match $res {
            Err(e) if e == $want => {}
            Err(e) => {
                println!("[credtst] FAIL: {} (got errno {})", $msg, e);
                syscall::proc_exit_code(255);
            }
            Ok(_) => {
                println!("[credtst] FAIL: {} (unexpected Ok)", $msg);
                syscall::proc_exit_code(255);
            }
        }
    };
}

/// `(mode, uid, gid, size)` for `path` via `STAT`.
fn stat_of(path: &str) -> Option<(u32, u32, u32, u64)> {
    let mut st = syscall::Stat {
        mode: 0,
        uid: 0,
        gid: 0,
        kind: 0,
        size: 0,
    };
    if syscall::stat(path, &mut st).is_err() {
        return None;
    }
    Some((st.mode, st.uid, st.gid, st.size))
}

fn test_root_identity() {
    let (ruid, euid, suid) = syscall::getresuid();
    let (rgid, egid, sgid) = syscall::getresgid();
    check!(syscall::getuid() == ruid && syscall::geteuid() == euid, "uid getters disagree");
    check!(syscall::getgid() == rgid && syscall::getegid() == egid, "gid getters disagree");
    check!(ruid == 0 && euid == 0 && suid == 0, "boot uid is not root");
    check!(rgid == 0 && egid == 0 && sgid == 0, "boot gid is not root");
    println!("[credtst] OK identity root ({ruid}/{euid}/{suid})");
}

fn test_umask() {
    check!(syscall::umask(0o027) == 0o022, "umask default is not 0o022");
    check!(syscall::umask(0o022) == 0o027, "umask round-trip");
    println!("[credtst] OK umask round-trip");
}

fn test_create_mode() {
    let _ = syscall::umask(0o027);
    let fd = match syscall::open("/tmp/mm1", syscall::O_WRONLY | syscall::O_CREAT, 0o666) {
        Ok(f) => f,
        Err(_) => fail(),
    };
    let n = syscall::write(fd, b"hello mm").expect("write mm1");
    syscall::close(fd).ok();
    check!(n == 8, "write length");
    let s = stat_of("/tmp/mm1").expect("stat mm1");
    check!(s.0 & 0o777 == 0o640, "O_CREAT mode ignores umask (want 0o640)");
    check!(s.3 == 8, "mm1 size");
    println!("[credtst] OK O_CREAT 0o666 & ~umask(0o027) -> 0o640, wrote 8 bytes");

    check_err!(
        syscall::open("/tmp/mm1", syscall::O_RDWR | syscall::O_CREAT | syscall::O_EXCL, 0o644),
        -17,
        "O_EXCL on existing file"
    );
    println!("[credtst] OK O_EXCL -> EEXIST");
    let _ = syscall::umask(0o022);
}

fn test_chmod_chown_root() {
    check!(syscall::chmod("/tmp/mm1", 0o600).is_ok(), "chmod own file");
    check!(stat_of("/tmp/mm1").unwrap().0 & 0o777 == 0o600, "chmod not applied");
    check!(syscall::chown("/tmp/mm1", u32::MAX, 1000).is_ok(), "root chown gid");
    check!(stat_of("/tmp/mm1").unwrap().2 == 1000, "chown gid not applied");
    println!("[credtst] OK chmod 0o600 + chown gid=1000 (root)");
}

fn test_groups_root() {
    check!(syscall::setgroups(&[117]).is_ok(), "root setgroups");
    let mut out = [0u32; 8];
    let n = syscall::getgroups(&mut out);
    check!(n == 1 && out[0] == 117, "getgroups after setgroups");
    check!(syscall::setgroups(&[]).is_ok(), "root setgroups reset");
    println!("[credtst] OK setgroups([117])/getgroups");
}

fn test_fsuid() {
    // Root may set any filesystem id; the previous value is returned.
    check!(syscall::setfsuid(1000) == 0, "setfsuid(1000) must return old 0");
    check!(syscall::setfsuid(0) == 1000, "setfsuid(0) must return old 1000");
    println!("[credtst] OK setfsuid returns-old as root");
}

/// Fork a root-owned child parked on an empty pipe. Returns `(pid, write_fd)`;
/// closing the write fd from the parent delivers EOF, unblocking the child.
fn fork_root_victim() -> (u64, usize) {
    let (r, w) = syscall::pipe().expect("pipe");
    let pid = syscall::fork();
    check!(pid >= 0, "fork victim");
    if pid == 0 {
        let _ = syscall::close(w);
        let mut b = [0u8; 1];
        let _ = syscall::read(r, &mut b); // EOF when parent closes w
        syscall::proc_exit_code(3);
    }
    let _ = syscall::close(r);
    (pid as u64, w)
}

fn test_after_drop(root_child: u64, wfd: usize) {
    let (ruid, euid, suid) = syscall::getresuid();
    let (rgid, egid, sgid) = syscall::getresgid();
    check!(ruid == 1000 && euid == 1000 && suid == 1000, "drop to uid 1000");
    check!(rgid == 1000 && egid == 1000 && sgid == 1000, "drop to gid 1000");
    println!("[credtst] OK dropped to uid/gid 1000");

    // The root-owned victim may not be signalled anymore.
    check_err!(syscall::kill(root_child, 0), -1, "kill(root-owned, 0)");
    check_err!(syscall::kill(root_child, 9), -1, "kill(root-owned, SIGKILL)");
    println!("[credtst] OK kill of root-owned process -> EPERM");

    // EOF unblocks the victim, which exits 3 and is reaped by us.
    syscall::close(wfd).ok();
    let mut st = -1;
    let _ = syscall::waitpid(root_child, &mut st);
    check!(st == 3, "root-owned victim exit status");
    println!("[credtst] OK root-owned victim reaped with status 3");

    // No route back to root.
    check_err!(syscall::setuid(0), -1, "setuid(0)");
    check_err!(syscall::seteuid(0), -1, "seteuid(0)");
    check_err!(syscall::setreuid(u32::MAX, 0), -1, "setreuid(.., 0)");
    check_err!(syscall::setresuid(u32::MAX, 0, u32::MAX), -1, "setresuid(.., 0, ..)");
    check_err!(syscall::setgid(0), -1, "setgid(0)");
    check_err!(syscall::setegid(0), -1, "setegid(0)");
    check_err!(syscall::setregid(u32::MAX, 0), -1, "setregid(.., 0)");
    check_err!(syscall::setresgid(u32::MAX, 0, u32::MAX), -1, "setresgid(.., 0, ..)");
    check_err!(syscall::setgroups(&[77]), -1, "setgroups as non-root");
    // A no-op reuid within the {real,effective,saved} set is allowed.
    check!(syscall::setreuid(u32::MAX, 1000).is_ok(), "setreuid(-1, 1000)");
    let (ru2, eu2, su2) = syscall::getresuid();
    check!(ru2 == 1000 && eu2 == 1000 && su2 == 1000, "reuid no-op ids");

    // Unprivileged setfsuid: values outside {real,effective,saved} are
    // silently ignored (previous fsuid returned unchanged); we are 1000 now.
    check!(syscall::setfsuid(2000) == 1000, "non-root setfsuid(2000) must be a no-op");
    check!(syscall::setfsuid(1000) == 1000, "non-root setfsuid(euid) allowed");
    check!(syscall::setfsuid(0) == 1000, "non-root setfsuid(0) must be a no-op");
    println!("[credtst] OK set*id denials + allowed no-op reuid + setfsuid no-op");

    // Non-owner chmod/chown denied.
    check_err!(syscall::chmod("/tmp/mm1", 0o644), -1, "chmod of root-owned file");
    check_err!(syscall::chown("/tmp/mm1", u32::MAX, 0), -1, "chown gid of root-owned file");
    println!("[credtst] OK non-owner chmod/chown denied");
}

fn test_user_files() {
    // Root-owned 0o600 file is unreadable by the dropped user.
    check_err!(syscall::open("/tmp/mm1", syscall::O_RDONLY, 0), -13, "open root 0o600 file");
    println!("[credtst] OK EACCES on root-owned 0o600 file");

    // Our own file: full owner rights, bounded chown.
    let fd = syscall::open("/tmp/u1", syscall::O_WRONLY | syscall::O_CREAT, 0o644).expect("open u1");
    let _ = syscall::close(fd);
    let s = stat_of("/tmp/u1").expect("stat u1");
    check!(s.1 == 1000 && s.2 == 1000, "u1 owner is 1000:1000");
    check!(syscall::chmod("/tmp/u1", 0o600).is_ok(), "chmod own file");
    check!(syscall::chown("/tmp/u1", u32::MAX, 1000).is_ok(), "chown own gid");
    check_err!(syscall::chown("/tmp/u1", u32::MAX, 117), -1, "chown foreign group");
    check_err!(syscall::chown("/tmp/u1", 1000, u32::MAX), -1, "chown uid change by non-root");
    println!("[credtst] OK own-file chmod + bounded chown");

    // fd access gating.
    let wfd = syscall::open("/tmp/u1", syscall::O_WRONLY, 0).expect("open w");
    let mut b = [0u8; 4];
    check_err!(syscall::read(wfd, &mut b), -9, "read on write-only fd");
    syscall::close(wfd).ok();
    let rfd = syscall::open("/tmp/u1", syscall::O_RDONLY, 0).expect("open r");
    check_err!(syscall::write(rfd, b"x"), -9, "write on read-only fd");
    syscall::close(rfd).ok();
    println!("[credtst] OK EBADF on wrong access mode");

    // O_TRUNC empties a file.
    let t = syscall::open("/tmp/trunc", syscall::O_WRONLY | syscall::O_CREAT, 0o644).expect("open trunc");
    let _ = syscall::write(t, b"hello trunc");
    syscall::close(t).ok();
    check!(stat_of("/tmp/trunc").unwrap().3 == 11, "trunc pre-size");
    let t2 = syscall::open("/tmp/trunc", syscall::O_WRONLY | syscall::O_TRUNC, 0).expect("open trunc2");
    syscall::close(t2).ok();
    check!(stat_of("/tmp/trunc").unwrap().3 == 0, "O_TRUNC did not empty");
    println!("[credtst] OK O_TRUNC empties file");
}

fn test_kill_allowed() {
    check_err!(syscall::kill(99999, 0), -3, "kill ESRCH");
    check_err!(syscall::kill(syscall::get_epid(), 99), -22, "kill EINVAL");
    println!("[credtst] OK kill ESRCH/EINVAL");

    // Same-uid child: SIGCHLD is non-fatal, SIGTERM/SIGKILL terminate with
    // the conventional 128+sig status.
    let c1 = syscall::fork();
    check!(c1 >= 0, "fork c1");
    if c1 == 0 {
        loop {
            syscall::yield_now();
        }
    }
    check!(syscall::kill(c1 as u64, signum::SIGCHLD).is_ok(), "SIGCHLD unexpectedly fatal");
    let mut st = 0;
    check!(syscall::kill(c1 as u64, signum::SIGTERM).is_ok(), "SIGTERM kill");
    let _ = syscall::waitpid(c1 as u64, &mut st);
    check!(st == 143, "SIGTERM status != 143");
    println!("[credtst] OK SIGCHLD non-fatal + SIGTERM -> status 143");

    let c2 = syscall::fork();
    check!(c2 >= 0, "fork c2");
    if c2 == 0 {
        loop {
            syscall::yield_now();
        }
    }
    check!(syscall::kill(c2 as u64, signum::SIGKILL).is_ok(), "SIGKILL kill");
    let mut st = 0;
    let _ = syscall::waitpid(c2 as u64, &mut st);
    check!(st == 137, "SIGKILL status != 137");
    println!("[credtst] OK SIGKILL -> status 137");
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    println!("[credtst] pid={} (root) starting", syscall::get_epid());

    test_root_identity();
    test_umask();
    test_create_mode();
    test_chmod_chown_root();
    test_groups_root();
    test_fsuid();

    // The victim must be forked while we are still root.
    let (root_child, wfd) = fork_root_victim();

    // Drop both identities to uid/gid 1000 (root setgid sets all group ids).
    check!(syscall::setgid(1000).is_ok(), "drop gid via setgid");
    check!(syscall::setuid(1000).is_ok(), "drop via setuid");
    test_after_drop(root_child, wfd);
    test_user_files();
    test_kill_allowed();

    println!("[credtst] ALL PASS");
    syscall::proc_exit_code(0);
}