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

use core::sync::atomic::Ordering;

use crate::prim;

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
    pub fn retain(&self) {
        self.count.fetch_add(1, Ordering::Relaxed);
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
        count.retain();
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
                count.retain();
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
}
