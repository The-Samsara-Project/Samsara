// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! Per-task file descriptor registry.
//!
//! Current limitation: descriptors live in a global map keyed by task id
//! rather than inside the TCB; this moves into the task struct together
//! with per-task kernel stacks once user tasks arrive.

use super::{FdTable, FsError, Vnode, VnodeRef};
use core::sync::atomic::Ordering;
use crate::sync::Spinlock;
use alloc::collections::BTreeMap;
use alloc::boxed::Box;

static TABLES: Spinlock<BTreeMap<usize, Box<FdTable>>> = Spinlock::new(BTreeMap::new());

/// Install `node` for `task`; returns the new descriptor number.
///
/// The vnode is told a new open file description references it, so vnodes
/// that track open instances (pipes) can count this descriptor.
pub fn install(task: usize, node: VnodeRef) -> usize {
    install_with(task, node, super::O_RDWR)
}

/// Install `node` for `task` with an explicit access mode (`O_*`), otherwise
/// like [`install`].
pub fn install_with(task: usize, node: VnodeRef, access: u32) -> usize {
    install_full(task, node, access, 0)
}

/// Install `node` for `task` with an access mode and a set of status flags
/// (see `O_*`) — the pair `open` derives from its `flags` argument.
pub fn install_full(task: usize, node: VnodeRef, access: u32, status: u32) -> usize {
    let mut tables = TABLES.lock();
    let fd = tables
        .entry(task)
        .or_default()
        .install_full(node.clone(), access, status);
    node.on_open();
    fd
}

/// The vnode behind `task`'s descriptor `fd`.
///
/// This is `fstat(2)`'s first half: the caller needs the node, not a
/// synthesized answer, so that a descriptor naming a pipe reports a pipe and
/// one naming a terminal reports a terminal.
pub fn node_of(task: usize, fd: usize) -> Result<VnodeRef, FsError> {
    let tables = TABLES.lock();
    let h = tables.get(&task).ok_or(FsError::BadDescriptor)?;
    h.get(fd)
}
/// Move `task`'s descriptor `fd` to `offset`, per `lseek(2)`'s `whence`.
///
/// Returns the new absolute position. `FsError::IllegalSeek` for a stream,
/// which is what tells stdio the descriptor cannot be repositioned.
///
/// Seeking past the end of a file is *legal* and is how a program makes a
/// sparse hole to write into; only a negative resulting position is an error,
/// and that is `EINVAL` rather than `ESPIPE` so a caller can tell a bad offset
/// from a bad descriptor.
pub fn seek(task: usize, fd: usize, offset: i64, whence: u32) -> Result<u64, FsError> {
    // `lseek`'s three bases. Anything else is a caller bug.
    const SEEK_SET: u32 = 0;
    const SEEK_CUR: u32 = 1;
    const SEEK_END: u32 = 2;

    let mut tables = TABLES.lock();
    let h = tables
        .get_mut(&task)
        .and_then(|t| t.get_mut(fd))
        .ok_or(FsError::BadDescriptor)?;

    if !h.node.is_seekable() {
        return Err(FsError::IllegalSeek);
    }

    let base: i64 = match whence {
        SEEK_SET => 0,
        SEEK_CUR => h.offset.load(Ordering::Relaxed) as i64,
        SEEK_END => h.node.file_size() as i64,
        _ => return Err(FsError::Invalid),
    };
    // Compute in i128 so a hostile `offset` near i64::MAX cannot wrap through
    // a negative intermediate and slip past the bounds check.
    let target = base as i128 + offset as i128;
    if target < 0 {
        return Err(FsError::Invalid);
    }
    let target = target as u64;
    h.offset.store(target, Ordering::Relaxed);
    Ok(target)
}

/// Duplicate `task`'s descriptor `oldfd` into the lowest free slot.
///
/// The duplicate refers to the *same* open file description, so it shares the
/// file offset rather than getting one of its own -- see `FileHandle::offset`.
/// This is what `dup(2)`, `dup2(2)` and every shell redirection are built on,
/// and getting the sharing wrong is not a subtle bug: a program that writes
/// through one descriptor and reads through the other would find the read
/// position following from the write, or not following at all, depending on
/// which mistake was made.
///
/// The access mode and status flags are copied as well, so the duplicate cannot
/// be more privileged than its original.
pub fn dup(task: usize, oldfd: usize) -> Result<usize, FsError> {
    let mut tables = TABLES.lock();
    let table = tables.get_mut(&task).ok_or(FsError::BadDescriptor)?;
    let handle = table.description(oldfd)?;
    Ok(table.install_description(handle))
}

