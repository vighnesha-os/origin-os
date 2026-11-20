//! Thread records: where a thread is, kept for it from its spawn to its reap, so that a wake-up
//! finds the thread wherever it runs (ADR-0020 S1; TASKS item 92).
//!
//! # The location
//!
//! A thread's **location** is the processor (its CPU index) and the slot of that processor's run
//! queue that hold it. It is clear before the thread is queued and after it has exited, and it
//! changes when the thread moves (ADR-0020 S6, not yet built).
//!
//! The location is written only under the run-queue lock of the processor it names: set by a spawn
//! once it has queued the thread, cleared by the thread's exit in the critical section that frees
//! its slot, and changed by a move holding both processors' locks. So a holder of that lock who
//! reads the location unchanged knows the thread is in that slot, and that it cannot leave it while
//! the lock is held.
//!
//! # The record and the name
//!
//! A thread has one [`Record`] from its spawn to its reap, taken from a [`Block`] and given back
//! there. The record holds the location, a **generation**, and the thread's state (`S`: in the
//! kernel, its stack, FPU area, user context and exit mark, `framekernel::threads`). A thread is
//! named by its record and that generation.
//!
//! The generation changes each time the record is handed to a new thread ([`Block::take`]), and
//! only then. A block hands out only records whose location is clear, and the spawn that took one
//! sets its location after queuing the thread, under that queue's lock. So a holder of the lock a
//! location names reads the generation of the thread that is there, and a name kept past its
//! thread's reap never names the record's next thread.
//!
//! # Locks
//!
//! In this order, and never the other way: a processor's run queue (two, in index order, for a
//! move), that processor's thread table (in the kernel, `sched::Threads`: its zombies and the boot
//! or idle flow's FPU area), a record's state, then a block's allocator. A switch takes the outgoing
//! record's state and then the incoming one's, each released before the next is taken. The kernel
//! holds each lock with interrupts off.
#![forbid(unsafe_code)]

use core::sync::atomic::Ordering::{Acquire, Relaxed, Release};

use crate::{SpinLock, prim};

/// Where a thread is: a processor, by CPU index, and a slot of that processor's run queue.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Location {
    cpu: u32,
    slot: u16,
}

// Only its name: the kernel's no-panic check refuses formatting beyond `write_str` in this crate's
// release object (`exits.rs` says the same of its types).
impl core::fmt::Debug for Location {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Location")
    }
}

/// A clear location, packed: no thread is anywhere.
const CLEAR: u64 = 0;

/// The bit a packed location that is set always has, so that no set location packs to [`CLEAR`].
const SET: u64 = 1 << 63;

impl Location {
    /// Slot `slot` of processor `cpu`'s run queue.
    pub const fn new(cpu: u32, slot: u16) -> Self {
        Self { cpu, slot }
    }

    /// The processor, by CPU index.
    pub const fn cpu(self) -> u32 {
        self.cpu
    }

    /// The run-queue slot.
    pub const fn slot(self) -> u16 {
        self.slot
    }

    /// One word: [`SET`], the processor in bits 16 to 47 and the slot in bits 0 to 15.
    const fn pack(self) -> u64 {
        SET | ((self.cpu as u64) << 16) | (self.slot as u64)
    }

    /// The location a word from [`Location::pack`] holds, or `None` for [`CLEAR`] (any word
    /// without [`SET`]).
    const fn unpack(word: u64) -> Option<Self> {
        if word & SET == 0 {
            return None;
        }
        // Each `as` keeps the bits `pack` put there: 32 for the processor, 16 for the slot.
        Some(Self {
            cpu: (word >> 16) as u32,
            slot: word as u16,
        })
    }
}

/// A record's state before its first thread, and once its thread has been reaped.
pub trait Empty {
    /// The empty state.
    const EMPTY: Self;
}

/// One thread's record, from its spawn to its reap; see the module documentation.
pub struct Record<S> {
    /// Changed only by [`Block::take`], while the location is clear.
    generation: prim::AtomicU32,
    /// A packed [`Location`], or [`CLEAR`].
    location: prim::AtomicU64,
    state: SpinLock<S>,
}

