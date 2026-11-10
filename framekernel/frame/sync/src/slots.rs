//! The slots of a virtual region, each holding one mapping at a time and reused only once no
//! processor can still reach what was mapped there (CLAUDE.md §28; ADR-0004 Amendment 1, A2:
//! the kernel stacks).
//!
//! The region is cut into `N` equal slots. A slot's holder maps what it needs at the top of the
//! slot and leaves the rest unmapped, so running off the bottom of what is mapped faults, and
//! nothing below it belongs to anyone else. Mapping and unmapping are the caller's: this is the
//! bookkeeping, generic over them so that the host tests and the Loom models (`models.rs`) run
//! the code the kernel runs.
//!
//! # A slot's life
//!
//! - **Free** to **Live** ([`Slots::take`]): nothing is mapped in it, and the taker maps.
//! - **Live** to **Idle** ([`Slots::give_back`], while fewer than `IDLE` are): its mapping stays,
//!   unused, for the next taker of the same size, which gets it as it was left. Reusing it
//!   changes no translation, so it needs no shootdown.
//! - **Live** to **Retired** ([`Slots::give_back`] otherwise, and every Idle slot on
//!   [`Slots::trim`]): to be unmapped.
//! - **Retired** to **Free** ([`Slots::reclaim`]): the caller unmaps it, a shootdown completes,
//!   and only then is what was mapped released and the slot free. A stalled shootdown leaves it
//!   **Lost**: never taken again, and what was mapped never released, since some processor may
//!   still reach it.
//! - **Live** to **Lost** ([`Slots::leak`]): its holder cannot give it back safely.
//!
//! # Locks and shootdowns
//!
//! Every change of state is made under one lock, and nothing else is done under it: no mapping,
//! no allocation, no shootdown. So [`Slots::give_back`], which never waits for anyone, may be
//! called under any lock, and only [`Slots::reclaim`] waits for other processors. The kernel
//! takes the lock with interrupts off.

use core::sync::atomic::Ordering;

use crate::shootdown::{Completed, Stalled};
use crate::{SpinLock, prim};

/// What a slot is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Nothing mapped; the next [`Slots::take`] may have it.
    Free,
    /// Its holder's.
    Live,
    /// Given back with its mapping kept, for a taker of the same size.
    Idle,
    /// Given back, still mapped, to be reclaimed.
    Retired,
    /// Being reclaimed: unmapped, waiting for its shootdown.
    Reclaiming,
    /// Never to be taken again.
    Lost,
}

/// A slot [`Slots::take`] handed out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Taken {
    /// Which slot.
    pub slot: usize,
    /// Whether the pages are mapped already: an Idle slot of the same size, as it was left. If
    /// not, nothing is mapped in the slot.
    pub mapped: bool,
}

/// What [`Slots::give_back`] did with a slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Given {
    /// Kept mapped for the next taker of its size.
    Idle,
    /// Left for [`Slots::reclaim`].
    Retired,
}

/// How many slots are in each state, and the pages their holders mapped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// Live slots: held.
    pub live: usize,
    /// Idle slots: kept for reuse.
    pub idle: usize,
    /// Retired slots, and slots being reclaimed: given back and not yet free.
    pub retired: usize,
    /// Lost slots: never reused.
    pub lost: usize,
    /// Pages mapped in Live, Idle and Retired slots, and in slots being reclaimed.
    pub pages: u64,
}

/// The bookkeeping, under the lock.
struct Table<const N: usize, const IDLE: usize> {
    states: [State; N],
    /// The pages mapped at the top of each slot that is not Free: what a taker of an Idle slot
    /// gets, and what a reclaim unmaps.
    pages: [u32; N],
    /// The Idle slots, most recently given back last.
    idle: [usize; IDLE],
    idle_count: usize,
    /// No slot below this one is Free.
    first_free: usize,
    /// Slots that are Retired, not counting those being reclaimed: what [`Slots::reclaim`] can
    /// still find.
    findable: usize,
    counts: Counts,
}

/// `N` slots, at most `IDLE` of them kept idle.
pub struct Slots<const N: usize, const IDLE: usize> {
    table: SpinLock<Table<N, IDLE>>,
    /// Retired slots, readable without the lock, so that a reclaim with nothing to do takes no
    /// lock. Written only under it.
    retired: prim::AtomicUsize,
}

