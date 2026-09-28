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
use alloc::sync::Arc;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
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

/// How many symbolic links a single path resolution may follow.
///
/// POSIX's answer is 8 (`SYMLOOP_MAX`), and the value matters: a symlink that
/// points at itself must fail rather than loop forever, and the failure has to be
/// `ELOOP`. A limit that is too small breaks a legitimate chain -- a system whose
/// `/lib` is a link, whose `/lib/foo` is a link, and whose `foo` is a link to
/// somewhere else is not pathological -- and one that is too large turns a bug
/// into a hang that looks like a slow machine.
pub const SYMLOOP_MAX: usize = 8;

/// How many components a single path resolution may examine, counting
/// components consumed *and* components introduced by symlink expansion.
///
/// A second limit for the same reason, and it bounds a different attack: a chain
/// where each link points at a path containing two links, so the link count stays
/// under `SYMLOOP_MAX` while the path length grows exponentially. Without this,
/// `a -> b/b`, `b -> a/a/a` and so on is a symlink loop that never trips the link
/// counter. The bound is on work done, not on links seen.
const MAX_RESOLVE_STEPS: usize = 256;

/// Resolve an absolute path to a vnode, crossing mount points and following
/// symbolic links.
///
/// The last component is followed too, which is what makes `open` work: a
/// program that opens `/bin/ls` wants the executable, not the link. Callers that
/// want the link itself use [`resolve_nofollow`] -- `lstat(2)`, and `readlink`.
pub fn resolve(path: &str) -> Result<VnodeRef, FsError> {
    resolve_inner(path, true, None)
}

/// Resolve an absolute path to a vnode *without* following a final symbolic
/// link. Intermediate components are still followed: a link to a directory
/// cannot be entered without being followed, or `/link/etc/passwd` would break.
pub fn resolve_nofollow(path: &str) -> Result<VnodeRef, FsError> {
    resolve_inner(path, false, None)
}

/// Resolve a path, optionally checking search permission on every directory
/// component against `cred`.
///
/// `follow_final` selects [`resolve_nofollow`] behaviour, and `cred` turns on the
/// permission check. `None` means the check is skipped, which is what the
/// kernel-internal callers want and what `resolve` has always done.
pub fn resolve_with(
    path: &str,
    follow_final: bool,
    cred: Option<&crate::cred::Credentials>,
) -> Result<VnodeRef, FsError> {
    resolve_inner(path, follow_final, cred)
}

fn resolve_inner(
    path: &str,
    follow_final: bool,
    cred: Option<&crate::cred::Credentials>,
) -> Result<VnodeRef, FsError> {
    let start = normalize(path);

    // `queue` holds the components still to be examined, in order.
    //
    // A queue of names rather than an index into one path string, because
    // following a link splices a *new* path in front of whatever is left of the
    // old one, and the leftovers have to survive that splice. A string with an
    // index cannot express "these components, then those, then more".
    //
    // `dir` is the absolute path of the directory the next component lives in,
    // maintained alongside the queue so that a *relative* link target can be
    // resolved. Resolving a relative target needs the link's own directory, and
    // the queue alone does not carry it: by the time the walker is one component
    // into `/a/b/c`, `cur` is the `b` node and `/a` is no longer recoverable from
    // the queue's tail. Keeping the prefix as text is what makes `b -> ../x` mean
    // `/a/x` and not `/x`.
    let mut queue: Vec<String> = Vec::new();
    let mut dir = String::from("/");
    let (mut cur, rest) = resolve_mount_root(&start)?;
    for comp in pending_components(rest) {
        queue.push(comp);
    }

    let mut links = 0usize;
    let mut steps = 0usize;

    while !queue.is_empty() {
        steps += 1;
        if steps > MAX_RESOLVE_STEPS {
            return Err(FsError::TooManyLinks);
        }
        let comp = queue.remove(0);

        match comp.as_str() {
            "." => continue,
            // A parent walk pops the directory prefix, which is what makes `..`
            // mean the right thing after a link has moved the walk into a
            // different subtree. At the root there is nowhere to go, so it stays.
            //
            // `..` also discards whatever is left in `queue`. That is required,
            // not incidental: a component that is about to be dropped can never
            // be reached, and leaving it would make `/a/link/../b` resolve
            // `/a/b/target/b` -- the parent walk would be applied to the
            // *target's* directory and then walk on past it.
            ".." => {
                dir = parent_of(&dir);
                queue.clear();
                if dir != "/" {
                    cur = resolve(&dir)?;
                }
                continue;
            }
            _ => {}
        }

        if let Some(c) = cred {
            if !search_ok(cur.as_ref(), c) {
                return Err(FsError::AccessDenied);
            }
        }

        // `dir` is the directory this component is looked up *in*, and that is
        // what a relative symlink target has to be joined against. The prefix is
        // therefore extended only once the component has been accepted as a real
        // step into the tree -- not before the lookup, and not at all when the
        // component turns out to be a link.
        //
        // Extending first is the mistake that makes relative links resolve as if
        // they were absolute: with `dir` already advanced to `/tmp/ln`, a target
        // of `target` joins to `/tmp/ln/target` instead of `/tmp/target`, and the
        // link silently stops working the moment it is not sitting in `/`.
        let next = cur.lookup(&comp)?;
        let is_last = queue.is_empty();
        if next.kind() == NodeKind::Symlink && !(is_last && !follow_final) {
            links += 1;
            if links > SYMLOOP_MAX {
                return Err(FsError::TooManyLinks);
            }
            let target = next.symlink_target().ok_or(FsError::Invalid)?;
            // An absolute target replaces the path outright. A relative one is
            // joined to `dir` -- the directory the link was found in, which is
            // exactly the directory POSIX says a relative link is relative to.
            let resolved = if target.starts_with('/') {
                normalize(&target)
            } else {
                let mut joined = String::from(&dir);
                if joined != "/" {
                    joined.push('/');
                }
                joined.push_str(&target);
                normalize(&joined)
            };
            let (root, rest) = resolve_mount_root(&resolved)?;
            cur = root;
            // The directory the target's own first component lives in: everything
            // up to the last component, which is the mount prefix plus whatever
            // directory components the target path had.
            let consumed = resolved.len() - rest.len();
            dir = if consumed == 0 {
                String::from("/")
            } else {
                String::from(&resolved[..consumed])
            };
            if dir.is_empty() {
                dir = String::from("/");
            }
            // The target's components go in *front* of what is left of the old
            // path, so `link/x` where link names `/y` resolves `/y/x` and not
            // `/x`. Appending would silently drop the link's own trailing
            // components, which is the bug this ordering is here to prevent.
            splice_front(&mut queue, pending_components(rest));
            continue;
        }

        dir = if dir == "/" {
            alloc::format!("/{}", comp)
        } else {
            alloc::format!("{}/{}", dir, comp)
        };
        cur = next;
    }
    Ok(cur)
}