impl<S> core::fmt::Debug for Record<S> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Record")
    }
}

impl<S: Empty> Record<S> {
    /// A record no thread has held: generation 0, location clear, state empty.
    #[cfg(not(all(test, loom)))]
    const fn new() -> Self {
        Self {
            generation: prim::AtomicU32::new(0),
            location: prim::AtomicU64::new(CLEAR),
            state: SpinLock::new(S::EMPTY),
        }
    }

    /// A record no thread has held: generation 0, location clear, state empty.
    #[cfg(all(test, loom))]
    fn new() -> Self {
        Self {
            generation: prim::AtomicU32::new(0),
            location: prim::AtomicU64::new(CLEAR),
            state: SpinLock::new(S::EMPTY),
        }
    }
}

impl<S> Record<S> {
    /// Where the thread is, or `None` if it is not queued anywhere: not yet queued, exited, or the
    /// record is free.
    ///
    /// Read without the queue's lock, this only says which lock to take: it may change at once.
    /// Read again under the lock of the processor it names and found the same, it holds until
    /// that lock is released.
    pub fn location(&self) -> Option<Location> {
        // Acquire: pairs with the Release of whoever wrote it, though the queue lock the reader
        // then takes is what orders what it does next.
        Location::unpack(self.location.load(Acquire))
    }

    /// The thread is now in slot `at.slot()` of processor `at.cpu()`'s run queue. The caller
    /// holds that queue's lock and has queued the thread there (a spawn), or (from ADR-0020 S6)
    /// holds that lock and the lock of the queue it came from (a move).
    pub fn set_location(&self, at: Location) {
        self.location.store(at.pack(), Release);
    }

    /// The thread has left its queue for good: its exit, in the critical section under the lock
    /// of the queue that freed its slot.
    pub fn clear_location(&self) {
        self.location.store(CLEAR, Release);
    }

    /// The record's generation: that of the thread a name for it must give.
    ///
    /// Relaxed, and meaningful only to a holder of the lock its location names: the generation is
    /// written by [`Block::take`] before the spawn sets the location under that lock, and not
    /// again until the location has been cleared under it.
    pub fn generation(&self) -> u32 {
        self.generation.load(Relaxed)
    }

    /// The thread's state, behind the record's own lock (see "Locks").
    pub fn state(&self) -> &SpinLock<S> {
        &self.state
    }
}

/// `N` records (at most 64), and which of them are held.
pub struct Block<S, const N: usize> {
    records: [Record<S>; N],
    /// Bit `i` set: record `i` is held, from its [`Block::take`] to its [`Block::give_back`].
    held: SpinLock<u64>,
}

impl<S, const N: usize> core::fmt::Debug for Block<S, N> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Block")
    }
}

/// Bit `index` of a word, or no bit past 63.
fn bit(index: usize) -> u64 {
    u32::try_from(index)
        .ok()
        .and_then(|shift| 1u64.checked_shl(shift))
        .unwrap_or(0)
}

impl<S: Empty, const N: usize> Block<S, N> {
    /// A block's records are the bits of one word.
    const FITS: () = assert!(N <= 64, "a block holds at most 64 records");

    /// `N` records, none held.
    #[cfg(not(all(test, loom)))]
    pub const fn new() -> Self {
        let () = Self::FITS;
        Self {
            records: [const { Record::new() }; N],
            held: SpinLock::new(0),
        }
    }

    /// `N` records, none held.
    #[cfg(all(test, loom))]
    pub fn new() -> Self {
        let () = Self::FITS;
        Self {
            records: core::array::from_fn(|_| Record::new()),
            held: SpinLock::new(0),
        }
    }
}

impl<S: Empty, const N: usize> Default for Block<S, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S, const N: usize> Block<S, N> {
    /// Record `index`, or `None` past `N`.
    pub fn get(&self, index: usize) -> Option<&Record<S>> {
        self.records.get(index)
    }

