//! The kernel's thread records (ADR-0020 S1; TASKS item 92): every thread's state, from its spawn
//! to its reap, in a record of its own, which names it to the wait queues and ports
//! (`sched::ThreadRef`) and through which a wake-up finds it wherever it runs. The record, the
//! block and that wake-up are `origin_sync::threads`; this module says what a record holds here,
//! and keeps one block of records per processor.
//!
//! **Numbers.** Record `index` of processor `cpu`'s block is number `cpu` × [`PER_CPU`] +
//! `index`. A spawn takes a record from the block of the processor it queues the thread on, or,
//! if that block is used up, from the next block round that has one (TASKS item 96: [`take`]), and
//! the thread's reap gives it back to the block it came from, by its number.
//!
//! **Enough.** While no thread moves, a processor's records are held by the threads of its run
//! queue (at most `CAPACITY` − 1: slot 0 is the boot or idle flow, which has none, ADR-0020 S2,
//! point 4) and by its zombies awaiting their reap (at most `CAPACITY`, `sched`'s list). The reap
//! gives a zombie's record back as it takes the zombie's state out, before that state is dropped,
//! so a reap in progress holds none. That is 2 × `CAPACITY` − 1 = 31 records. A spawn takes its
//! record before its push, under the queue's lock, so a spawn that is about to find the queue
//! full holds one more for a moment: 2 × `CAPACITY` = 32 = [`PER_CPU`], so a block never runs out
//! before its queue does (the review of TASKS item 92, R92-7). A spawn that finds none is still
//! refused safely, as a full queue refuses it (`QueueError::Full`).
//!
//! **With moves** (ADR-0020 S6; TASKS item 96), a processor's records can be held by threads that
//! moved to others, so the count above no longer holds block by block. It holds for the machine:
//! every held record belongs to a queued thread (at most `CAPACITY` − 1 per processor) or a
//! zombie (at most `CAPACITY` per processor), wherever it came from, so at most 31 × P are held
//! of the 32 × P there are. [`take`] looks in every block before it refuses, so a spawn fails for
//! want of a record only if none is free anywhere. Its search past the own block runs only when
//! that block is used up, which no boot does today.
//!
//! **Accounted for.** Every settle checks that the records the blocks hold are the queued threads'
//! and the zombies', counted for the machine, and that no record's give-back has been refused
//! ([`unaccounted`]; the review's R92-2). For the machine, not block by block: a thread that moved,
//! or took its record from another processor's block, is queued or listed on one processor with
//! a record of another's (the review of TASKS item 96, R96-3).
//!
//! **Memory.** Every block is made when the processors are discovered (`percpu::size_per_cpu`;
//! ADR-0008 Amendment 1, D1), never moved and never freed: the boot processor's is static, and the
//! others are one allocation. Nothing is allocated when a thread starts. A processor's block is in
//! the same per-processor table as its [`Table`] of zombies, [`THREADS`], so the kernel keeps the
//! number of per-processor tables it had.
#![forbid(unsafe_code)]

use core::sync::atomic::{AtomicU64, Ordering};

use origin_frame::objref::ObjectRef;
use origin_frame::percpu::PerCpu;
use origin_frame::stack::{FpuArea, GuardedStack};
use origin_frame::sync::SpinLock;
use origin_frame::sync::exits::Mark;
use origin_frame::sync::threads::{Block, Empty, Record, take_from_any};

use crate::cpu::{CpuIndex, ThisCpu, without_interrupts};
use crate::process::UserContext;
use crate::runqueue::CAPACITY;

/// Records in each processor's block.
pub(crate) const PER_CPU: usize = 32;

const _: () = assert!(
    PER_CPU >= 2 * CAPACITY,
    "a block holds a record for every thread its run queue and zombie list can hold, and a spawn's"
);

// A record's location names its slot in 16 bits (`origin_sync::threads::Location`).
const _: () = assert!(CAPACITY <= u16::MAX as usize, "a slot fits a location");

// A record is two 64-byte lines, as TASKS item 92 measured it: its pin count (TASKS item 93) and
// its movable flag (TASKS item 96) took room that was there, and did not grow it (the
// coordinating session's rulings Q9 of TASKS item 93 and Q3 of TASKS item 96).
const _: () = assert!(
    core::mem::size_of::<Record<State>>() == 128,
    "a record is 128 bytes, its pin count and movable flag included"
);

/// What a record holds: everything a thread owns from its spawn to its reap.
pub(crate) struct State {
    /// Its stack.
    pub(crate) stack: Option<GuardedStack>,
    /// Its FPU/SIMD save area.
    pub(crate) fpu: Option<FpuArea>,
    /// What it runs in (`None`: a kernel thread), and the references that keep its process and
    /// address space until its processor has left them.
    pub(crate) user: Option<ObjectRef<UserContext>>,
    /// Its count as ending, from the moment it counted itself ([`crate::sched::end_with`]) until
    /// its exit consumes it (TASKS item 79).
    pub(crate) ending: Option<Mark<'static>>,
}

