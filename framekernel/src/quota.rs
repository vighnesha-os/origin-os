//! Count quotas (ADR-0005 P11), tested on the host: how many objects of a kind a process may
//! hold at once, set by its creator.
//!
//! - **A count** is charged before the thing it counts is made and refunded once that thing is
//!   gone. A charge at the quota is `limit` and takes nothing. Charges from several processors
//!   never pass the quota between them: the check and the increment are one atomic step.
//! - **A quota of 0 allows nothing.** A creator gives each kind to a process deliberately, so a
//!   process whose creator said nothing gets nothing ([`ObjectLimits::NONE`]).
//! - **Channels:** a channel is charged to the process that created it, once, until the channel
//!   is destroyed.
//! - **Events, ports and bindings** are counted apart, each against its own quota
//!   ([`ObjectCounts`]; ADR-0005 Amendment 7, F7). An event or a port is charged to the process
//!   that created it until the object is destroyed. A binding is charged to the process that
//!   bound it, from the bind until its one packet has left the port. Task S3-6's calls charge
//!   them; until they are built nothing does.
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
                // The sum is taken only under the quota, so it cannot overflow. `then_some`
                // would add first: on a count wrapped to the top of its range, a build that
                // checks overflow would panic where the charge must be refused.
                if n < self.max { Some(n + 1) } else { None }
            })
            .map(|_| ())
            .map_err(|_| Error::Limit)
    }

    /// One that was charged here is gone.
    ///
    /// A refund of something never charged is a kernel bug. It fails closed: the count wraps
    /// to the top of its range, so every later charge is refused.
    pub fn refund(&self) {
        self.held.fetch_sub(1, Ordering::AcqRel);
    }

    /// How many are charged now.
    pub fn held(&self) -> usize {
        self.held.load(Ordering::Acquire)
    }
}

/// How many events, ports and bindings a process may hold at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjectLimits {
    /// Events it created that still exist.
    pub events: usize,
    /// Ports it created that still exist.
    pub ports: usize,
    /// Bindings it made whose packet has not left its port.
    pub bindings: usize,
}

impl ObjectLimits {
    /// No events, no ports and no bindings.
    pub const NONE: Self = Self {
        events: 0,
        ports: 0,
        bindings: 0,
    };
}

/// A process's events, ports and bindings, each counted against its own quota.
#[derive(Debug)]
pub struct ObjectCounts {
    /// Events it created that still exist.
    pub events: Count,
    /// Ports it created that still exist.
    pub ports: Count,
    /// Bindings it made whose packet has not left its port.
    pub bindings: Count,
}

impl ObjectCounts {
    /// Nothing held, under `limits`.
    pub const fn new(limits: ObjectLimits) -> Self {
        Self {
            events: Count::new(limits.events),
            ports: Count::new(limits.ports),
            bindings: Count::new(limits.bindings),
        }
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

        let none = ObjectCounts::new(ObjectLimits::NONE);
        assert_eq!(none.events.charge(), Err(Error::Limit));
        assert_eq!(none.ports.charge(), Err(Error::Limit));
        assert_eq!(none.bindings.charge(), Err(Error::Limit));
    }

    #[test]
    fn the_largest_quota_does_not_overflow() {
        let count = Count::new(usize::MAX);
        assert_eq!(count.charge(), Ok(()));
        assert_eq!(count.held(), 1);
    }

    #[test]
    fn a_refund_too_many_refuses_every_later_charge() {
        let count = Count::new(4);
        count.refund();
        assert_eq!(count.charge(), Err(Error::Limit));
        // Even under the largest quota there is.
        let widest = Count::new(usize::MAX);
        widest.refund();
        assert_eq!(widest.charge(), Err(Error::Limit));
    }

    #[test]
    fn each_kind_is_counted_against_its_own_quota() {
        let counts = ObjectCounts::new(ObjectLimits {
            events: 1,
            ports: 2,
            bindings: 3,
        });
        assert_eq!(fill(&counts.events), 1);
        assert_eq!(fill(&counts.ports), 2);
        assert_eq!(fill(&counts.bindings), 3);

        // Room made in one kind is room in that kind only.
        counts.bindings.refund();
        assert_eq!(counts.events.charge(), Err(Error::Limit));
        assert_eq!(counts.ports.charge(), Err(Error::Limit));
        assert_eq!(counts.bindings.charge(), Ok(()));
        assert_eq!(
            (
                counts.events.held(),
                counts.ports.held(),
                counts.bindings.held()
            ),
            (1, 2, 3)
        );
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