/// As [`dup`], but into a specific slot, for `dup2(2)`.
///
/// `newfd == oldfd` is *not* an error and must not close anything: POSIX
/// requires dup2 to succeed and leave the descriptor alone in that case.
/// Treating it as a no-op-by-error would make a program that checks the return
/// value close a descriptor it meant to keep.
pub fn dup2(task: usize, oldfd: usize, newfd: usize) -> Result<usize, FsError> {
    if oldfd == newfd {
        // Still a validity check, so dup2 on a bad descriptor is EBADF rather
        // than a silent success.
        let mut tables = TABLES.lock();
        let table = tables.get(&task).ok_or(FsError::BadDescriptor)?;
        table.description(oldfd)?;
        return Ok(newfd);
    }
    let mut tables = TABLES.lock();
    let table = tables.get_mut(&task).ok_or(FsError::BadDescriptor)?;
    let handle = table.description(oldfd)?;
    // Replaces whatever was in the slot, and closes it. The displaced
    // descriptor's vnode is told the reference is gone, which is what lets a
    // pipe see its last reader disappear.
    let displaced = table.replace_at(newfd, handle);
    if let Some(old) = displaced {
        old.node.on_close();
    }
    Ok(newfd)
}

/// Current status flags of `task`'s descriptor (`F_GETFL`).
pub fn fd_status(task: usize, fd: usize) -> Result<u32, FsError> {
    let mut tables = TABLES.lock();
    tables
        .get_mut(&task)
        .and_then(|t| t.get_mut(fd))
        .map(|h| h.status)
        .ok_or(FsError::BadDescriptor)
}

/// Replace the status flags of `task`'s descriptor (`F_SETFL`). Only the bits
/// in `F_SETFL_MASK` are honored; the access mode is fixed at `open` time and
/// cannot be changed this way.
pub fn set_fd_status(task: usize, fd: usize, status: u32) -> Result<(), FsError> {
    let mut tables = TABLES.lock();
    let h = tables
        .get_mut(&task)
        .and_then(|t| t.get_mut(fd))
        .ok_or(FsError::BadDescriptor)?;
    h.status = status & super::F_SETFL_MASK;
    Ok(())
}

/// The three values `F_GETFL` reports: access mode plus status flags.
pub fn fd_flags(task: usize, fd: usize) -> Result<(u32, u32), FsError> {
    let mut tables = TABLES.lock();
    let h = tables
        .get_mut(&task)
        .and_then(|t| t.get_mut(fd))
        .ok_or(FsError::BadDescriptor)?;
    Ok((h.access, h.status))
}

/// Look up the vnode behind `task`'s descriptor.
pub fn get(task: usize, fd: usize) -> Result<VnodeRef, FsError> {
    TABLES
        .lock()
        .get(&task)
        .and_then(|t| t.get(fd).ok())
        .ok_or(FsError::BadDescriptor)
}

/// Look up the vnode and its open access mode for `task`'s descriptor.
/// The `poll` syscall uses this to mask readiness by the open mode.
pub fn get_with_access(task: usize, fd: usize) -> Result<(VnodeRef, u32), FsError> {
    TABLES
        .lock()
        .get(&task)
        .and_then(|t| t.get_with_access(fd).ok())
        .ok_or(FsError::BadDescriptor)
}

/// Close `task`'s descriptor, notifying the vnode that one fewer open file
/// description references it (pipes use this for EOF/`EPIPE` wakeups).
pub fn close(task: usize, fd: usize) -> Result<VnodeRef, FsError> {
    let node = match TABLES.lock().get_mut(&task) {
        Some(t) => t.close(fd),
        None => Err(FsError::BadDescriptor),
    }?;
    node.on_close();
    Ok(node)
}

