// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! Pipes: kernel-mediated byte streams between processes.
//!
//! A `pipe()` syscall creates two vnodes — a read end and a write end —
//! sharing one bounded ring buffer. Both ends are ordinary descriptors, so
//! everything the VFS already provides (per-task fd tables, fork duplication
//! via `fdtab::copy_table`) applies unchanged.
//!
//! Semantics follow the classic pipe contract:
//!
//! * a read on an empty buffer **blocks** until data arrives, or returns
//!   `0` (EOF) once every writer end has been closed;
//! * a write **blocks** until room appears; writes no larger than
//!   [`PIPE_BUF`] are atomic with respect to other writers;
//! * a write whose last reader end has closed fails with `EPIPE`;
//! * operating on the wrong end of a pipe fails with `EBADF`.
//!
//! Reads and writes park the calling task via [`crate::task::sched::block_current`]
//! and are resumed with [`crate::task::sched::wake`] — the same machinery the
//! IPC layer uses, so a blocked end releases the CPU rather than spinning.
//! All pipe state lives behind one [`Spinlock`], and no other lock is ever
//! held across a `block_current`, so a parked reader can never jam a writer.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use crate::sync::Spinlock;
use crate::task::TaskId;
use driver_common::{FsError, NodeKind, Vnode, VnodeRef};

/// Maximum size below which a single write is atomic (cannot interleave with
/// another writer). Larger writes may be broken up.
pub const PIPE_BUF: usize = 4096;
/// Ring buffer capacity in bytes.
pub const PIPE_CAPACITY: usize = 64 * 1024;

/// Mask for the ring-index arithmetic; `PIPE_CAPACITY` must stay a power of
/// two so wrapping is a cheap `&` rather than a divide.
const RING_MASK: usize = PIPE_CAPACITY - 1;

/// Mutable per-pipe state, guarded by [`PipeCore::state`].
struct Inner {
    /// Ring storage; only the `[head, head + len)` region is live.
    buf: Vec<u8>,
    /// Index of the next byte to read.
    head: usize,
    /// Number of bytes currently buffered.
    len: usize,
    /// Number of open read-end file descriptions.
    readers: usize,
    /// Number of open write-end file descriptions.
    writers: usize,
    /// Tasks blocked in `read` waiting for data (or EOF).
    read_waiters: VecDeque<TaskId>,
    /// Tasks blocked in `write` waiting for space (or `EPIPE`).
    write_waiters: VecDeque<TaskId>,
    /// Tasks in `poll` waiting for read readiness (`POLLIN` / EOF).
    read_pollers: VecDeque<TaskId>,
    /// Tasks in `poll` waiting for write readiness (`POLLOUT` / `EPIPE`).
    write_pollers: VecDeque<TaskId>,
}

impl Inner {
    /// Bytes of free space left in the buffer.
    fn space(&self) -> usize {
        PIPE_CAPACITY - self.len
    }

    /// Copy `bytes` into the ring at the tail; caller must have room.
    fn push(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.buf[(self.head + self.len) & RING_MASK] = b;
            self.len += 1;
        }
    }

    /// Copy up to `out.len()` buffered bytes out of the ring (FIFO).
    fn pop_into(&mut self, out: &mut [u8]) -> usize {
        let n = self.len.min(out.len());
        for i in 0..n {
            out[i] = self.buf[(self.head + i) & RING_MASK];
        }
        self.head = (self.head + n) & RING_MASK;
        self.len -= n;
        n
    }
}

/// State shared between the two ends of one pipe.
struct PipeCore {
    state: Spinlock<Inner>,
}

/// Which side of the pipe a vnode represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EndRole {
    /// The readable end.
    Reader,
    /// The writable end.
    Writer,
}

/// One end of a pipe, usable as a VFS vnode.
struct PipeEnd {
    core: Arc<PipeCore>,
    role: EndRole,
}