    /// A free record for a new thread, with its next generation: its index and that generation.
    /// `None` if every record is held. The record's location is clear (a record is given back
    /// only with its location clear) and its state is what its last thread's reap left; the
    /// caller fills the state, queues the thread and then sets the location.
    pub fn take(&self) -> Option<(usize, u32)> {
        let mut held = self.held.lock();
        let index = (0..N).find(|&i| *held & bit(i) == 0)?;
        let record = self.records.get(index)?;
        *held |= bit(index);
        // Under the allocator's lock and with the location clear: no holder of a queue lock can
        // be comparing this generation (see `Record::generation`).
        let generation = record.generation.load(Relaxed).wrapping_add(1);
        record.generation.store(generation, Relaxed);
        Some((index, generation))
    }

    /// Gives back record `index`: its thread has been reaped, or was never queued. Refused, the
    /// record staying held, if it is not held or its location is set, so a record whose thread
    /// can still be found through it is never handed to another. The state is the caller's to
    /// have emptied.
    pub fn give_back(&self, index: usize) -> bool {
        let Some(record) = self.records.get(index) else {
            return false;
        };
        if record.location().is_some() {
            return false;
        }
        let mut held = self.held.lock();
        if *held & bit(index) == 0 {
            return false;
        }
        *held &= !bit(index);
        true
    }

    /// How many records are held.
    pub fn held(&self) -> u32 {
        self.held.lock().count_ones()
    }
}

/// The most times [`wake`] looks for its thread under a queue's lock (ADR-0020 S1).
pub const WAKE_TRIES: u32 = 64;

/// What [`wake`] needs of the kernel: each processor's run queue, under its lock.
pub trait Queues {
    /// A processor's run queue.
    type Queue;

    /// Runs `f` on processor `cpu`'s run queue with that queue's lock held (in the kernel, with
    /// interrupts off), or returns `None` if there is no such processor.
    fn locked<R>(&self, cpu: u32, f: impl FnOnce(&mut Self::Queue) -> R) -> Option<R>;

    /// Wakes the thread in `slot` of `queue`, as `RunQueue::wake` does for the slot's own
    /// generation: whether a blocked thread became ready. A thread not blocked keeps the wake-up
    /// for its next block.
    fn wake_slot(queue: &mut Self::Queue, slot: u16) -> bool;
}

/// How a [`wake`] ended.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The wake-up reached the thread, in processor `cpu`'s queue. `ready`: it was blocked and is
    /// now ready, so that processor should reschedule; otherwise it keeps the wake-up for its
    /// next block.
    Reached {
        /// Whether a blocked thread became ready.
        ready: bool,
        /// The processor whose queue holds it.
        cpu: u32,
    },
    /// The record holds another thread: the name's thread has been reaped.
    Stale,
    /// The record's location is clear: its thread has exited, and has no wake-up to keep.
    Gone,
    /// The location had changed at each of [`WAKE_TRIES`] looks under the lock it named.
    Exhausted,
}

/// What a [`wake`] did: how it ended, and how many times it found, under the lock of the queue it
/// had read, that the location had changed, and followed it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Wake {
    /// How it ended.
    pub outcome: Outcome,
    /// Locations followed.
    pub retries: u32,
}

// Only their names, as for `Location`.
impl core::fmt::Debug for Outcome {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Reached { .. } => "Reached",
            Self::Stale => "Stale",
            Self::Gone => "Gone",
            Self::Exhausted => "Exhausted",
        })
    }
}

impl core::fmt::Debug for Wake {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Wake")
    }
}

