//! The floor under the kernel's monotonic clock (CLAUDE.md §68; ADR-0012).
//!
//! Intel and AMD each promise that one processor's time-stamp counter only counts forward.
//! Neither promises how two processors' counters compare, and both say the read is not ordered
//! with the instructions around it. So a clock that is only "the counter minus a zero" can step
//! back between processors, and in practice did (ADR-0012, Context).
//!
//! [`MonotonicFloor`] makes that impossible whatever the counters do: it holds the latest time
//! anyone has been given, and every reading passes through it. A reading below the floor comes
//! back as the floor.
//!
//! # What it guarantees
//!
//! Each value in `latest` is larger than the one before it: the compare-exchange replaces
//! `floor` only with a larger `reading`, and replaces exactly the value it read. Every value
//! [`MonotonicFloor::advance`] returns is a value `latest` held. So if one call happens before
//! another, by any synchronization between them, the later call returns at least as much as the
//! earlier one.
//!
//! The orderings are `Relaxed` because the floor carries nothing but itself, and the guarantee
//! needs only the coherence of this one variable: a load that happens after a load or write of
//! `latest` cannot see an older value. The ordering between the two calls is the caller's: a
//! lock, a release/acquire pair, a wake-up. The Loom models check exactly that, and a negative
//! control removes the caller's ordering to show that the models see the difference.
//! On x86-64 the generated code is the same for every ordering: a plain load and `lock cmpxchg`.
//!
//! # What it does not guarantee
//!
//! Time standing still is allowed: two calls can return the same value, and a processor whose
//! counter is behind reads the floor until its counter catches up (ADR-0012 C7).
//!
//! `advance` is lock-free and safe in any context, interrupt and NMI handlers included. A
//! failed compare-exchange means another reader raised the floor, so the loop ends as soon as
//! the floor reaches this reading.

use core::sync::atomic::Ordering;

use crate::prim;

/// The latest time any reader has been given; see the module documentation.
///
/// Every reader writes this variable, so it gets cache lines of its own: 128 bytes, because
/// x86-64 processors can fetch cache lines in adjacent pairs. Sharing a pair with anything read
/// on the clock's path would cost that reader a coherence miss on every write here.
#[derive(Debug)]
#[repr(align(128))]
pub struct MonotonicFloor {
    latest: prim::AtomicU64,
}

impl MonotonicFloor {
    /// A floor of 0.
    #[cfg(not(all(test, loom)))]
    pub const fn new() -> Self {
        Self {
            latest: prim::AtomicU64::new(0),
        }
    }

    /// A floor of 0. Loom's atomics cannot be constructed in a `const` context.
    #[cfg(all(test, loom))]
    pub fn new() -> Self {
        Self {
            latest: prim::AtomicU64::new(0),
        }
    }

    /// Passes `reading` through the floor: returns `reading` if it is above every value given
    /// out so far, and raises the floor to it, or otherwise the floor.
    #[inline]
    pub fn advance(&self, reading: u64) -> u64 {
        let mut floor = self.latest.load(Ordering::Relaxed);
        loop {
            if reading <= floor {
                return floor;
            }
            match self.latest.compare_exchange_weak(
                floor,
                reading,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return reading,
                Err(newer) => floor = newer,
            }
        }
    }
}

impl Default for MonotonicFloor {
    fn default() -> Self {
        Self::new()
    }
}

