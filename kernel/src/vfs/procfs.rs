// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! procfs: dynamically generated kernel information files under `/proc`.
//!
//! Every file is a content closure evaluated at read time, so statistics are
//! always live.

use super::{FsError, NodeKind, Vnode, VnodeRef};
use crate::sync::Spinlock;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

/// Content generator invoked whenever a proc file is read.
pub type Generator = Box<dyn Fn() -> Vec<u8> + Send + Sync>;

struct ProcFile {
    generate: Generator,
}

struct ProcDir {
    files: Spinlock<BTreeMap<String, Arc<ProcFile>>>,
    dirs: BTreeMap<String, VnodeRef>,
}

impl Vnode for ProcFile {
    fn kind(&self) -> NodeKind {
        NodeKind::File
    }

    fn mode(&self) -> u32 {
        0o444
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        let content = (self.generate)();
        let start = (offset as usize).min(content.len());
        let end = (start + buf.len()).min(content.len());
        let n = end - start;
        buf[..n].copy_from_slice(&content[start..end]);
        Ok(n)
    }

    fn size_hint(&self) -> u64 {
        (self.generate)().len() as u64
    }
}

impl Vnode for ProcDir {
    fn kind(&self) -> NodeKind {
        NodeKind::Dir
    }

    fn mode(&self) -> u32 {
        0o555
    }

    fn lookup(&self, name: &str) -> Result<VnodeRef, FsError> {
        if let Some(f) = self.files.lock().get(name) {
            return Ok(f.clone());
        }
        if let Some(d) = self.dirs.get(name) {
            return Ok(d.clone());
        }
        Err(FsError::NotFound)
    }

    fn list(&self) -> Result<Vec<(String, NodeKind)>, FsError> {
        let mut out: Vec<(String, NodeKind)> = Vec::new();
        for k in self.files.lock().keys() {
            out.push((k.clone(), NodeKind::File));
        }
        for k in self.dirs.keys() {
            out.push((k.clone(), NodeKind::Dir));
        }
        out.sort();
        Ok(out)
    }
}

static PROC_DIR: Spinlock<Option<Arc<ProcDir>>> = Spinlock::new(None);

/// Bring up procfs and return its root vnode.
pub fn new_root() -> VnodeRef {
    let dir = Arc::new(ProcDir {
        files: Spinlock::new(BTreeMap::new()),
        dirs: BTreeMap::new(),
    });
    *PROC_DIR.lock() = Some(dir.clone());
    dir
}

/// Add a generated file to the procfs root.
pub fn add_file(name: &str, gen: Generator) {
    let guard = PROC_DIR.lock();
    match guard.as_ref() {
        Some(d) => {
            d.files
                .lock()
                .insert(String::from(name), Arc::new(ProcFile { generate: gen }));
        }
        None => crate::log::kwarn!("procfs: not mounted"),
    }
}

// ---------------------------------------------------------------------------
// Standard kernel-provided files
// ---------------------------------------------------------------------------

/// Register the default set of informational files.
pub fn register_defaults() {
    add_file("version", Box::new(|| {
        crate::abi::KERNEL_VERSION_STRING.to_vec()
    }));

    add_file("uptime", Box::new(|| {
        alloc::format!(
            "{}.00 s ({} ticks @ {} Hz)\n",
            crate::time::millis() / 1000,
            crate::time::ticks(),
            crate::time::TIMER_HZ
        )
        .into_bytes()
    }));

    add_file("meminfo", Box::new(|| {
        let (total, used) = crate::memory::pmm::stats();
        let frames_free = total - used;
        alloc::format!(
            "total_kib {}\nused_kib {}\nfree_kib {}\nheap_mib {}\n",
            total * 4,
            used * 4,
            frames_free * 4,
            crate::memory::HEAP_SIZE >> 10 >> 0
        )
        .into_bytes()
    }));

    add_file("tasks", Box::new(|| {
        let mut out = alloc::string::String::from("id name state level\n");
        for (id, name, state, level) in crate::task::sched::snapshot() {
            out.push_str(&alloc::format!("{id} {name} {state} {level}\n"));
        }
        out.into_bytes()
    }));

    add_file("devices", Box::new(|| {
        // Served through the devfs listing.
        match crate::vfs::resolve("/dev").and_then(|d| d.list()) {
            Ok(entries) => {
                let mut out = alloc::string::String::from("path type driver\n");
                for (name, kind) in entries {
                    out.push_str(&alloc::format!("/dev/{name} {kind} -\n"));
                }
                out.into_bytes()
            }
            Err(_) => b"devfs unavailable\n".to_vec(),
        }
    }));
}