impl<const N: usize, const IDLE: usize> core::fmt::Debug for Slots<N, IDLE> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Slots").finish_non_exhaustive()
    }
}

impl<const N: usize, const IDLE: usize> Table<N, IDLE> {
    const fn new() -> Self {
        Self {
            states: [State::Free; N],
            pages: [0; N],
            idle: [0; IDLE],
            idle_count: 0,
            first_free: 0,
            findable: 0,
            counts: Counts {
                live: 0,
                idle: 0,
                retired: 0,
                lost: 0,
                pages: 0,
            },
        }
    }

    fn state(&self, slot: usize) -> Option<State> {
        self.states.get(slot).copied()
    }

    fn set(&mut self, slot: usize, state: State) {
        if let Some(s) = self.states.get_mut(slot) {
            *s = state;
        }
    }

    fn pages_of(&self, slot: usize) -> u32 {
        self.pages.get(slot).copied().unwrap_or(0)
    }

    /// Takes the most recently idled slot holding `pages` pages out of the idle list.
    fn take_idle(&mut self, pages: u32) -> Option<usize> {
        let listed = self.idle.get(..self.idle_count)?;
        let at = listed
            .iter()
            .rposition(|&slot| self.pages.get(slot) == Some(&pages))?;
        let slot = *listed.get(at)?;
        // Close the gap: the order of the rest stays.
        let last = self.idle_count - 1;
        if let Some(rest) = self.idle.get_mut(at..=last) {
            rest.rotate_left(1);
        }
        self.idle_count = last;
        Some(slot)
    }

    /// Makes `slot` Free again, after a reclaim.
    fn free(&mut self, slot: usize) {
        self.set(slot, State::Free);
        if let Some(p) = self.pages.get_mut(slot) {
            *p = 0;
        }
        self.first_free = self.first_free.min(slot);
    }

    /// Makes `slot` Retired, findable by a reclaim.
    fn retire(&mut self, slot: usize) {
        self.set(slot, State::Retired);
        self.counts.retired += 1;
        self.findable += 1;
    }
}

