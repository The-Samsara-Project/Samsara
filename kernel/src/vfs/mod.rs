// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! Samsara virtual filesystem.
//!
//! A small vnode-style core: every filesystem exposes [`Vnode`] objects and
//! the core handles absolute path resolution across registered mounts plus
//! per-task file descriptor tables.

pub mod devfs;
pub mod fdtab;
pub mod procfs;
pub mod ramfs;

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use crate::sync::Spinlock;

// Node type, error space, and the vnode abstraction live in `driver_common`
// so that drivers can implement filesystems and device nodes against the exact
// same trait the kernel uses. Re-exported here for in-kernel convenience.
pub use driver_common::{FsError, NodeKind, Vnode, VnodeRef};
use driver_common::S_ISVTX;

/// Open access-mode: read-only (low two flag bits, `O_ACCMODE`).
pub const O_RDONLY: u32 = 0;
/// Open access-mode: write-only.
pub const O_WRONLY: u32 = 1;
/// Open access-mode: read/write.
pub const O_RDWR: u32 = 2;
/// Mask over the low two access-mode bits.
pub const O_ACCMODE: u32 = 3;
/// Create the file if it does not exist.
pub const O_CREAT: u32 = 0o100;
/// Fail with `EEXIST` if the file already exists (implies create).
pub const O_EXCL: u32 = 0o200;
/// Truncate an existing regular file to zero length.
pub const O_TRUNC: u32 = 0o1000;
/// Do not block: a `read` with no data available returns `EAGAIN` instead of
/// waiting, and a `write` that cannot proceed does the same.
///
/// The value matches Linux so a ported program passing the numeric flag lands
/// on the right behavior. This is the only status flag Samsara tracks after
/// `open`; everything else in `flags` is consumed during the open itself.
pub const O_NONBLOCK: u32 = 0o4000;
/// Mask over the status flags `fcntl(F_SETFL)` is allowed to change.
pub const F_SETFL_MASK: u32 = O_NONBLOCK;

struct MountTable {
    root: Option<VnodeRef>,
    /// Mount points keyed by normalized absolute path ("/dev", "/proc", ...).
    mounts: Vec<(String, VnodeRef)>,
}

static MOUNTS: Spinlock<MountTable> = Spinlock::new(MountTable {
    root: None,
    mounts: Vec::new(),
});

/// Install the root filesystem. Must be called before any path resolution.
pub fn init(root: VnodeRef) {
    let mut m = MOUNTS.lock();
    m.root = Some(root);
}

/// Mount a filesystem root at an absolute path (single-component depth is
/// typical: "/dev", "/proc"). The mount shadows any existing node there.
pub fn mount(path: &str, fs_root: VnodeRef) -> Result<(), FsError> {
    let norm = normalize(path);
    let mut m = MOUNTS.lock();
    if m.root.is_none() {
        return Err(FsError::NotFound);
    }
    if m.mounts.iter().any(|(p, _)| *p == norm) {
        return Err(FsError::Exists);
    }
    m.mounts.push((norm, fs_root));
    Ok(())
}

fn normalize(path: &str) -> String {
    let mut comps: Vec<&str> = Vec::new();
    for comp in path.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                comps.pop();
            }
            c => comps.push(c),
        }
    }
    let mut out = String::from("/");
    out.push_str(&comps.join("/"));
    out
}

/// Public wrapper around [`normalize`]: collapse `.`/`..` and duplicate
/// slashes in an absolute path, returning the canonical form. Used when
/// recording a process's working directory.
pub fn normalize_abs(path: &str) -> String {
    normalize(path)
}

