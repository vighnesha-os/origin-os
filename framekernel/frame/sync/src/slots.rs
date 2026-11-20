//! The slots of a virtual region, each holding one mapping at a time and reused only once no
//! processor can still reach what was mapped there (CLAUDE.md §28; ADR-0004 Amendment 1, A2:
//! the kernel stacks).
//!
//! The region is cut into `N` equal slots, and may be given more once, at run time
//! ([`Slots::grow`]; ADR-0008 Amendment 1, D3: the kernel stacks' region grows with the processors
//! discovered). A slot's holder maps what it needs at the top of the
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
//!   and only then is what was mapped released and the slot free.
//! - **Retired** to **Waiting** ([`Slots::reclaim`], when the shootdown stalls): some processor
//!   did not answer, so it may still reach what was mapped. What the unmap returned is kept here,
//!   with the stall, and the slot stays counted as retired (TASKS item 73).
//! - **Waiting** to **Free** ([`Slots::reclaim`] again, once the processor the stall named has
//!   answered something since): a fresh shootdown of the same pages, sent after the unmap,
//!   completes on every processor, and only then is what was kept released. If it stalls, the
//!   slot waits again. A processor that never answers keeps the slot Waiting for good.
//! - **Live** to **Lost** ([`Slots::leak`]): its holder cannot give it back safely. No reclaim
//!   makes a slot Lost, except one that finds the waiting slot's record gone, which the one
//!   reclaim role makes unreachable: the slot is then given up as Lost, so the count of lost
//!   slots shows it (the independent review of TASKS item 73, T73-6).
//!
//! # One reclaim at a time, and at most one slot waiting
//!
//! A reclaim holds a role that a second one cannot take: the second returns [`Reclaim::Busy`] at
//! once, waiting for nobody. And while a slot waits, a reclaim unmaps no other: it asks the
//! waiting slot again, or does nothing. So at most one slot is out of the tables and not yet
//! free, and the table keeps what one unmap returned. Retired slots behind a waiting one stay
//! mapped, with what they hold, until it is free. Once every slot is taken or retired, a taker
//! gets `None`: the caller refuses what needed the slot.
//!
//! # Locks and shootdowns
//!
//! Every change of state is made under one lock, and nothing else is done under it: no mapping,
//! no allocation, no shootdown. So [`Slots::give_back`], which never waits for anyone, may be
//! called under any lock, and only [`Slots::reclaim`] waits for other processors. The kernel
//! takes the lock with interrupts off.
#![forbid(unsafe_code)]

use alloc::vec::Vec;
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
    /// Being reclaimed: unmapped, waiting for its shootdown, its first or a fresh one.
    Reclaiming,
    /// Unmapped, and its shootdown stalled: what was mapped is kept until a fresh shootdown
    /// completes ([`Slots::reclaim`]).
    Waiting,
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

/// What one [`Slots::reclaim`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reclaim {
    /// A slot is Free again: a shootdown of its pages completed on every processor, and what it
    /// held was released.
    Freed {
        /// That shootdown.
        completed: Completed,
        /// How many fresh shootdowns the slot was sent after a stall, the one that completed
        /// included: 0 if its first shootdown completed.
        asked_again: u32,
    },
    /// A shootdown stalled, the slot's first or a fresh one: the slot waits, with what it held.
    Stalled {
        /// Who did not answer.
        stall: Stalled,
        /// How many fresh shootdowns the slot has been sent since its first stall, this one
        /// included: 0 if this was its first shootdown.
        asked_again: u32,
    },
    /// A slot waits, and the processor its last stall named has not answered anything since:
    /// nothing was done, and no shootdown sent.
    Unanswered(Stalled),
    /// Another reclaim is under way: nothing was done.
    Busy,
}

/// How many slots are in each state, and the pages their holders mapped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// Live slots: held.
    pub live: usize,
    /// Idle slots: kept for reuse.
    pub idle: usize,
    /// Retired slots, slots being reclaimed and the slot waiting after a stall: given back and
    /// not yet free.
    pub retired: usize,
    /// Lost slots: never reused. [`Slots::leak`] makes one, and so would a reclaim that found
    /// the waiting slot's record gone, which the reclaim role makes unreachable (T73-6).
    pub lost: usize,
    /// Pages mapped in the region: those of Live, Idle and Retired slots, of slots being
    /// reclaimed for the first time, and of slots lost by [`Slots::leak`], which stay mapped
    /// because a processor may still run on them. A slot waiting after a stall counts none: its
    /// pages left the tables before its first shootdown.
    pub pages: u64,
}

/// Why [`Slots::grow`] added nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grow {
    /// The slots were grown once already: they are grown once.
    Already,
    /// The storage for the new slots could not be had.
    NoMemory,
}