impl Empty for State {
    const EMPTY: Self = Self {
        stack: None,
        fpu: None,
        user: None,
        ending: None,
    };
}

/// One processor's threads: its block of records, its [`Table`], and the moves its wake-ups
/// followed ([`Follows`]).
pub(crate) struct Processor {
    records: Block<State, PER_CPU>,
    table: SpinLock<Table>,
    follows: Follows,
}

/// The moves of movable threads that this processor's wake-ups followed (ADR-0020 S1; TASKS item
/// 96, the coordinating session's rulings Q8 and Q13, and D93-2), by the waker's processor. Only
/// this processor writes them, inside a wake-up's interrupts-off section, out of line
/// (`sched::wake`), with plain loads and stores; any processor reads them, for the delivery
/// proof's line. A wake-up that follows nothing touches none of them.
pub(crate) struct Follows {
    /// Moves followed, in all.
    all: AtomicU64,
    /// The most one wake-up followed.
    most: AtomicU64,
    /// Wake-ups that followed more than 8: with moves paced (S10, and `sched::migrate`'s rule),
    /// none should.
    over_eight: AtomicU64,
}

impl Follows {
    const fn new() -> Self {
        Self {
            all: AtomicU64::new(0),
            most: AtomicU64::new(0),
            over_eight: AtomicU64::new(0),
        }
    }

    /// Notes a wake-up that followed `follows` moves. This processor only, interrupts off.
    pub(crate) fn note(&self, follows: u32) {
        let follows = u64::from(follows);
        self.all.store(
            self.all.load(Ordering::Relaxed).saturating_add(follows),
            Ordering::Relaxed,
        );
        if follows > self.most.load(Ordering::Relaxed) {
            self.most.store(follows, Ordering::Relaxed);
        }
        if follows > 8 {
            self.over_eight.store(
                self.over_eight.load(Ordering::Relaxed).saturating_add(1),
                Ordering::Relaxed,
            );
        }
    }

    /// Moves followed, the most one wake-up followed, and the wake-ups that followed more than 8.
    pub(crate) fn read(&self) -> (u64, u64, u64) {
        (
            self.all.load(Ordering::Relaxed),
            self.most.load(Ordering::Relaxed),
            self.over_eight.load(Ordering::Relaxed),
        )
    }
}

/// One processor's threads that are not in a record: the exited ones awaiting their reap
/// (`sched::reap`), and the boot or idle flow's FPU area. Locked after the run queue and before any
/// record, always with interrupts disabled.
pub(crate) struct Table {
    /// The records of exited threads awaiting their reap: each still holds its thread's stack,
    /// FPU area and, for a user thread, the references that keep its process and address space
    /// until this processor has left them.
    pub(crate) zombies: [Option<u32>; CAPACITY],
    /// The boot or idle flow's FPU/SIMD save area (slot 0, which has no record).
    pub(crate) idle_fpu: Option<FpuArea>,
}

impl Processor {
    /// No thread.
    const fn new() -> Self {
        Self {
            records: Block::new(),
            table: SpinLock::new(Table {
                zombies: [None; CAPACITY],
                idle_fpu: None,
            }),
            follows: Follows::new(),
        }
    }
}

/// Each processor's threads, by CPU index.
pub(crate) static THREADS: PerCpu<Processor> = PerCpu::new(
    "framekernel::threads::THREADS",
    Processor::new(),
    Processor::new,
);

/// Processor `cpu`'s [`Table`].
pub(crate) fn table(cpu: usize) -> Option<&'static SpinLock<Table>> {
    Some(&THREADS.get(cpu)?.table)
}

/// This processor's [`Table`], lent for the life of the witness `me` was read under (ADR-0020 S3;
/// TASKS item 93).
pub(crate) fn table_here(me: ThisCpu<'_>) -> Option<&SpinLock<Table>> {
    Some(&THREADS.this(me)?.table)
}

/// This processor's [`Follows`], lent for the life of the witness `me` was read under.
pub(crate) fn follows_here(me: ThisCpu<'_>) -> Option<&Follows> {
    Some(&THREADS.this(me)?.follows)
}

/// The follows of every processor's wake-ups, summed, with the most one wake-up followed anywhere:
/// moves followed, the most, and the wake-ups past 8.
pub(crate) fn follows() -> (u64, u64, u64) {
    THREADS
        .iter()
        .map(|processor| processor.follows.read())
        .fold((0, 0, 0), |(all, most, over), (a, m, o)| {
            (all.saturating_add(a), most.max(m), over.saturating_add(o))
        })
}

/// What a run-queue slot holds of its thread's record (`RunQueue<Placed>`): its number, for the
/// thread's name and the zombie list, and the record itself, resolved once when the thread is
/// placed in the queue, so that a switch reaches the thread's state without a lookup (TASKS item
/// 92; the coordinating session's second ruling on its Q6). A record is never moved or freed, so
/// the reference stays good wherever the thread runs.
///
/// It holds the record's identity twice, so only [`take`] makes one, from one index of one block,
/// and its fields are read through [`Placed::number`] and [`Placed::record`] alone: a move (ADR-0020
/// S6) carries the value its slot holds, and builds none (the review of TASKS item 92, R92-15).
#[derive(Clone, Copy)]
pub(crate) struct Placed {
    number: u32,
    record: &'static Record<State>,
}

