//! A spin lock, the kernel's first synchronization primitive (CLAUDE.md §13: explicit memory
//! ordering; §24, §26 later add per-CPU and scheduler-aware locks).
//!
//! Acquire on lock and Release on unlock make every write done under the lock visible to
//! the next holder. The lock does not disable interrupts, so **no interrupt handler may take
//! a lock that interrupted code can hold**; today no handler takes any lock at all.

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

/// Mutual exclusion by spinning.
pub(crate) struct SpinLock<T> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

// SAFETY: the lock hands out access to `value` to one holder at a time (`lock` succeeds only
// by changing `locked` from false to true, and only the guard's drop sets it back), so
// sharing the lock between threads shares `T` only by moving access, which needs `T: Send`.
unsafe impl<T: Send> Sync for SpinLock<T> {}

impl<T> SpinLock<T> {
    /// An unlocked lock holding `value`.
    pub(crate) const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    /// Spins until the lock is free, then holds it until the guard is dropped.
    pub(crate) fn lock(&self) -> SpinGuard<'_, T> {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            while self.locked.load(Ordering::Relaxed) {
                core::hint::spin_loop();
            }
        }
        SpinGuard { lock: self }
    }
}

/// Access to the value while the lock is held.
pub(crate) struct SpinGuard<'a, T> {
    lock: &'a SpinLock<T>,
}

impl<T> Deref for SpinGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: this guard exists only while `locked` is true and was set by this guard's
        // `lock` call, so no other reference to the value exists.
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> DerefMut for SpinGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: as for `deref`; `&mut self` makes this the only access through the guard.
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T> Drop for SpinGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
    }
}