/// The slot whose shootdown stalled, and what its unmap returned.
struct Waiting<K> {
    slot: usize,
    pages: u32,
    /// What the unmap returned: released only once a fresh shootdown completes.
    kept: K,
    /// The last stall: the processor a fresh shootdown waits to hear from first.
    stall: Stalled,
    /// How many fresh shootdowns it has been sent since its first stall.
    asked_again: u32,
}

/// The bookkeeping, under the lock.
struct Table<const N: usize, const IDLE: usize, K> {
    states: [State; N],
    /// The pages mapped at the top of each slot that is not Free: what a taker of an Idle slot
    /// gets, and what a reclaim unmaps.
    pages: [u32; N],
    /// The states of the slots past the first `N`, added once by [`Slots::grow`].
    more_states: Vec<State>,
    /// Their pages, as `pages`.
    more_pages: Vec<u32>,
    /// The Idle slots, most recently given back last.
    idle: [usize; IDLE],
    idle_count: usize,
    /// No slot below this one is Free.
    first_free: usize,
    /// Slots that are Retired, not counting those being reclaimed: what [`Slots::reclaim`] can
    /// still find.
    findable: usize,
    /// The slot waiting after a stall, if one is: at most one (see the module docs).
    waiting: Option<Waiting<K>>,
    counts: Counts,
}

/// `N` slots, at most `IDLE` of them kept idle, and room to keep one `K`, what an unmap returns,
/// for a slot whose shootdown stalled.
pub struct Slots<const N: usize, const IDLE: usize, K> {
    table: SpinLock<Table<N, IDLE, K>>,
    /// What a reclaim has to do: the Retired slots it can find, and one more while a slot waits.
    /// Readable without the lock, so that a reclaim with nothing to do takes no lock. Written
    /// only under it.
    pending: prim::AtomicUsize,
    /// Whether a reclaim is under way ([`Role`]).
    reclaiming: prim::AtomicBool,
    /// The compare-and-swaps tried on `reclaiming`, counted in this crate's host tests only: for
    /// the test that a reclaim that reads the role held tries none.
    #[cfg(all(test, not(loom)))]
    role_tries: core::sync::atomic::AtomicUsize,
}

impl<const N: usize, const IDLE: usize, K> core::fmt::Debug for Slots<N, IDLE, K> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Slots").finish_non_exhaustive()
    }
}

/// The one reclaim under way: taken with a compare-and-swap that a second reclaim fails rather
/// than waits on ([`Slots::role`]), and given up when dropped, so every way out of
/// [`Slots::reclaim`], an early return included, gives it up.
struct Role<'a>(&'a prim::AtomicBool);

impl Drop for Role<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// What a reclaim found to do, under the lock.
enum Job {
    /// Unmap a Retired slot and shoot it down for the first time.
    First { slot: usize, pages: u32 },
    /// Shoot the waiting slot down again, if the processor its stall named has answered since.
    Again {
        slot: usize,
        pages: u32,
        stall: Stalled,
    },
}

impl<const N: usize, const IDLE: usize, K> Table<N, IDLE, K> {
    const fn new() -> Self {
        Self {
            states: [State::Free; N],
            pages: [0; N],
            more_states: Vec::new(),
            more_pages: Vec::new(),
            idle: [0; IDLE],
            idle_count: 0,
            first_free: 0,
            findable: 0,
            waiting: None,
            counts: Counts {
                live: 0,
                idle: 0,
                retired: 0,
                lost: 0,
                pages: 0,
            },
        }
    }

    /// How many slots there are: the first `N` and those [`Slots::grow`] added.
    fn len(&self) -> usize {
        N + self.more_states.len()
    }

    fn state(&self, slot: usize) -> Option<State> {
        match slot.checked_sub(N) {
            None => self.states.get(slot).copied(),
            Some(past) => self.more_states.get(past).copied(),
        }
    }

    fn set(&mut self, slot: usize, state: State) {
        let s = match slot.checked_sub(N) {
            None => self.states.get_mut(slot),
            Some(past) => self.more_states.get_mut(past),
        };
        if let Some(s) = s {
            *s = state;
        }
    }

    /// The pages recorded for `slot`, or `None` past the last.
    fn page(&self, slot: usize) -> Option<u32> {
        match slot.checked_sub(N) {
            None => self.pages.get(slot).copied(),
            Some(past) => self.more_pages.get(past).copied(),
        }
    }

    fn page_mut(&mut self, slot: usize) -> Option<&mut u32> {
        match slot.checked_sub(N) {
            None => self.pages.get_mut(slot),
            Some(past) => self.more_pages.get_mut(past),
        }
    }

    fn pages_of(&self, slot: usize) -> u32 {
        self.page(slot).unwrap_or(0)
    }