impl<const N: usize, const IDLE: usize> Slots<N, IDLE> {
    /// Every slot Free.
    #[cfg(not(all(test, loom)))]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            table: SpinLock::new(Table::new()),
            retired: prim::AtomicUsize::new(0),
        }
    }

    /// Every slot Free. Loom's atomics cannot be built in a `const`.
    #[cfg(all(test, loom))]
    pub fn new() -> Self {
        Self {
            table: SpinLock::new(Table::new()),
            retired: prim::AtomicUsize::new(0),
        }
    }

    /// A slot for `pages` pages: an Idle one that holds that many, mapped as it was left, else
    /// the lowest Free one, so that the slots in use stay together and few page tables are
    /// touched. `None` if every slot is taken.
    pub fn take(&self, pages: u32) -> Option<Taken> {
        let mut table = self.table.lock();
        let (slot, mapped) = match table.take_idle(pages) {
            Some(slot) => {
                table.counts.idle -= 1;
                (slot, true)
            }
            None => {
                let from = table.first_free;
                let slot = (from..N).find(|&s| table.state(s) == Some(State::Free))?;
                table.first_free = slot + 1;
                if let Some(p) = table.pages.get_mut(slot) {
                    *p = pages;
                }
                table.counts.pages += u64::from(pages);
                (slot, false)
            }
        };
        table.set(slot, State::Live);
        table.counts.live += 1;
        Some(Taken { slot, mapped })
    }

    /// Gives back a Live slot: kept Idle, with its mapping, if `keep` and there is room for
    /// another, otherwise Retired. `None`, and nothing done, if the slot is not Live. Waits for
    /// nobody, so it may be called under any lock.
    pub fn give_back(&self, slot: usize, keep: bool) -> Option<Given> {
        let mut table = self.table.lock();
        if table.state(slot) != Some(State::Live) {
            return None;
        }
        table.counts.live -= 1;
        if keep && table.idle_count < IDLE {
            let at = table.idle_count;
            if let Some(entry) = table.idle.get_mut(at) {
                *entry = slot;
                table.idle_count += 1;
                table.counts.idle += 1;
                table.set(slot, State::Idle);
                return Some(Given::Idle);
            }
        }
        table.retire(slot);
        self.retired.store(table.findable, Ordering::Relaxed);
        Some(Given::Retired)
    }

    /// Frees a Live slot in which its taker mapped nothing at all ([`Taken::mapped`] was false,
    /// and mapping failed before its first page). A slot with anything mapped in it must go
    /// through [`Slots::give_back`] and [`Slots::reclaim`] instead: some processor may have
    /// cached what was mapped. Returns whether the slot was Live.
    pub fn abandon(&self, slot: usize) -> bool {
        let mut table = self.table.lock();
        if table.state(slot) != Some(State::Live) {
            return false;
        }
        table.counts.live -= 1;
        table.counts.pages -= u64::from(table.pages_of(slot));
        table.free(slot);
        true
    }

    /// Gives up a Live slot for good: its holder could not give it back safely (a processor
    /// may still be running on what is mapped there). Returns whether the slot was Live.
    pub fn leak(&self, slot: usize) -> bool {
        let mut table = self.table.lock();
        if table.state(slot) != Some(State::Live) {
            return false;
        }
        table.counts.live -= 1;
        table.counts.lost += 1;
        table.set(slot, State::Lost);
        true
    }

    /// Retires every Idle slot, so that a reclaim gives back what they hold. Returns how many.
    pub fn trim(&self) -> usize {
        let mut table = self.table.lock();
        let trimmed = table.idle_count;
        for at in 0..trimmed {
            if let Some(&slot) = table.idle.get(at) {
                table.retire(slot);
            }
        }
        table.idle_count = 0;
        table.counts.idle = 0;
        self.retired.store(table.findable, Ordering::Relaxed);
        trimmed
    }

    /// Reclaims one Retired slot, if there is one: `unmap(slot, pages)` takes out of the page
    /// tables what was mapped there and returns it; `shoot(slot, pages)` drops it from every
    /// processor's TLB; and only once that has completed does `release` get what `unmap`
    /// returned, and the slot become Free.
    ///
    /// On [`Stalled`] a processor may still reach what was mapped: `release` does not run, what
    /// `unmap` returned is forgotten (what it owns stays allocated for good), and the slot is
    /// Lost. Returns the shootdown's outcome, or `None` if no slot was Retired.
    ///
    /// No lock is held while `unmap`, `shoot` and `release` run, so the caller may hold none
    /// that another processor waits for with interrupts off: the shootdown waits for every
    /// processor.
    pub fn reclaim<R>(
        &self,
        unmap: impl FnOnce(usize, u32) -> R,
        shoot: impl FnOnce(usize, u32) -> Result<Completed, Stalled>,
        release: impl FnOnce(R),
    ) -> Option<Result<Completed, Stalled>> {
        if self.retired.load(Ordering::Relaxed) == 0 {
            return None;
        }
        let (slot, pages) = {
            let mut table = self.table.lock();
            let slot = (0..N).find(|&s| table.state(s) == Some(State::Retired))?;
            table.set(slot, State::Reclaiming);
            // Still counted as retired until it is Free or Lost, but no longer findable.
            table.findable -= 1;
            self.retired.store(table.findable, Ordering::Relaxed);
            (slot, table.pages_of(slot))
        };
        let unmapped = unmap(slot, pages);
        let outcome = shoot(slot, pages);
        match outcome {
            Ok(_) => release(unmapped),
            Err(_) => core::mem::forget(unmapped),
        }
        let mut table = self.table.lock();
        table.counts.retired -= 1;
        table.counts.pages -= u64::from(pages);
        if outcome.is_ok() {
            table.free(slot);
        } else {
            table.set(slot, State::Lost);
            table.counts.lost += 1;
        }
        Some(outcome)
    }

    /// What slot `slot` is now, or `None` past the last.
    pub fn state(&self, slot: usize) -> Option<State> {
        self.table.lock().state(slot)
    }

    /// How many slots are in each state.
    pub fn counts(&self) -> Counts {
        self.table.lock().counts
    }
}

impl<const N: usize, const IDLE: usize> Default for Slots<N, IDLE> {
    fn default() -> Self {
        Self::new()
    }
}

