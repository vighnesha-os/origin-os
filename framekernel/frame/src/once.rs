//! A value that is set once and then only read, from any processor, without a lock.
//!
//! Interrupt and exception paths read these: they must never wait on a lock that a processor
//! it interrupted may hold. So a reader sees either the finished value or nothing at all.

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicU8, Ordering};

const EMPTY: u8 = 0;
const WRITING: u8 = 1;
const READY: u8 = 2;

/// A cell written at most once. Readers that find it unset see `None`; once [`set`] has
/// returned, every reader sees the value, on every processor.
///
/// [`set`]: SetOnce::set
#[derive(Debug)]
pub struct SetOnce<T> {
    state: AtomicU8,
    value: UnsafeCell<MaybeUninit<T>>,
}

// SAFETY: the value is written by the one caller of `set` that wins the `EMPTY` to `WRITING`
// transition, before `READY` is published, and never again; after that it is only read, through
// shared references, which needs `T: Sync` (and `Send`, because whichever processor reads it may
// not be the one that wrote it).
unsafe impl<T: Send + Sync> Sync for SetOnce<T> {}

impl<T> SetOnce<T> {
    /// An empty cell.
    pub const fn new() -> Self {
        Self {
            state: AtomicU8::new(EMPTY),
            value: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    /// Stores `value`, or gives it back if the cell was already set (or is being set).
    pub fn set(&self, value: T) -> Result<(), T> {
        if self
            .state
            .compare_exchange(EMPTY, WRITING, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return Err(value);
        }
        // SAFETY: this caller alone won the EMPTY to WRITING transition, so nothing else writes
        // `value`, and no reader looks at it before READY is published below.
        unsafe { (*self.value.get()).write(value) };
        self.state.store(READY, Ordering::Release);
        Ok(())
    }

    /// The value, if it has been set.
    pub fn get(&self) -> Option<&T> {
        if self.state.load(Ordering::Acquire) != READY {
            return None;
        }
        // SAFETY: READY was published after the value was written and nothing writes it again,
        // so this shared reference aliases no mutation; the Acquire load above orders the
        // value's initialization before this read.
        let slot = unsafe { &*self.value.get() };
        // SAFETY: as above: READY means `set` finished writing a valid `T`.
        Some(unsafe { slot.assume_init_ref() })
    }
}

impl<T> Default for SetOnce<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_until_set_and_set_only_once() {
        let cell = SetOnce::new();
        assert_eq!(cell.get(), None);
        assert_eq!(cell.set(1), Ok(()));
        assert_eq!(cell.get(), Some(&1));
        assert_eq!(cell.set(2), Err(2));
        assert_eq!(cell.get(), Some(&1));
    }

    #[test]
    fn set_from_many_threads_stores_exactly_one_value() {
        let cell = SetOnce::new();
        let wins: u32 = std::thread::scope(|s| {
            let handles: Vec<_> = (0..8u32)
                .map(|i| {
                    s.spawn({
                        let cell = &cell;
                        move || cell.set(i).map_or(0, |()| 1)
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).sum()
        });
        assert_eq!(wins, 1);
        let value = *cell.get().expect("someone set it");
        assert!(value < 8);
    }
}