/// Create a pipe; returns the `(read_end, write_end)` vnode pair sharing one
/// ring buffer. Reader and writer counts start at zero — each descriptor
/// installed signals the appropriate end via [`Vnode::on_open`], and each
/// [`Vnode::on_close`] releases it again.
pub fn create() -> (VnodeRef, VnodeRef) {
    let core = Arc::new(PipeCore {
        state: Spinlock::new(Inner {
            buf: vec![0; PIPE_CAPACITY],
            head: 0,
            len: 0,
            readers: 0,
            writers: 0,
            read_waiters: VecDeque::new(),
            write_waiters: VecDeque::new(),
            read_pollers: VecDeque::new(),
            write_pollers: VecDeque::new(),
        }),
    });
    let read: VnodeRef = Arc::new(PipeEnd {
        core: core.clone(),
        role: EndRole::Reader,
    });
    let write: VnodeRef = Arc::new(PipeEnd {
        core,
        role: EndRole::Writer,
    });
    (read, write)
}

/// Drain the waiters that must wake for one pipe state transition into a
/// wake batch: a data push wakes one blocking reader plus every poller waiting
/// on `POLLIN`; freeing space wakes one blocked writer plus every poller
/// waiting on `POLLOUT`. Batches are woken after the state lock is dropped.
fn wake_batch(one: &mut VecDeque<TaskId>, all: &mut VecDeque<TaskId>) -> Vec<TaskId> {
    let mut batch = Vec::new();
    if let Some(w) = one.pop_front() {
        batch.push(w);
    }
    batch.extend(all.drain(..));
    batch
}

impl Vnode for PipeEnd {
    fn kind(&self) -> NodeKind {
        NodeKind::Pipe
    }

    /// Blocking stream read. Returns buffered bytes when available, parks the
    /// caller while the buffer is empty and a writer still exists, and returns
    /// `Ok(0)` — EOF — once every writer end is closed.
    fn read_at(&self, _offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        if self.role != EndRole::Reader {
            return Err(FsError::BadDescriptor);
        }
        loop {
            // Decide under the lock, park outside it (mirrors the IPC
            // recv_wait pattern: a wake re-checks the state that caused it).
            let (n, woke, parked) = {
                let mut g = self.core.state.lock();
                if g.len > 0 {
                    let n = g.pop_into(buf);
                    // Room freed: wake a blocked writer and every write poller.
                    let woke = {
                        let Inner {
                            write_waiters,
                            write_pollers,
                            ..
                        } = &mut *g;
                        wake_batch(write_waiters, write_pollers)
                    };
                    (n, woke, false)
                } else if g.writers == 0 {
                    // All writer ends gone and nothing left to drain: EOF.
                    return Ok(0);
                } else {
                    if let Some(cur) = crate::task::sched::current_task_id() {
                        g.read_waiters.push_back(cur);
                    }
                    (0, Vec::new(), true)
                }
            };
            for w in woke {
                crate::task::sched::wake(w);
            }
            if parked {
                // A deliverable catchable/fatal signal interrupts the block
                // (`EINTR`). Drop our waiter slot so a later writer cannot
                // wake a dead queue entry.
                if crate::sig::deliverable_now() {
                    {
                        let mut g = self.core.state.lock();
                        if let Some(cur) = crate::task::sched::current_task_id() {
                            g.read_waiters.retain(|t| *t != cur);
                        }
                    }
                    return Err(FsError::Interrupted);
                }
                crate::task::sched::block_current();
            } else {
                return Ok(n);
            }
        }
    }

    /// Blocking stream write. Writes no larger than [`PIPE_BUF`] wait until
    /// the whole thing fits (atomicity); larger writes send whatever room
    /// exists now. Fails with `EPIPE` when no reader end is open.
    fn write_at(&self, _offset: u64, buf: &[u8]) -> Result<usize, FsError> {
        if self.role != EndRole::Writer {
            return Err(FsError::BadDescriptor);
        }
        let atomic = buf.len() <= PIPE_BUF;
        loop {
            let (written, woke, parked) = {
                let mut g = self.core.state.lock();
                if g.readers == 0 {
                    return Err(FsError::BrokenPipe);
                }
                let space = g.space();
                if space == 0 || (atomic && space < buf.len()) {
                    // Not enough room for a whole atomic write: park for space.
                    if let Some(cur) = crate::task::sched::current_task_id() {
                        g.write_waiters.push_back(cur);
                    }
                    (0, Vec::new(), true)
                } else {
                    let take = if atomic {
                        buf.len()
                    } else {
                        space.min(buf.len())
                    };
                    g.push(&buf[..take]);
                    // Data arrived: wake a blocked reader and every read poller.
                    let woke = {
                        let Inner {
                            read_waiters,
                            read_pollers,
                            ..
                        } = &mut *g;
                        wake_batch(read_waiters, read_pollers)
                    };
                    (take, woke, false)
                }
            };
            for w in woke {
                crate::task::sched::wake(w);
            }
            if parked {
                if crate::sig::deliverable_now() {
                    {
                        let mut g = self.core.state.lock();
                        if let Some(cur) = crate::task::sched::current_task_id() {
                            g.write_waiters.retain(|t| *t != cur);
                        }
                    }
                    return Err(FsError::Interrupted);
                }
                crate::task::sched::block_current();
            } else {
                return Ok(written);
            }
        }
    }