/// Offset-aware read through `task`'s descriptor.
///
/// The table lock is released before the vnode is invoked: a blocking vnode
/// (a pipe) parks this thread while waiting for data, and a writer on another
/// task must be able to reach the table (and the pipe) to make progress.
pub fn read(task: usize, fd: usize, buf: &mut [u8]) -> Result<usize, FsError> {
    let (node, offset, access, status) = {
        let mut tables = TABLES.lock();
        let h = tables
            .get_mut(&task)
            .and_then(|t| t.get_mut(fd))
            .ok_or(FsError::BadDescriptor)?;
        (h.node.clone(), h.offset.load(Ordering::Relaxed), h.access, h.status)
    };
    if access == super::O_WRONLY {
        return Err(FsError::BadDescriptor);
    }
    if buf.is_empty() {
        return Ok(0);
    }
    // A terminal is the one node where "no input yet" is a normal state rather
    // than end-of-file. A libc's stdio calls `read` in a loop and treats 0 as
    // EOF, so returning 0 here would make every interactive program exit the
    // instant it started. Wait for input instead, unless the descriptor was
    // opened `O_NONBLOCK`.
    if node.is_terminal() {
        wait_readable(task, &node, status)?;
    }
    let n = node.read_at(offset, buf)?;
    {
        let mut tables = TABLES.lock();
        if let Some(h) = tables.get_mut(&task).and_then(|t| t.get_mut(fd)) {
            h.offset.store(offset + n as u64, Ordering::Relaxed);
        }
    }
    Ok(n)
}

/// Block until `node` reports readable, or return `EAGAIN` when the descriptor
/// is non-blocking.
///
/// The loop re-checks for a deliverable signal before each park so `^C` and a
/// `kill` interrupt a pending read rather than waiting out the whole wait.
fn wait_readable(task: usize, node: &VnodeRef, status: u32) -> Result<(), FsError> {
    loop {
        if node.readable_now() {
            return Ok(());
        }
        if status & super::O_NONBLOCK != 0 {
            return Err(FsError::WouldBlock);
        }
        if crate::sig::deliverable_now() {
            if crate::sig::should_restart() {
                crate::sig::defer_pending();
            } else {
                return Err(FsError::Interrupted);
            }
        }
        // `poll_park` re-checks readiness under the device's own lock and
        // registers the task, so input arriving between the probe above and
        // this call cannot be lost. A `true` means it is already readable.
        if node.poll_park(task, driver_common::POLLIN) {
            continue;
        }
        // Nothing registered and nothing ready would spin forever, so a device
        // that cannot be woken yields the CPU and retries on the next tick.
        //
        // The boot context has no task to park and no one to wake it, so a
        // kernel-side read must not wait at all: there is no scheduler tick
        // coming to release it. Report "would block" instead of hanging.
        if crate::task::sched::current_task_id().is_none() {
            return Err(FsError::WouldBlock);
        }
        crate::task::sched::block_until(None);
    }
}

/// Offset-aware write through `task`'s descriptor.
///
/// Like [`read`], the table lock is dropped while the vnode runs so a
/// blocking pipe write can park without jamming the writer's own progress.
pub fn write(task: usize, fd: usize, buf: &[u8]) -> Result<usize, FsError> {
    let (node, offset, access) = {
        let mut tables = TABLES.lock();
        let h = tables
            .get_mut(&task)
            .and_then(|t| t.get_mut(fd))
            .ok_or(FsError::BadDescriptor)?;
        (h.node.clone(), h.offset.load(Ordering::Relaxed), h.access)
    };
    if access == super::O_RDONLY {
        return Err(FsError::BadDescriptor);
    }
    let n = node.write_at(offset, buf)?;
    {
        let mut tables = TABLES.lock();
        if let Some(h) = tables.get_mut(&task).and_then(|t| t.get_mut(fd)) {
            h.offset.store(offset + n as u64, Ordering::Relaxed);
        }
    }
    Ok(n)
}

/// Copy the whole descriptor table (with open file offsets) from `src` to
/// `dst`, creating `dst`'s table if it does not exist yet. Used by `fork` so
/// the child starts with a snapshot of the parent's open files.
pub fn copy_table(src: usize, dst: usize) {
    let mut tables = TABLES.lock();
    let snapshot = match tables.get(&src) {
        Some(f) => f.fork(),
        None => return,
    };
    let slot = tables.entry(dst).or_default();
    **slot = snapshot;
    // Every duplicated open file description is one more reference to its
    // vnode; tally each so instance counters (pipes) stay correct across
    // `fork`.
    for h in slot.fds.iter().flatten() {
        h.node.on_open();
    }
}

/// Remove `task`'s whole descriptor table, notifying every vnode that its
/// open file descriptions have gone away.
///
/// Called from process teardown so descriptors that a process failed to close
/// are released immediately — a dying pipe writer wakes blocked readers with
/// EOF, a dying pipe reader wakes blocked writers with `EPIPE`, and otherwise
/// unreferenced vnodes (and their backing data) drop out of the kernel heap.
pub fn close_all(task: usize) {
    let table = {
        let mut tables = TABLES.lock();
        tables.remove(&task)
    };
    if let Some(t) = table {
        for h in t.fds.iter().flatten() {
            h.node.on_close();
        }
    }
}