    /// Takes the most recently idled slot holding `pages` pages out of the idle list.
    fn take_idle(&mut self, pages: u32) -> Option<usize> {
        let listed = self.idle.get(..self.idle_count)?;
        let at = listed
            .iter()
            .rposition(|&slot| self.page(slot) == Some(pages))?;
        let slot = *listed.get(at)?;
        // Close the gap: the order of the rest stays.
        let last = self.idle_count - 1;
        if let Some(rest) = self.idle.get_mut(at..=last) {
            rest.rotate_left(1);
        }
        self.idle_count = last;
        Some(slot)
    }

    /// Gives up `slot`, whose reclaim found the waiting slot's record gone, as Lost: what it held
    /// is no longer known, so it is never reused, and the count of lost slots shows it (T73-6).
    /// Its pages left the count at its first stall.
    fn give_up(&mut self, slot: usize) {
        self.set(slot, State::Lost);
        self.counts.retired -= 1;
        self.counts.lost += 1;
    }

    /// Makes `slot` Free again, after a reclaim.
    fn free(&mut self, slot: usize) {
        self.set(slot, State::Free);
        if let Some(p) = self.page_mut(slot) {
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

    /// What a reclaim has to do, for [`Slots::pending`].
    fn pending(&self) -> usize {
        self.findable + usize::from(self.waiting.is_some())
    }

    /// The next job, the waiting slot's before any other: while a slot waits no other is
    /// unmapped. A Retired slot it hands out is Reclaiming from now on.
    fn job(&mut self) -> Option<Job> {
        if let Some(w) = &self.waiting {
            return Some(Job::Again {
                slot: w.slot,
                pages: w.pages,
                stall: w.stall,
            });
        }
        let slot = (0..self.len()).find(|&s| self.state(s) == Some(State::Retired))?;
        self.set(slot, State::Reclaiming);
        // Still counted as retired until it is Free, but no longer findable.
        self.findable -= 1;
        Some(Job::First {
            slot,
            pages: self.pages_of(slot),
        })
    }
}

impl<const N: usize, const IDLE: usize, K> Slots<N, IDLE, K> {
    /// Every slot Free.
    #[cfg(not(all(test, loom)))]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            table: SpinLock::new(Table::new()),
            pending: prim::AtomicUsize::new(0),
            reclaiming: prim::AtomicBool::new(false),
            #[cfg(all(test, not(loom)))]
            role_tries: core::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Every slot Free. Loom's atomics cannot be built in a `const`.
    #[cfg(all(test, loom))]
    pub fn new() -> Self {
        Self {
            table: SpinLock::new(Table::new()),
            pending: prim::AtomicUsize::new(0),
            reclaiming: prim::AtomicBool::new(false),
        }
    }

    /// The reclaim role ([`Role`]), if no reclaim holds it. The flag is read first, and the
    /// compare-and-swap tried only if it reads free: while one reclaim works through a burst of
    /// retired stacks, every idle processor asks in its reap (TASKS item 73's diagnostic round:
    /// 30 000 to 80 000 refusals a boot on 65 processors), and a plain load keeps them off the
    /// flag's cache line, which a compare-and-swap takes exclusive even when it fails. The
    /// compare-and-swap still decides: a reclaim that reads the flag free may lose it.
    fn role(&self) -> Option<Role<'_>> {
        if self.reclaiming.load(Ordering::Relaxed) {
            return None;
        }
        #[cfg(all(test, not(loom)))]
        self.role_tries.fetch_add(1, Ordering::Relaxed);
        self.reclaiming
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| Role(&self.reclaiming))
    }

    /// Publishes what a reclaim has to do. Called under the lock, after each change to it.
    fn publish(&self, table: &Table<N, IDLE, K>) {
        self.pending.store(table.pending(), Ordering::Relaxed);
    }

