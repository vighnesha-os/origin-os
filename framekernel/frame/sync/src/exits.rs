//! What one processor's ending threads still hold (TASKS item 79): the counts a settle waits on
//! before a boot proof compares the whole machine's heap and free frames.
//!
//! A thread's end is seen before the thread has given anything back. A user thread's
//! `Process::thread_ended` raises the thread's `TERMINATED`, then closes the process's handles
//! and makes its `ending` visible, all with interrupts on. Only then does the thread reach the
//! scheduler's exit, which queues its stack, its FPU area and its reference to the address space
//! as a zombie for its own processor to reap. A kernel thread raises its own done-flag and then
//! exits. Between the two, a settle that counted zombies alone found none, and read the counts
//! with the thread's stack, FPU area and space still held (TASKS item 79: the directory proof's
//! first count on `land/batch-7` `6c754c45`, 2026-11-20).
//!
//! So each processor counts two things in an [`Exits`]:
//!
//! - **`ending`**: threads whose end may have been seen and that have not yet left the
//!   processor. The ending thread raises it before anything about its end is visible
//!   ([`Exits::end`]), and the processor lowers it in the thread's exit, once the zombie is
//!   queued ([`Exits::queued`]).
//! - **`zombies`**: zombies queued and not yet reaped. Raised in the exit before `ending` is
//!   lowered, and lowered by the reaper only after everything the zombie held has been given back
//!   ([`Exits::reap`]), the process's `TERMINATED` among it when it was the last thread.
//!
//! A settle reads every processor's `ending` first, then every processor's `zombies`
//! ([`pending`]). If both sums are zero, every thread whose end it could have seen has been
//! reaped, and what it held has been given back.
//!
//! # Why the orderings are enough
//!
//! Every write of either count is a read-modify-write with AcqRel, and every read is an Acquire
//! load. Take a thread T whose end the settler saw: an Acquire read of something T published
//! after its [`Exits::end`] marked it.
//!
//! - The mark happens before the settler's load of T's processor's `ending`, so that load reads
//!   the mark or a later value. Each thread's mark comes before its own lowering in the count's
//!   modification order, so a value of 0 means T's lowering is at or before the value read.
//! - Every write after T's lowering is a read-modify-write, so all of them are in its release
//!   sequence. Reading any of them synchronizes with T's lowering, and so with T's raising of
//!   `zombies`, made just before it.
//! - The settler's later load of `zombies` therefore reads T's raising or a later value. By the
//!   same argument, 0 there means the reaper's lowering of T's zombie is at or before it, and the
//!   settler synchronizes with it. Everything the reaper gave back before lowering it happens
//!   before whatever the settler does next.
//!
//! A count split over processors changes nothing in this: T's two counts are on its own
//! processor, and the other processors' counts only add to the sums. On x86-64 each write is one
//! `lock xadd` or `lock add`, and each read a plain move.
//!
//! The Loom models (`models.rs`, "exits") check this in the memory model Rust's atomics follow,
//! with negative controls that break each step: a mark made after the end is published, a queue
//! that lowers `ending` before it raises `zombies`, a settle that reads `zombies` first, a reap
//! that lowers `zombies` before it gives back, and a settle whose `ending` loads are Relaxed.
//!
//! # Who calls what
//!
//! - [`Exits::end`]: the ending thread, on its own processor, at most once per thread. The
//!   scheduler keeps a mark per thread, so that its exit lowers `ending` only for a thread that
//!   raised it.
//! - [`Exits::queued`] and [`Exits::not_queued`]: the thread's processor, in its exit, with
//!   interrupts off. `not_queued` is the exit that could keep no zombie and leaked what the thread
//!   held: that thread has left the processor, so a settle should not wait on it. What it leaked
//!   is the count compare's to find.
//! - [`Exits::reap`]: the processor's reaper, in thread context, one zombie at a time.
//! - [`pending`]: any processor.
#![forbid(unsafe_code)]

use core::sync::atomic::Ordering::{AcqRel, Acquire};

use crate::prim;

/// One processor's ending threads and unreaped zombies.
pub struct Exits {
    ending: prim::AtomicU64,
    zombies: prim::AtomicU64,
}

/// What a settle found still held: threads whose end may have been seen that have not yet left
/// their processor, and zombies not yet reaped, summed over every processor.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Pending {
    /// Threads whose end may have been seen and that have not yet left their processor.
    pub ending: u64,
    /// Exited threads that await their reaping.
    pub zombies: u64,
}

// Only their names: the kernel's no-panic check refuses formatting beyond `write_str` in this
// crate's release object, and a derived `Debug` would format the counts. The kernel says what a
// settle found in its own words (`framekernel::settled`).
impl core::fmt::Debug for Exits {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Exits")
    }
}

impl core::fmt::Debug for Pending {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Pending")
    }
}

impl Pending {
    /// Whether nothing was held.
    pub fn none(self) -> bool {
        self == Self::default()
    }
}

impl Exits {
    /// No thread ending, and no zombie.
    #[cfg(not(all(test, loom)))]
    pub const fn new() -> Self {
        Self {
            ending: prim::AtomicU64::new(0),
            zombies: prim::AtomicU64::new(0),
        }
    }

    /// No thread ending, and no zombie.
    #[cfg(all(test, loom))]
    pub fn new() -> Self {
        Self {
            ending: prim::AtomicU64::new(0),
            zombies: prim::AtomicU64::new(0),
        }
    }

