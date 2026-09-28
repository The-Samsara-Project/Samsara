// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! ramfs: a fully in-memory filesystem with mutable files and directories.
//!
//! Used as the Samsara root filesystem; contents vanish on reboot.

use super::{FsError, NodeKind, Vnode, VnodeRef};
use crate::sync::Spinlock;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

/// Per-file size cap (4 MiB) so a bad writer cannot exhaust the heap.
const MAX_FILE: usize = 4 * 1024 * 1024;

struct RamFile {
    data: Spinlock<Vec<u8>>,
}

struct RamDir {
    children: Spinlock<BTreeMap<String, VnodeRef>>,
}

enum Inner {
    File(RamFile),
    Dir(RamDir),
    /// A symbolic link. The target is a path, not content, and is kept behind
    /// its own lock rather than in [`Meta`] so that the ordinary file write path
    /// cannot reach it -- see [`Vnode::symlink_target`].
    Symlink(Spinlock<String>),
}

/// Permission/ownership metadata shared by files and directories.
#[derive(Clone)]
struct Meta {
    mode: u32,
    uid: u32,
    gid: u32,
    /// Creation and content-change time, as (seconds, nanoseconds) since the
    /// Unix epoch. Stamped from the RTC so `ls -l` shows a real date.
    mtime: (i64, u32),
}

/// Source of inode numbers.
///
/// `st_ino` has to distinguish nodes or user space cannot tell two files
/// apart, so this is a plain monotonic counter rather than a pointer. A pointer
/// would work and would also leak a heap address into every `ls -l`.
static NEXT_INODE: AtomicU64 = AtomicU64::new(1);

/// The device number reported for the whole ramfs. One filesystem, one number.
const RAMFS_DEV: u32 = 0x5241; // 'RA'

/// A ramfs vnode.
pub struct RamNode {
    inner: Inner,
    meta: Spinlock<Meta>,
    ino: u64,
}

impl RamNode {
    fn new(inner: Inner, uid: u32, gid: u32, mode: u32) -> VnodeRef {
        Arc::new(RamNode {
            inner,
            meta: Spinlock::new(Meta { mode, uid, gid, mtime: now() }),
            ino: NEXT_INODE.fetch_add(1, Ordering::Relaxed),
        })
    }

    /// Touch the content-change time, for a write or a truncate.
    fn touch(&self) {
        self.meta.lock().mtime = now();
    }
}

/// Wall-clock `(secs, nanos)` for stamping nodes.
///
/// Falls back to `(0, 0)` when there is no RTC, which is the same "obviously
/// wrong" answer the rest of the system gives: a node with no timestamp is
/// better than a node with a fabricated plausible one.
fn now() -> (i64, u32) {
    (crate::rtc::epoch_secs(), 0)
}

impl Vnode for RamNode {
    fn kind(&self) -> NodeKind {
        match self.inner {
            Inner::File(_) => NodeKind::File,
            Inner::Dir(_) => NodeKind::Dir,
            Inner::Symlink(_) => NodeKind::Symlink,
        }
    }

    fn mode(&self) -> u32 {
        self.meta.lock().mode
    }

    fn uid(&self) -> u32 {
        self.meta.lock().uid
    }

    fn gid(&self) -> u32 {
        self.meta.lock().gid
    }

    fn set_mode(&self, mode: u32) -> Result<(), FsError> {
        self.meta.lock().mode = mode;
        Ok(())
    }

    fn set_owner(&self, uid: u32, gid: u32) -> Result<(), FsError> {
        let mut m = self.meta.lock();
        if uid != u32::MAX {
            m.uid = uid;
        }
        if gid != u32::MAX {
            m.gid = gid;
        }
        Ok(())
    }