impl Placed {
    /// The record's number: the thread's name, its zombie entry and its give-back.
    #[inline]
    pub(crate) const fn number(self) -> u32 {
        self.number
    }

    /// The record: the thread's state, location and generation.
    #[inline]
    pub(crate) const fn record(self) -> &'static Record<State> {
        self.record
    }
}

/// Record `number`, if there is one.
pub(crate) fn record(number: u32) -> Option<&'static Record<State>> {
    let (block, index) = Block::<State, PER_CPU>::split(number)?;
    THREADS.get(block)?.records.get(index)
}

/// The number of `record` ([`record`]'s inverse), found by its address in each processor's block
/// in turn (`Block::index_of`): `None` if no block holds it. Out of line only: a wake-up that met
/// an inconsistency finds its thread's number this way (`sched::wake_inconsistent`; TASKS item
/// 96), so that the wake-up's location loop keeps no register for the number.
pub(crate) fn number_of(record: &Record<State>) -> Option<u32> {
    (0..THREADS.indexes()).find_map(|block| {
        let index = THREADS.get(block)?.records.index_of(record)?;
        Block::<State, PER_CPU>::number(block, index)
    })
}

/// A record for a new thread on processor `cpu`, from that processor's block or, if it is used up,
/// from the next block round that has one (`origin_sync::threads::take_from_any`; TASKS item 96),
/// given its next generation (`Block::take`), and resolved for its run-queue slot ([`Placed`]).
/// `None` if no block holds a free record (the review's R92-18).
pub(crate) fn take(cpu: CpuIndex) -> Option<Placed> {
    let (from, index, _) = take_from_any(cpu.get(), THREADS.indexes(), |b| {
        THREADS.get(b).map(|processor| &processor.records)
    })?;
    let block = &THREADS.get(from)?.records;
    let placed = Block::<State, PER_CPU>::number(from, index)
        .zip(block.get(index))
        .map(|(number, record)| Placed { number, record });
    if placed.is_none() {
        // A processor index past what a `u32` numbers (the record itself is always there): the
        // record goes back unused.
        give_back_index(block, index);
    }
    placed
}

/// Give-backs a block refused since the boot began: each a record that stays held for good.
static REFUSED: AtomicU64 = AtomicU64::new(0);

/// Gives record `index` back to `block`, counting a refusal ([`REFUSED`]).
fn give_back_index(block: &Block<State, PER_CPU>, index: usize) {
    if !block.give_back(index) {
        REFUSED.fetch_add(1, Ordering::Relaxed);
    }
}

/// Gives record `number` back to its block: its thread was reaped, or never queued. Its location
/// is clear and its state empty. A block that refuses it (see `Block::give_back`) keeps it held for
/// good, which is counted, and every later settle names ([`unaccounted`]).
pub(crate) fn give_back(number: u32) {
    let found = Block::<State, PER_CPU>::split(number)
        .and_then(|(block, index)| Some((THREADS.get(block)?, index)));
    match found {
        Some((processor, index)) => give_back_index(&processor.records, index),
        None => {
            REFUSED.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// The records out of place: those whose give-back was refused since the boot began, and the
/// difference between the records every block holds and the machine's threads (queued, or zombies
/// awaiting their reap). 0 while every thread has its one record and every reaped thread gave its
/// record back (the review of TASKS item 92, R92-2). Summed over the machine, since a thread's
/// record may be another processor's (R96-3).
///
/// Each processor is read under its run queue's lock and then its table's, so no spawn, exit or
/// reap there is half done: a spawn takes its record and queues the thread under the queue's lock,
/// an exit lists its zombie or gives its record back under both, and a reap takes a zombie off
/// the list and gives its record back under the table's. The processors are read one after
/// another, so a move during the reading could count its thread twice or not at all. A settle
/// runs on the boot flow, the only caller of `sched::migrate`, so no directed move is under way
/// then. An idle pull (TASKS item 98) could be: it takes only a thread declared for balancing,
/// and every proof that spawns such threads waits for them all to end before it settles, so no
/// thread a pull may take exists while a settle reads.
pub(crate) fn unaccounted() -> u64 {
    let (mut held, mut threads) = (0u64, 0u64);
    for (cpu, processor) in THREADS.iter().enumerate() {
        let Some(queue) = crate::sched::QUEUES.get(cpu) else {
            continue;
        };
        let (here_held, here_threads) = without_interrupts(|| {
            let queue = queue.lock();
            let table = processor.table.lock();
            let zombies = table.zombies.iter().flatten().count();
            (
                processor.records.held(),
                queue.live().saturating_sub(1).saturating_add(zombies),
            )
        });
        held = held.saturating_add(u64::from(here_held));
        threads = threads.saturating_add(u64::try_from(here_threads).unwrap_or(u64::MAX));
    }
    REFUSED
        .load(Ordering::Relaxed)
        .saturating_add(held.abs_diff(threads))
}
