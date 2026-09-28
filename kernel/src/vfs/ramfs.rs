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
        }
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> Result<usize, FsError> {
        match &self.inner {
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
            Inner::File(_) => Err(FsError::NotADirectory),
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
            Inner::File(_) => Err(FsError::NotADirectory),
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
            Inner::File(f) => {
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
            Inner::File(_) => Err(FsError::NotADirectory),
        }
    }

    fn size_hint(&self) -> u64 {
        match &self.inner {
            Inner::File(f) => f.data.lock().len() as u64,
            Inner::Dir(_) => 0,
        }
    }

    fn is_seekable(&self) -> bool {
        // A directory's "offset" is a cookie into a listing, not a byte count,
        // and this filesystem hands out no cookie. Reporting it as unseekable
        // keeps stdio from treating a directory as a repositionable file.
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