    /// A slot for `pages` pages: an Idle one that holds that many, mapped as it was left, else
    /// the lowest Free one, so that the slots in use stay together and few page tables are
    /// touched. `None` if every slot is taken, retired or waiting.
    pub fn take(&self, pages: u32) -> Option<Taken> {
        let mut table = self.table.lock();
        let (slot, mapped) = match table.take_idle(pages) {
            Some(slot) => {
                table.counts.idle -= 1;
                (slot, true)
            }
            None => {
                let from = table.first_free;
                let slot = (from..table.len()).find(|&s| table.state(s) == Some(State::Free))?;
                table.first_free = slot + 1;
                if let Some(p) = table.page_mut(slot) {
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
        self.publish(&table);
        Some(Given::Retired)
    }

    /// Frees a Live slot with nothing mapped in it and no translation of it left on any
    /// processor: its taker mapped nothing at all ([`Taken::mapped`] was false, and mapping
    /// failed before the first page), or took out what it mapped and has completed a shootdown
    /// of it. A slot with anything else in it must go through [`Slots::give_back`] and
    /// [`Slots::reclaim`] instead: some processor may have cached what was mapped. Returns
    /// whether the slot was Live.
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
        self.publish(&table);
        trimmed
    }

    /// Reclaims one slot, if a reclaim has anything to do and no other reclaim is under way.
    ///
    /// - **A slot waits** after a stall: if `answered(stall)` says the processor the stall named
    ///   has answered something since, `shoot(slot, pages)` sends a fresh shootdown of the same
    ///   pages. If it completes, `release` gets what the slot's unmap returned, and the slot is
    ///   Free. If it stalls, the slot waits again. If `answered` says no, nothing is done.
    ///   `answered` only says when asking again is worth an interrupt: nothing here takes it as
    ///   a sign that any translation was dropped. Only a completed shootdown, published after
    ///   the unmap, lets `release` run.
    /// - **Otherwise a slot is Retired:** `unmap(slot, pages)` takes out of the page tables what
    ///   was mapped there and returns it, and `shoot(slot, pages)` drops it from every
    ///   processor's TLB. If that completes, `release` gets what `unmap` returned, and the slot
    ///   is Free. If it stalls, the slot waits, and the table keeps what `unmap` returned.
    ///
    /// `release` runs before the slot is Free, and never for a shootdown that stalled.
    ///
    /// Returns `None` if no slot was Retired or waiting, or what was done.
    ///
    /// No lock is held while `unmap`, `shoot`, `answered` and `release` run, so the caller may
    /// hold none that another processor waits for with interrupts off: the shootdown waits for
    /// every processor.
    pub fn reclaim(
        &self,
        unmap: impl FnOnce(usize, u32) -> K,
        shoot: impl FnOnce(usize, u32) -> Result<Completed, Stalled>,
        answered: impl FnOnce(Stalled) -> bool,
        release: impl FnOnce(K),
    ) -> Option<Reclaim> {
        if self.pending.load(Ordering::Relaxed) == 0 {
            return None;
        }
        let Some(_role) = self.role() else {
            return Some(Reclaim::Busy);
        };
        let job = {
            let mut table = self.table.lock();
            let job = table.job();
            self.publish(&table);
            job?
        };
        match job {
            Job::First { slot, pages } => {
                let unmapped = unmap(slot, pages);
                let outcome = shoot(slot, pages);
                match outcome {
                    Ok(completed) => {
                        release(unmapped);
                        let mut table = self.table.lock();
                        table.counts.retired -= 1;
                        table.counts.pages -= u64::from(pages);
                        table.free(slot);
                        Some(Reclaim::Freed {
                            completed,
                            asked_again: 0,
                        })
                    }
                    Err(stall) => {
                        let mut table = self.table.lock();
                        // Out of the tables, so no longer mapped, though still retired.
                        table.counts.pages -= u64::from(pages);
                        table.set(slot, State::Waiting);
                        table.waiting = Some(Waiting {
                            slot,
                            pages,
                            kept: unmapped,
                            stall,
                            asked_again: 0,
                        });
                        self.publish(&table);
                        Some(Reclaim::Stalled {
                            stall,
                            asked_again: 0,
                        })
                    }
                }
            }
            Job::Again { slot, pages, stall } => {
                if !answered(stall) {
                    return Some(Reclaim::Unanswered(stall));
                }
                self.table.lock().set(slot, State::Reclaiming);
                let outcome = shoot(slot, pages);
                let mut table = self.table.lock();
                match outcome {
                    Ok(completed) => {
                        // The role's holder took this job from `waiting`, and only it writes
                        // `waiting`, so the record is there; were it gone, the slot is given up
                        // where the count of lost slots shows it, not left Reclaiming (T73-6).
                        let Some(waiting) = table.waiting.take() else {
                            table.give_up(slot);
                            self.publish(&table);
                            return None;
                        };
                        self.publish(&table);
                        drop(table);
                        release(waiting.kept);
                        let mut table = self.table.lock();
                        table.counts.retired -= 1;
                        table.free(slot);
                        Some(Reclaim::Freed {
                            completed,
                            asked_again: waiting.asked_again + 1,
                        })
                    }
                    Err(again) => {
                        table.set(slot, State::Waiting);
                        if let Some(waiting) = table.waiting.as_mut() {
                            waiting.stall = again;
                            waiting.asked_again += 1;
                            Some(Reclaim::Stalled {
                                stall: again,
                                asked_again: waiting.asked_again,
                            })
                        } else {
                            // As in the arm above (T73-6).
                            table.give_up(slot);
                            self.publish(&table);
                            None
                        }
                    }
                }
            }
        }
    }

    /// The last stall of the slot waiting after one, if a slot is waiting.
    pub fn waiting(&self) -> Option<Stalled> {
        self.table.lock().waiting.as_ref().map(|w| w.stall)
    }

    /// Adds Free slots past the first `N`, once, so that there are `total` (ADR-0008 Amendment
    /// 1, D3). Their storage is made before the lock is taken, so nothing is allocated under it.
    /// Returns how many slots there are now: `N` if `total` is not more.
    ///
    /// # Errors
    ///
    /// [`Grow::Already`] if slots were added before, and [`Grow::NoMemory`] if the storage could
    /// not be had. Nothing changes either way.
    pub fn grow(&self, total: usize) -> Result<usize, Grow> {
        let Some(extra) = total.checked_sub(N).filter(|&e| e > 0) else {
            return Ok(N);
        };
        let mut states = Vec::new();
        let mut pages = Vec::new();
        states
            .try_reserve_exact(extra)
            .and_then(|()| pages.try_reserve_exact(extra))
            .map_err(|_| Grow::NoMemory)?;
        states.resize(extra, State::Free);
        pages.resize(extra, 0);
        let mut table = self.table.lock();
        if !table.more_states.is_empty() {
            drop(table);
            return Err(Grow::Already);
        }
        table.more_states = states;
        table.more_pages = pages;
        Ok(table.len())
    }

    /// How many slots there are: `N`, and those [`Slots::grow`] added.
    pub fn slot_count(&self) -> usize {
        self.table.lock().len()
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

impl<const N: usize, const IDLE: usize, K> Default for Slots<N, IDLE, K> {
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
    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize};
    use std::thread;

    fn done() -> Result<Completed, Stalled> {
        Ok(Completed {
            processors: 1,
            ticks: 0,
        })
    }

    /// The stall the tests' shootdowns report.
    const STALL: Stalled = Stalled {
        cpu: 1,
        generation: 7,
    };

    fn stalled() -> Result<Completed, Stalled> {
        Err(STALL)
    }

    /// A processor that stalled has answered since: worth asking again.
    fn answered(_: Stalled) -> bool {
        true
    }

    /// A processor that stalled has not answered since.
    fn unanswered(_: Stalled) -> bool {
        false
    }

    /// Reclaims every slot it can with shootdowns that complete; returns how many were freed.
    fn reclaim_all<const N: usize, const IDLE: usize>(slots: &Slots<N, IDLE, ()>) -> usize {
        let mut reclaimed = 0;
        while let Some(Reclaim::Freed { .. }) =
            slots.reclaim(|_, _| (), |_, _| done(), answered, |()| ())
        {
            reclaimed += 1;
        }
        reclaimed
    }

    /// ADR-0008 Amendment 1, D3: slots added at run time are taken, given back, kept idle and
    /// reclaimed like the first `N`; the table grows once, and asking for no more adds nothing.
    #[test]
    fn slots_added_at_run_time_are_like_the_first() {
        let slots: Slots<2, 1, ()> = Slots::new();
        assert_eq!(slots.slot_count(), 2);
        assert_eq!(slots.grow(1), Ok(2), "fewer than there are: nothing added");
        assert_eq!(slots.grow(2), Ok(2));
        assert_eq!(slots.grow(5), Ok(5));
        assert_eq!(slots.grow(9), Err(Grow::Already), "grown once");
        assert_eq!(slots.slot_count(), 5);
        let taken: Vec<_> = (0..5).map(|_| slots.take(1).unwrap().slot).collect();
        assert_eq!(taken, [0, 1, 2, 3, 4]);
        assert!(slots.take(1).is_none(), "every slot is taken");
        assert_eq!(slots.state(5), None, "nothing past the last");
        assert_eq!(slots.give_back(4, true), Some(Given::Idle));
        assert_eq!(slots.take(1).map(|t| (t.slot, t.mapped)), Some((4, true)));
        assert_eq!(slots.give_back(3, false), Some(Given::Retired));
        let mut unmapped = None;
        let reclaimed = slots.reclaim(
            |slot, pages| unmapped = Some((slot, pages)),
            |_, _| done(),
            answered,
            |()| (),
        );
        assert!(matches!(reclaimed, Some(Reclaim::Freed { .. })));
        assert_eq!(unmapped, Some((3, 1)));
        assert_eq!(slots.state(3), Some(State::Free));
        assert_eq!(slots.take(2).map(|t| (t.slot, t.mapped)), Some((3, false)));
        let counts = slots.counts();
        assert_eq!((counts.live, counts.pages), (5, 6));
    }

    #[test]
    fn takers_get_the_lowest_free_slots_until_none_is_left() {
        let slots = Slots::<3, 0, ()>::new();
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
        let slots = Slots::<4, 2, ()>::new();
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
        let slots = Slots::<2, 0, &str>::new();
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
            |_| unreachable!("asked only for a slot that waits"),
            |frames| {
                assert_eq!(frames, "frames");
                order.borrow_mut().push(("release", taken.slot, 0));
                assert_eq!(slots.state(taken.slot), Some(State::Reclaiming));
            },
        );
        assert_eq!(
            outcome,
            Some(Reclaim::Freed {
                completed: done().unwrap(),
                asked_again: 0
            })
        );
        assert_eq!(
            order.into_inner(),
            [("unmap", 0, 3), ("shoot", 0, 3), ("release", 0, 0)]
        );
        assert_eq!(slots.state(0), Some(State::Free));
        assert_eq!(
            slots.reclaim(|_, _| "", |_, _| done(), answered, |_| ()),
            None
        );
        assert_eq!(slots.counts().retired, 0);
    }

    /// Set when dropped: what a reclaim must release only after a completed shootdown.
    struct Frames<'a>(&'a AtomicBool);

    impl Drop for Frames<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }

