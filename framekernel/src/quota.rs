//! Count quotas (ADR-0005 P11), tested on the host: how many objects of a kind a process may
//! hold at once, set by its creator.
//!
//! - **A count** is charged before the thing it counts is made and refunded once that thing is
//!   gone. A charge at the quota is `limit` and takes nothing. Charges from several processors
//!   never pass the quota between them: the check and the increment are one atomic step.
//! - **A quota of 0 allows nothing.**
//! - **Channels** are the one kind counted today: a channel is charged to the process that
//!   created it, once, until the channel is destroyed.
#![forbid(unsafe_code)]

use core::sync::atomic::{AtomicUsize, Ordering};

use origin_abi::Error;

/// How many of one kind are held, against the most that may be.
#[derive(Debug)]
pub struct Count {
    held: AtomicUsize,
    max: usize,
}

impl Count {
    /// Nothing held, and at most `max` at once.
    pub const fn new(max: usize) -> Self {
        Self {
            held: AtomicUsize::new(0),
            max,
        }
    }

    /// One more, or [`Error::Limit`] at the quota, with nothing taken.
    pub fn charge(&self) -> Result<(), Error> {
        self.held
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < self.max).then_some(n + 1)
            })
            .map(|_| ())
            .map_err(|_| Error::Limit)
    }

    /// One that was charged here is gone.
    pub fn refund(&self) {
        self.held.fetch_sub(1, Ordering::AcqRel);
    }

    /// How many are charged now.
    pub fn held(&self) -> usize {
        self.held.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Charges `count` until it refuses, and returns how many it took.
    fn fill(count: &Count) -> usize {
        let mut taken = 0;
        while count.charge().is_ok() {
            taken += 1;
        }
        taken
    }

    #[test]
    fn a_count_stops_at_its_quota_and_a_refused_charge_takes_nothing() {
        let count = Count::new(2);
        assert_eq!(count.charge(), Ok(()));
        assert_eq!(count.charge(), Ok(()));
        assert_eq!(count.held(), 2);
        assert_eq!(count.charge(), Err(Error::Limit));
        assert_eq!(count.charge(), Err(Error::Limit));
        assert_eq!(count.held(), 2);
    }

    #[test]
    fn a_refund_makes_room_for_exactly_one_more() {
        let count = Count::new(3);
        assert_eq!(fill(&count), 3);
        count.refund();
        assert_eq!(count.held(), 2);
        assert_eq!(count.charge(), Ok(()));
        assert_eq!(count.charge(), Err(Error::Limit));
        for _ in 0..3 {
            count.refund();
        }
        assert_eq!(count.held(), 0);
        assert_eq!(fill(&count), 3);
    }

    #[test]
    fn a_quota_of_zero_allows_nothing() {
        let count = Count::new(0);
        assert_eq!(count.charge(), Err(Error::Limit));
        assert_eq!(count.held(), 0);
    }

    #[test]
    fn the_largest_quota_does_not_overflow() {
        let count = Count::new(usize::MAX);
        assert_eq!(count.charge(), Ok(()));
        assert_eq!(count.held(), 1);
    }

    /// Several threads charge and refund one count at once. None of them may ever see more
    /// held than the quota, and what they hold at the end is what they kept.
    #[test]
    fn charges_from_several_threads_never_pass_the_quota() {
        const THREADS: usize = 8;
        const ROUNDS: usize = 20_000;
        const QUOTA: usize = 3;

        let count = Count::new(QUOTA);
        std::thread::scope(|scope| {
            for _ in 0..THREADS {
                scope.spawn(|| {
                    for _ in 0..ROUNDS {
                        if count.charge().is_ok() {
                            assert!(count.held() <= QUOTA);
                            count.refund();
                        }
                    }
                });
            }
        });
        assert_eq!(count.held(), 0);

        // With nothing refunded, the threads get the quota between them and no more.
        let kept = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..THREADS {
                scope.spawn(|| {
                    for _ in 0..ROUNDS {
                        if count.charge().is_ok() {
                            kept.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });
            }
        });
        assert_eq!(kept.load(Ordering::Relaxed), QUOTA);
        assert_eq!(count.held(), QUOTA);
    }
}
