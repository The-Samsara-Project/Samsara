// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! Process credentials: real/effective/saved/filesystem uid+gid, supplementary
//! groups, the per-process umask, and the `set*id` state machines.
//!
//! Credentials live in a global registry keyed by task id (mirroring the fd
//! table) rather than inside the scheduler's [`Task`](crate::task::Task), so
//! this subsystem is a purely additive kernel module. Lookups for a task with
//! no registered entry yield the default unprivileged identity.

use crate::sync::Spinlock;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;

/// Sentinel meaning "leave this id unchanged", seen from user space as `-1`.
pub const ID_UNCHANGED: u32 = u32::MAX;

/// Default uid/gid for ordinary processes.
pub const DEFAULT_UID: u32 = 1000;
/// Default gid for ordinary processes.
pub const DEFAULT_GID: u32 = 1000;
/// Default permission mask every process starts with.
pub const DEFAULT_UMASK: u32 = 0o022;

/// A process's complete user/group identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Credentials {
    /// Real user id.
    pub uid: u32,
    /// Real group id.
    pub gid: u32,
    /// Effective user id (drives privilege checks).
    pub euid: u32,
    /// Effective group id.
    pub egid: u32,
    /// Saved user id (setuid/setgid restore target).
    pub suid: u32,
    /// Saved group id.
    pub sgid: u32,
    /// Filesystem user id (used for file-access checks).
    pub fsuid: u32,
    /// Filesystem group id.
    pub fsgid: u32,
    /// Supplementary group ids.
    pub groups: Vec<u32>,
    /// True once `setfsuid`/`setfsgid` decoupled the filesystem id from the
    /// effective id (Linux semantics).
    decoupled: bool,
}

impl Credentials {
    /// A fresh identity for an unprivileged process: every id equals the base
    /// uid/gid and there are no supplementary groups.
    pub fn user(uid: u32, gid: u32) -> Self {
        Credentials {
            uid,
            gid,
            euid: uid,
            egid: gid,
            suid: uid,
            sgid: gid,
            fsuid: uid,
            fsgid: gid,
            groups: Vec::new(),
            decoupled: false,
        }
    }

    /// The fully privileged root identity (uid = gid = 0).
    pub fn root() -> Self {
        Self::user(0, 0)
    }

    /// True when `euid == 0`, granting the privilege overrides below.
    pub fn is_privileged(&self) -> bool {
        self.euid == 0
    }

    fn follow_fsuid(&mut self) {
        if !self.decoupled {
            self.fsuid = self.euid;
        }
    }

    fn follow_fsgid(&mut self) {
        if !self.decoupled {
            self.fsgid = self.egid;
        }
    }

    /// `setuid`: a privileged process sets all four ids; an unprivileged one
    /// may only set the effective (and filesystem) id to the real or saved id.
    pub fn setuid(&mut self, uid: u32) -> Result<(), i64> {
        if self.is_privileged() {
            self.uid = uid;
            self.euid = uid;
            self.suid = uid;
            self.fsuid = uid;
            Ok(())
        } else if uid == self.uid || uid == self.suid {
            self.euid = uid;
            self.fsuid = uid;
            Ok(())
        } else {
            Err(crate::abi::errno::EPERM)
        }
    }

    /// `setgid`: group mirror of [`Credentials::setuid`].
    pub fn setgid(&mut self, gid: u32) -> Result<(), i64> {
        if self.is_privileged() {
            self.gid = gid;
            self.egid = gid;
            self.sgid = gid;
            self.fsgid = gid;
            Ok(())
        } else if gid == self.gid || gid == self.sgid {
            self.egid = gid;
            self.fsgid = gid;
            Ok(())
        } else {
            Err(crate::abi::errno::EPERM)
        }
    }

    /// `seteuid`: privileged processes may pick any id; others only the real
    /// or saved one. The filesystem id follows unless decoupled by `setfsuid`.
    pub fn seteuid(&mut self, euid: u32) -> Result<(), i64> {
        if self.is_privileged() {
            self.euid = euid;
            self.follow_fsuid();
            Ok(())
        } else if euid == self.uid || euid == self.suid {
            self.euid = euid;
            self.follow_fsuid();
            Ok(())
        } else {
            Err(crate::abi::errno::EPERM)
        }
    }

    /// `setegid`: group mirror of [`Credentials::seteuid`].
    pub fn setegid(&mut self, egid: u32) -> Result<(), i64> {
        if self.is_privileged() {
            self.egid = egid;
            self.follow_fsgid();
            Ok(())
        } else if egid == self.gid || egid == self.sgid {
            self.egid = egid;
            self.follow_fsgid();
            Ok(())
        } else {
            Err(crate::abi::errno::EPERM)
        }
    }