// Not under Loom: its atomics exist only inside a model (`models.rs` has this protocol's models).
#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    use std::thread;

    fn done() -> Result<Completed, Stalled> {
        Ok(Completed {
            processors: 1,
            ticks: 0,
        })
    }

    fn stalled() -> Result<Completed, Stalled> {
        Err(Stalled { cpu: 1 })
    }

    /// Reclaims every Retired slot with a shootdown that completes; returns how many.
    fn reclaim_all<const N: usize, const IDLE: usize>(slots: &Slots<N, IDLE>) -> usize {
        let mut reclaimed = 0;
        while slots.reclaim(|_, _| (), |_, _| done(), |()| ()).is_some() {
            reclaimed += 1;
        }
        reclaimed
    }

    #[test]
    fn takers_get_the_lowest_free_slots_until_none_is_left() {
        let slots = Slots::<3, 0>::new();
        let taken: Vec<_> = (0..3).map(|_| slots.take(4).unwrap()).collect();
        assert_eq!(
            taken,
            [0, 1, 2].map(|slot| Taken {
                slot,
                mapped: false
            })
        );
        assert_eq!(slots.take(4), None, "every slot is taken");
        assert_eq!(
            slots.counts(),
            Counts {
                live: 3,
                pages: 12,
                ..Counts::default()
            }
        );
        // The middle slot comes back through a reclaim and is the next one taken.
        assert_eq!(
            slots.give_back(1, true),
            Some(Given::Retired),
            "no room to idle"
        );
        assert_eq!(slots.take(4), None, "a retired slot is not free");
        assert_eq!(reclaim_all(&slots), 1);
        assert_eq!(slots.take(2).map(|t| t.slot), Some(1));
        assert_eq!(slots.counts().pages, 10);
    }

    #[test]
    fn an_idle_slot_goes_to_a_taker_of_its_size_as_it_was_left() {
        let slots = Slots::<4, 2>::new();
        for _ in 0..3 {
            slots.take(4).unwrap();
        }
        assert_eq!(slots.give_back(0, true), Some(Given::Idle));
        assert_eq!(slots.give_back(1, true), Some(Given::Idle));
        assert_eq!(
            slots.give_back(2, true),
            Some(Given::Retired),
            "two kept at most"
        );
        assert_eq!(slots.state(0), Some(State::Idle));
        // Another size gets a fresh slot; the same size gets the idle one given back last.
        assert_eq!(
            slots.take(16),
            Some(Taken {
                slot: 3,
                mapped: false
            })
        );
        assert_eq!(
            slots.take(4),
            Some(Taken {
                slot: 1,
                mapped: true
            })
        );
        assert_eq!(
            slots.take(4),
            Some(Taken {
                slot: 0,
                mapped: true
            })
        );
        assert_eq!(slots.take(4), None);
        let counts = slots.counts();
        assert_eq!((counts.live, counts.idle, counts.retired), (3, 0, 1));
        assert_eq!(counts.pages, 4 + 4 + 4 + 16);
    }

    #[test]
    fn a_reclaim_unmaps_then_shoots_down_then_releases_then_frees() {
        let slots = Slots::<2, 0>::new();
        let taken = slots.take(3).unwrap();
        slots.give_back(taken.slot, false).unwrap();
        let order = RefCell::new(Vec::new());
        let outcome = slots.reclaim(
            |slot, pages| {
                order.borrow_mut().push(("unmap", slot, pages));
                // The slot is out of everyone's reach while this runs.
                assert_eq!(slots.state(slot), Some(State::Reclaiming));
                "frames"
            },
            |slot, pages| {
                order.borrow_mut().push(("shoot", slot, pages));
                assert_eq!(slots.take(3).map(|t| t.slot), Some(1), "not this one");
                done()
            },
            |frames| {
                assert_eq!(frames, "frames");
                order.borrow_mut().push(("release", taken.slot, 0));
                assert_eq!(slots.state(taken.slot), Some(State::Reclaiming));
            },
        );
        assert_eq!(outcome, Some(done()));
        assert_eq!(
            order.into_inner(),
            [("unmap", 0, 3), ("shoot", 0, 3), ("release", 0, 0)]
        );
        assert_eq!(slots.state(0), Some(State::Free));
        assert_eq!(slots.reclaim(|_, _| (), |_, _| done(), |()| ()), None);
        assert_eq!(slots.counts().retired, 0);
    }

    /// Set when dropped: what a stalled reclaim must never release.
    struct Frames<'a>(&'a AtomicBool);

    impl Drop for Frames<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }

    #[test]
    fn a_stalled_shootdown_loses_the_slot_and_releases_nothing() {
        let slots = Slots::<1, 0>::new();
        slots.take(2).unwrap();
        slots.give_back(0, false).unwrap();
        let dropped = AtomicBool::new(false);
        let released = AtomicBool::new(false);
        let outcome = slots.reclaim(
            |_, _| Frames(&dropped),
            |_, _| stalled(),
            |_| released.store(true, Ordering::Relaxed),
        );
        assert_eq!(outcome, Some(stalled()));
        assert!(!released.load(Ordering::Relaxed), "released after a stall");
        assert!(
            !dropped.load(Ordering::Relaxed),
            "what was mapped was dropped"
        );
        assert_eq!(slots.state(0), Some(State::Lost));
        assert_eq!(slots.take(2), None, "a lost slot is never taken");
        assert_eq!(
            slots.counts(),
            Counts {
                lost: 1,
                ..Counts::default()
            }
        );
    }

    #[test]
    fn trim_retires_every_idle_slot() {
        let slots = Slots::<4, 4>::new();
        for _ in 0..3 {
            slots.take(4).unwrap();
        }
        for slot in 0..3 {
            assert_eq!(slots.give_back(slot, true), Some(Given::Idle));
        }
        assert_eq!(slots.trim(), 3);
        assert_eq!(slots.counts().retired, 3);
        assert_eq!(slots.take(4).map(|t| (t.slot, t.mapped)), Some((3, false)));
        assert_eq!(reclaim_all(&slots), 3);
        assert_eq!(
            slots.counts(),
            Counts {
                live: 1,
                pages: 4,
                ..Counts::default()
            }
        );
    }

    #[test]
    fn only_a_live_slot_can_be_given_back_abandoned_or_leaked() {
        let slots = Slots::<3, 1>::new();
        assert_eq!(slots.give_back(0, true), None, "a free slot");
        assert!(!slots.abandon(0) && !slots.leak(0));
        assert_eq!(slots.give_back(7, false), None, "past the last slot");
        slots.take(1).unwrap();
        slots.take(1).unwrap();
        assert!(slots.abandon(0), "nothing mapped yet: free at once");
        assert_eq!(slots.state(0), Some(State::Free));
        assert!(slots.leak(1));
        assert_eq!(slots.give_back(1, true), None, "a lost slot");
        assert_eq!(slots.take(1).map(|t| t.slot), Some(0));
        assert_eq!(slots.give_back(0, true), Some(Given::Idle));
        assert_eq!(slots.give_back(0, true), None, "given back twice");
        let counts = slots.counts();
        assert_eq!((counts.live, counts.idle, counts.lost), (0, 1, 1));
    }

    /// Four threads take, give back and reclaim at once; each slot has one holder at a time,
    /// and a slot reused after a reclaim had its shootdown first.
    #[test]
    fn concurrent_holders_never_share_a_slot() {
        const SLOTS: usize = 8;
        let slots = Arc::new(Slots::<SLOTS, 2>::new());
        let holders: Arc<[AtomicUsize; SLOTS]> =
            Arc::new(core::array::from_fn(|_| AtomicUsize::new(0)));
        // Each slot's reclaims that have completed their shootdown and not yet been reused.
        let shot: Arc<[AtomicBool; SLOTS]> =
            Arc::new(core::array::from_fn(|_| AtomicBool::new(true)));
        let threads: Vec<_> = (0..4)
            .map(|t| {
                let (slots, holders, shot) =
                    (Arc::clone(&slots), Arc::clone(&holders), Arc::clone(&shot));
                thread::spawn(move || {
                    for round in 0..2000u32 {
                        let pages = 1 + (round + t) % 2;
                        if let Some(taken) = slots.take(pages) {
                            assert_eq!(
                                holders[taken.slot].fetch_add(1, Ordering::AcqRel),
                                0,
                                "two holders"
                            );
                            if !taken.mapped {
                                assert!(
                                    shot[taken.slot].swap(false, Ordering::AcqRel),
                                    "reused before its shootdown"
                                );
                            }
                            holders[taken.slot].fetch_sub(1, Ordering::AcqRel);
                            slots.give_back(taken.slot, round % 3 != 0).unwrap();
                        }
                        if round % 5 == 0 {
                            slots.reclaim(
                                |_, _| (),
                                |slot, _| {
                                    shot[slot].store(true, Ordering::Release);
                                    done()
                                },
                                |()| (),
                            );
                        }
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        slots.trim();
        reclaim_all(&slots);
        assert_eq!(slots.counts(), Counts::default());
    }
}