/// Resolve an absolute path to a vnode, crossing mount points.
pub fn resolve(path: &str) -> Result<VnodeRef, FsError> {
    let norm = normalize(path);
    let m = MOUNTS.lock();
    let root = m.root.clone().ok_or(FsError::NotFound)?;

    // Find longest matching mount prefix.
    let mut node = root.clone();
    let mut rest: &str = norm.as_str();
    let mut best_len = 0usize;
    for (mp, mvnode) in m.mounts.iter() {
        let matches = norm.as_str() == mp.as_str() || norm.starts_with(&alloc::format!("{}/", mp));
        if matches && mp.len() > best_len {
            best_len = mp.len();
            node = mvnode.clone();
        }
    }
    if best_len > 0 {
        rest = &norm[best_len..];
        if let Some(stripped) = rest.strip_prefix('/') {
            rest = stripped;
        } else {
            // Path is exactly the mount point itself.
            return Ok(node);
        }
    } else if let Some(stripped) = norm.strip_prefix('/') {
        rest = stripped;
    }

    let mut cur = node;
    for comp in rest.split('/') {
        if comp.is_empty() || comp == "." {
            continue;
        }
        if comp == ".." {
            continue; // ramfs dirs are flat; parent walk unsupported here
        }
        cur = cur.lookup(comp)?;
        // Cross a mount if one sits on this exact node's path later; the
        // prefix scan above already handled mounted subtrees during the
        // string phase, which keeps resolution O(components).
    }
    Ok(cur)
}

/// Resolve an absolute path to a vnode, like [`resolve`], checking search
/// (execute) permission on every directory component against `cred`.
///
/// The final component is *not* access-checked here: callers decide whether
/// they need read/write on the leaf (open), ownership (chmod/chown) or
/// nothing at all (stat).
pub fn resolve_checked(path: &str, cred: &crate::cred::Credentials) -> Result<VnodeRef, FsError> {
    let norm = normalize(path);
    let m = MOUNTS.lock();
    let root = m.root.clone().ok_or(FsError::NotFound)?;

    fn search_ok(node: &dyn Vnode, cred: &crate::cred::Credentials) -> bool {
        crate::cred::may_access(
            cred,
            node.uid(),
            node.gid(),
            node.mode(),
            crate::cred::Access::Exec,
        )
    }

    // Find longest matching mount prefix.
    let mut node = root.clone();
    let mut rest: &str = norm.as_str();
    let mut best_len = 0usize;
    for (mp, mvnode) in m.mounts.iter() {
        let matches = norm.as_str() == mp.as_str() || norm.starts_with(&alloc::format!("{}/", mp));
        if matches && mp.len() > best_len {
            best_len = mp.len();
            node = mvnode.clone();
        }
    }
    if best_len > 0 {
        rest = &norm[best_len..];
        if let Some(stripped) = rest.strip_prefix('/') {
            rest = stripped;
        } else {
            // Path is exactly the mount point itself.
            return Ok(node);
        }
    } else if let Some(stripped) = norm.strip_prefix('/') {
        rest = stripped;
    }

    let mut cur = node;
    for comp in rest.split('/') {
        if comp.is_empty() || comp == "." {
            continue;
        }
        if comp == ".." {
            continue; // ramfs dirs are flat; parent walk unsupported here
        }
        if !search_ok(cur.as_ref(), cred) {
            return Err(FsError::AccessDenied);
        }
        cur = cur.lookup(comp)?;
    }
    Ok(cur)
}

/// An open file description held by a task's descriptor table.
#[derive(Clone)]
pub struct FileHandle {
    /// Backing vnode.
    pub node: VnodeRef,
    /// Read/write cursor.
    pub offset: u64,
    /// Access mode granted by `open` (`O_RDONLY`/`O_WRONLY`/`O_RDWR`).
    pub access: u32,
    /// Status flags currently in effect, as set by `open` and changed by
    /// `fcntl(F_SETFL)` (see `F_SETFL`/`O_*`). Only the flags that alter
    /// blocking behavior are tracked; the rest of `O_*` is consumed at open
    /// time and never needs to be remembered.
    pub status: u32,
}

impl FileHandle {
    /// Read at the current cursor and advance it on success.
    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize, FsError> {
        if self.access == O_WRONLY {
            return Err(FsError::BadDescriptor);
        }
        let n = self.node.read_at(self.offset, buf)?;
        self.offset += n as u64;
        Ok(n)
    }

    /// Write at the current cursor and advance it on success.
    pub fn write(&mut self, buf: &[u8]) -> Result<usize, FsError> {
        if self.access == O_RDONLY {
            return Err(FsError::BadDescriptor);
        }
        let n = self.node.write_at(self.offset, buf)?;
        self.offset += n as u64;
        Ok(n)
    }
}