    /// `setreuid`: set the real and effective ids independently. Unprivileged
    /// callers may only pick from the existing real/effective/saved set. The
    /// saved id is refreshed to the effective id when the real id changes or
    /// the effective id moves away from the previous real id.
    pub fn setreuid(&mut self, ruid: u32, euid: u32) -> Result<(), i64> {
        let (old_ruid, old_euid, old_suid) = (self.uid, self.euid, self.suid);
        let member = |v: u32| v == old_ruid || v == old_euid || v == old_suid;
        let priv_ = self.is_privileged();
        if !priv_ && ruid != ID_UNCHANGED && !member(ruid) {
            return Err(crate::abi::errno::EPERM);
        }
        if !priv_ && euid != ID_UNCHANGED && !member(euid) {
            return Err(crate::abi::errno::EPERM);
        }
        if ruid != ID_UNCHANGED {
            self.uid = ruid;
        }
        if euid != ID_UNCHANGED {
            self.euid = euid;
        }
        if (ruid != ID_UNCHANGED && ruid != old_ruid) || (euid != ID_UNCHANGED && euid != old_ruid) {
            self.suid = self.euid;
        }
        self.follow_fsuid();
        Ok(())
    }

    /// `setregid`: group mirror of [`Credentials::setreuid`].
    pub fn setregid(&mut self, rgid: u32, egid: u32) -> Result<(), i64> {
        let (old_rgid, old_egid, old_sgid) = (self.gid, self.egid, self.sgid);
        let member = |v: u32| v == old_rgid || v == old_egid || v == old_sgid;
        let priv_ = self.is_privileged();
        if !priv_ && rgid != ID_UNCHANGED && !member(rgid) {
            return Err(crate::abi::errno::EPERM);
        }
        if !priv_ && egid != ID_UNCHANGED && !member(egid) {
            return Err(crate::abi::errno::EPERM);
        }
        if rgid != ID_UNCHANGED {
            self.gid = rgid;
        }
        if egid != ID_UNCHANGED {
            self.egid = egid;
        }
        if (rgid != ID_UNCHANGED && rgid != old_rgid) || (egid != ID_UNCHANGED && egid != old_rgid)
        {
            self.sgid = self.egid;
        }
        self.follow_fsgid();
        Ok(())
    }

    /// `setresuid`: set real, effective and saved ids. Unprivileged callers
    /// may only pick values from the existing triple.
    pub fn setresuid(&mut self, ruid: u32, euid: u32, suid: u32) -> Result<(), i64> {
        let (old_ruid, old_euid, old_suid) = (self.uid, self.euid, self.suid);
        if !self.is_privileged() {
            let member = |v: u32| v == old_ruid || v == old_euid || v == old_suid;
            if (ruid != ID_UNCHANGED && !member(ruid))
                || (euid != ID_UNCHANGED && !member(euid))
                || (suid != ID_UNCHANGED && !member(suid))
            {
                return Err(crate::abi::errno::EPERM);
            }
        }
        if ruid != ID_UNCHANGED {
            self.uid = ruid;
        }
        if euid != ID_UNCHANGED {
            self.euid = euid;
        }
        if suid != ID_UNCHANGED {
            self.suid = suid;
        }
        self.follow_fsuid();
        Ok(())
    }

    /// `setresgid`: group mirror of [`Credentials::setresuid`].
    pub fn setresgid(&mut self, rgid: u32, egid: u32, sgid: u32) -> Result<(), i64> {
        let (old_rgid, old_egid, old_sgid) = (self.gid, self.egid, self.sgid);
        if !self.is_privileged() {
            let member = |v: u32| v == old_rgid || v == old_egid || v == old_sgid;
            if (rgid != ID_UNCHANGED && !member(rgid))
                || (egid != ID_UNCHANGED && !member(egid))
                || (sgid != ID_UNCHANGED && !member(sgid))
            {
                return Err(crate::abi::errno::EPERM);
            }
        }
        if rgid != ID_UNCHANGED {
            self.gid = rgid;
        }
        if egid != ID_UNCHANGED {
            self.egid = egid;
        }
        if sgid != ID_UNCHANGED {
            self.sgid = sgid;
        }
        self.follow_fsgid();
        Ok(())
    }

    /// `setfsuid`: set the filesystem user id. Linux semantics: only the real,
    /// effective, saved or current fsuid is accepted (any value under `euid
    /// == 0`); the previous fsuid is returned whether or not it changed.
    pub fn setfsuid(&mut self, fsuid: u32) -> u32 {
        let old = self.fsuid;
        let allowed =
            self.is_privileged() || fsuid == self.uid || fsuid == self.euid || fsuid == self.suid;
        if allowed {
            self.fsuid = fsuid;
            self.decoupled = true;
        }
        old
    }

    /// `setfsgid`: group mirror of [`Credentials::setfsuid`].
    pub fn setfsgid(&mut self, fsgid: u32) -> u32 {
        let old = self.fsgid;
        let allowed =
            self.is_privileged() || fsgid == self.gid || fsgid == self.egid || fsgid == self.sgid;
        if allowed {
            self.fsgid = fsgid;
            self.decoupled = true;
        }
        old
    }

    /// `setgroups`: replace the supplementary group set (privileged only).
    pub fn setgroups(&mut self, groups: Vec<u32>) -> Result<(), i64> {
        if !self.is_privileged() {
            return Err(crate::abi::errno::EPERM);
        }
        self.groups = groups;
        Ok(())
    }
}

