//! The kernel's thread records (ADR-0020 S1; TASKS item 92): every thread's state, from its spawn
//! to its reap, in a record of its own, which names it to the wait queues and ports
//! (`sched::ThreadRef`) and through which a wake-up finds it wherever it runs. The record, the
//! block and that wake-up are `origin_sync::threads`; this module says what a record holds here,
//! and keeps one block of records per processor.
//!
//! **Numbers.** Record `index` of processor `cpu`'s block is number `cpu` × [`PER_CPU`] +
//! `index`. A spawn takes a record from the block of the processor it queues the thread on, and
//! the thread's reap gives it back there.
//!
//! **Enough.** While no thread moves, a processor's records are held by the threads of its run
//! queue (at most `CAPACITY` − 1: slot 0 is the boot or idle flow, which has none, ADR-0020 S2,
//! point 4) and by its zombies awaiting their reap (at most `CAPACITY`, `sched`'s list). The reap
//! gives a zombie's record back as it takes the zombie's state out, before that state is dropped,
//! so a reap in progress holds none. That is 2 × `CAPACITY` − 1 = 31 records, and a block has
//! [`PER_CPU`] = 32, so a block never runs out before its queue does. A spawn that finds none is
//! still refused safely, as a full queue refuses it (`QueueError::Full`).
//!
//! **Memory.** Every block is made when the processors are discovered (`percpu::size_per_cpu`;
//! ADR-0008 Amendment 1, D1), never moved and never freed: the boot processor's is static, and the
//! others are one allocation. Nothing is allocated when a thread starts. A processor's block is in
//! the same per-processor table as its [`Table`] of zombies, [`THREADS`], so the kernel keeps the
//! number of per-processor tables it had.
#![forbid(unsafe_code)]

use origin_frame::objref::ObjectRef;
use origin_frame::percpu::PerCpu;
use origin_frame::stack::{FpuArea, GuardedStack};
use origin_frame::sync::SpinLock;
use origin_frame::sync::exits::Mark;
use origin_frame::sync::threads::{Block, Empty, Record};

use crate::cpu::CpuIndex;
use crate::process::UserContext;
use crate::runqueue::CAPACITY;

/// Records in each processor's block.
pub(crate) const PER_CPU: usize = 32;

const _: () = assert!(
    PER_CPU >= 2 * CAPACITY - 1,
    "a block holds a record for every thread its run queue and zombie list can hold"
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

/// One processor's threads: its block of records, and its [`Table`].
pub(crate) struct Processor {
    records: Block<State, PER_CPU>,
    table: SpinLock<Table>,
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

/// The block and index of record `number`.
fn split(number: u32) -> Option<(usize, usize)> {
    let number = usize::try_from(number).ok()?;
    Some((number / PER_CPU, number % PER_CPU))
}

/// Record `number`, if there is one.
pub(crate) fn record(number: u32) -> Option<&'static Record<State>> {
    let (block, index) = split(number)?;
    THREADS.get(block)?.records.get(index)
}

/// A record for a new thread on processor `cpu`, from that processor's block: its number and
/// its new generation. `None` if the block holds no free record.
pub(crate) fn take(cpu: CpuIndex) -> Option<(u32, u32)> {
    let block = &THREADS.get(cpu.get())?.records;
    let (index, generation) = block.take()?;
    let number = cpu
        .get()
        .checked_mul(PER_CPU)
        .and_then(|first| first.checked_add(index))
        .and_then(|number| u32::try_from(number).ok());
    if number.is_none() {
        // A processor index past what a `u32` numbers: the record goes back unused.
        let _ = block.give_back(index);
    }
    Some((number?, generation))
}

/// Gives record `number` back to its block: its thread was reaped, or never queued. Its location
/// is clear and its state empty. `false` if the block refused it (see `Block::give_back`).
pub(crate) fn give_back(number: u32) -> bool {
    split(number)
        .and_then(|(block, index)| Some(THREADS.get(block)?.records.give_back(index)))
        .unwrap_or(false)
}