    /// The ending thread, before anything about its end is visible: counts it as ending, then
    /// runs `publish`, which makes the end visible, and returns what `publish` returns.
    pub fn end<R>(&self, publish: impl FnOnce() -> R) -> R {
        self.ending.fetch_add(1, AcqRel);
        publish()
    }

    /// The thread's processor, in its exit, once its zombie is queued: counts the zombie, then,
    /// if the thread raised `ending` (`marked`), lowers it.
    pub fn queued(&self, marked: bool) {
        self.zombies.fetch_add(1, AcqRel);
        if marked {
            self.ending.fetch_sub(1, AcqRel);
        }
    }

    /// The thread's processor, in an exit that could queue no zombie: the thread has left the
    /// processor all the same, so if it raised `ending` (`marked`), it is lowered.
    pub fn not_queued(&self, marked: bool) {
        if marked {
            self.ending.fetch_sub(1, AcqRel);
        }
    }

    /// The reaper, for one zombie: runs `release`, which gives back everything the zombie held,
    /// then counts it reaped, and returns what `release` returns.
    pub fn reap<R>(&self, release: impl FnOnce() -> R) -> R {
        let released = release();
        self.zombies.fetch_sub(1, AcqRel);
        released
    }

    /// Zombies this processor has queued and not reaped: a cheap test before the reaper takes
    /// its lock.
    pub fn zombies(&self) -> u64 {
        self.zombies.load(Acquire)
    }
}

impl Default for Exits {
    fn default() -> Self {
        Self::new()
    }
}

/// What every processor's exits still hold: `each()` names every processor's [`Exits`], and is
/// called twice, so that every `ending` is read before any `zombies`.
pub fn pending<'a, I: Iterator<Item = &'a Exits>>(each: impl Fn() -> I) -> Pending {
    let ending = each()
        .map(|exits| exits.ending.load(Acquire))
        .fold(0, u64::saturating_add);
    let zombies = each()
        .map(|exits| exits.zombies.load(Acquire))
        .fold(0, u64::saturating_add);
    Pending { ending, zombies }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    fn one(exits: &Exits) -> Pending {
        pending(|| core::iter::once(exits))
    }

    #[test]
    fn an_ended_thread_is_pending_until_its_zombie_is_reaped() {
        let exits = Exits::new();
        assert!(one(&exits).none());
        exits.end(|| ());
        assert_eq!(
            one(&exits),
            Pending {
                ending: 1,
                zombies: 0
            }
        );
        exits.queued(true);
        assert_eq!(
            one(&exits),
            Pending {
                ending: 0,
                zombies: 1
            }
        );
        let released = exits.reap(|| 7);
        assert_eq!(released, 7);
        assert!(one(&exits).none());
    }

    #[test]
    fn the_end_is_counted_before_it_is_published() {
        let exits = Exits::new();
        let seen = exits.end(|| one(&exits));
        assert_eq!(seen.ending, 1);
    }

    #[test]
    fn the_zombie_is_counted_until_after_its_release() {
        let exits = Exits::new();
        exits.end(|| ());
        exits.queued(true);
        let during = exits.reap(|| one(&exits));
        assert_eq!(
            during,
            Pending {
                ending: 0,
                zombies: 1
            }
        );
        assert!(one(&exits).none());
    }

    #[test]
    fn an_unmarked_exit_lowers_no_ending() {
        // A thread that never raised `ending` (one that exited without `end`) leaves it alone, so
        // another thread's mark is not lost.
        let exits = Exits::new();
        exits.end(|| ());
        exits.queued(false);
        assert_eq!(
            one(&exits),
            Pending {
                ending: 1,
                zombies: 1
            }
        );
        exits.not_queued(false);
        assert_eq!(one(&exits).ending, 1);
    }

    #[test]
    fn an_exit_that_queued_nothing_still_lowers_its_mark() {
        let exits = Exits::new();
        exits.end(|| ());
        exits.not_queued(true);
        assert!(one(&exits).none());
    }

    #[test]
    fn every_processor_is_summed() {
        let cpus = [Exits::new(), Exits::new(), Exits::new()];
        cpus[0].end(|| ());
        cpus[2].end(|| ());
        cpus[2].queued(true);
        assert_eq!(
            pending(|| cpus.iter()),
            Pending {
                ending: 1,
                zombies: 1
            }
        );
    }

    #[test]
    fn host_threads_lose_no_count() {
        // Real threads on the host, each ending, queuing and reaping many times against one
        // processor's counts while a reader sums them: every count comes back to zero.
        const THREADS: usize = 4;
        const ROUNDS: usize = 10_000;
        let exits = std::sync::Arc::new(Exits::new());
        let workers: Vec<_> = (0..THREADS)
            .map(|_| {
                let exits = std::sync::Arc::clone(&exits);
                std::thread::spawn(move || {
                    for _ in 0..ROUNDS {
                        exits.end(|| ());
                        exits.queued(true);
                        exits.reap(|| ());
                    }
                })
            })
            .collect();
        for _ in 0..ROUNDS {
            let seen = one(&exits);
            assert!(seen.ending <= THREADS as u64 && seen.zombies <= THREADS as u64);
        }
        for worker in workers {
            worker.join().unwrap();
        }
        assert!(one(&exits).none());
    }
}