/// Push `items` onto the front of `queue`, preserving their order.
fn splice_front(queue: &mut Vec<String>, items: Vec<String>) {
    if items.is_empty() {
        return;
    }
    let mut merged = items;
    merged.append(queue);
    *queue = merged;
}

/// The parent of an absolute normalized path. `"/"` is its own parent.
fn parent_of(path: &str) -> String {
    match path.rfind('/') {
        None | Some(0) => String::from("/"),
        Some(pos) => String::from(&path[..pos]),
    }
}

fn search_ok(node: &dyn Vnode, cred: &crate::cred::Credentials) -> bool {
    crate::cred::may_access(
        cred,
        node.uid(),
        node.gid(),
        node.mode(),
        crate::cred::Access::Exec,
    )
}

fn pending_components(rest: &str) -> Vec<String> {
    rest.split('/')
        .filter(|c| !c.is_empty())
        .map(|c| String::from(c))
        .collect()
}

/// Cross the mount table for `norm`, returning the mounted root and the part of
/// the path still to be walked inside it.
fn resolve_mount_root(norm: &str) -> Result<(VnodeRef, &str), FsError> {
    let m = MOUNTS.lock();
    let root = m.root.clone().ok_or(FsError::NotFound)?;

    let mut node = root;
    let mut best_len = 0usize;
    for (mp, mvnode) in m.mounts.iter() {
        let matches = norm == mp.as_str() || norm.starts_with(&alloc::format!("{}/", mp));
        if matches && mp.len() > best_len {
            best_len = mp.len();
            node = mvnode.clone();
        }
    }
    let rest = if best_len > 0 {
        match norm[best_len..].strip_prefix('/') {
            Some(stripped) => stripped,
            // Path is exactly the mount point itself.
            None => "",
        }
    } else {
        norm.strip_prefix('/').unwrap_or("")
    };
    Ok((node, rest))
}

/// Resolve an absolute path to a vnode, like [`resolve`], checking search
/// (execute) permission on every directory component against `cred`.
///
/// The final component is *not* access-checked here: callers decide whether
/// they need read/write on the leaf (open), ownership (chmod/chown) or
/// nothing at all (stat).
pub fn resolve_checked(path: &str, cred: &crate::cred::Credentials) -> Result<VnodeRef, FsError> {
    resolve_with(path, true, Some(cred))
}

/// As [`resolve_checked`], but a final symbolic link is not followed.
///
/// This is `lstat(2)`: the answer describes the link rather than what it names.
/// [`resolve_checked`] is right for `stat`, `open`, `chmod` and `unlink`; a
/// program asking what a directory entry *is* -- `ls -l` drawing the `@` marker,
/// a shell testing `-L`, an installer refusing to clobber a link -- needs this.
pub fn resolve_checked_nofollow(
    path: &str,
    cred: &crate::cred::Credentials,
) -> Result<VnodeRef, FsError> {
    resolve_with(path, false, Some(cred))
}

