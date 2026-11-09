//! The start-up handshake between the boot processor and one application processor
//! (CLAUDE.md §24 SMP start-up and the CPU online/offline lifecycle; ADR-0003 K6; ADR-0008
//! C4 and C7, Proposed).
//!
//! The boot processor starts the APs one at a time through one trampoline, whose data page
//! (stack, entry and argument) it rewrites for each AP (`src/smp.rs`). An AP that misses its
//! handshake may be slow rather than dead. Before the page is rewritten for the next AP, the
//! boot processor must know two things about the AP it gave up on:
//! - it will not join late. Each start-up is settled exactly once, by a compare-and-swap
//!   from "undecided", by whichever side gets there first: the AP with its answer, or the
//!   boot processor with [`Outcome::Abandoned`] when it stops waiting. An AP whose answer
//!   comes second never joins: it halts;
//! - it will not read the page again. The boot processor parks it with INIT, which puts it
//!   in wait-for-SIPI wherever it was (`smp::park` in the binary).
//!
//! This module is the first half. Safe code, host-tested, including the race between the
//! two sides on real threads.

use core::sync::atomic::{AtomicU64, Ordering};

/// Why an AP refused its start-up. It halts either way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// It has no x2APIC, which the kernel requires (ADR-0003 K5).
    NoX2Apic,
    /// It is not the processor its CPU index was assigned to (ADR-0008 C4).
    NotMine,
}

/// How one AP's start-up was settled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The AP answered with this x2APIC ID.
    Answered(u32),
    /// The AP refused.
    Refused(Refusal),
    /// The boot processor stopped waiting before the AP answered.
    Abandoned,
}

// The word's encodings. 0 is undecided and an answer is its ID + 1 (1 to 2^32); the others
// are above 2^32, so no x2APIC ID can be read as one of them.
const UNDECIDED: u64 = 0;
const NO_X2APIC: u64 = u64::MAX - 2;
const NOT_MINE: u64 = u64::MAX - 1;
const ABANDONED: u64 = u64::MAX;

impl Outcome {
    const fn encode(self) -> u64 {
        match self {
            Self::Answered(id) => id as u64 + 1,
            Self::Refused(Refusal::NoX2Apic) => NO_X2APIC,
            Self::Refused(Refusal::NotMine) => NOT_MINE,
            Self::Abandoned => ABANDONED,
        }
    }

    /// The outcome a word holds, or `None` while it is undecided. Only [`Handshake`] writes
    /// the word, so any other value is one of the encodings above. A value that is not
    /// reads as [`Outcome::Abandoned`], under which no AP joins.
    fn decode(word: u64) -> Option<Self> {
        match word {
            UNDECIDED => None,
            NO_X2APIC => Some(Self::Refused(Refusal::NoX2Apic)),
            NOT_MINE => Some(Self::Refused(Refusal::NotMine)),
            ABANDONED => Some(Self::Abandoned),
            answer => {
                Some(u32::try_from(answer.wrapping_sub(1)).map_or(Self::Abandoned, Self::Answered))
            }
        }
    }
}

/// One AP's start-up handshake: undecided until one side settles it, then fixed.
#[derive(Debug)]
pub struct Handshake(AtomicU64);

impl Handshake {
    /// An undecided start-up.
    pub const fn new() -> Self {
        Self(AtomicU64::new(UNDECIDED))
    }

    /// The AP's answer: it is up, with x2APIC ID `apic_id`. Returns whether this settled
    /// the start-up. Only then may the AP join; otherwise the boot processor had already
    /// stopped waiting for it.
    pub fn answer(&self, apic_id: u32) -> bool {
        self.settle(Outcome::Answered(apic_id)).is_ok()
    }

    /// The AP's refusal. Returns whether this settled the start-up.
    pub fn refuse(&self, why: Refusal) -> bool {
        self.settle(Outcome::Refused(why)).is_ok()
    }

    /// The boot processor stops waiting. Returns the outcome in force: the AP's answer or
    /// refusal if one came first, else [`Outcome::Abandoned`], after which no answer can
    /// settle the start-up.
    pub fn abandon(&self) -> Outcome {
        match self.settle(Outcome::Abandoned) {
            Ok(()) => Outcome::Abandoned,
            Err(first) => first,
        }
    }

    /// The outcome, once settled.
    pub fn outcome(&self) -> Option<Outcome> {
        Outcome::decode(self.0.load(Ordering::Acquire))
    }

    /// Settles the start-up with `outcome` if it is still undecided; otherwise returns the
    /// outcome that came first. Release on success publishes what this side wrote before
    /// (the AP's tables); acquire on failure sees what the other side wrote.
    fn settle(&self, outcome: Outcome) -> Result<(), Outcome> {
        match self.0.compare_exchange(
            UNDECIDED,
            outcome.encode(),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Ok(()),
            Err(first) => Err(Outcome::decode(first).unwrap_or(Outcome::Abandoned)),
        }
    }
}