    /// TASKS item 73: a stalled shootdown leaves the slot waiting, still retired, with what its
    /// unmap returned kept and not released. It is asked again only once the processor that
    /// stalled has answered since; a fresh shootdown that stalls leaves it waiting, and one that
    /// completes releases what was kept, once, and frees the slot.
    #[test]
    fn a_stalled_shootdown_keeps_the_slot_retired_until_a_fresh_one_completes() {
        let dropped = AtomicBool::new(false);
        let released = AtomicU32::new(0);
        let slots = Slots::<1, 0, Frames<'_>>::new();
        slots.take(2).unwrap();
        slots.give_back(0, false).unwrap();
        let release = |frames: Frames<'_>| {
            released.fetch_add(1, Ordering::Relaxed);
            drop(frames);
        };
        let outcome = slots.reclaim(
            |_, _| Frames(&dropped),
            |_, _| stalled(),
            |_| unreachable!("its first shootdown is not a fresh one"),
            release,
        );
        assert_eq!(
            outcome,
            Some(Reclaim::Stalled {
                stall: STALL,
                asked_again: 0
            })
        );
        let held = |slots: &Slots<1, 0, Frames<'_>>| {
            assert_eq!(
                released.load(Ordering::Relaxed),
                0,
                "released after a stall"
            );
            assert!(
                !dropped.load(Ordering::Relaxed),
                "what was mapped was dropped"
            );
            assert_eq!(slots.state(0), Some(State::Waiting));
            assert_eq!(slots.take(2), None, "a waiting slot is never taken");
            assert_eq!(
                slots.counts(),
                Counts {
                    retired: 1,
                    ..Counts::default()
                },
                "still retired, with its pages out of the tables"
            );
            assert_eq!(slots.waiting(), Some(STALL));
        };
        held(&slots);

        // Not answered yet: nothing is done, and no shootdown is sent.
        let outcome = slots.reclaim(
            |_, _| unreachable!("a waiting slot is not unmapped again"),
            |_, _| unreachable!("no shootdown before the processor answers"),
            unanswered,
            release,
        );
        assert_eq!(outcome, Some(Reclaim::Unanswered(STALL)));
        held(&slots);

        // Answered, asked again, and the fresh shootdown stalls on another processor.
        let other = Stalled {
            cpu: 2,
            generation: 9,
        };
        let outcome = slots.reclaim(
            |_, _| unreachable!("a waiting slot is not unmapped again"),
            |slot, pages| {
                assert_eq!((slot, pages), (0, 2), "the same pages");
                assert_eq!(slots.state(0), Some(State::Reclaiming));
                Err(other)
            },
            |stall| {
                assert_eq!(stall, STALL);
                true
            },
            release,
        );
        assert_eq!(
            outcome,
            Some(Reclaim::Stalled {
                stall: other,
                asked_again: 1
            })
        );
        assert_eq!(released.load(Ordering::Relaxed), 0);
        assert_eq!(
            slots.waiting(),
            Some(other),
            "the last stall is the one asked about"
        );

        // Asked again, and it completes: released once, and the slot is free.
        let outcome = slots.reclaim(
            |_, _| unreachable!("a waiting slot is not unmapped again"),
            |_, _| done(),
            |stall| stall == other,
            release,
        );
        assert_eq!(
            outcome,
            Some(Reclaim::Freed {
                completed: done().unwrap(),
                asked_again: 2
            })
        );
        assert_eq!(released.load(Ordering::Relaxed), 1);
        assert!(dropped.load(Ordering::Relaxed));
        assert_eq!(slots.state(0), Some(State::Free));
        assert_eq!(slots.counts(), Counts::default());
        assert_eq!(slots.waiting(), None);
        assert_eq!(slots.take(2).map(|t| t.slot), Some(0));
        assert_eq!(
            slots.reclaim(
                |_, _| unreachable!(),
                |_, _| unreachable!(),
                answered,
                release
            ),
            None,
            "nothing left to do"
        );
    }

    /// While a slot waits, no other Retired slot is unmapped: they stay mapped, counted, until
    /// it is free. Once every slot is taken or retired, a taker is refused, cleanly: the cost of
    /// a processor that never answers (TASKS item 73, the coordinating session's addition (b)).
    #[test]
    fn a_waiting_slot_holds_back_every_other_reclaim_and_takers_are_refused_cleanly() {
        let slots = Slots::<3, 0, ()>::new();
        for _ in 0..3 {
            slots.take(2).unwrap();
        }
        for slot in 0..3 {
            assert_eq!(slots.give_back(slot, false), Some(Given::Retired));
        }
        let unmapped = RefCell::new(Vec::new());
        let unmap = |slot, _| unmapped.borrow_mut().push(slot);
        assert!(matches!(
            slots.reclaim(unmap, |_, _| stalled(), answered, |()| ()),
            Some(Reclaim::Stalled { .. })
        ));
        // A processor that never answers: every reclaim after it does nothing.
        for _ in 0..5 {
            assert_eq!(
                slots.reclaim(unmap, |_, _| done(), unanswered, |()| ()),
                Some(Reclaim::Unanswered(STALL))
            );
        }
        assert_eq!(*unmapped.borrow(), [0], "nothing else left the tables");
        assert_eq!(slots.state(1), Some(State::Retired));
        assert_eq!(slots.state(2), Some(State::Retired));
        assert_eq!(
            slots.counts(),
            Counts {
                retired: 3,
                pages: 4,
                ..Counts::default()
            }
        );
        assert_eq!(slots.take(1), None, "refused, not a panic");
        assert_eq!(slots.take(2), None);

        // It answers: the waiting slot first, then the others, each with its own shootdown.
        let mut freed = Vec::new();
        while let Some(outcome) = slots.reclaim(unmap, |_, _| done(), answered, |()| ()) {
            match outcome {
                Reclaim::Freed { asked_again, .. } => freed.push(asked_again),
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(freed, [1, 0, 0]);
        assert_eq!(*unmapped.borrow(), [0, 1, 2]);
        assert_eq!(slots.counts(), Counts::default());
    }

    /// TASKS item 73, the coordinating session's addition (a): the reclaim role is given up on
    /// every way out of a reclaim, an early return and an unwinding panic included, and a reclaim
    /// started while it is held does nothing.
    #[test]
    fn the_reclaim_role_is_given_up_on_every_way_out() {
        let slots = Slots::<3, 0, ()>::new();
        for _ in 0..3 {
            slots.take(1).unwrap();
        }
        slots.give_back(0, false).unwrap();
        slots.give_back(1, false).unwrap();

        // Held: a second reclaim, here from inside the first's shootdown, with slot 1 still to
        // reclaim, does nothing.
        let outcome = slots.reclaim(
            |_, _| (),
            |_, _| {
                assert_eq!(
                    slots.reclaim(|_, _| (), |_, _| done(), answered, |()| ()),
                    Some(Reclaim::Busy)
                );
                stalled()
            },
            answered,
            |()| (),
        );
        assert!(matches!(outcome, Some(Reclaim::Stalled { .. })));

        // An early return, before any shootdown: given up.
        assert_eq!(
            slots.reclaim(|_, _| (), |_, _| done(), unanswered, |()| ()),
            Some(Reclaim::Unanswered(STALL))
        );
        assert!(matches!(
            slots.reclaim(|_, _| (), |_, _| done(), answered, |()| ()),
            Some(Reclaim::Freed { asked_again: 1, .. })
        ));

        // A panic between taking the role and giving it back, in slot 1's shootdown: given up as
        // the stack unwinds.
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            slots.reclaim(
                |_, _| (),
                |_, _| panic!("a shootdown that panics"),
                answered,
                |()| (),
            )
        }));
        assert!(panicked.is_err());
        slots.give_back(2, false).unwrap();
        assert!(
            matches!(
                slots.reclaim(|_, _| (), |_, _| done(), answered, |()| ()),
                Some(Reclaim::Freed { .. })
            ),
            "the role was left held"
        );
    }

    /// A reclaim that reads the role held is refused without trying the compare-and-swap; one
    /// that reads it free tries it.
    #[test]
    fn a_reclaim_that_reads_the_role_held_tries_no_compare_and_swap() {
        let slots = Slots::<3, 0, ()>::new();
        for _ in 0..3 {
            slots.take(1).unwrap();
        }
        slots.give_back(0, false).unwrap();
        slots.give_back(1, false).unwrap();
        let tries = || slots.role_tries.load(Ordering::Relaxed);
        let outcome = slots.reclaim(
            |_, _| (),
            |_, _| {
                // The first reclaim tried one, and holds the role; slot 1 is still to reclaim.
                assert_eq!(tries(), 1);
                assert_eq!(
                    slots.reclaim(|_, _| (), |_, _| done(), answered, |()| ()),
                    Some(Reclaim::Busy)
                );
                assert_eq!(
                    tries(),
                    1,
                    "a reclaim that read the role held tried to take it"
                );
                done()
            },
            answered,
            |()| (),
        );
        assert!(matches!(outcome, Some(Reclaim::Freed { .. })));
        assert!(matches!(
            slots.reclaim(|_, _| (), |_, _| done(), answered, |()| ()),
            Some(Reclaim::Freed { .. })
        ));
        assert_eq!(tries(), 2);
    }

    /// T73-6: a re-ask that finds the waiting slot's record gone, which the role makes
    /// unreachable and only a test can force, gives the slot up as Lost, whether its shootdown
    /// completed or stalled: the count of lost slots shows it, nothing is released, and no later
    /// reclaim touches it.
    #[test]
    fn a_reask_that_finds_no_waiting_record_gives_the_slot_up_as_lost() {
        for completes in [true, false] {
            let slots = Slots::<1, 0, ()>::new();
            slots.take(1).unwrap();
            slots.give_back(0, false).unwrap();
            assert!(matches!(
                slots.reclaim(|_, _| (), |_, _| stalled(), answered, |()| ()),
                Some(Reclaim::Stalled { .. })
            ));
            let outcome = slots.reclaim(
                |_, _| (),
                |_, _| {
                    slots.table.lock().waiting = None;
                    if completes { done() } else { stalled() }
                },
                answered,
                |()| panic!("nothing is released"),
            );
            assert_eq!(outcome, None, "completes: {completes}");
            let counts = slots.counts();
            assert_eq!((counts.retired, counts.lost, counts.pages), (0, 1, 0));
            assert_eq!(slots.state(0), Some(State::Lost));
            assert_eq!(
                slots.reclaim(|_, _| (), |_, _| done(), answered, |()| ()),
                None
            );
        }
    }

    /// A slot lost by `leak` stays mapped, so its pages stay counted; one waiting after a stall
    /// left the tables, so its pages do not (the tests above). No reclaim makes a slot Lost.
    #[test]
    fn a_leaked_slot_keeps_its_pages_counted() {
        let slots = Slots::<2, 0, ()>::new();
        let taken = slots.take(3).unwrap();
        assert!(slots.leak(taken.slot));
        assert_eq!(slots.state(taken.slot), Some(State::Lost));
        assert_eq!(
            slots.counts(),
            Counts {
                lost: 1,
                pages: 3,
                ..Counts::default()
            }
        );
        assert!(!slots.leak(taken.slot), "only a Live slot is leaked");
    }

    #[test]
    fn trim_retires_every_idle_slot() {
        let slots = Slots::<4, 4, ()>::new();
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
        let slots = Slots::<4, 1, ()>::new();
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
        // A waiting slot is nobody's to give back, abandon or leak.
        let waiting = slots.take(2).unwrap().slot;
        slots.give_back(waiting, false).unwrap();
        assert!(matches!(
            slots.reclaim(|_, _| (), |_, _| stalled(), answered, |()| ()),
            Some(Reclaim::Stalled { .. })
        ));
        assert_eq!(slots.state(waiting), Some(State::Waiting));
        assert_eq!(slots.give_back(waiting, false), None);
        assert!(!slots.abandon(waiting) && !slots.leak(waiting));
        assert_eq!(slots.state(waiting), Some(State::Waiting));
    }

    /// Four threads take, give back and reclaim at once, and some shootdowns stall; each slot
    /// has one holder at a time, a slot reused after a reclaim had a completed shootdown first,
    /// and at the end every slot is free again.
    #[test]
    fn concurrent_holders_never_share_a_slot() {
        const SLOTS: usize = 8;
        let slots = Arc::new(Slots::<SLOTS, 2, ()>::new());
        let holders: Arc<[AtomicUsize; SLOTS]> =
            Arc::new(core::array::from_fn(|_| AtomicUsize::new(0)));
        // Each slot's reclaims that have completed their shootdown and not yet been reused.
        let shot: Arc<[AtomicBool; SLOTS]> =
            Arc::new(core::array::from_fn(|_| AtomicBool::new(true)));
        let stalls = Arc::new(AtomicUsize::new(0));
        let threads: Vec<_> = (0..4)
            .map(|t| {
                let (slots, holders, shot, stalls) = (
                    Arc::clone(&slots),
                    Arc::clone(&holders),
                    Arc::clone(&shot),
                    Arc::clone(&stalls),
                );
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
                                    "reused before a completed shootdown"
                                );
                            }
                            holders[taken.slot].fetch_sub(1, Ordering::AcqRel);
                            slots.give_back(taken.slot, round % 3 != 0).unwrap();
                        }
                        if round % 5 == 0 {
                            slots.reclaim(
                                |_, _| (),
                                |slot, _| {
                                    if (round / 5 + t) % 7 == 0 {
                                        stalls.fetch_add(1, Ordering::Relaxed);
                                        return stalled();
                                    }
                                    shot[slot].store(true, Ordering::Release);
                                    done()
                                },
                                |_| round % 2 == 0,
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
        assert!(stalls.load(Ordering::Relaxed) > 0, "no shootdown stalled");
        slots.trim();
        reclaim_all(&slots);
        assert_eq!(slots.counts(), Counts::default());
    }
}
