// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! Kernel synchronization primitives.
//!
//! These are deliberately hand-rolled so Samsara stays fully independent of
//! external crates. Interrupt management is layered on top of these as the
//! kernel grows; for now the spinlock is the single-core workhorse.

use core::arch::asm;
use core::cell::UnsafeCell;
use core::fmt;
use core::marker::PhantomData;
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

pub use core::sync::atomic::AtomicU64;
pub use core::sync::atomic::AtomicUsize;

/// Re-export of [`AtomicBool`] used by early boot code before locking exists.
pub type SpinAtomicFlag = AtomicBool;

/// Disable interrupts, returning whether they were enabled beforehand. The
/// CPU must restore [`irq_enable`] the flag with the same answer, otherwise a
/// lock taken from an already-interrupt-disabled context would spuriously
/// re-enable interrupts on release.
fn irq_disable() -> bool {
    let flags: u64;
    unsafe {
        asm!("pushfq", "pop {0}", out(reg) flags, options(nomem, nostack));
        asm!("cli", options(nostack));
    }
    flags & (1 << 9) != 0
}

/// Re-enable interrupts if (and only if) they were on when [`irq_disable`]
/// ran.
fn irq_enable(was_enabled: bool) {
    if was_enabled {
        unsafe {
            asm!("sti", options(nostack));
        }
    }
}

/// A mutual exclusion primitive built on an atomic test-and-set spin loop.
///
/// The lock deliberately masks interrupts for the duration of the critical
/// section (restoring the prior IF state on release). This single-CPU kernel
/// has no other CPU to starve, so the only way a holder can be preempted is
/// by a local IRQ; if an IRQ handler then re-enters the same lock it spins
/// forever with interrupts pinned off and the original owner never resumes.
/// Disabling interrupts inside the lock closes that deadlock class.
pub struct Spinlock<T> {
    locked: AtomicBool,
    data: UnsafeCell<T>,
}

// SAFETY: access is serialized through the atomic flag; T is only reachable
// behind a guard that holds exclusive access for the lock lifetime.
unsafe impl<T: Send> Sync for Spinlock<T> {}
unsafe impl<T: Send> Send for Spinlock<T> {}

impl<T> Spinlock<T> {
    /// Create a spinlock holding `value`.
    pub const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            data: UnsafeCell::new(value),
        }
    }

    /// Acquire the lock, spinning until it becomes available.
    pub fn lock(&self) -> SpinlockGuard<'_, T> {
        let if_was_set = irq_disable();
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        SpinlockGuard {
            lock: self,
            if_was_set,
            _not_send: PhantomData,
        }
    }

    /// Acquire the lock if it is currently free.
    pub fn try_lock(&self) -> Option<SpinlockGuard<'_, T>> {
        let if_was_set = irq_disable();
        if self
            .locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            Some(SpinlockGuard {
                lock: self,
                if_was_set,
                _not_send: PhantomData,
            })
        } else {
            irq_enable(if_was_set);
            None
        }
    }

    /// True if some CPU currently holds the lock.
    pub fn is_locked(&self) -> bool {
        self.locked.load(Ordering::Relaxed)
    }
}

/// RAII guard released when dropped.
pub struct SpinlockGuard<'a, T> {
    lock: &'a Spinlock<T>,
    if_was_set: bool,
    _not_send: PhantomData<*const ()>,
}

impl<T> core::ops::Deref for SpinlockGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: guard holds exclusive access to the inner value.
        unsafe { &*self.lock.data.get() }
    }
}

impl<T> core::ops::DerefMut for SpinlockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: guard holds exclusive access to the inner value.
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T: fmt::Debug> fmt::Debug for SpinlockGuard<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (**self).fmt(f)
    }
}

impl<T> Drop for SpinlockGuard<'_, T> {
    fn drop(&mut self) {
        // Release the flag before relifting IF: an interrupt that fires while
        // the lock is still held would re-enter the owner spinning forever.
        self.lock.locked.store(false, Ordering::Release);
        irq_enable(self.if_was_set);
    }
}

/// A write-once cell usable in static context (no lazy initialization).
pub struct OnceCell<T> {
    state: AtomicU8,
    value: UnsafeCell<core::mem::MaybeUninit<T>>,
}

const ONCE_UNSET: u8 = 0;
const ONCE_READY: u8 = 1;

unsafe impl<T: Send + Sync> Sync for OnceCell<T> {}

impl<T> OnceCell<T> {
    /// Create an empty cell.
    pub const fn new() -> Self {
        Self {
            state: AtomicU8::new(ONCE_UNSET),
            value: UnsafeCell::new(core::mem::MaybeUninit::uninit()),
        }
    }

    /// Store `value`, returning `Err(value)` if it was already initialized.
    pub fn set(&self, value: T) -> Result<(), T> {
        match self.state.compare_exchange(
            ONCE_UNSET,
            ONCE_READY,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                // SAFETY: state transition guarantees exclusive init rights.
                unsafe {
                    (*self.value.get()).write(value);
                }
                Ok(())
            }
            Err(_) => Err(value),
        }
    }

    /// Borrow the stored value, or `None` until initialization completes.
    pub fn get(&self) -> Option<&T> {
        if self.state.load(Ordering::Acquire) == ONCE_READY {
            // SAFETY: value was written exactly once before READY became visible.
            Some(unsafe { (*self.value.get()).assume_init_ref() })
        } else {
            None
        }
    }
}