/// Per-task open file descriptor table.
#[derive(Default)]
pub struct FdTable {
    fds: Vec<Option<FileHandle>>,
}

impl FdTable {
    /// Install `node` in the lowest free slot; returns its fd number. The
    /// descriptor grants both read and write (used by pipes and the legacy
    /// open path).
    pub fn install(&mut self, node: VnodeRef) -> usize {
        self.install_with(node, O_RDWR)
    }

    /// Install `node` with an explicit access mode (see `O_*`).
    pub fn install_with(&mut self, node: VnodeRef, access: u32) -> usize {
        self.install_full(node, access, 0)
    }

    /// Install `node` with an access mode and a set of status flags (see
    /// `O_*`), the pair `open` derives from its `flags` argument.
    pub fn install_full(&mut self, node: VnodeRef, access: u32, status: u32) -> usize {
        for (i, slot) in self.fds.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(FileHandle {
                    node,
                    offset: 0,
                    access,
                    status,
                });
                return i;
            }
        }
        self.fds.push(Some(FileHandle {
            node,
            offset: 0,
            access,
            status,
        }));
        self.fds.len() - 1
    }

    /// Fetch a mutable open-file description behind `fd`.
    pub fn get_mut(&mut self, fd: usize) -> Option<&mut FileHandle> {
        self.fds.get_mut(fd).and_then(|s| s.as_mut())
    }

    /// Fetch a clone of the vnode behind `fd`.
    pub fn get(&self, fd: usize) -> Result<VnodeRef, FsError> {
        self.fds
            .get(fd)
            .and_then(|s| s.as_ref())
            .map(|h| h.node.clone())
            .ok_or(FsError::BadDescriptor)
    }

    /// Fetch the vnode and the access mode granted when the descriptor was
    /// opened. `poll` needs both: readiness is masked by whether the open
    /// description can actually read and/or write.
    pub fn get_with_access(&self, fd: usize) -> Result<(VnodeRef, u32), FsError> {
        self.fds
            .get(fd)
            .and_then(|s| s.as_ref())
            .map(|h| (h.node.clone(), h.access))
            .ok_or(FsError::BadDescriptor)
    }

    /// Close `fd`, returning its vnode.
    pub fn close(&mut self, fd: usize) -> Result<VnodeRef, FsError> {
        match self.fds.get_mut(fd) {
            Some(slot) if slot.is_some() => {
                let h = slot.take().unwrap();
                Ok(h.node)
            }
            _ => Err(FsError::BadDescriptor),
        }
    }

    /// Deep-enough copy for `fork`: duplicate slotted vnodes and their shared
    /// file cursors into a fresh table.
    pub fn fork(&self) -> FdTable {
        FdTable {
            fds: self.fds.clone(),
        }
    }
}

/// Allocate an fd-table owned by the kernel bootstrap context.
pub fn new_fd_table() -> Box<FdTable> {
    Box::new(FdTable::default())
}

/// Create a file (or directory) at `path`, creating missing parents. Uses the
/// default ownership (root:root) and mode (dir `0o755`, file `0o666`); the
/// kernel-internal bootstrap path.
pub fn create(path: &str, kind: NodeKind) -> Result<VnodeRef, FsError> {
    let (uid, gid, mode) = match kind {
        NodeKind::Dir => (0, 0, 0o755),
        _ => (0, 0, 0o666),
    };
    create_as(path, kind, uid, gid, mode)
}

/// Create a file (or directory) at `path` with explicit owner and mode bits.
/// Does not apply a umask — callers that honor one mask `mode` themselves.
pub fn create_as(
    path: &str,
    kind: NodeKind,
    uid: u32,
    gid: u32,
    mode: u32,
) -> Result<VnodeRef, FsError> {
    let norm = normalize(path);
    let (parent, name) = match norm.rfind('/') {
        Some(0) => ("/", &norm[1..]),
        Some(pos) => (&norm[..pos], &norm[pos + 1..]),
        None => return Err(FsError::NotFound),
    };
    if name.is_empty() {
        return Err(FsError::NotFound);
    }
    let dir = resolve(parent)?;
    if dir.kind() != NodeKind::Dir {
        return Err(FsError::NotADirectory);
    }
    // Existing file? Return it instead of failing (open-or-create).
    if let Ok(existing) = dir.lookup(name) {
        return Ok(existing);
    }
    dir.create_child(name, kind, uid, gid, mode)
}