    fn truncate(&self) -> Result<(), FsError> {
        match &self.inner {
            Inner::File(f) => {
                f.data.lock().clear();
                self.touch();
                Ok(())
            }
            Inner::Dir(_) => Err(FsError::IsADirectory),
            // Truncating a symlink has nothing to truncate. EACCES rather than
            // EISDIR: the node is not a directory, and the error a caller can
            // act on here is "you may not write through this", not "wrong node
            // type" -- the type is already visible from `lstat`.
            Inner::Symlink(_) => Err(FsError::AccessDenied),
        }
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        match &self.inner {
            Inner::File(f) => {
                let data = f.data.lock();
                let start = (offset as usize).min(data.len());
                let end = (start + buf.len()).min(data.len());
                let n = end - start;
                buf[..n].copy_from_slice(&data[start..end]);
                Ok(n)
            }
            Inner::Dir(_) => Err(FsError::IsADirectory),
            // A symlink's target is deliberately not readable. `readlink(2)` is
            // how a program asks, and it goes through `symlink_target()`. Serving
            // the target here too would mean `cat` on a symlink printed its
            // target, which is not what any system does and is a way to exfiltrate
            // a path a program was not meant to learn.
            Inner::Symlink(_) => Err(FsError::AccessDenied),
        }
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> Result<usize, FsError> {
        match &self.inner {
            // A symlink is immutable through the normal file path, for the same
            // reason it is not readable: the target is not content, and a
            // `write` that landed here would be silently changing a path rather
            // than writing bytes. Repointing a symlink is `rename`, and creating
            // one is `symlink`.
            Inner::Symlink(_) => Err(FsError::AccessDenied),
            Inner::File(f) => {
                let mut data = f.data.lock();
                let off = (offset as usize).min(MAX_FILE);
                let end = off + buf.len();
                if end > MAX_FILE {
                    return Err(FsError::OutOfSpace);
                }
                // Always grow to fit: extending past EOF appends/zero-fills.
                if data.len() < end {
                    data.resize(end, 0);
                }
                data[off..end].copy_from_slice(buf);
                // A write changes the contents, so the modification time has
                // to move. Leaving it at creation time makes `ls -l` report a
                // file as older than it is, and a build system that compares
                // timestamps will then decide a stale object is up to date.
                drop(data);
                self.touch();
                Ok(buf.len())
            }
            Inner::Dir(_) => Err(FsError::IsADirectory),
        }
    }

    fn lookup(&self, name: &str) -> Result<VnodeRef, FsError> {
        match &self.inner {
            Inner::Dir(d) => d
                .children
                .lock()
                .get(name)
                .cloned()
                .ok_or(FsError::NotFound),
            Inner::File(_) | Inner::Symlink(_) => Err(FsError::NotADirectory),
        }
    }

    fn symlink_target(&self) -> Option<String> {
        match &self.inner {
            Inner::Symlink(t) => Some(t.lock().clone()),
            _ => None,
        }
    }

    fn create_symlink(
        &self,
        name: &str,
        target: &str,
        uid: u32,
        gid: u32,
        mode: u32,
    ) -> Result<VnodeRef, FsError> {
        match &self.inner {
            Inner::Dir(d) => {
                let mut kids = d.children.lock();
                if kids.contains_key(name) {
                    return Err(FsError::Exists);
                }
                let node = RamNode::new(
                    Inner::Symlink(Spinlock::new(String::from(target))),
                    uid,
                    gid,
                    mode,
                );
                kids.insert(String::from(name), node.clone());
                Ok(node)
            }
            // A symlink is not a directory, whatever its target happens to be.
            // Making this depend on the target would mean a symlink to a
            // directory could be created inside one and then not traversed,
            // which is the kind of rule that is only true until someone adds
            // the case that needs it.
            Inner::File(_) | Inner::Symlink(_) => Err(FsError::NotADirectory),
        }
    }

    fn create_child(&self, name: &str, kind: NodeKind, uid: u32, gid: u32, mode: u32) -> Result<VnodeRef, FsError> {
        match &self.inner {
            Inner::Dir(d) => {
                let mut kids = d.children.lock();
                if kids.contains_key(name) {
                    return Err(FsError::Exists);
                }
                let node: VnodeRef = match kind {
                    NodeKind::Dir => RamNode::new(
                        Inner::Dir(RamDir {
                            children: Spinlock::new(BTreeMap::new()),
                        }),
                        uid,
                        gid,
                        mode,
                    ),
                    _ => RamNode::new(
                        Inner::File(RamFile {
                            data: Spinlock::new(Vec::new()),
                        }),
                        uid,
                        gid,
                        mode,
                    ),
                };
                kids.insert(String::from(name), node.clone());
                Ok(node)
            }
            Inner::File(_) | Inner::Symlink(_) => Err(FsError::NotADirectory),
        }
    }

    fn list(&self) -> Result<Vec<(String, NodeKind)>, FsError> {
        match &self.inner {
            Inner::Dir(d) => {
                let mut out: Vec<(String, NodeKind)> = d
                    .children
                    .lock()
                    .iter()
                    .map(|(k, v)| (k.clone(), v.kind()))
                    .collect();
                out.sort();
                Ok(out)
            }
            // The `f` binding is unused; taken so the match arm reads as a
            // statement about the node type rather than about its contents.
            Inner::File(_) | Inner::Symlink(_) => {
                let _ = format!(""); // keep format import used on no_std paths
                Err(FsError::NotADirectory)
            }
        }
    }

    fn remove_child(&self, name: &str) -> Result<(), FsError> {
        match &self.inner {
            Inner::Dir(d) => {
                d.children
                    .lock()
                    .remove(name)
                    .map(|_| ())
                    .ok_or(FsError::NotFound)
            }
            Inner::File(_) | Inner::Symlink(_) => Err(FsError::NotADirectory),
        }
    }

    fn size_hint(&self) -> u64 {
        match &self.inner {
            Inner::File(f) => f.data.lock().len() as u64,
            // A symlink's size is its target's length, as on every other system.
            // `ls -l` prints it, and a program checking whether a link is
            // suspiciously long reads it.
            Inner::Symlink(t) => t.lock().len() as u64,
            Inner::Dir(_) => 0,
        }
    }

    fn is_seekable(&self) -> bool {
        // A directory's "offset" is a cookie into a listing, not a byte count,
        // and this filesystem hands out no cookie. Reporting it as unseekable
        // keeps stdio from treating a directory as a repositionable file.
        //
        // A symlink is unseekable for the same underlying reason: there is no
        // content to position within. Its target is not bytes.
        matches!(self.inner, Inner::File(_))
    }

    fn file_size(&self) -> u64 {
        self.size_hint()
    }

    fn inode(&self) -> u64 {
        self.ino
    }

    fn device(&self) -> u32 {
        RAMFS_DEV
    }

    fn mtime(&self) -> Option<(i64, u32)> {
        Some(self.meta.lock().mtime)
    }
}

/// Create and return the empty ramfs root directory.
pub fn new_root() -> VnodeRef {
    RamNode::new(
        Inner::Dir(RamDir {
            children: Spinlock::new(BTreeMap::new()),
        }),
        0,
        0,
        0o755,
    )
}
