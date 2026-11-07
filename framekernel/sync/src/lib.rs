//! `origin-sync`: the Framekernel's synchronization primitives (CLAUDE.md §13: explicit
//! memory ordering; §72–§73: concurrency testing).
//!
//! A separate `no_std` crate so the primitives, which need `unsafe` (`UnsafeCell`), are
//! stress-tested on the host with real threads, while the kernel library stays
//! `forbid(unsafe_code)`. Loom models are planned (the `loom` crate needs the owner's
//! approval to download).
//!
//! [`SpinLock`]: Acquire on lock and Release on unlock make every write done under the lock
//! visible to the next holder. The lock does not disable interrupts, so **no interrupt
//! handler may take a lock that interrupted code can hold**; in the kernel, code that
//! shares a lock with an interrupt handler holds it with interrupts disabled.
#![cfg_attr(not(test), no_std)]
#![cfg_attr(
    not(test),
    deny(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::unreachable
    )
)]

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

/// Mutual exclusion by spinning.
pub struct SpinLock<T> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

// SAFETY: the lock hands out access to `value` to one holder at a time (`lock` succeeds only
// by changing `locked` from false to true, and only the guard's drop sets it back), so
// sharing the lock between threads shares `T` only by moving access, which needs `T: Send`.
unsafe impl<T: Send> Sync for SpinLock<T> {}

/// Shows only whether the lock is held: reading the value would race its holder.
impl<T> core::fmt::Debug for SpinLock<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SpinLock")
            .field("locked", &self.locked.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl<T> SpinLock<T> {
    /// An unlocked lock holding `value`.
    pub const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    /// Spins until the lock is free, then holds it until the guard is dropped.
    pub fn lock(&self) -> SpinGuard<'_, T> {
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

    /// Takes the lock if it is free, without waiting.
    pub fn try_lock(&self) -> Option<SpinGuard<'_, T>> {
        // The guard is built only on success: a guard built and dropped after a failed
        // exchange would unlock a lock that someone else holds.
        match self
            .locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        {
            Ok(_) => Some(SpinGuard { lock: self }),
            Err(_) => None,
        }
    }
}

/// Access to the value while the lock is held.
pub struct SpinGuard<'a, T> {
    lock: &'a SpinLock<T>,
}

impl<T> core::fmt::Debug for SpinGuard<'_, T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SpinGuard").finish_non_exhaustive()
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn try_lock_fails_only_while_the_lock_is_held() {
        let lock = SpinLock::new(5u32);
        {
            let mut held = lock.try_lock().unwrap();
            *held += 1;
            assert!(lock.try_lock().is_none());
            // Another thread cannot take it either.
            thread::scope(|s| {
                s.spawn(|| assert!(lock.try_lock().is_none()));
            });
        }
        assert_eq!(*lock.try_lock().unwrap(), 6);
        assert_eq!(*lock.lock(), 6);
    }

    #[test]
    fn guards_give_exclusive_access_under_contention() {
        // A non-atomic read-modify-write under the lock: any lost update shows in the total.
        let lock = Arc::new(SpinLock::new(0u64));
        let threads = 8;
        let rounds = 20_000;
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    for _ in 0..rounds {
                        let mut g = lock.lock();
                        let v = *g;
                        std::hint::black_box(v);
                        *g = v + 1;
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(*lock.lock(), threads * rounds);
    }

    #[test]
    fn writes_under_the_lock_are_visible_to_the_next_holder() {
        // Two fields updated together must always be seen together.
        let lock = Arc::new(SpinLock::new((0u64, 0u64)));
        let writer = {
            let lock = Arc::clone(&lock);
            thread::spawn(move || {
                for i in 1..=50_000u64 {
                    let mut g = lock.lock();
                    g.0 = i;
                    g.1 = i * 3;
                }
            })
        };
        let reader = {
            let lock = Arc::clone(&lock);
            thread::spawn(move || {
                for _ in 0..50_000 {
                    let g = lock.lock();
                    assert_eq!(g.1, g.0 * 3, "torn update");
                }
            })
        };
        writer.join().unwrap();
        reader.join().unwrap();
    }

    #[test]
    fn the_lock_is_free_again_after_a_guard_drops() {
        let lock = SpinLock::new(5);
        {
            let mut g = lock.lock();
            *g += 1;
        }
        assert_eq!(*lock.lock(), 6);
    }
}