impl Default for Handshake {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Rng, cases};
    use core::sync::atomic::AtomicUsize;
    use std::sync::Arc;
    use std::thread;

    /// Arrives at `ready`, spins until the other side has too, then spins `jitter` more
    /// times: both sides start together, neither one sleeping, and either may be first.
    fn start_together(ready: &AtomicUsize, jitter: usize) {
        ready.fetch_add(1, Ordering::AcqRel);
        while ready.load(Ordering::Acquire) < 2 {
            core::hint::spin_loop();
        }
        for _ in 0..jitter {
            core::hint::spin_loop();
        }
    }

    #[test]
    fn an_answer_in_time_stands() {
        let h = Handshake::new();
        assert_eq!(h.outcome(), None);
        assert!(h.answer(256));
        assert_eq!(h.abandon(), Outcome::Answered(256));
        assert_eq!(h.outcome(), Some(Outcome::Answered(256)));
    }

    #[test]
    fn an_answer_after_the_deadline_does_not_join() {
        let h = Handshake::new();
        assert_eq!(h.abandon(), Outcome::Abandoned);
        assert!(!h.answer(1), "a late AP must not join");
        assert!(!h.refuse(Refusal::NotMine));
        assert_eq!(h.outcome(), Some(Outcome::Abandoned));
        // Giving up twice changes nothing.
        assert_eq!(h.abandon(), Outcome::Abandoned);
    }

    #[test]
    fn a_refusal_settles_the_start_up_like_an_answer() {
        for why in [Refusal::NoX2Apic, Refusal::NotMine] {
            let h = Handshake::new();
            assert!(h.refuse(why));
            assert!(!h.answer(3));
            assert_eq!(h.abandon(), Outcome::Refused(why));
            assert_eq!(h.outcome(), Some(Outcome::Refused(why)));
        }
    }

    #[test]
    fn only_the_first_answer_counts() {
        // Two processors answering on one start record: the second is told it did not join.
        let h = Handshake::new();
        assert!(h.answer(2));
        assert!(!h.answer(3));
        assert_eq!(h.outcome(), Some(Outcome::Answered(2)));
    }

    #[test]
    fn every_apic_id_round_trips_and_differs_from_the_other_outcomes() {
        let others = [
            Outcome::Refused(Refusal::NoX2Apic),
            Outcome::Refused(Refusal::NotMine),
            Outcome::Abandoned,
        ];
        let edges = [
            0,
            1,
            254,
            255,
            256,
            420,
            u32::MAX - 2,
            u32::MAX - 1,
            u32::MAX,
        ];
        let mut rng = Rng::new(0xA9_5747);
        let random = (0..cases(10_000)).map(|_| rng.next() as u32);
        for id in edges.into_iter().chain(random) {
            let h = Handshake::new();
            assert!(h.answer(id));
            assert_eq!(h.outcome(), Some(Outcome::Answered(id)), "ID {id}");
            for other in others {
                assert_ne!(Outcome::Answered(id).encode(), other.encode(), "ID {id}");
            }
        }
        for other in others {
            assert_eq!(Outcome::decode(other.encode()), Some(other));
        }
    }

    #[test]
    fn a_word_no_outcome_encodes_never_reads_as_an_answer() {
        for word in [(1 << 32) + 1, 1 << 40, u64::MAX - 3] {
            assert_eq!(Outcome::decode(word), Some(Outcome::Abandoned), "{word:#x}");
        }
    }

    /// The AP's answer and the boot processor's deadline race on real threads, started
    /// together with random jitter: exactly one side settles each start-up, both agree on
    /// which, and each side wins some rounds (so the race really ran both ways).
    #[test]
    fn the_answer_and_the_deadline_race_with_one_winner_both_sides_agree_on() {
        let rounds = cases(2000);
        let mut rng = Rng::new(0x0DEA_D11E);
        let (mut answered, mut abandoned) = (0, 0);
        for round in 0..rounds {
            let id = round as u32;
            let h = Arc::new(Handshake::new());
            let ready = Arc::new(AtomicUsize::new(0));
            let (ap_jitter, bsp_jitter) = (rng.below(200), rng.below(200));
            let ap = {
                let (h, ready) = (Arc::clone(&h), Arc::clone(&ready));
                thread::spawn(move || {
                    start_together(&ready, ap_jitter);
                    h.answer(id)
                })
            };
            start_together(&ready, bsp_jitter);
            let bsp = h.abandon();
            let joined = ap.join().expect("the AP thread");
            match bsp {
                Outcome::Answered(got) => {
                    assert!(
                        joined,
                        "round {round}: the boot processor saw an answer the AP lost"
                    );
                    assert_eq!(got, id);
                    answered += 1;
                }
                Outcome::Abandoned => {
                    assert!(
                        !joined,
                        "round {round}: the AP joined after the boot processor gave up"
                    );
                    abandoned += 1;
                }
                Outcome::Refused(why) => panic!("round {round}: refused {why:?}"),
            }
            assert_eq!(h.outcome(), Some(bsp));
        }
        assert_eq!(answered + abandoned, rounds);
        assert!(
            answered > 0 && abandoned > 0,
            "the race never went one way: {answered} answered, {abandoned} abandoned"
        );
    }
}