/// Wakes the thread `record` and `generation` name, wherever it is.
///
/// It reads the location, takes the lock of the queue that location names, and reads the
/// location again. If it has changed, a move or an exit completed in between: the lock is
/// released and the wake-up follows the new location, at most [`WAKE_TRIES`] times. If it is
/// unchanged, the thread is in that slot and cannot leave it while the lock is held; the
/// generation is compared under the lock, and the slot is woken as `RunQueue::wake` wakes it,
/// with its pending wake-up for a thread not yet blocked. A clear location means the thread has
/// exited: it is waiting for nothing.
///
/// While no thread moves, a location changes only at a spawn and at an exit, and after an exit it
/// stays clear until the record's next spawn: so a wake-up follows at most one location, and
/// that only when it read its thread's location before the exit and locked after it. The kernel
/// counts what it follows, and its boot proofs require 0 (TASKS item 92).
pub fn wake<S, Q: Queues>(queues: &Q, record: &Record<S>, generation: u32) -> Wake {
    let mut retries = 0;
    while retries < WAKE_TRIES {
        let Some(at) = record.location() else {
            return Wake {
                outcome: Outcome::Gone,
                retries,
            };
        };
        let looked = queues.locked(at.cpu(), |queue| {
            // A move or an exit since the first read: this queue no longer holds the thread.
            if record.location() != Some(at) {
                return None;
            }
            Some(if record.generation() == generation {
                Outcome::Reached {
                    ready: Q::wake_slot(queue, at.slot()),
                    cpu: at.cpu(),
                }
            } else {
                Outcome::Stale
            })
        });
        match looked {
            Some(Some(outcome)) => return Wake { outcome, retries },
            Some(None) => retries = retries.saturating_add(1),
            // A location names only a processor the kernel queued the thread on.
            None => {
                return Wake {
                    outcome: Outcome::Gone,
                    retries,
                };
            }
        }
    }
    Wake {
        outcome: Outcome::Exhausted,
        retries,
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use core::cell::{Cell, RefCell};

    use super::*;

    #[test]
    fn a_location_packs_and_unpacks_at_its_edges() {
        for at in [
            Location::new(0, 0),
            Location::new(0, u16::MAX),
            Location::new(u32::MAX, 0),
            Location::new(u32::MAX, u16::MAX),
            Location::new(64, 15),
        ] {
            let word = at.pack();
            assert_ne!(word, CLEAR, "a set location never packs to the clear one");
            assert_eq!(Location::unpack(word), Some(at));
            assert_eq!(
                (at.cpu(), at.slot()),
                (
                    Location::unpack(word).map_or(0, Location::cpu),
                    Location::unpack(word).map_or(0, Location::slot)
                )
            );
        }
    }

    #[test]
    fn the_clear_location_is_none_and_distinct_from_every_set_one() {
        assert_eq!(Location::unpack(CLEAR), None);
        // Any word without the set bit reads as clear, whatever else it holds.
        assert_eq!(Location::unpack(!SET), None);
        // The processor and the slot do not overlap in the packed word.
        assert_ne!(Location::new(1, 0).pack(), Location::new(0, 1).pack());
        assert_ne!(
            Location::new(0, 1 << 15).pack(),
            Location::new(1 << 15, 0).pack()
        );
    }

    /// The host tests' state: a number, 0 when empty.
    impl Empty for u32 {
        const EMPTY: Self = 0;
    }

    #[test]
    fn a_record_starts_clear_and_keeps_the_location_it_is_given() {
        let block = Block::<u32, 2>::new();
        let record = block.get(1).expect("two records");
        assert_eq!(record.location(), None);
        assert_eq!(record.generation(), 0);
        assert_eq!(*record.state().lock(), 0);
        record.set_location(Location::new(3, 7));
        assert_eq!(record.location(), Some(Location::new(3, 7)));
        record.clear_location();
        assert_eq!(record.location(), None);
        assert!(block.get(2).is_none());
    }

    #[test]
    fn a_block_hands_out_every_record_once_then_refuses() {
        let block = Block::<u32, 4>::new();
        let mut taken: Vec<_> = (0..4)
            .map(|_| block.take().expect("a free record"))
            .collect();
        taken.sort_unstable();
        assert_eq!(taken, [(0, 1), (1, 1), (2, 1), (3, 1)]);
        assert_eq!(block.held(), 4);
        assert_eq!(block.take(), None, "a full block refuses");
    }

    #[test]
    fn a_record_given_back_comes_back_with_a_new_generation() {
        let block = Block::<u32, 2>::new();
        assert_eq!(block.take(), Some((0, 1)));
        assert_eq!(block.take(), Some((1, 1)));
        assert!(block.give_back(0));
        assert_eq!(block.held(), 1);
        assert_eq!(
            block.take(),
            Some((0, 2)),
            "the same record, its next generation"
        );
        assert_eq!(block.get(0).map(Record::generation), Some(2));
        // The generation wraps rather than stopping a record from being reused.
        block
            .get(1)
            .expect("two records")
            .generation
            .store(u32::MAX, Relaxed);
        assert!(block.give_back(1));
        assert_eq!(block.take(), Some((1, 0)));
    }

    #[test]
    fn a_record_not_held_or_still_located_is_not_given_back() {
        let block = Block::<u32, 2>::new();
        assert!(!block.give_back(0), "never taken");
        assert!(!block.give_back(2), "no such record");
        let (index, _) = block.take().expect("a free record");
        let record = block.get(index).expect("taken from the block");
        record.set_location(Location::new(0, 1));
        assert!(!block.give_back(index), "its thread can still be found");
        assert_eq!(block.held(), 1);
        record.clear_location();
        assert!(block.give_back(index));
        assert!(!block.give_back(index), "given back twice");
        assert_eq!(block.held(), 0);
    }

    #[test]
    fn a_block_of_sixty_four_uses_every_bit() {
        let block = Block::<u32, 64>::new();
        for i in 0..64 {
            assert_eq!(block.take(), Some((i, 1)));
        }
        assert_eq!(block.take(), None);
        assert!(block.give_back(63));
        assert_eq!(block.take(), Some((63, 2)));
    }

    /// Processors the stub has: a location past them names no queue.
    const CPUS: u32 = 1000;

    /// What a stub queue was asked: its processor, and every slot woken there so far.
    struct Calls {
        cpu: u32,
        woken: Vec<(u32, u16)>,
    }

    /// Queues that count their locks and wake-ups, and can change the record's location as a lock
    /// is taken, after `wake` has read it: a move, or an exit, completing in between.
    struct Stub<'a> {
        record: &'a Record<u32>,
        /// Locks still to come before which the record moves to the next processor.
        moves: Cell<u32>,
        /// Whether the record's thread exits before the next lock.
        exits: Cell<bool>,
        locks: Cell<u32>,
        woken: RefCell<Vec<(u32, u16)>>,
    }

    impl<'a> Stub<'a> {
        fn new(record: &'a Record<u32>) -> Self {
            Self {
                record,
                moves: Cell::new(0),
                exits: Cell::new(false),
                locks: Cell::new(0),
                woken: RefCell::new(Vec::new()),
            }
        }
    }

    impl Queues for Stub<'_> {
        type Queue = Calls;

        fn locked<R>(&self, cpu: u32, f: impl FnOnce(&mut Calls) -> R) -> Option<R> {
            if cpu >= CPUS {
                return None;
            }
            self.locks.set(self.locks.get() + 1);
            if self.exits.replace(false) {
                self.record.clear_location();
            } else if self.moves.get() > 0 {
                self.moves.set(self.moves.get() - 1);
                let at = self.record.location().expect("a located thread moves");
                self.record
                    .set_location(Location::new(at.cpu() + 1, at.slot()));
            }
            let mut calls = Calls {
                cpu,
                woken: self.woken.take(),
            };
            let result = f(&mut calls);
            self.woken.replace(calls.woken);
            Some(result)
        }

        fn wake_slot(queue: &mut Calls, slot: u16) -> bool {
            queue.woken.push((queue.cpu, slot));
            true
        }
    }

    /// A record taken from a block and located at slot 5 of processor 2, and its generation.
    fn located(block: &Block<u32, 2>) -> (&Record<u32>, u32) {
        let (index, generation) = block.take().expect("a free record");
        let record = block.get(index).expect("taken from the block");
        record.set_location(Location::new(2, 5));
        (record, generation)
    }

    const fn reached(cpu: u32, retries: u32) -> Wake {
        Wake {
            outcome: Outcome::Reached { ready: true, cpu },
            retries,
        }
    }

    #[test]
    fn a_wake_up_reaches_a_thread_that_stays_put_at_the_first_look() {
        let block = Block::new();
        let (record, generation) = located(&block);
        let stub = Stub::new(record);
        assert_eq!(wake(&stub, record, generation), reached(2, 0));
        assert_eq!(stub.locks.get(), 1);
        assert_eq!(*stub.woken.borrow(), [(2, 5)]);
    }

    #[test]
    fn a_wake_up_follows_a_location_that_changed_before_its_lock() {
        let block = Block::new();
        let (record, generation) = located(&block);
        let stub = Stub::new(record);
        stub.moves.set(1);
        assert_eq!(wake(&stub, record, generation), reached(3, 1));
        assert_eq!(stub.locks.get(), 2);
        assert_eq!(*stub.woken.borrow(), [(3, 5)], "woken where it went, once");
    }

    #[test]
    fn a_wake_up_finds_no_one_at_a_clear_location() {
        let block = Block::new();
        let (record, generation) = located(&block);
        record.clear_location();
        let stub = Stub::new(record);
        let gone = Wake {
            outcome: Outcome::Gone,
            retries: 0,
        };
        assert_eq!(wake(&stub, record, generation), gone);
        assert_eq!(
            stub.locks.get(),
            0,
            "no queue is locked for an exited thread"
        );
        // The one way a wake-up follows a location while no thread moves: it read the location
        // before its thread's exit and locked after it.
        record.set_location(Location::new(2, 5));
        stub.exits.set(true);
        let followed = Wake {
            outcome: Outcome::Gone,
            retries: 1,
        };
        assert_eq!(wake(&stub, record, generation), followed);
        assert_eq!(stub.locks.get(), 1);
        assert!(stub.woken.borrow().is_empty());
    }

    #[test]
    fn a_stale_name_wakes_no_one() {
        let block = Block::new();
        let (record, generation) = located(&block);
        let stub = Stub::new(record);
        let stale = Wake {
            outcome: Outcome::Stale,
            retries: 0,
        };
        assert_eq!(wake(&stub, record, generation.wrapping_sub(1)), stale);
        assert_eq!(wake(&stub, record, generation.wrapping_add(1)), stale);
        assert_eq!(stub.locks.get(), 2, "compared under the lock");
        assert!(stub.woken.borrow().is_empty(), "the slot is never woken");
    }

    #[test]
    fn a_wake_up_whose_thread_moves_at_every_look_gives_up_after_the_last_try() {
        let block = Block::new();
        let (record, generation) = located(&block);
        let stub = Stub::new(record);
        stub.moves.set(u32::MAX);
        let exhausted = Wake {
            outcome: Outcome::Exhausted,
            retries: WAKE_TRIES,
        };
        assert_eq!(wake(&stub, record, generation), exhausted);
        assert_eq!(stub.locks.get(), WAKE_TRIES);
        assert!(stub.woken.borrow().is_empty());
        // One move fewer than the tries: the last look finds it.
        let block = Block::new();
        let (record, generation) = located(&block);
        let stub = Stub::new(record);
        stub.moves.set(WAKE_TRIES - 1);
        assert_eq!(
            wake(&stub, record, generation),
            reached(2 + WAKE_TRIES - 1, WAKE_TRIES - 1)
        );
    }

    #[test]
    fn a_location_naming_no_processor_is_no_one_to_wake() {
        let block = Block::new();
        let (record, generation) = located(&block);
        record.set_location(Location::new(CPUS, 0));
        let stub = Stub::new(record);
        let gone = Wake {
            outcome: Outcome::Gone,
            retries: 0,
        };
        assert_eq!(wake(&stub, record, generation), gone);
    }
}