    /// Readiness snapshot for poll: the read end is readable with data
    /// buffered, or at EOF once every writer is gone; the write end is
    /// writable while a reader exists and the buffer has room, and reports
    /// `POLLERR` (the pre-image of an `EPIPE` write) once no reader exists.
    fn poll_events(&self, _interest: u16) -> u16 {
        let g = self.core.state.lock();
        match self.role {
            EndRole::Reader => {
                let mut r = 0;
                if g.len > 0 {
                    r |= driver_common::POLLIN;
                }
                if g.writers == 0 {
                    // EOF: a read returns `0` immediately, so this is POLLIN
                    // plus a hangup notification.
                    r |= driver_common::POLLIN | driver_common::POLLHUP;
                }
                r
            }
            EndRole::Writer => {
                let mut r = 0;
                if g.readers == 0 {
                    // Writing would fail with EPIPE immediately.
                    r |= driver_common::POLLERR;
                } else if g.space() > 0 {
                    r |= driver_common::POLLOUT;
                }
                r
            }
        }
    }

    /// Register `task` for a readiness wakeup, atomically with the readiness
    /// check (under the pipe lock), so a writer/reader/close between the two
    /// cannot be missed. Returns `true` when already ready.
    fn poll_park(&self, task: usize) -> bool {
        let cur = crate::task::TaskId(task);
        let mut g = self.core.state.lock();
        match self.role {
            EndRole::Reader => {
                if g.len > 0 || g.writers == 0 {
                    return true;
                }
                if !g.read_pollers.contains(&cur) {
                    g.read_pollers.push_back(cur);
                }
                false
            }
            EndRole::Writer => {
                if g.readers == 0 || g.space() > 0 {
                    return true;
                }
                if !g.write_pollers.contains(&cur) {
                    g.write_pollers.push_back(cur);
                }
                false
            }
        }
    }

    /// Remove `task` from both poller lists; idempotent.
    fn poll_cancel(&self, task: usize) {
        let cur = crate::task::TaskId(task);
        let mut g = self.core.state.lock();
        g.read_pollers.retain(|t| *t != cur);
        g.write_pollers.retain(|t| *t != cur);
    }

    /// One more open file description references this end.
    fn on_open(&self) {
        let mut g = self.core.state.lock();
        match self.role {
            EndRole::Reader => g.readers += 1,
            EndRole::Writer => g.writers += 1,
        }
    }

    /// One open file description referencing this end was released. When the
    /// last writer closes, parked readers are woken so they observe EOF; when
    /// the last reader closes, parked writers are woken so they observe
    /// `EPIPE`.
    fn on_close(&self) {
        let to_wake: Vec<TaskId> = {
            let mut g = self.core.state.lock();
            match self.role {
                EndRole::Reader => {
                    g.readers = g.readers.saturating_sub(1);
                    if g.readers == 0 {
                        // Last reader gone: writers observe EPIPE, write pollers
                        // observe the same error readiness.
                        let mut waiters: Vec<TaskId> = g.write_waiters.drain(..).collect();
                        waiters.extend(g.write_pollers.drain(..));
                        waiters
                    } else {
                        Vec::new()
                    }
                }
                EndRole::Writer => {
                    g.writers = g.writers.saturating_sub(1);
                    if g.writers == 0 {
                        // Last writer gone: readers observe EOF, read pollers
                        // observe the same EOF readiness.
                        let mut waiters: Vec<TaskId> = g.read_waiters.drain(..).collect();
                        waiters.extend(g.read_pollers.drain(..));
                        waiters
                    } else {
                        Vec::new()
                    }
                }
            }
        };
        for t in to_wake {
            crate::task::sched::wake(t);
        }
    }
}