/// Create a leaf under `path` after checking search permission on the parent
/// directory and write permission on it against `cred`. Used by `open` with
/// `O_CREAT`; missing parents stay an error.
pub fn create_checked(
    path: &str,
    kind: NodeKind,
    uid: u32,
    gid: u32,
    mode: u32,
    cred: &crate::cred::Credentials,
) -> Result<VnodeRef, FsError> {
    let norm = normalize(path);
    let (parent, name) = match norm.rfind('/') {
        Some(0) => ("/", &norm[1..]),
        Some(pos) => (&norm[..pos], &norm[pos + 1..]),
        None => return Err(FsError::NotFound),
    };
    if name.is_empty() {
        return Err(FsError::NotFound);
    }
    let dir = resolve_checked(parent, cred)?;
    if dir.kind() != NodeKind::Dir {
        return Err(FsError::NotADirectory);
    }
    if !crate::cred::may_access(
        cred,
        dir.uid(),
        dir.gid(),
        dir.mode(),
        crate::cred::Access::Write,
    ) {
        return Err(FsError::AccessDenied);
    }
    dir.create_child(name, kind, uid, gid, mode)
}

/// Remove the entry `path` from its parent directory, after permission checks.
///
/// Two rules make this more than a map delete, and both are POSIX:
///
///   * the caller needs **write** permission on the *parent directory*, not on
///     the file being removed. Removal is a change to the directory, so a
///     read-only directory is a wall regardless of who owns the file -- this is
///     what makes an immutable `/usr` work, and what stops a user from deleting
///     root's files out of a directory they can only read;
///   * in a directory with the **sticky bit** set (`/tmp`), the caller must
///     additionally own the file, or be privileged. Without that, any user
///     could delete any other user's files in a shared directory.
///
/// Returns the removed node's owner and kind so the caller can report `EPERM`
/// vs `EISDIR` correctly if it needs to.
pub fn remove_checked(
    path: &str,
    cred: &crate::cred::Credentials,
) -> Result<(VnodeRef,), FsError> {
    let norm = normalize(path);
    let (parent, name) = match norm.rfind('/') {
        Some(0) => ("/", &norm[1..]),
        Some(pos) => (&norm[..pos], &norm[pos + 1..]),
        // A bare name with no directory component; the caller is expected to
        // have made the path absolute already.
        None => return Err(FsError::NotFound),
    };
    if name.is_empty() {
        return Err(FsError::NotFound);
    }
    // "/" and "/.." name the root and can never be unlinked.
    if name == "." || name == ".." {
        return Err(FsError::AccessDenied);
    }
    let dir = resolve_checked(parent, cred)?;
    if dir.kind() != NodeKind::Dir {
        return Err(FsError::NotADirectory);
    }
    if !crate::cred::may_access(
        cred,
        dir.uid(),
        dir.gid(),
        dir.mode(),
        crate::cred::Access::Write,
    ) {
        return Err(FsError::AccessDenied);
    }
    // Sticky bit: only the file's owner (or a privileged caller) may take it
    // out of a shared directory. The entry has to be resolved before removal,
    // both to learn its owner and to distinguish "not there" from "not yours".
    if dir.mode() & S_ISVTX != 0 && !cred.is_privileged() {
        let victim = dir.lookup(name)?;
        if victim.uid() != cred.fsuid {
            return Err(FsError::AccessDenied);
        }
    }
    dir.remove_child(name)?;
    Ok((dir,))
}

/// Open (or create for files) a path and install it in `task`'s table.
pub fn open_fd(task_id: usize, path: &str, kind: NodeKind) -> Result<usize, FsError> {
    let node = resolve(path).or_else(|e| match e {
        FsError::NotFound if kind == NodeKind::File => create(path, kind),
        other => Err(other),
    })?;
    Ok(fdtab::install(task_id, node))
}

/// Install `node` in `task`'s descriptor table with an explicit access mode.
pub fn install_with(task_id: usize, node: VnodeRef, access: u32) -> usize {
    fdtab::install_with(task_id, node, access)
}