/// Which kind of access a system call wants from `may_access`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// Read.
    Read,
    /// Write.
    Write,
    /// Execute / directory search.
    Exec,
}

impl Access {
    /// The low three-bit mask slot for this access on the matching class.
    pub fn bits(self) -> u32 {
        match self {
            Access::Read => 0o4,
            Access::Write => 0o2,
            Access::Exec => 0o1,
        }
    }
}

/// Classic Unix permission model: pick owner/group/other by the filesystem
/// ids (respecting supplementary groups), then test the requested bit.
///
/// A privileged caller (`euid == 0`) overrides read and write access; execute
/// still requires at least one execute bit somewhere, per Linux semantics.
pub fn may_access(cred: &Credentials, fuid: u32, fgid: u32, mode: u32, want: Access) -> bool {
    let any_x = mode & 0o111 != 0;
    let class_bits = if cred.fsuid == fuid {
        (mode >> 6) & 0o7
    } else if cred.fsgid == fgid || cred.groups.iter().any(|&g| g == fgid) {
        (mode >> 3) & 0o7
    } else {
        mode & 0o7
    };
    if cred.is_privileged() {
        match want {
            Access::Read | Access::Write => true,
            Access::Exec => any_x,
        }
    } else {
        class_bits & want.bits() != 0
    }
}

/// POSIX `kill` rule: a sender whose real or effective uid equals the target's
/// real or saved uid (or that is privileged) may signal it.
pub fn may_signal(sender: &Credentials, target_uid: u32, target_suid: u32) -> bool {
    if sender.is_privileged() {
        return true;
    }
    sender.uid == target_uid
        || sender.euid == target_uid
        || sender.uid == target_suid
        || sender.euid == target_suid
}

/// One task's full credential state: identity plus process umask.
#[derive(Clone)]
struct ProcState {
    cred: Credentials,
    umask: u32,
}

/// Global per-task credential registry; see the module docs for the rationale.
static STATES: Spinlock<BTreeMap<usize, ProcState>> = Spinlock::new(BTreeMap::new());

fn default_creds() -> Credentials {
    Credentials::user(DEFAULT_UID, DEFAULT_GID)
}

/// The current credentials of `task` (defaults to the unprivileged identity
/// when the task has no registered state).
pub fn get(task: usize) -> Credentials {
    STATES
        .lock()
        .get(&task)
        .map(|s| s.cred.clone())
        .unwrap_or_else(default_creds)
}

/// Mutate `task`'s credentials exactly once per syscall. If the task had no
/// registered state it first inherits the default identity. On error the
/// registry is left untouched.
pub fn update<F: FnOnce(&mut Credentials) -> Result<(), i64>>(task: usize, f: F) -> Result<(), i64> {
    let mut map = STATES.lock();
    let mut cred = map
        .get(&task)
        .map(|s| s.cred.clone())
        .unwrap_or_else(default_creds);
    f(&mut cred)?;
    map.entry(task)
        .or_insert(ProcState {
            cred: default_creds(),
            umask: DEFAULT_UMASK,
        })
        .cred = cred;
    Ok(())
}

/// Install an explicit identity (used at process spawn: default user, or root
/// for privileged boot programs).
pub fn seed(task: usize, cred: Credentials, umask: u32) {
    STATES.lock().insert(task, ProcState { cred, umask });
}

/// Install the root identity for `task`.
pub fn seed_root(task: usize) {
    seed(task, Credentials::root(), DEFAULT_UMASK);
}

/// Copy `src`'s credential state to `dst` (mirrors `fork`'s fd-table copy).
/// An unregistered source propagates the default unprivileged identity.
pub fn fork_creds(src: usize, dst: usize) {
    let mut map = STATES.lock();
    let state = map
        .get(&src)
        .cloned()
        .unwrap_or_else(|| ProcState {
            cred: default_creds(),
            umask: DEFAULT_UMASK,
        });
    map.insert(dst, state);
}

/// Drop a task's credential state (process teardown).
pub fn drop_creds(task: usize) {
    STATES.lock().remove(&task);
}

/// The task's umask (the default if it has no state).
pub fn umask_of(task: usize) -> u32 {
    STATES
        .lock()
        .get(&task)
        .map(|s| s.umask)
        .unwrap_or(DEFAULT_UMASK)
}

/// Replace `task`'s umask; returns the previous value.
pub fn set_umask(task: usize, mask: u32) -> u32 {
    let old = umask_of(task);
    let mut map = STATES.lock();
    map.entry(task)
        .or_insert(ProcState {
            cred: default_creds(),
            umask: DEFAULT_UMASK,
        })
        .umask = mask & 0o777;
    old
}

/// Credentials of the currently executing thread (root for the scheduler
/// context).
pub fn current() -> Credentials {
    match crate::task::sched::current_task_id() {
        Some(t) => get(t.0),
        None => Credentials::root(),
    }
}