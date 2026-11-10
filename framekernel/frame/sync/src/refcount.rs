//! The count behind a shared object: how many references remain, and who was last (CLAUDE.md
//! §13, §28, §72).
//!
//! A separate type so the Loom models run it (`models.rs`): `origin_frame::objref::ObjectRef`
//! is built on it, and through that, every object a handle names and every frame shared between
//! address spaces.
//!
//! The orderings are the classic ones. Taking a reference needs none: the taker already holds
//! one. Releasing one is `Release`, so everything its holder did is published, and the last
//! release is followed by an `Acquire` fence, so that whoever destroys the object sees all of it.
//!
//! The count cannot wrap round to zero, which would free the object under its holders: past
//! [`MAX_REFERENCES`], [`RefCount::retain`] reports [`TooMany`] and its caller stops, as `Arc`
//! aborts (review housekeeping of S3-5a). This crate cannot panic (`cargo xtask check`'s
//! no-panic check), so the stop is the caller's.

use core::sync::atomic::Ordering;

use crate::prim;

/// The most references a [`RefCount`] keeps counting, as for `Arc`: half the range of the
/// count, so that it cannot wrap even if every processor took one more reference at once
/// before any of them stopped.
pub const MAX_REFERENCES: usize = isize::MAX as usize;

/// The count went past [`MAX_REFERENCES`]. Whoever took the reference must not go on: the
/// kernel's `ObjectRef` panics, and a kernel panic halts and never unwinds, so no holder can
/// later bring the count down to zero early.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TooMany;

/// How many references to one object exist.
#[derive(Debug)]
pub struct RefCount {
    count: prim::AtomicUsize,
}

impl RefCount {
    /// One reference: the creator's.
    #[cfg(not(all(test, loom)))]
    #[must_use]
    pub const fn one() -> Self {
        Self {
            count: prim::AtomicUsize::new(1),
        }
    }

    /// One reference: the creator's. Loom's atomics cannot be built in a `const`.
    #[cfg(all(test, loom))]
    pub fn one() -> Self {
        Self {
            count: prim::AtomicUsize::new(1),
        }
    }

    /// One more reference. The caller holds one already, so the object cannot go away.
    ///
    /// # Errors
    ///
    /// [`TooMany`] once the count has passed [`MAX_REFERENCES`]: the caller must stop rather
    /// than use or give back the reference (see [`TooMany`]).
    pub fn retain(&self) -> Result<(), TooMany> {
        if self.count.fetch_add(1, Ordering::Relaxed) > MAX_REFERENCES {
            return Err(TooMany);
        }
        Ok(())
    }

    /// Gives up one reference, and says whether it was the last: the caller of the last release
    /// sees everything every other holder did, and is the only one left to destroy the object.
    #[must_use]
    pub fn release(&self) -> bool {
        if self.count.fetch_sub(1, Ordering::Release) != 1 {
            return false;
        }
        prim::fence(Ordering::Acquire);
        true
    }

    /// Whether this is the only reference. Ordered like [`release`](Self::release), so that a
    /// holder that finds it is alone sees what the earlier holders did.
    #[must_use]
    pub fn is_unique(&self) -> bool {
        self.count.load(Ordering::Acquire) == 1
    }
}

// Not under Loom: its atomics exist only inside a model.
#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    #[test]
    fn only_the_last_release_says_so() {
        let count = RefCount::one();
        assert!(count.is_unique());
        count.retain().unwrap();
        assert!(!count.is_unique());
        assert!(!count.release());
        assert!(count.is_unique());
        assert!(count.release());
    }

    #[test]
    fn of_many_threads_exactly_one_is_last() {
        for _ in 0..100 {
            let count = RefCount::one();
            for _ in 1..8 {
                count.retain().unwrap();
            }
            let last: u32 = std::thread::scope(|s| {
                let handles: Vec<_> = (0..8)
                    .map(|_| s.spawn(|| u32::from(count.release())))
                    .collect();
                handles.into_iter().map(|h| h.join().unwrap()).sum()
            });
            assert_eq!(last, 1);
        }
    }

    /// A count cannot be driven round to zero: the reference after `MAX_REFERENCES` is
    /// refused, and so is every one after it.
    #[test]
    fn past_the_limit_retain_says_stop() {
        let count = RefCount {
            count: prim::AtomicUsize::new(MAX_REFERENCES),
        };
        assert_eq!(count.retain(), Ok(()), "the last one that is counted");
        assert_eq!(count.retain(), Err(TooMany));
        assert_eq!(count.retain(), Err(TooMany));
        assert!(!count.release(), "nowhere near zero");
    }
}
