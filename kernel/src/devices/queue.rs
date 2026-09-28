// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! Bounded byte queues with scheduler-integrated blocking reads.
//!
//! IRQ handlers push bytes and wake parked readers; reader threads pop and
//! block (without spinning) when the queue is empty.

use crate::sync::Spinlock;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

struct Inner {
    data: Vec<u8>,
    waiters: Vec<crate::task::TaskId>,
}

/// A single-producer-ish byte queue shared between IRQ and task contexts.
pub struct ByteQueue {
    inner: Spinlock<Inner>,
    overflow: AtomicBool,
}

const MAX_PENDING: usize = 4096;

impl ByteQueue {
    /// Create an empty queue.
    pub const fn new() -> Self {
        Self {
            inner: Spinlock::new(Inner {
                data: Vec::new(),
                waiters: Vec::new(),
            }),
            overflow: AtomicBool::new(false),
        }
    }

    /// Push bytes from (usually) interrupt context; wakes all readers.
    pub fn push(&self, bytes: &[u8]) {
        let wake = {
            let mut q = self.inner.lock();
            for &b in bytes {
                if q.data.len() >= MAX_PENDING {
                    self.overflow.store(true, Ordering::Relaxed);
                    break;
                }
                q.data.push(b);
            }
            !q.waiters.is_empty()
        };
        if wake {
            let ids = {
                let mut q = self.inner.lock();
                core::mem::take(&mut q.waiters)
            };
            for id in ids {
                crate::task::sched::wake(id);
            }
        }
        crate::task::sched::request_resched();
    }

    /// Non-blocking availability probe.
    pub fn has_data(&self) -> bool {
        !self.inner.lock().data.is_empty()
    }

    /// True if bytes were ever dropped due to a full queue.
    #[allow(dead_code)]
    pub fn overflowed(&self) -> bool {
        self.overflow.load(Ordering::Relaxed)
    }
}

impl ByteQueue {
    /// Non-blocking read: returns bytes available right now (possibly 0).
    pub fn try_read(&self, buf: &mut [u8]) -> usize {
        let mut q = self.inner.lock();
        if q.data.is_empty() || buf.is_empty() {
            return 0;
        }
        let n = buf.len().min(q.data.len());
        let drained: Vec<u8> = q.data.drain(..n).collect();
        buf[..n].copy_from_slice(&drained);
        n
    }

    /// Register the current task to be woken when data arrives.
    pub fn park_reader(&self) {
        let mut q = self.inner.lock();
        if let Some(id) = crate::task::sched::current_task_id() {
            q.waiters.push(id);
        }
    }

    /// Register the current task for a readiness (poll) wakeup. Returns `true`
    /// if data is already queued, otherwise registers and returns `false`.
    /// Done under the queue lock so a push between the availability check and
    /// the registration cannot be missed ([`push`] broadcasts to every waiter).
    pub fn poll_park(&self) -> bool {
        let mut q = self.inner.lock();
        if !q.data.is_empty() {
            return true;
        }
        if let Some(id) = crate::task::sched::current_task_id() {
            if !q.waiters.contains(&id) {
                q.waiters.push(id);
            }
        }
        false
    }

    /// Undo a [`poll_park`] registration; idempotent.
    pub fn poll_cancel(&self) {
        let mut q = self.inner.lock();
        if let Some(id) = crate::task::sched::current_task_id() {
            q.waiters.retain(|t| *t != id);
        }
    }
}