// Not under Loom: its atomics exist only inside a model (`models.rs` has this type's models).
#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use crate::SpinLock;
    use std::sync::atomic::AtomicU64;
    use std::thread;

    #[test]
    fn it_has_its_cache_lines_to_itself() {
        assert_eq!(core::mem::align_of::<MonotonicFloor>(), 128);
        assert_eq!(core::mem::size_of::<MonotonicFloor>(), 128);
    }

    #[test]
    fn a_reading_above_the_floor_raises_it_and_a_reading_below_returns_it() {
        let floor = MonotonicFloor::new();
        assert_eq!(floor.advance(10), 10);
        assert_eq!(floor.advance(5), 10, "behind: the floor comes back");
        assert_eq!(floor.advance(10), 10, "level with it");
        assert_eq!(floor.advance(11), 11);
        assert_eq!(floor.advance(0), 11);
        assert_eq!(floor.advance(u64::MAX), u64::MAX);
        assert_eq!(floor.advance(1), u64::MAX, "and it stays there");
    }

    /// Any readings at all come out as their running maximum (CLAUDE.md §72).
    #[test]
    fn any_sequence_of_readings_comes_out_as_its_running_maximum() {
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = || {
            x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            match x >> 60 {
                0 => 0,
                1 => 1,
                2 => u64::MAX,
                3 => u64::MAX - 1,
                n => x >> (n * 4),
            }
        };
        for _ in 0..200 {
            let floor = MonotonicFloor::new();
            let mut maximum = 0;
            for _ in 0..500 {
                let reading = next();
                maximum = maximum.max(reading);
                assert_eq!(floor.advance(reading), maximum);
            }
        }
    }

    /// A counter that is `offset` ahead of the shared time. Each thread is a "processor"
    /// whose counter disagrees with the others'.
    fn read_counter(time: &AtomicU64, offset: u64) -> u64 {
        time.fetch_add(1, Ordering::Relaxed) + offset
    }

    /// `threads` processors read the clock strictly in rotation under a lock, so every handoff
    /// happens, each comparing with the reading before it (the check the boot proof makes).
    /// Returns the steps back seen.
    fn steps_back_across_handoffs(
        threads: u64,
        rounds: u64,
        skew: u64,
        clock: impl Fn(u64) -> u64 + Sync,
    ) -> u64 {
        let time = AtomicU64::new(0);
        let next = AtomicU64::new(0);
        // The last value and the steps back.
        let turn = SpinLock::new((0u64, 0u64));
        thread::scope(|s| {
            for index in 0..threads {
                let (turn, time, next, clock) = (&turn, &time, &next, &clock);
                s.spawn(move || {
                    let mut done = 0;
                    while done < rounds {
                        if next.load(Ordering::Acquire) % threads != index {
                            thread::yield_now();
                            continue;
                        }
                        let mut held = turn.lock();
                        let got = clock(read_counter(time, index * skew));
                        let (last, backwards) = *held;
                        *held = (got, backwards + u64::from(got < last));
                        drop(held);
                        next.fetch_add(1, Ordering::Release);
                        done += 1;
                    }
                });
            }
        });
        turn.lock().1
    }

    #[test]
    fn counters_that_disagree_step_back_without_the_floor_and_never_with_it() {
        // The control: the harness does see the problem the floor exists for. Each processor is
        // a million ticks ahead of the one before it, so every handoff to a lower index steps
        // back, whatever the threads' timing.
        assert!(steps_back_across_handoffs(4, 1_000, 1_000_000, |reading| reading) > 0);
        let floor = MonotonicFloor::new();
        let backwards =
            steps_back_across_handoffs(4, 1_000, 1_000_000, |reading| floor.advance(reading));
        assert_eq!(backwards, 0);
    }

    /// What a read through the floor costs when every reader writes the shared variable on every
    /// read: the worst case for it, and close to how a real TSC behaves, since each read of a
    /// fine-grained counter is above the floor. Not a pass/fail test: the numbers are for
    /// CLAUDE.md §66. `cargo test -p origin-sync --release floor_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "a measurement, not a test"]
    fn floor_cost_with_every_reader_writing() {
        const READS: u32 = 2_000_000;
        for readers in [1usize, 2, 4, 8, 10] {
            let floor = MonotonicFloor::new();
            let elapsed: Vec<std::time::Duration> = thread::scope(|s| {
                let threads: Vec<_> = (0..readers)
                    .map(|_| {
                        s.spawn(|| {
                            let start = std::time::Instant::now();
                            for _ in 0..READS {
                                let seen = floor.advance(0);
                                core::hint::black_box(floor.advance(seen + 1));
                            }
                            start.elapsed()
                        })
                    })
                    .collect();
                threads.into_iter().map(|t| t.join().unwrap()).collect()
            });
            let nanos: u128 = elapsed.iter().map(std::time::Duration::as_nanos).sum();
            let reads = u128::from(READS) * readers as u128;
            println!(
                "floor cost: {readers} reader(s): {} ns per read",
                nanos / reads
            );
        }
    }

    #[test]
    fn readers_racing_keep_every_thread_monotonic_and_end_at_the_largest_reading() {
        let floor = MonotonicFloor::new();
        let time = AtomicU64::new(0);
        let largest = AtomicU64::new(0);
        let rounds = 50_000;
        thread::scope(|s| {
            for index in 0..8 {
                let (floor, time, largest) = (&floor, &time, &largest);
                s.spawn(move || {
                    let mut last = 0;
                    for _ in 0..rounds {
                        let reading = read_counter(time, (index % 3) * 1000);
                        largest.fetch_max(reading, Ordering::Relaxed);
                        let got = floor.advance(reading);
                        assert!(got >= last, "{got} after {last}");
                        last = got;
                    }
                });
            }
        });
        assert_eq!(floor.advance(0), largest.load(Ordering::Relaxed));
    }
}