/// Create a symbolic link at `path` pointing at `target`.
///
/// `target` is stored verbatim. Resolving it is the path walker's job, and doing
/// it here would make the link wrong the moment it moved: a relative target is
/// meaningless without knowing the directory the link sits in, and this function
/// is handed a path rather than the directory node.
///
/// A default mode of `0o777` is used. That is not a grant: on every system a
/// symlink's own permission bits are ignored, because following one is a
/// directory-traversal decision made about the *target* and the directories
/// leading to it. `ls -l` still shows it, so the number is not invisible, and
/// `0o777` is the value users expect to see. The alternative, `0o666`, implies
/// write access that is equally meaningless and reads as though the link were a
/// file.
pub fn symlink_as(
    path: &str,
    target: &str,
    uid: u32,
    gid: u32,
    mode: u32,
    cred: &crate::cred::Credentials,
) -> Result<(), FsError> {
    if target.is_empty() {
        // A link to nowhere is a mistake, and a silent one: every later
        // operation on it fails with ENOENT pointing at a path the user never
        // wrote. Rejecting it here names the actual problem.
        return Err(FsError::Invalid);
    }
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
    // A symlink whose own name already exists is EEXIST even if it points
    // nowhere. The alternative -- succeeding because the target is missing, or
    // failing because it is missing -- makes `symlink` depend on a path it is
    // explicitly not responsible for, and the two cases are indistinguishable to
    // a caller that has just fixed the target and is retrying.
    if dir.lookup(name).is_ok() {
        return Err(FsError::Exists);
    }
    dir.create_symlink(name, target, uid, gid, mode).map(|_| ())
}

/// An open file description held by a task's descriptor table.
#[derive(Clone)]
pub struct FileHandle {
    /// Backing vnode.
    pub node: VnodeRef,
    /// Read/write cursor.
    ///
    /// Shared, not per-descriptor, and that is the whole point of `dup(2)`:
    /// a duplicated descriptor refers to the *same* open file description, so
    /// the two must advance together. Two descriptors over a plain `u64` would
    /// each keep their own cursor, and a program writing through one while
    /// reading through the other -- which is what a shell's `cmd > f` does with
    /// a shared output -- would silently overwrite itself.
    ///
    /// It is an atomic rather than a plain cell for the same reason: the
    /// descriptors are not mutually exclusive, and two tasks sharing a pipe
    /// through a duplicated descriptor can be inside `read` at the same time.
    pub offset: Arc<AtomicU64>,
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
        let at = self.offset.load(Ordering::Relaxed);
        let n = self.node.read_at(at, buf)?;
        // Advance by however much was actually read, not by what was asked
        // for: a short read must not skip the bytes that were not delivered.
        self.offset.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }

    /// Write at the current cursor and advance it on success.
    pub fn write(&mut self, buf: &[u8]) -> Result<usize, FsError> {
        if self.access == O_RDONLY {
            return Err(FsError::BadDescriptor);
        }
        let at = self.offset.load(Ordering::Relaxed);
        let n = self.node.write_at(at, buf)?;
        self.offset.fetch_add(n as u64, Ordering::Relaxed);
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
                    offset: Arc::new(AtomicU64::new(0)),
                    access,
                    status,
                });
                return i;
            }
        }
        self.fds.push(Some(FileHandle {
            node,
            offset: Arc::new(AtomicU64::new(0)),
            access,
            status,
        }));
        self.fds.len() - 1
    }

    /// Fetch a clone of the whole open-file description behind `fd`.
    ///
    /// Cloning shares the offset rather than copying it, which is what makes
    /// the clone usable as a `dup(2)`. A caller wanting only the node should use
    /// [`FdTable::get`].
    pub fn description(&self, fd: usize) -> Result<FileHandle, FsError> {
        self.fds
            .get(fd)
            .and_then(|s| s.as_ref())
            .cloned()
            .ok_or(FsError::BadDescriptor)
    }

    /// Install an existing open-file description, keeping its shared offset.
    ///
    /// The counterpart to [`FdTable::install_full`], which starts a *new*
    /// description at offset zero. Confusing the two is the bug `dup` exists to
    /// avoid, so the distinction is in the name and the doc rather than left to
    /// the caller.
    pub fn install_description(&mut self, handle: FileHandle) -> usize {
        self.insert(handle)
    }

    fn insert(&mut self, handle: FileHandle) -> usize {
        for (i, slot) in self.fds.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(handle);
                return i;
            }
        }
        self.fds.push(Some(handle));
        self.fds.len() - 1
    }

    /// Place `handle` at exactly `fd`, displacing and returning whatever was
    /// there. For `dup2(2)`.
    pub fn replace_at(&mut self, fd: usize, handle: FileHandle) -> Option<FileHandle> {
        while self.fds.len() <= fd {
            self.fds.push(None);
        }
        self.fds[fd].replace(handle)
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
