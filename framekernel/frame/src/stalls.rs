//! The record of TLB shootdowns that gave up waiting for a processor (TASKS item 66, R8): how
//! many, and the last one's processors, wait and later answer. Plain atomics, so that `tlb`
//! notes a stall where it happens and the kernel reports it, and tested on the host (the
//! review's T66-R8). It names processors and TSC ticks only, never an address.
#![forbid(unsafe_code)]

use core::sync::atomic::{AtomicU64, Ordering};

/// A shootdown that gave up waiting for a processor, as the boot reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stall {
    /// Shootdowns that have stalled this boot, this one included.
    pub count: u64,
    /// The processor that did not answer within the patience.
    pub target: usize,
    /// The processor that asked.
    pub asker: usize,
    /// How long the asker waited, in TSC ticks.
    pub waited: u64,
    /// TSC ticks from the give-up to the target's next answer, if it has answered since.
    pub answered_after: Option<u64>,
}

/// The boot's stalls. The last one's fields are written together by its asker and read for a
/// report; a report racing a second stall may mix the two, which only a report can show.
#[derive(Debug)]
pub struct StallLog {
    count: AtomicU64,
    /// The processor that did not answer and the one that asked; `u64::MAX` before any.
    target: AtomicU64,
    asker: AtomicU64,
    /// TSC ticks waited, and the TSC at the give-up.
    waited: AtomicU64,
    gave_up: AtomicU64,
    /// TSC ticks from the give-up to the target's next answer, plus one; 0 while it has not.
    answered_after: AtomicU64,
}

impl StallLog {
    /// A log with no stall.
    pub const fn new() -> Self {
        Self {
            count: AtomicU64::new(0),
            target: AtomicU64::new(u64::MAX),
            asker: AtomicU64::new(u64::MAX),
            waited: AtomicU64::new(0),
            gave_up: AtomicU64::new(0),
            answered_after: AtomicU64::new(0),
        }
    }

    /// Notes that `asker`, which asked at TSC `asked_at`, gave up on `target` at TSC `now`.
    pub fn note(&self, target: usize, asker: usize, asked_at: u64, now: u64) {
        self.answered_after.store(0, Ordering::Relaxed);
        self.target.store(target as u64, Ordering::Relaxed);
        self.asker.store(asker as u64, Ordering::Relaxed);
        self.waited
            .store(now.wrapping_sub(asked_at), Ordering::Relaxed);
        self.gave_up.store(now, Ordering::Release);
        self.count.fetch_add(1, Ordering::AcqRel);
    }

    /// Notes, on processor `cpu` at TSC `now`, that it served a shootdown: if it is the last
    /// stall's target and has not answered since, how long after the give-up it did.
    pub fn answered(&self, cpu: usize, now: u64) {
        if self.target.load(Ordering::Relaxed) == cpu as u64
            && self.answered_after.load(Ordering::Relaxed) == 0
        {
            let after = now.wrapping_sub(self.gave_up.load(Ordering::Acquire));
            let _ = self.answered_after.compare_exchange(
                0,
                after.saturating_add(1),
                Ordering::Relaxed,
                Ordering::Relaxed,
            );
        }
    }

    /// The last stall, if any.
    pub fn last(&self) -> Option<Stall> {
        let count = self.count.load(Ordering::Acquire);
        let target = self.target.load(Ordering::Relaxed);
        if count == 0 || target == u64::MAX {
            return None;
        }
        Some(Stall {
            count,
            target: target as usize,
            asker: self.asker.load(Ordering::Relaxed) as usize,
            waited: self.waited.load(Ordering::Relaxed),
            answered_after: match self.answered_after.load(Ordering::Relaxed) {
                0 => None,
                after => Some(after - 1),
            },
        })
    }
}

impl Default for StallLog {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_stall_reports_nothing() {
        let log = StallLog::new();
        assert_eq!(log.last(), None);
        log.answered(3, 100);
        assert_eq!(log.last(), None);
    }

    #[test]
    fn a_stall_is_named_with_its_wait_and_count() {
        let log = StallLog::new();
        log.note(2, 1, 1_000, 1_001_000);
        assert_eq!(
            log.last(),
            Some(Stall {
                count: 1,
                target: 2,
                asker: 1,
                waited: 1_000_000,
                answered_after: None,
            })
        );
    }

    #[test]
    fn only_the_target_answering_is_noted_and_only_its_first_answer() {
        let log = StallLog::new();
        log.note(2, 1, 0, 500);
        // Another processor serving a shootdown says nothing about the target.
        log.answered(1, 900);
        assert_eq!(log.last().unwrap().answered_after, None);
        log.answered(2, 800);
        log.answered(2, 5_000);
        assert_eq!(log.last().unwrap().answered_after, Some(300));
        // An answer right at the give-up is 0 ticks after it, and is still an answer.
        let at_once = StallLog::new();
        at_once.note(4, 0, 0, 10);
        at_once.answered(4, 10);
        assert_eq!(at_once.last().unwrap().answered_after, Some(0));
    }

    #[test]
    fn a_second_stall_replaces_the_first_and_counts_both() {
        let log = StallLog::new();
        log.note(2, 1, 0, 500);
        log.answered(2, 600);
        log.note(3, 0, 1_000, 2_000);
        let last = log.last().unwrap();
        assert_eq!((last.count, last.target, last.asker), (2, 3, 0));
        assert_eq!((last.waited, last.answered_after), (1_000, None));
    }
}
