//! The two boot proofs of moving threads between processors (ADR-0020 S6, S20 and S23; TASKS item
//! 96, the contract's §3): **"thread migration: integrity"** (ADR-0013 R1 (b), Refuse) and
//! **"thread migration: delivery"** (R2, Withdraw: "no thread moves").
//!
//! Both run at the end of `sched::prove`, after every other proof there, integrity first, each
//! between two settles with a compare of the heap, the frames and the stacks, as the IPC proof's
//! compare is made. Only their own threads are movable (`sched::spawn_movable`), and every one of
//! them has ended and been reaped before the next proof starts, so no other proof's scheduling,
//! figure or verdict changes (S23). The mover is the boot flow, which never moves (S2, point 4).
//! It moves the proofs' threads among the processors that take part in moves
//! (`smp::Online::takes_part_in_moves`), at most once per thread per tick of its own processor:
//! S10's rate stands in until TASKS item 98 builds it (the coordinating session's ruling Q13).
//!
//! **Integrity.** Two movable threads per processor that takes part run while the boot flow moves
//! them: for 2 s, and on until every processor has been a source and a destination of a move,
//! within a budget of 30 s, so that how many moves a loaded machine allows in 2 s cannot fail it
//! (ADR-0013 R1 (b); the review of TASKS item 96, R96-4). Each holds six
//! canary values kept across its loop, which the compiler may hold in registers or spill to the
//! stack (so they are a second check of the restored stack, not of the general registers, which
//! only an `asm!` helper could pin; R96-8), 256 bytes of stack canary and four XMM canaries, which
//! only the thread's FPU area holds, and a "running on" word
//! that it takes by compare-and-swap first thing after each return from a yield or block, and
//! releases by compare-and-swap last thing before the next one, running with interrupts off in
//! between, so that nothing switches it out while it holds the word: S23's "set at switch-in,
//! cleared at switch-out", done by the thread itself at no cost to the switch path (the ruling's
//! Q9). A failed swap is a second processor running the same context. The threads pass a turn back
//! and forth in pairs through a wait queue, and yield instead in one iteration in four, so that
//! they block, are woken and yield while the boot flow moves them.
//!
//! **At capacity** (ADR-0020 N13; the coordinating session's ruling on the review of TASKS item 96,
//! R96-13). After the moves, on the first two processors that take part, the others' threads left
//! as they are: one integrity thread is placed on the source; the destination's queue is filled to
//! its capacity with pinned fillers, and moves of the thread into it must each be refused as full
//! ("queue full", the run queue filling before the table of contexts does), the thread staying on
//! its source, alive; the destination's fillers end; then the source's queue is filled, and the
//! thread must move out of it. Its own line, "thread migration: at capacity", and a pass
//! condition of integrity.
//!
//! **Delivery.** Two movable receivers per processor that takes part, one pinned sender per such
//! processor. A sender posts a token to a receiver (Release) and wakes it, by its name directly and
//! through the receiver's own wait queue. A receiver takes its tokens and waits, in turn by its
//! wait queue's `wait_until` and by blocking directly after a spin to its processor's next tick,
//! the window in which it is registered, preempted, moved, woken and then blocks (N1). After 2 s
//! the senders stop, and every token posted must be taken within 2 s more. Each proof's run is
//! timed from when every one of its threads has started (R96-4), on the monotonic clock, since
//! the movable threads read it on whichever processor they run on (ADR-0020 N10; R96-15).
//!
//! **What makes a lost wake-up fail it** (the coordinating session's ruling on control C96-2,
//! 2026-11-22). In the direct form the receiver reads its tokens once, before the spin, and blocks
//! after the spin on that reading, without reading again: a token posted during the spin reaches it
//! only through the pending wake-up its sender's wake-up left, which a move must carry (S6, N1).
//! A sender posts only to a receiver holding no token it has not taken, so a receiver whose
//! wake-up was lost is posted nothing more and stays short, which the drain fails. Reading the
//! tokens again after the spin, or posting to a receiver that holds one, would hide a lost
//! wake-up from this proof: C96-2, a move that drops the pending wake-up, then passed it.
//!
//! **A null section beside each wake-up** (report only; the coordinating session's ruling on the
//! first boot's 1.1 ms wake-up, 2026-11-22). After each timed wake-up, a sender spins on the TSC
//! with interrupts off for the median of its last [`WINDOW`] wake-up spans, the same exposure to
//! the host as a wake-up's section without its locks. The line prints, for both kinds of section,
//! the ticks spent in all, how many spans passed B18's 50 µs and how many passed 400 µs, and the
//! longest. Similar rates of long spans per tick spent mean the host descheduled a virtual
//! processor; a rate clearly higher for the wake-ups means they waited on a lock.
//!
//! **Pulled** (TASKS item 98; the coordinating session's ruling Q10 on its contract). The idle
//! pull is another kind of mover: several processors at once, each from its own idle flow, where
//! the phases above have one, the boot flow. A property that stops the boot must not rest on a
//! Report proof alone (ADR-0013 R1 (b)), so each proof has a second phase, with a line of its own,
//! in which the movers are the idle processors:
//!
//! - **integrity, pulled:** the same threads with the same "running on" word and canaries,
//!   declared for balancing (`sched::spawn_balanced`) and started together on one processor.
//!   Here they run with **interrupts on**, and in every other iteration spin for a tick and a
//!   half while they hold their word, so that a tick or a reschedule interrupt switches them out
//!   under it: the tick's kick, the upcall's kick and the ask then act on a thread that the same
//!   trap switched out, which threads that run with interrupts off never show (the review of
//!   TASKS item 98, R98-3). A thread's word stays its own while it is switched out, so a second
//!   processor that ran its context would still fail the word's swap.
//! - **delivery, pulled:** the same receivers, declared for balancing and started together on one
//!   processor, with one pinned sender beside them.
//!
//! The idle processors pull them away, and the boot flow moves each back to where it started, at
//! most once per thread per tick, so that there is always something to pull: both kinds of mover
//! work on the same threads at once. The start processor is an application processor where three
//! or more take part, so that its neighbours pull; with two, it is the boot processor, since the
//! boot processor's own flow is not an idle flow and never pulls.
//!
//! **The start processor stays crowded, by construction** (the review of TASKS item 98, D98-4).
//! The boot flow can move back only a thread that is ready and not running, and a thread alone
//! on its processor is hardly ever that. So where more than [`PULLERS_MOST`] processors could
//! pull, the others are occupied for the phase by pinned threads that only halt between their
//! processor's interrupts: busy to the scheduler, so their idle flows make no pass, and nothing
//! to the host. The few left free, the two nearest the start processor in index and the two
//! farthest, then hold several of the phase's threads each, the start processor keeps the rest,
//! and both movers always find a thread waiting.
//!
//! Each phase counts from when every one of its threads has started, runs for [`SECONDS`], and
//! goes on until what it requires was seen, within [`COVER_SECONDS`]:
//!
//! - for integrity, at least one pull of a thread that had been moved to its processor before:
//!   its processor has then ticked since it arrived, so the move rate's test passed on a real
//!   stamp, where a first pull can be of a thread that has never run;
//! - for delivery, at least one **wake-up that crossed a pull with its token untaken** (the
//!   review's D98-3), counted by the two threads that know: after its wake-up for a token, the
//!   sender marks the receiver if it then finds it on the start processor
//!   (`Movable::woken_at_start`); a receiver that, after its spin and before it blocks on its
//!   reading from before the spin, finds that mark for a token posted in the spin and itself on
//!   another processor, was pulled away after the mark, with the pending wake-up of a token it
//!   has not taken, since only a pull takes a thread from the start processor and it consumed
//!   nothing meanwhile. If that pull dropped the wake-up, the receiver now blocks for good and
//!   the drain fails. The kernel's own count of pulls that carried any pending wake-up
//!   (`pull::Counts::pulls_carrying`) is printed beside it; it includes stale ones, and is not
//!   the condition.
//!
//! Both phases also fail if a bound of the balancer was passed, as the run queues counted it
//! against their own ticks: more than one balancing interrupt sent by one processor in one tick,
//! more than one ask taken by one processor in one tick (ADR-0020 Amendment 1), a thread pulled
//! sooner than its move's period, or a cache-hot thread pulled across caches (S10). And they fail
//! where two or more processors take part and balancing is not set up. The asks and the kicks
//! made are printed and not required: with two processors the one puller mostly finds published
//! frames, and one asker cannot pass the bound of one ask a tick whatever the kernel does. The
//! capacity phase is unchanged: its thread is movable and not declared for balancing, so no idle
//! processor takes it.
//!
//! On fewer than two processors taking part, each proof says so on its line and counts as passed
//! (ADR-0013 R6's convention). Nothing here prints an address (ADR-0004 Amendment 2, A6).
#![forbid(unsafe_code)]

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use origin_frame::sync::SpinLock;
use origin_frame::trap::Pinned;

use crate::console::Console;
use crate::cpu::{self, CpuIndex, rdtsc};
use crate::memory;
use crate::sched::{self, ThreadRef, Unmoved, pinned, pull};
use crate::smp::Online;
use crate::wait::WaitQueue;

/// Movable threads per processor that takes part (the ruling's Q10: `K` = 2).
const PER_CPU: usize = 2;
/// How long each proof moves threads, in seconds (Q10).
const SECONDS: u64 = 2;
/// Integrity's budget, in seconds, for every processor to have been a source and a destination: its
/// mover goes on past [`SECONDS`] until they have, or this runs out (ADR-0013 R1 (b)'s "a budget of
/// seconds"; the coordinating session's ruling on the review of TASKS item 96, R96-4). Running out
/// means moves did not progress, which a correct kernel on a loaded machine does not cause.
const COVER_SECONDS: u64 = 30;
/// How long the delivery proof waits for every token to be taken after the senders stop.
const DRAIN_SECONDS: u64 = 2;
/// An iteration in this many yields instead of passing the turn (integrity).
const YIELD_EVERY: u64 = 4;
/// The capacity phase's budget, in seconds, for each of its waits: a thread placed, fillers
/// started or ended, moves answered (R96-13).
const PHASE_SECONDS: u64 = 10;
/// Moves the capacity phase tries into a full queue, each answered "queue full" or made.
const INTO_FULL: u64 = 8;

/// What the proofs' threads share with the boot flow, through [`SHARED`]: made before the first
/// spawn, cloned by each thread at its start and dropped before it ends, and dropped by the proof
/// before its compare, so the heap is back where it was.
struct Shared {
    /// Raised to end the threads' loops.
    stop: AtomicBool,
    /// The monotonic clock's reading, in nanoseconds, at which the threads' loops end on their own:
    /// `u64::MAX` until every thread has started, so the time spent starting them takes none of
    /// their run (R96-4). On the monotonic clock, since the movable threads read it on whichever
    /// processor they run on (ADR-0020 N10; R96-15).
    deadline: AtomicU64,
    /// One entry per movable thread.
    threads: Vec<Movable>,
    /// Integrity: the passes of a turn, one per pair of threads.
    pairs: Vec<Pair>,
    /// Delivery: one entry per sender, by processor.
    senders: Vec<Sender>,
    /// Integrity: failed compare-and-swaps of a "running on" word.
    swaps_failed: AtomicU64,
    /// Integrity: canaries found changed.
    canaries_changed: AtomicU64,
    /// Threads that have ended (counted by `sched::finish`'s signal).
    ended: AtomicU64,
    /// Threads that have started, for the boot flow to wait on before it moves any.
    started: AtomicU64,
    /// Integrity's capacity phase (R96-13): raised to end the fillers, where they wait, and the
    /// fillers that have started and ended.
    fill_release: AtomicBool,
    fill_queue: WaitQueue,
    fill_up: AtomicU64,
    fill_ended: AtomicU64,
    /// The TSC rate, for the senders' measures in ticks.
    tsc_hz: u64,
    /// A pulled phase's own (TASKS item 98); `None` in the phases the boot flow moves alone.
    pulled: Option<InPulled>,
}

/// What a pulled phase's threads share beside the rest (the module documentation, "Pulled").
struct InPulled {
    /// The processor the threads start on, and its index.
    start: CpuIndex,
    source: u64,
    /// Integrity: times a thread found, after a spin under its word, that it was on another
    /// processor than the one it took the word on.
    moved_holding: AtomicU64,
    /// Delivery: wake-ups that crossed a pull with their token untaken, as the receivers counted
    /// them from the sender's marks.
    crossed: AtomicU64,
}

impl Shared {
    /// A pulled phase's shared state for `threads` threads declared for balancing, `pairs` pairs
    /// and `senders` senders, started on the processor of index `source`; set as the current one
    /// ([`SHARED`]).
    fn for_pulled(
        threads: usize,
        pairs: usize,
        senders: usize,
        source: CpuIndex,
        tsc_hz: u64,
    ) -> Arc<Self> {
        let shared = Arc::new(Self {
            stop: AtomicBool::new(false),
            deadline: AtomicU64::new(u64::MAX),
            threads: (0..threads).map(|_| Movable::new()).collect(),
            pairs: (0..pairs)
                .map(|_| Pair {
                    turn: AtomicU64::new(0),
                    queue: WaitQueue::new(),
                })
                .collect(),
            senders: (0..senders)
                .map(|_| Sender {
                    posted: AtomicU64::new(0),
                    longest_followed: AtomicU64::new(0),
                    longest_followed_follows: AtomicU64::new(0),
                    longest_plain: AtomicU64::new(0),
                    wake: Spans::new(),
                    null: Spans::new(),
                })
                .collect(),
            swaps_failed: AtomicU64::new(0),
            canaries_changed: AtomicU64::new(0),
            ended: AtomicU64::new(0),
            started: AtomicU64::new(0),
            fill_release: AtomicBool::new(false),
            fill_queue: WaitQueue::new(),
            fill_up: AtomicU64::new(0),
            fill_ended: AtomicU64::new(0),
            tsc_hz,
            pulled: Some(InPulled {
                start: source,
                source: u64::try_from(source.get()).unwrap_or(u64::MAX),
                moved_holding: AtomicU64::new(0),
                crossed: AtomicU64::new(0),
            }),
        });
        set_shared(Some(Arc::clone(&shared)));
        shared
    }

    /// Whether the threads' run is over ([`Shared::deadline`]).
    fn past(&self) -> bool {
        crate::time::monotonic_nanos() >= self.deadline.load(Ordering::Acquire)
    }

    /// Starts the threads' run: it ends `seconds` from now.
    fn run_for(&self, seconds: u64) {
        let ends =
            crate::time::monotonic_nanos().saturating_add(seconds.saturating_mul(1_000_000_000));
        self.deadline.store(ends, Ordering::Release);
    }
}

/// One movable thread of either proof.
struct Movable {
    /// Its name, which it registers at its start: what the mover and the senders use.
    name: SpinLock<Option<ThreadRef>>,
    /// Integrity: the processor index plus 1 it runs on while it holds the word, or 0.
    running_on: AtomicU64,
    /// Delivery: tokens posted to it, and tokens it has taken.
    posted: AtomicU64,
    taken: AtomicU64,
    /// Delivery: where it waits in the `wait_until` form.
    queue: WaitQueue,
    /// Delivery, pulled: the last token whose wake-up its sender then found it on the start
    /// processor for (the module documentation, "Pulled").
    woken_at_start: AtomicU64,
    /// The mover's pacing: the tick of the mover's processor at which it last moved this thread.
    last_moved: AtomicU64,
}

impl Movable {
    fn new() -> Self {
        Self {
            name: SpinLock::new(None),
            running_on: AtomicU64::new(0),
            posted: AtomicU64::new(0),
            taken: AtomicU64::new(0),
            queue: WaitQueue::new(),
            woken_at_start: AtomicU64::new(0),
            last_moved: AtomicU64::new(u64::MAX),
        }
    }

    fn name(&self) -> Option<ThreadRef> {
        cpu::without_interrupts(|| *self.name.lock())
    }
}

/// Integrity: two threads that pass a turn back and forth.
struct Pair {
    /// Which of the two may go: 0 or 1.
    turn: AtomicU64,
    queue: WaitQueue,
}

/// Delivery: one sender's counts and spans.
struct Sender {
    /// Tokens it posted.
    posted: AtomicU64,
    /// Its longest interrupts-off span around a wake-up that followed a move, in TSC ticks, and
    /// that wake-up's follows (D93-2; the contract's "Additions", B).
    longest_followed: AtomicU64,
    longest_followed_follows: AtomicU64,
    /// Its longest span around a wake-up that followed none, for scale.
    longest_plain: AtomicU64,
    /// Its wake-ups' interrupts-off sections, every one.
    wake: Spans,
    /// Its null sections, each spinning for its wake-ups' recent median span.
    null: Spans,
}

/// The wake-up spans whose median a null section spins for: a sender's last few.
const WINDOW: usize = 15;

/// B18 (an interrupts-off section's budget, 50 µs) and a far mark, 400 µs, in microseconds.
const B18_MICROS: u64 = 50;
const FAR_MICROS: u64 = 400;

/// One kind of interrupts-off section a sender timed: the ticks spent in all, the spans past
/// B18 and past the far mark, and the longest. Written only by its sender.
struct Spans {
    ticks: AtomicU64,
    over_b18: AtomicU64,
    over_far: AtomicU64,
    longest: AtomicU64,
}

impl Spans {
    const fn new() -> Self {
        Self {
            ticks: AtomicU64::new(0),
            over_b18: AtomicU64::new(0),
            over_far: AtomicU64::new(0),
            longest: AtomicU64::new(0),
        }
    }

    /// Notes one span of `took` ticks, against the marks `b18` and `far` in ticks.
    fn note(&self, took: u64, b18: u64, far: u64) {
        self.ticks.fetch_add(took, Ordering::Relaxed);
        if took > b18 {
            self.over_b18.fetch_add(1, Ordering::Relaxed);
        }
        if took > far {
            self.over_far.fetch_add(1, Ordering::Relaxed);
        }
        self.longest.fetch_max(took, Ordering::Relaxed);
    }

    /// Every sender's spans of one kind, summed (the longest, the largest): ticks, past B18,
    /// past the far mark, longest.
    fn total<'a>(spans: impl Iterator<Item = &'a Spans>) -> (u64, u64, u64, u64) {
        spans.fold((0, 0, 0, 0), |(t, b, f, l), s| {
            (
                t.saturating_add(s.ticks.load(Ordering::Relaxed)),
                b.saturating_add(s.over_b18.load(Ordering::Relaxed)),
                f.saturating_add(s.over_far.load(Ordering::Relaxed)),
                l.max(s.longest.load(Ordering::Relaxed)),
            )
        })
    }
}

/// The median of `spans`, 0 if there are none.
fn median(spans: &[u64]) -> u64 {
    let mut sorted = [0u64; WINDOW];
    let Some(sorted) = sorted.get_mut(..spans.len()) else {
        return 0;
    };
    sorted.copy_from_slice(spans);
    sorted.sort_unstable();
    sorted.get(sorted.len() / 2).copied().unwrap_or(0)
}

/// The current proof's shared state, while its threads live.
static SHARED: SpinLock<Option<Arc<Shared>>> = SpinLock::new(None);

/// The running proof thread's handle on [`SHARED`].
fn shared() -> Option<Arc<Shared>> {
    cpu::without_interrupts(|| SHARED.lock().clone())
}

/// Sets or clears [`SHARED`].
fn set_shared(value: Option<Arc<Shared>>) {
    cpu::without_interrupts(|| *SHARED.lock() = value);
}

/// What the mover did over one proof.
#[derive(Default)]
struct Moves {
    made: u64,
    carried_wake: u64,
    /// Delivery: moves that carried a pending wake-up of a receiver holding a token it had not
    /// taken, both before and after the move: a token's wake-up carried across a move (S6, N1), the
    /// path the proof is for (the review of TASKS item 96, R96-2). A pending wake-up alone may be
    /// the stale one a sender's second wake-up, through the receiver's queue, leaves.
    carried_token: u64,
    /// Refusals by reason, in [`REASONS`]' order.
    refused: [u64; REASONS.len()],
    /// Each move's critical section, in TSC ticks (B20).
    ticks: Vec<u64>,
    /// By processor index: whether it was a source, and a destination, at least once.
    source: Vec<bool>,
    destination: Vec<bool>,
    /// Integrity: the mover's TSC ticks from its start until every processor had been both.
    covered_after: Option<u64>,
}

/// The refusals the lines name, in order.
const REASONS: [(Unmoved, &str); 11] = [
    (Unmoved::AlreadyThere, "already there"),
    (Unmoved::Gone, "gone"),
    (Unmoved::Stale, "stale"),
    (Unmoved::Pinned, "pinned"),
    (Unmoved::NotReady, "not ready"),
    (Unmoved::FpuFormat, "FPU format"),
    (Unmoved::QueueFull, "queue full"),
    (Unmoved::TableFull, "table full"),
    (Unmoved::TryLater, "being switched out"),
    (Unmoved::Moving, "moving"),
    (Unmoved::Inconsistent, "inconsistent"),
];

impl Moves {
    fn new(cpus: usize) -> Self {
        Self {
            source: alloc::vec![false; cpus],
            destination: alloc::vec![false; cpus],
            ..Self::default()
        }
    }
}

/// The refusals `refused` counts, as `reason n` pairs for a line, the reasons never met left out.
/// Made after the proof's compare of the heap, so that its own allocation is not in it.
fn refusals(refused: &[u64; REASONS.len()]) -> alloc::string::String {
    let mut out = alloc::string::String::new();
    for ((_, named), &n) in REASONS.iter().zip(refused) {
        if n != 0 {
            let _ = write!(out, "{}{named} {n}", if out.is_empty() { "" } else { ", " });
        }
    }
    if out.is_empty() {
        out.push_str("none");
    }
    out
}

impl Moves {
    /// Whether every processor in `cpus` was a source and a destination at least once.
    fn reached_every(&self, cpus: &[CpuIndex]) -> bool {
        cpus.iter().all(|cpu| {
            self.source.get(cpu.get()) == Some(&true)
                && self.destination.get(cpu.get()) == Some(&true)
        })
    }

    /// The median and largest critical section, in TSC ticks.
    fn sections(&mut self) -> (u64, u64) {
        self.ticks.sort_unstable();
        let median = self.ticks.get(self.ticks.len() / 2).copied().unwrap_or(0);
        (median, self.ticks.last().copied().unwrap_or(0))
    }
}

/// xorshift64: the mover's and the senders' choices, the same sequence for the same seed. Not a
/// source of randomness for anything else (ADR-0016 keeps that to `origin_frame::random`).
struct Choice(u64);

impl Choice {
    fn next(&mut self, below: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        let below = u64::try_from(below.max(1)).unwrap_or(u64::MAX);
        usize::try_from(self.0 % below).unwrap_or(0)
    }
}

/// The processors that take part in moves, in index order.
fn taking_part(online: &Online) -> Vec<CpuIndex> {
    online
        .cpus()
        .iter()
        .copied()
        .filter(|&cpu| online.takes_part_in_moves(cpu))
        .collect()
}

/// TSC ticks as nanoseconds.
fn nanos(ticks: u64, tsc_hz: u64) -> u64 {
    u64::try_from(u128::from(ticks) * 1_000_000_000 / u128::from(tsc_hz.max(1))).unwrap_or(u64::MAX)
}

/// The boot flow's mover: for [`SECONDS`], round-robin over `shared`'s threads, it moves each to a
/// processor of `cpus` chosen by `choice`, at most once per tick of its own processor. With
/// `cover`, it goes on past [`SECONDS`] until every processor of `cpus` has been a source and a
/// destination, within [`COVER_SECONDS`] (integrity; R96-4). On the boot processor's TSC alone.
fn move_for(
    flow: Pinned<'_>,
    shared: &Shared,
    cpus: &[CpuIndex],
    online: &Online,
    tsc_hz: u64,
    cover: bool,
    moves: &mut Moves,
) {
    let me = cpu::this_cpu(flow.here());
    let mut choice = Choice(0x2545_f491_4f6c_dd1d);
    let start = rdtsc();
    let (window, budget) = (
        tsc_hz.saturating_mul(SECONDS),
        tsc_hz.saturating_mul(COVER_SECONDS),
    );
    loop {
        let elapsed = rdtsc().wrapping_sub(start);
        if cover && moves.covered_after.is_none() && moves.reached_every(cpus) {
            moves.covered_after = Some(elapsed);
        }
        let covering = cover && moves.covered_after.is_none() && elapsed < budget;
        if elapsed >= window && !covering {
            break;
        }
        for thread in &shared.threads {
            let Some(name) = thread.name() else {
                continue;
            };
            let tick = sched::queue_ticks(me);
            if thread.last_moved.load(Ordering::Relaxed) == tick {
                continue;
            }
            let Some(&to) = cpus.get(choice.next(cpus.len())) else {
                continue;
            };
            let outstanding =
                || thread.posted.load(Ordering::Acquire) > thread.taken.load(Ordering::Acquire);
            let before = outstanding();
            match sched::migrate(name, to, online) {
                Ok(moved) => {
                    moves.made += 1;
                    moves.carried_wake += u64::from(moved.carried_wake);
                    moves.carried_token += u64::from(moved.carried_wake && before && outstanding());
                    moves.ticks.push(moved.ticks);
                    if let Some(s) = moves.source.get_mut(moved.from.get()) {
                        *s = true;
                    }
                    if let Some(d) = moves.destination.get_mut(to.get()) {
                        *d = true;
                    }
                    thread.last_moved.store(tick, Ordering::Relaxed);
                }
                Err(refused) => {
                    if let Some(i) = REASONS.iter().position(|&(r, _)| r == refused)
                        && let Some(n) = moves.refused.get_mut(i)
                    {
                        *n += 1;
                    }
                }
            }
        }
        sched::yield_now();
        cpu::interrupt_window();
        let _ = sched::reap();
    }
}

/// Spawns [`PER_CPU`] movable threads running `entry` on each processor of `cpus`, each given its
/// index. `false` if one was refused.
fn spawn_movable(cpus: &[CpuIndex], entry: extern "sysv64" fn(u64) -> !) -> bool {
    cpus.iter().enumerate().all(|(i, &cpu)| {
        (0..PER_CPU).all(|k| {
            let index = u64::try_from(i * PER_CPU + k).unwrap_or(u64::MAX);
            sched::spawn_movable(cpu, entry, index).is_ok()
        })
    })
}

/// What a proof's bracket compares before and after: the heap in use, the free frames and the
/// stacks held. The settle gate lists this function, since it reads `memory::stats` without
/// settling itself: in `prove_integrity` and `prove_delivery`, `sched::settle` is called just
/// before the first read, and `bracket_after` calls it again before the second; a new caller of
/// `counts` must settle first (TASKS item 96).
fn counts() -> (Option<(u64, u64)>, usize) {
    (
        memory::stats().map(|(h, f)| (h.bytes_in_use, f)),
        origin_frame::stack::counts().live,
    )
}

/// The settle after a proof, and its compare with `before`: `Ok` if the counts are back. The
/// shared state is dropped first.
fn bracket_after(
    settled_before: Result<(), sched::Unsettled>,
    before: (Option<(u64, u64)>, usize),
    tsc_hz: u64,
) -> (bool, alloc::string::String) {
    set_shared(None);
    let settled_after = sched::settle(tsc_hz);
    sched::yield_until(|| counts() == before, tsc_hz, 2);
    let back = counts() == before && settled_before.is_ok() && settled_after.is_ok();
    (back, sched::not_settled(settled_before, settled_after))
}

// ---------------------------------------------------------------------------------- integrity

/// The six canary values integrity's thread `index` keeps across its loop, wherever the compiler
/// keeps them (R96-8).
fn kept_canaries(index: u64) -> [u64; 6] {
    [
        index ^ 0x0123_4567_89ab_cdef,
        index.wrapping_mul(0x9e37_79b9_7f4a_7c15),
        !index,
        index.rotate_left(17) ^ 0x5a5a_5a5a_5a5a_5a5a,
        index.wrapping_add(0x1111_2222_3333_4444),
        index ^ 0xfedc_ba98_7654_3210,
    ]
}

/// Byte `k` of the stack canary of integrity's thread `index`.
fn stack_byte(index: u64, k: usize) -> u8 {
    let k = u64::try_from(k).unwrap_or(0);
    u8::try_from(index.wrapping_mul(31).wrapping_add(k) & 0xff).unwrap_or(0)
}

/// The XMM canaries of integrity's thread `index`.
fn xmm_canaries(index: u64) -> [u64; 4] {
    [
        index.wrapping_add(0x7777_0000_0000_0001),
        index.rotate_right(9),
        !index ^ 0x00ff_00ff_00ff_00ff,
        index.wrapping_mul(0x0101_0101_0101_0101),
    ]
}

/// Takes `word` for this processor, by compare-and-swap from 0, and answers the value it set: a
/// failed swap is a second processor running the same context, counted in `shared`.
fn take_word(shared: &Shared, word: &AtomicU64) -> u64 {
    let on = index_now().saturating_add(1);
    if word
        .compare_exchange(0, on, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        shared.swaps_failed.fetch_add(1, Ordering::Relaxed);
    }
    on
}

/// The index of the processor this thread runs on as it asks, read in a short interrupts-off
/// section: it may be on another by the time the answer is used.
fn index_now() -> u64 {
    cpu::without_interrupts_here(|here| {
        u64::try_from(cpu::this_cpu(here).index().get()).unwrap_or(u64::MAX - 1)
    })
}

/// Whether the canaries of integrity's thread `index` are as it set them: the six kept values,
/// its frame's 256 bytes, and its four XMM registers.
fn canaries_intact(index: u64, kept: [u64; 6], stack: &[u8; 256]) -> bool {
    let stack_ok = core::hint::black_box(stack)
        .iter()
        .enumerate()
        .all(|(k, &b)| b == stack_byte(index, k));
    core::hint::black_box(kept) == kept_canaries(index)
        && stack_ok
        && cpu::xmm_read() == xmm_canaries(index)
}

/// Releases `word`, by compare-and-swap from `on`, counting a failure as [`take_word`] does.
fn release_word(shared: &Shared, word: &AtomicU64, on: u64) {
    if word
        .compare_exchange(on, 0, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        shared.swaps_failed.fetch_add(1, Ordering::Relaxed);
    }
}

/// Integrity's thread `index`: see the module documentation.
///
/// It runs its whole loop with interrupts off, so that nothing but its own yields and blocks
/// switches it out (the coordinating session's ruling Q9). It takes its "running on" word first
/// thing after each return from one and releases it last thing before the next, so it holds the
/// word whenever it runs, but for two short windows a switch point leaves: from the release to
/// the switch, and from the switch's return to the take.
///
/// In a pulled phase ([`Shared::pulled`]) it runs with interrupts on instead, and in every other
/// iteration spins for a tick and a half under its word: a tick or a reschedule interrupt
/// switches it out there, holding the word, and an idle processor may take it before it runs
/// again (the module documentation, "Pulled"). It checks its canaries again after the spin.
extern "sysv64" fn integrity_thread(index: u64) -> ! {
    let Some(shared) = shared() else {
        sched::finish(|| {});
    };
    let i = usize::try_from(index).unwrap_or(usize::MAX);
    let named = sched::current_thread();
    if i >= shared.threads.len() {
        sched::finish(move || drop(shared));
    }
    let Some(me) = shared.threads.get(i) else {
        sched::finish(|| {});
    };
    cpu::without_interrupts(|| *me.name.lock() = named);
    shared.started.fetch_add(1, Ordering::AcqRel);
    let pair = shared.pairs.get(i / 2);
    let mine = index % 2;
    let spin = 1_500_000_000 / crate::timer::HZ;
    let run = || {
        // The canaries: six values kept across the loop, in registers or spilled; 256 bytes in
        // this frame; four XMM registers, which only this thread's FPU area holds.
        let kept = core::hint::black_box(kept_canaries(index));
        let stack: [u8; 256] =
            core::hint::black_box(core::array::from_fn(|k| stack_byte(index, k)));
        cpu::xmm_load(xmm_canaries(index));
        let mut iteration = 0u64;
        loop {
            let on = take_word(&shared, &me.running_on);
            if !canaries_intact(index, kept, &stack) {
                shared.canaries_changed.fetch_add(1, Ordering::Relaxed);
            }
            iteration += 1;
            let ending = shared.stop.load(Ordering::Acquire) || shared.past();
            if let Some(pulled) = &shared.pulled
                && !ending
                && !iteration.is_multiple_of(2)
            {
                // Under the word, with interrupts on, for longer than a tick, on the monotonic
                // clock (it may be read on another processor before the spin ends; N10).
                let start = crate::time::monotonic_nanos();
                while crate::time::monotonic_nanos().saturating_sub(start) < spin {
                    core::hint::spin_loop();
                }
                if !canaries_intact(index, kept, &stack) {
                    shared.canaries_changed.fetch_add(1, Ordering::Relaxed);
                }
                if index_now().saturating_add(1) != on {
                    pulled.moved_holding.fetch_add(1, Ordering::Relaxed);
                }
            }
            match pair {
                _ if ending => {
                    // Leave the partner nothing to wait for.
                    if let Some(pair) = pair {
                        pair.turn.store(1 - mine, Ordering::Release);
                        pair.queue.wake_one();
                    }
                    release_word(&shared, &me.running_on, on);
                    break;
                }
                Some(pair) if !iteration.is_multiple_of(YIELD_EVERY) => {
                    // Pass the turn and wake the partner, holding the word; release it, and wait
                    // for the turn back.
                    pair.turn.store(1 - mine, Ordering::Release);
                    pair.queue.wake_one();
                    release_word(&shared, &me.running_on, on);
                    pair.queue.wait_until(|| {
                        pair.turn.load(Ordering::Acquire) == mine
                            || shared.stop.load(Ordering::Acquire)
                            || shared.past()
                    });
                }
                _ => {
                    release_word(&shared, &me.running_on, on);
                    sched::yield_now();
                }
            }
        }
    };
    if shared.pulled.is_some() {
        run();
    } else {
        cpu::without_interrupts(run);
    }
    sched::finish(move || {
        shared.ended.fetch_add(1, Ordering::AcqRel);
        drop(shared);
    })
}

/// The capacity phase's filler: a pinned thread that only takes a slot, waiting until released.
extern "sysv64" fn filler(_: u64) -> ! {
    let Some(shared) = shared() else {
        sched::finish(|| {});
    };
    shared.fill_up.fetch_add(1, Ordering::AcqRel);
    shared
        .fill_queue
        .wait_until(|| shared.fill_release.load(Ordering::Acquire));
    sched::finish(move || {
        shared.fill_ended.fetch_add(1, Ordering::AcqRel);
        drop(shared);
    })
}

/// Lines naming every movable thread's state, each pair's turn and every processor's run queue,
/// written when a capacity wait runs out or a proof's threads do not end (the coordinating
/// session's ruling on the gates' stop of 2026-11-22): report only, so that such a failure
/// explains itself. No address (ADR-0004 Amendment 2, A6).
fn describe_stuck(shared: &Shared, why: &str) {
    for (i, thread) in shared.threads.iter().enumerate() {
        let state = thread.name().map_or_else(
            || alloc::string::String::from("no name"),
            sched::describe_thread,
        );
        let _ = writeln!(
            Console::new(),
            "origin-kernel: scheduler: thread migration: state when {why}: thread {i}: {state}; running on {}",
            thread.running_on.load(Ordering::Relaxed)
        );
    }
    for (i, pair) in shared.pairs.iter().enumerate() {
        let _ = writeln!(
            Console::new(),
            "origin-kernel: scheduler: thread migration: state when {why}: pair {i}: turn {}",
            pair.turn.load(Ordering::Relaxed)
        );
    }
    let _ = writeln!(
        Console::new(),
        "origin-kernel: scheduler: thread migration: state when {why}: {}",
        sched::describe_cpus()
    );
}

/// What integrity's capacity phase found (ADR-0020 N13, "integrity runs at the queue's capacity";
/// the coordinating session's ruling on the review of TASKS item 96, R96-13).
#[derive(Default)]
struct Capacity {
    /// Fillers spawned on the destination, and whether its queue was then full.
    filled: usize,
    full: bool,
    /// Moves into the full destination refused as full, and made (none may be).
    refused_full: u64,
    made_into_full: u64,
    /// Whether the thread tried stayed on its source, alive, through those moves.
    stayed: bool,
    /// Fillers spawned on the source, whether its queue was then full, and whether the thread then
    /// moved out of it.
    filled_source: usize,
    source_full: bool,
    moved_out: bool,
    /// Whether every filler ended.
    ended: bool,
    /// The phase's time, in TSC ticks of the boot processor.
    ticks: u64,
}

impl Capacity {
    fn ok(&self) -> bool {
        self.full
            && self.refused_full != 0
            && self.made_into_full == 0
            && self.stayed
            && self.source_full
            && self.moved_out
            && self.ended
    }
}

/// Runs `step` until it answers, yielding between tries, for at most [`PHASE_SECONDS`]: `None`
/// if it never did.
fn within<T>(tsc_hz: u64, mut step: impl FnMut() -> Option<T>) -> Option<T> {
    let start = rdtsc();
    while rdtsc().wrapping_sub(start) < tsc_hz.saturating_mul(PHASE_SECONDS) {
        if let Some(answer) = step() {
            return Some(answer);
        }
        sched::yield_now();
        cpu::interrupt_window();
        let _ = sched::reap();
    }
    None
}

/// Spawns fillers on `cpu` until its queue refuses one, and waits for them to have started: how
/// many it spawned.
fn fill(shared: &Shared, cpu: CpuIndex, tsc_hz: u64) -> usize {
    let up = shared.fill_up.load(Ordering::Acquire);
    let spawned = (0..crate::runqueue::CAPACITY)
        .take_while(|_| sched::spawn(cpu, filler, 0).is_ok())
        .count();
    let wanted = up.saturating_add(u64::try_from(spawned).unwrap_or(u64::MAX));
    let _ = within(tsc_hz, || {
        (shared.fill_up.load(Ordering::Acquire) >= wanted).then_some(())
    });
    spawned
}

/// Releases every filler and waits for them to have ended and been reaped: whether they all ended.
fn release(shared: &Shared, tsc_hz: u64) -> bool {
    shared.fill_release.store(true, Ordering::Release);
    shared.fill_queue.wake_all();
    let ended = within(tsc_hz, || {
        (shared.fill_ended.load(Ordering::Acquire) == shared.fill_up.load(Ordering::Acquire))
            .then_some(())
    })
    .is_some();
    // Every filler so far has ended: the next fill's wait for its release again.
    shared.fill_release.store(false, Ordering::Release);
    ended
}

/// Integrity's capacity phase, on the first two processors that take part, the others' threads
/// left as they are: one integrity thread is placed on the source; the destination's queue is
/// filled to its capacity with fillers, and moves of the thread into it must each be refused as
/// full, the thread staying on its source, alive; the destination's fillers end; the source's
/// queue is filled, and the thread must then move out of it, to the destination. The integrity
/// thread's own canaries and word go on being checked throughout (R96-13).
fn capacity_phase(shared: &Shared, cpus: &[CpuIndex], online: &Online, tsc_hz: u64) -> Capacity {
    let start = rdtsc();
    let mut found = Capacity::default();
    let (Some(&source), Some(&destination), Some(thread)) = (
        cpus.first(),
        cpus.get(1),
        shared.threads.first().and_then(Movable::name),
    ) else {
        return found;
    };
    // A move answered "not ready" or "being switched out" is tried again.
    let again = |refused: Unmoved| {
        matches!(
            refused,
            Unmoved::NotReady | Unmoved::TryLater | Unmoved::Moving
        )
    };
    let placed = within(tsc_hz, || match sched::migrate(thread, source, online) {
        Ok(_) | Err(Unmoved::AlreadyThere) => Some(true),
        Err(refused) if again(refused) => None,
        Err(_) => Some(false),
    }) == Some(true);
    found.filled = fill(shared, destination, tsc_hz);
    found.full = !sched::has_room(destination);
    if placed && found.full {
        let _ = within(tsc_hz, || {
            match sched::migrate(thread, destination, online) {
                Ok(_) => found.made_into_full += 1,
                Err(Unmoved::QueueFull) => found.refused_full += 1,
                Err(refused) if again(refused) => {}
                Err(_) => return Some(()),
            }
            (found.refused_full + found.made_into_full >= INTO_FULL).then_some(())
        });
        found.stayed = sched::queued_on(thread, source);
        if found.refused_full + found.made_into_full == 0 {
            describe_stuck(shared, "no move into the full queue was answered");
        }
    }
    let destination_ended = release(shared, tsc_hz);
    found.filled_source = fill(shared, source, tsc_hz);
    found.source_full = !sched::has_room(source);
    if found.stayed && found.source_full {
        found.moved_out = within(tsc_hz, || {
            match sched::migrate(thread, destination, online) {
                Ok(_) => Some(true),
                Err(refused) if again(refused) => None,
                Err(_) => Some(false),
            }
        }) == Some(true);
    }
    if !found.moved_out {
        describe_stuck(shared, "the move out of the full queue was not made");
    }
    found.ended = destination_ended && release(shared, tsc_hz);
    found.ticks = rdtsc().wrapping_sub(start);
    found
}

/// "thread migration: integrity" (Refuse; the contract's §3.1): moved by the boot flow, then
/// pulled by the idle processors (TASKS item 98). See the module documentation.
pub(crate) fn prove_integrity(
    flow: Pinned<'_>,
    com1: &mut Console,
    online: &Online,
    tsc_hz: u64,
) -> bool {
    let moved = integrity_by_moves(flow, com1, online, tsc_hz);
    let pulled = integrity_by_pulls(flow, com1, online, tsc_hz);
    moved && pulled
}

/// Integrity's first phase: its threads moved by the boot flow, and the capacity phase.
fn integrity_by_moves(flow: Pinned<'_>, com1: &mut Console, online: &Online, tsc_hz: u64) -> bool {
    let cpus = taking_part(online);
    if cpus.len() < 2 {
        let _ = writeln!(
            com1,
            "origin-kernel: scheduler: thread migration: integrity: fewer than two processors take part in moves ({}): nothing to move",
            cpus.len()
        );
        return true;
    }
    let settled_before = sched::settle(tsc_hz);
    let before = counts();
    let unready_before = sched::unready_switches();
    let gated_before = cpu::gated_scans();
    let threads = cpus.len() * PER_CPU;
    let shared = Arc::new(Shared {
        stop: AtomicBool::new(false),
        deadline: AtomicU64::new(u64::MAX),
        threads: (0..threads).map(|_| Movable::new()).collect(),
        pairs: (0..threads / 2)
            .map(|_| Pair {
                turn: AtomicU64::new(0),
                queue: WaitQueue::new(),
            })
            .collect(),
        senders: Vec::new(),
        swaps_failed: AtomicU64::new(0),
        canaries_changed: AtomicU64::new(0),
        ended: AtomicU64::new(0),
        started: AtomicU64::new(0),
        fill_release: AtomicBool::new(false),
        fill_queue: WaitQueue::new(),
        fill_up: AtomicU64::new(0),
        fill_ended: AtomicU64::new(0),
        tsc_hz,
        pulled: None,
    });
    set_shared(Some(Arc::clone(&shared)));
    let spawned = spawn_movable(&cpus, integrity_thread);
    let total = u64::try_from(threads).unwrap_or(u64::MAX);
    sched::yield_until(
        || shared.started.load(Ordering::Acquire) == total,
        tsc_hz,
        2,
    );
    // Their own end, past the mover's longest run and the capacity phase's: the boot flow's stop
    // ends them first.
    shared.run_for(COVER_SECONDS + SECONDS + 6 * PHASE_SECONDS);
    let mut moves = Moves::new(online.cpus().iter().map(|c| c.get() + 1).max().unwrap_or(0));
    let mut capacity = Capacity::default();
    if spawned {
        move_for(flow, &shared, &cpus, online, tsc_hz, true, &mut moves);
        capacity = capacity_phase(&shared, &cpus, online, tsc_hz);
    }
    shared.stop.store(true, Ordering::Release);
    for pair in &shared.pairs {
        pair.queue.wake_all();
    }
    sched::yield_until(
        || shared.ended.load(Ordering::Acquire) == shared.started.load(Ordering::Acquire),
        tsc_hz,
        sched::OTHERS_PATIENCE,
    );
    let ended = shared.ended.load(Ordering::Acquire) == shared.started.load(Ordering::Acquire);
    if !ended {
        describe_stuck(&shared, "threads did not end");
    }
    let failed_swaps = shared.swaps_failed.load(Ordering::Relaxed);
    let changed = shared.canaries_changed.load(Ordering::Relaxed);
    drop(shared);
    let unready = sched::unready_switches().wrapping_sub(unready_before);
    let reached = moves.reached_every(&cpus);
    let covered_after = moves.covered_after;
    let (median, largest) = moves.sections();
    let (gated_after, longest_gate) = cpu::gated_scans();
    let (made, refused) = (moves.made, moves.refused);
    drop(moves);
    let (back, unsettled) = bracket_after(settled_before, before, tsc_hz);
    let refusals = refusals(&refused);
    let pinned_mode = pinned::entered();
    let ok = spawned
        && ended
        && failed_swaps == 0
        && changed == 0
        && unready == 0
        && back
        && reached
        && capacity.ok()
        && pinned_mode.is_none();
    let (source, destination) = (
        cpus.first().map_or(0, |c| c.get()),
        cpus.get(1).map_or(0, |c| c.get()),
    );
    let _ = writeln!(
        com1,
        "origin-kernel: scheduler: thread migration: integrity: {threads} movable threads on {} processors, {made} moves, refused: {refusals}; a second processor in a context {failed_swaps} times, canaries changed {changed} times, threads run before their state existed {unready}; every processor a source and a destination: {}; a move's critical section: median {median} ticks ({} ns), largest {largest} ticks ({} ns); scans that made the gated pass {}, the longest {longest_gate} ticks ({} ns); spawned {spawned}; ended {ended}; heap, frames and stacks back {back}{}{}{}",
        cpus.len(),
        match covered_after {
            Some(after) => alloc::format!("yes, after {} ms", nanos(after, tsc_hz) / 1_000_000),
            None => alloc::format!("no, in {COVER_SECONDS} s"),
        },
        nanos(median, tsc_hz),
        nanos(largest, tsc_hz),
        gated_after.wrapping_sub(gated_before.0),
        nanos(longest_gate, tsc_hz),
        match pinned_mode {
            Some((trigger, _)) => alloc::format!("; pinned mode entered: {trigger:?}"),
            None => alloc::string::String::new(),
        },
        unsettled,
        if ok { "" } else { ": scheduler failed" }
    );
    let _ = writeln!(
        com1,
        "origin-kernel: scheduler: thread migration: at capacity: CPU {destination} filled with {} threads, full {}; {} moves into it refused as full, {} made; the thread stayed on CPU {source} {}; CPU {source} filled with {}, full {}; the move out of it made {}; every filler ended {}; in {} ms{}",
        capacity.filled,
        capacity.full,
        capacity.refused_full,
        capacity.made_into_full,
        capacity.stayed,
        capacity.filled_source,
        capacity.source_full,
        capacity.moved_out,
        capacity.ended,
        nanos(capacity.ticks, tsc_hz) / 1_000_000,
        if capacity.ok() {
            ""
        } else {
            ": scheduler failed"
        }
    );
    ok
}

// ----------------------------------------------------------------------------------- delivery

/// Delivery's receiver `index`: see the module documentation.
extern "sysv64" fn receiver(index: u64) -> ! {
    let Some(shared) = shared() else {
        sched::finish(|| {});
    };
    let i = usize::try_from(index).unwrap_or(usize::MAX);
    if let Some(me) = shared.threads.get(i) {
        let named = sched::current_thread();
        cpu::without_interrupts(|| *me.name.lock() = named);
        shared.started.fetch_add(1, Ordering::AcqRel);
        // A tick, in nanoseconds of the monotonic clock: the receiver may be moved mid-spin, and
        // no TSC reading is compared across processors (ADR-0020 N10; R96-15).
        let period = 1_000_000_000 / crate::timer::HZ;
        let mut taken = 0u64;
        let mut direct = false;
        loop {
            let posted = me.posted.load(Ordering::Acquire);
            if posted > taken {
                taken = posted;
                me.taken.store(taken, Ordering::Release);
                continue;
            }
            if shared.stop.load(Ordering::Acquire) {
                break;
            }
            direct = !direct;
            if direct {
                // Registered (its name, from its start), then a spin to the next tick: the window
                // in which it can be preempted, moved and woken before it blocks (N1). The tokens
                // are read once, before the spin, and not again after it: a token posted in the
                // spin must reach the receiver through the pending wake-up alone, which is what
                // this proof tests (the module's documentation; control C96-2).
                let before = me.posted.load(Ordering::Acquire);
                let start = crate::time::monotonic_nanos();
                while crate::time::monotonic_nanos().saturating_sub(start) < period {
                    core::hint::spin_loop();
                }
                // A pulled phase's count, and nothing more (the module documentation, "Pulled"):
                // the sender's mark for a token posted in the spin, read first, and then this
                // receiver on another processor than the start. It decides nothing: the block
                // below is still on the reading from before the spin, and the tokens are not read.
                let crossed = shared.pulled.as_ref().filter(|pulled| {
                    me.woken_at_start.load(Ordering::Acquire) > before
                        && index_now() != pulled.source
                });
                if before == taken && !shared.stop.load(Ordering::Acquire) {
                    if let Some(pulled) = crossed {
                        pulled.crossed.fetch_add(1, Ordering::Relaxed);
                    }
                    sched::block();
                }
            } else {
                me.queue.wait_until(|| {
                    me.posted.load(Ordering::Acquire) > taken || shared.stop.load(Ordering::Acquire)
                });
            }
        }
    }
    sched::finish(move || {
        shared.ended.fetch_add(1, Ordering::AcqRel);
        drop(shared);
    })
}

/// Delivery's sender on the processor of index `index` among those that take part: pinned (it is
/// not declared movable).
extern "sysv64" fn sender(index: u64) -> ! {
    let Some(shared) = shared() else {
        sched::finish(|| {});
    };
    shared.started.fetch_add(1, Ordering::AcqRel);
    let me = shared
        .senders
        .get(usize::try_from(index).unwrap_or(usize::MAX));
    let mut choice = Choice(0x9e37_79b9_7f4a_7c15 ^ index.wrapping_add(1));
    let mut sent = 0u64;
    let micros = |us: u64| shared.tsc_hz.saturating_mul(us) / 1_000_000;
    let (b18, far) = (micros(B18_MICROS), micros(FAR_MICROS));
    let (mut window, mut filled, mut next) = ([0u64; WINDOW], 0usize, 0usize);
    while let Some(me) = me
        && !shared.stop.load(Ordering::Acquire)
        && !shared.past()
    {
        let receivers = shared.threads.len();
        let first = choice.next(receivers);
        // One token at most outstanding per receiver (the module's documentation; control
        // C96-2): a receiver that has taken every token posted to it, claimed by a compare and
        // swap of its count, so no two senders post to it at once.
        let claimed = (0..receivers)
            .filter_map(|k| {
                shared
                    .threads
                    .get(first.wrapping_add(k).checked_rem(receivers)?)
            })
            .find(|to| {
                to.name().is_some() && {
                    let taken = to.taken.load(Ordering::Acquire);
                    to.posted
                        .compare_exchange(
                            taken,
                            taken.wrapping_add(1),
                            Ordering::AcqRel,
                            Ordering::Relaxed,
                        )
                        .is_ok()
                }
            });
        let Some((to, name)) = claimed.and_then(|to| to.name().map(|name| (to, name))) else {
            sched::yield_now();
            continue;
        };
        me.posted.fetch_add(1, Ordering::Relaxed);
        // The wake-up in an interrupts-off section of the sender's own: the span holds `wake`'s
        // whole section, and the count's change on this processor is this wake-up's follows
        // (D93-2; the contract's "Additions", B).
        let took = cpu::without_interrupts_here(|here| {
            let follows =
                || crate::threads::follows_here(cpu::this_cpu(here)).map_or(0, |f| f.read().0);
            let (count, start) = (follows(), rdtsc());
            let _ = sched::wake(name);
            let (took, followed) = (
                rdtsc().saturating_sub(start),
                follows().saturating_sub(count),
            );
            if followed != 0 {
                if took > me.longest_followed.load(Ordering::Relaxed) {
                    me.longest_followed.store(took, Ordering::Relaxed);
                    me.longest_followed_follows
                        .store(followed, Ordering::Relaxed);
                }
            } else if took > me.longest_plain.load(Ordering::Relaxed) {
                me.longest_plain.store(took, Ordering::Relaxed);
            }
            took
        });
        me.wake.note(took, b18, far);
        // A pulled phase's mark (the module documentation, "Pulled"): after the wake-up, the
        // receiver is on the start processor, so the wake-up for this token is with it there.
        if let Some(pulled) = &shared.pulled
            && sched::queued_on(name, pulled.start)
        {
            to.woken_at_start
                .store(to.posted.load(Ordering::Acquire), Ordering::Release);
        }
        // The null section: as long as the recent wake-ups' median, with interrupts off and no
        // lock, so that its long spans are the host's alone (report only).
        if let Some(slot) = window.get_mut(next) {
            *slot = took;
        }
        next = (next + 1) % WINDOW;
        filled = (filled + 1).min(WINDOW);
        let target = median(window.get(..filled).unwrap_or(&[]));
        let spun = cpu::without_interrupts_here(|_| {
            let start = rdtsc();
            let until = start.saturating_add(target);
            while rdtsc() < until {
                core::hint::spin_loop();
            }
            rdtsc().saturating_sub(start)
        });
        me.null.note(spun, b18, far);
        to.queue.wake_one();
        sent += 1;
        if sent.is_multiple_of(8) {
            sched::sleep(1);
        } else {
            sched::yield_now();
        }
    }
    sched::finish(move || {
        shared.ended.fetch_add(1, Ordering::AcqRel);
        drop(shared);
    })
}

/// "thread migration: delivery" (Withdraw: "no thread moves"; the contract's §3.2): receivers
/// moved by the boot flow, then pulled by the idle processors (TASKS item 98). See the module
/// documentation. A failure of either phase enters pinned mode (S20, case 1; trigger 1).
pub(crate) fn prove_delivery(
    flow: Pinned<'_>,
    com1: &mut Console,
    online: &Online,
    tsc_hz: u64,
) -> bool {
    let moved = delivery_by_moves(flow, com1, online, tsc_hz);
    let pulled = delivery_by_pulls(flow, com1, online, tsc_hz);
    // Every count of the balancer so far, report only (the review of TASKS item 98, D98-6).
    pull::report(com1, tsc_hz);
    moved && pulled
}

/// Delivery's first phase: its receivers moved by the boot flow.
/// On any failure it enters pinned mode (S20, case 1; trigger 1), so the
/// boot goes on with moves withdrawn as its verdict says (ADR-0013 R2): undelivered tokens enter
/// it before the receivers are woken a last time, so that those wake-ups reach them with moves
/// withdrawn, and every other failure once it is known, after the compare (the review of TASKS
/// item 96, R96-1). An entry into pinned mode before it, during the boot's proofs, fails it too,
/// naming the trigger (the ruling's Q5).
fn delivery_by_moves(flow: Pinned<'_>, com1: &mut Console, online: &Online, tsc_hz: u64) -> bool {
    let cpus = taking_part(online);
    if cpus.len() < 2 {
        let _ = writeln!(
            com1,
            "origin-kernel: scheduler: thread migration: delivery: fewer than two processors take part in moves ({}): nothing to move{}",
            cpus.len(),
            match pinned::entered() {
                Some((trigger, _)) =>
                    alloc::format!("; pinned mode entered: {trigger:?}: scheduler failed"),
                None => alloc::string::String::new(),
            }
        );
        return pinned::entered().is_none();
    }
    let settled_before = sched::settle(tsc_hz);
    let before = counts();
    let follows_before = crate::threads::follows();
    let unclassed_before = sched::follows_unclassed();
    let receivers = cpus.len() * PER_CPU;
    let shared = Arc::new(Shared {
        stop: AtomicBool::new(false),
        deadline: AtomicU64::new(u64::MAX),
        threads: (0..receivers).map(|_| Movable::new()).collect(),
        pairs: Vec::new(),
        senders: (0..cpus.len())
            .map(|_| Sender {
                posted: AtomicU64::new(0),
                longest_followed: AtomicU64::new(0),
                longest_followed_follows: AtomicU64::new(0),
                longest_plain: AtomicU64::new(0),
                wake: Spans::new(),
                null: Spans::new(),
            })
            .collect(),
        swaps_failed: AtomicU64::new(0),
        canaries_changed: AtomicU64::new(0),
        ended: AtomicU64::new(0),
        started: AtomicU64::new(0),
        fill_release: AtomicBool::new(false),
        fill_queue: WaitQueue::new(),
        fill_up: AtomicU64::new(0),
        fill_ended: AtomicU64::new(0),
        tsc_hz,
        pulled: None,
    });
    set_shared(Some(Arc::clone(&shared)));
    let mut spawned = spawn_movable(&cpus, receiver);
    for (i, &cpu) in cpus.iter().enumerate() {
        spawned &= sched::spawn(cpu, sender, u64::try_from(i).unwrap_or(u64::MAX)).is_ok();
    }
    let total = u64::try_from(receivers + cpus.len()).unwrap_or(u64::MAX);
    sched::yield_until(
        || shared.started.load(Ordering::Acquire) == total,
        tsc_hz,
        2,
    );
    // The senders' run, from now that every thread has started (R96-4).
    shared.run_for(SECONDS);
    let mut moves = Moves::new(online.cpus().iter().map(|c| c.get() + 1).max().unwrap_or(0));
    if spawned {
        move_for(flow, &shared, &cpus, online, tsc_hz, false, &mut moves);
    }
    // The senders stop at their deadline; then every token posted must be taken.
    sched::yield_until(
        || shared.ended.load(Ordering::Acquire) == u64::try_from(cpus.len()).unwrap_or(0),
        tsc_hz,
        sched::OTHERS_PATIENCE,
    );
    let all_taken = || {
        shared
            .threads
            .iter()
            .all(|t| t.taken.load(Ordering::Acquire) == t.posted.load(Ordering::Acquire))
    };
    sched::yield_until(all_taken, tsc_hz, DRAIN_SECONDS);
    let delivered = all_taken();
    let short = shared
        .threads
        .iter()
        .filter(|t| t.taken.load(Ordering::Acquire) != t.posted.load(Ordering::Acquire))
        .count();
    let posted: u64 = shared
        .senders
        .iter()
        .map(|s| s.posted.load(Ordering::Relaxed))
        .sum();
    let taken: u64 = shared
        .threads
        .iter()
        .map(|t| t.taken.load(Ordering::Acquire))
        .sum();
    // S20, case 1: a failure withdraws moves for the rest of the boot. Undelivered tokens now;
    // any other failure below, once it is known (R96-1).
    let mut entered_here = !delivered && pinned::enter(pinned::Trigger::DeliveryFailed);
    // Every receiver ends: woken by name and through its queue, with moves withdrawn if they
    // failed, so the wake-ups reach them.
    shared.stop.store(true, Ordering::Release);
    for thread in &shared.threads {
        if let Some(name) = thread.name() {
            let _ = sched::wake(name);
        }
        thread.queue.wake_all();
    }
    sched::yield_until(
        || shared.ended.load(Ordering::Acquire) == shared.started.load(Ordering::Acquire),
        tsc_hz,
        sched::OTHERS_PATIENCE,
    );
    let ended = shared.ended.load(Ordering::Acquire) == shared.started.load(Ordering::Acquire);
    if !ended {
        describe_stuck(&shared, "threads did not end");
    }
    let spans = shared.senders.iter().fold((0, 0, 0), |(f, ff, p), s| {
        let sf = s.longest_followed.load(Ordering::Relaxed);
        if sf > f {
            (
                sf,
                s.longest_followed_follows.load(Ordering::Relaxed),
                p.max(s.longest_plain.load(Ordering::Relaxed)),
            )
        } else {
            (f, ff, p.max(s.longest_plain.load(Ordering::Relaxed)))
        }
    });
    let wakes = Spans::total(shared.senders.iter().map(|s| &s.wake));
    let nulls = Spans::total(shared.senders.iter().map(|s| &s.null));
    drop(shared);
    let (follows, most, over_eight) = {
        let (a, m, o) = crate::threads::follows();
        (
            a.wrapping_sub(follows_before.0),
            m,
            o.wrapping_sub(follows_before.2),
        )
    };
    let unclassed = sched::follows_unclassed().wrapping_sub(unclassed_before);
    // The path the proof is for: a token's wake-up carried across a move (R96-2).
    let reached = moves.carried_token != 0;
    let (made, carried, carried_token, refused) = (
        moves.made,
        moves.carried_wake,
        moves.carried_token,
        moves.refused,
    );
    drop(moves);
    let (back, unsettled) = bracket_after(settled_before, before, tsc_hz);
    let refusals = refusals(&refused);
    // Made after the compare, as `refusals` is: no text is on the heap while it is compared.
    let sections = |(ticks, over_b18, over_far, longest): (u64, u64, u64, u64)| {
        alloc::format!(
            "{ticks} ticks ({} ns) in all, {over_b18} over B18's {} ns, {over_far} over {} ns, longest {longest} ticks ({} ns)",
            nanos(ticks, tsc_hz),
            B18_MICROS * 1000,
            FAR_MICROS * 1000,
            nanos(longest, tsc_hz)
        )
    };
    let (wakes, nulls) = (sections(wakes), sections(nulls));
    // Pinned mode entered by anything but this proof's own failure fails it (Q5).
    let entered_before = pinned::entered().is_some() && !entered_here;
    let ok = spawned && delivered && posted == taken && ended && back && reached && !entered_before;
    if !ok && !entered_here {
        entered_here = pinned::enter(pinned::Trigger::DeliveryFailed);
    }
    let pinned_mode = pinned::entered();
    let _ = writeln!(
        com1,
        "origin-kernel: scheduler: thread migration: delivery: {receivers} movable receivers and {} pinned senders, {posted} tokens posted, {taken} taken, {short} receivers short; spawned {spawned}; ended {ended}; heap, frames and stacks back {back}; {made} moves, {carried} carrying a pending wake-up, {carried_token} of them with a token not yet taken, refused: {refusals}; wake-ups followed {follows} moves of movable threads, the most one followed {most}, {over_eight} followed more than 8, {unclassed} of threads gone before they were counted; a sender's wake-up that followed a move: longest {} ticks ({} ns) with {} follows; one that followed none: longest {} ticks ({} ns); senders' wake-up sections: {wakes}; their null sections, each as long as their wake-ups' recent median: {nulls}{}{}{}",
        cpus.len(),
        spans.0,
        nanos(spans.0, tsc_hz),
        spans.1,
        spans.2,
        nanos(spans.2, tsc_hz),
        match pinned_mode {
            Some((trigger, _)) => alloc::format!("; pinned mode entered: {trigger:?}"),
            None => alloc::string::String::new(),
        },
        unsettled,
        if ok { "" } else { ": scheduler failed" }
    );
    if entered_here {
        pinned::line(
            pinned::Trigger::DeliveryFailed,
            CpuIndex::BOOT,
            "; the boot goes on with moves withdrawn",
        );
    }
    ok
}

// ------------------------------------------------------------------------------------- pulled

/// The most threads a pulled phase starts on one processor: its run queue holds them all, with
/// room for a sender and for threads the boot flow brings back.
const PULLED_MOST: usize = 12;

/// Where a pulled phase starts its threads, and how many: an application processor where three or
/// more processors take part, the boot processor with two (the module documentation, "Pulled");
/// an even number of threads, [`PER_CPU`] for each processor and at most [`PULLED_MOST`].
fn pulled_start(cpus: &[CpuIndex]) -> Option<(CpuIndex, usize)> {
    let source = *if cpus.len() >= 3 {
        cpus.get(1)?
    } else {
        cpus.first()?
    };
    Some((source, (cpus.len() * PER_CPU).min(PULLED_MOST) & !1))
}

/// Idle processors a pulled phase leaves free to pull, at most (the module documentation,
/// "Pulled").
const PULLERS_MOST: usize = 4;

/// The processors a pulled phase occupies: those that take part and could pull, which is every
/// one but the boot processor and `source`, less the [`PULLERS_MOST`] left free, the two nearest
/// `source` in index and the two farthest. None where no more than that could pull.
fn occupied(cpus: &[CpuIndex], source: CpuIndex) -> Vec<CpuIndex> {
    let others: Vec<CpuIndex> = cpus
        .iter()
        .copied()
        .filter(|&cpu| cpu != CpuIndex::BOOT && cpu != source)
        .collect();
    let near = PULLERS_MOST / 2;
    let far = PULLERS_MOST - near;
    others
        .len()
        .checked_sub(far)
        .filter(|_| others.len() > PULLERS_MOST)
        .and_then(|end| others.get(near..end))
        .map(<[CpuIndex]>::to_vec)
        .unwrap_or_default()
}

/// A pulled phase's occupant: a pinned thread that keeps its processor from being idle and halts
/// between its interrupts until released, so that it is busy to the scheduler and costs the host
/// nothing.
extern "sysv64" fn occupant(_: u64) -> ! {
    let Some(shared) = shared() else {
        sched::finish(|| {});
    };
    shared.fill_up.fetch_add(1, Ordering::AcqRel);
    while !shared.fill_release.load(Ordering::Acquire) {
        cpu::idle();
    }
    sched::finish(move || {
        shared.fill_ended.fetch_add(1, Ordering::AcqRel);
        drop(shared);
    })
}

/// Starts a pulled phase's occupants and waits for them to run: how many, and whether every one
/// started.
fn occupy(shared: &Shared, cpus: &[CpuIndex], source: CpuIndex, tsc_hz: u64) -> (usize, bool) {
    let on = occupied(cpus, source);
    let spawned = on.iter().all(|&cpu| sched::spawn(cpu, occupant, 0).is_ok());
    let want = u64::try_from(on.len()).unwrap_or(u64::MAX);
    sched::yield_until(
        || shared.fill_up.load(Ordering::Acquire) == want,
        tsc_hz,
        PHASE_SECONDS,
    );
    (
        on.len(),
        spawned && shared.fill_up.load(Ordering::Acquire) == want,
    )
}

/// Releases a pulled phase's occupants and waits for them to end: whether every one that started
/// has ended.
fn vacate(shared: &Shared, tsc_hz: u64) -> bool {
    shared.fill_release.store(true, Ordering::Release);
    let ended =
        || shared.fill_ended.load(Ordering::Acquire) == shared.fill_up.load(Ordering::Acquire);
    sched::yield_until(ended, tsc_hz, PHASE_SECONDS);
    ended()
}

/// The balancer's counts a pulled phase reads twice, for what happened in between.
#[derive(Clone, Copy, Default)]
struct PullCounts {
    pulls: u64,
    carrying: u64,
    again: u64,
    asks: u64,
    kicks_tick: u64,
    kicks_upcall: u64,
    kicks_here: u64,
}

impl PullCounts {
    fn read() -> Self {
        let of = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        let counts = &pull::COUNTS;
        Self {
            pulls: of(&counts.pulls),
            carrying: of(&counts.pulls_carrying),
            again: of(&counts.pulls_again),
            asks: of(&counts.asks),
            kicks_tick: of(&counts.kicks_tick),
            kicks_upcall: of(&counts.kicks_upcall),
            kicks_here: of(&counts.kicks_here),
        }
    }

    fn since(self, before: Self) -> Self {
        Self {
            pulls: self.pulls.wrapping_sub(before.pulls),
            carrying: self.carrying.wrapping_sub(before.carrying),
            again: self.again.wrapping_sub(before.again),
            asks: self.asks.wrapping_sub(before.asks),
            kicks_tick: self.kicks_tick.wrapping_sub(before.kicks_tick),
            kicks_upcall: self.kicks_upcall.wrapping_sub(before.kicks_upcall),
            kicks_here: self.kicks_here.wrapping_sub(before.kicks_here),
        }
    }
}

/// What the idle processors and the boot flow did over one pulled phase.
struct Pulled {
    /// The balancer's counts when every thread of the phase had started (the review of TASKS
    /// item 98, R98-3: a pull of a thread that has never run is not counted for the phase).
    before: PullCounts,
    /// Moves the boot flow made back to the start processor, and how many it was refused.
    back: u64,
    back_refused: u64,
    /// When the pulls the phase requires were first seen, in TSC ticks from its start.
    reached_after: Option<u64>,
}

impl Pulled {
    fn begin() -> Self {
        Self {
            before: PullCounts::read(),
            back: 0,
            back_refused: 0,
            reached_after: None,
        }
    }

    fn counts(&self) -> PullCounts {
        PullCounts::read().since(self.before)
    }

    fn reached(&self, tsc_hz: u64) -> alloc::string::String {
        match self.reached_after {
            Some(after) => alloc::format!("yes, after {} ms", nanos(after, tsc_hz) / 1_000_000),
            None => alloc::format!("no, in {COVER_SECONDS} s"),
        }
    }
}

/// The balancer's bounds, as counted since the boot began, apart from what enforces each (the
/// module documentation, "Pulled"): by each run queue against its own ticks, the most balancing
/// interrupts one processor sent in one tick and the most asks one took in one tick; pulls of a
/// thread sooner than its move's period; pulls of a cache-hot thread across caches. And the
/// longest idle pass, which is printed and not judged (budget B18's trend).
struct Bounds {
    sent: (u64, u32),
    asks: (u64, u32),
    too_soon: u64,
    hot_across: u64,
    longest_pass: u64,
}

impl Bounds {
    fn read() -> Self {
        let (sent, asks) = pull::bounds();
        Self {
            sent,
            asks,
            too_soon: pull::COUNTS.too_soon.load(Ordering::Relaxed),
            hot_across: pull::COUNTS.hot_across.load(Ordering::Relaxed),
            longest_pass: pull::COUNTS.longest_pass.load(Ordering::Relaxed),
        }
    }

    fn hold(&self) -> bool {
        self.sent.1 <= 1 && self.asks.1 <= 1 && self.too_soon == 0 && self.hot_across == 0
    }

    fn line(&self, tsc_hz: u64) -> alloc::string::String {
        alloc::format!(
            "bounds since boot: of {} balancing interrupts the most one processor sent in one tick {}, of {} asks the most one took in one tick {}, pulls sooner than the move's period {}, of a cache-hot thread across caches {}: held {}; longest idle pass {} ns",
            self.sent.0,
            self.sent.1,
            self.asks.0,
            self.asks.1,
            self.too_soon,
            self.hot_across,
            self.hold(),
            nanos(self.longest_pass, tsc_hz),
        )
    }
}

/// A pulled phase's run: the idle processors pull `shared`'s threads away from `source`, and the
/// boot flow moves each one it finds elsewhere back to `source`, at most once per thread per tick
/// of its own processor. For [`SECONDS`], and on until `reached` holds (the pulls the phase
/// requires), within [`COVER_SECONDS`]. On the boot processor's TSC alone.
fn pull_back_for(
    flow: Pinned<'_>,
    shared: &Shared,
    source: CpuIndex,
    online: &Online,
    tsc_hz: u64,
    pulled: &mut Pulled,
    reached: impl Fn(PullCounts) -> bool,
) {
    let me = cpu::this_cpu(flow.here());
    let start = rdtsc();
    let (window, budget) = (
        tsc_hz.saturating_mul(SECONDS),
        tsc_hz.saturating_mul(COVER_SECONDS),
    );
    loop {
        let elapsed = rdtsc().wrapping_sub(start);
        if pulled.reached_after.is_none() && reached(pulled.counts()) {
            pulled.reached_after = Some(elapsed);
        }
        if (elapsed >= window && pulled.reached_after.is_some()) || elapsed >= budget {
            break;
        }
        for thread in &shared.threads {
            let Some(name) = thread.name() else {
                continue;
            };
            let tick = sched::queue_ticks(me);
            if thread.last_moved.load(Ordering::Relaxed) == tick || sched::queued_on(name, source) {
                continue;
            }
            match sched::migrate(name, source, online) {
                Ok(_) => {
                    pulled.back += 1;
                    thread.last_moved.store(tick, Ordering::Relaxed);
                }
                Err(Unmoved::AlreadyThere) => {}
                Err(_) => pulled.back_refused += 1,
            }
        }
        sched::yield_now();
        cpu::interrupt_window();
        let _ = sched::reap();
    }
}

/// What a pulled phase says where it cannot run, and whether that is a pass: fewer than two
/// processors is nothing to pull (a pass, as the first phases say of nothing to move); moves
/// withdrawn is the first phase's failure, already told; and balancing not set up where two or
/// more processors take part is a failure of its own (the review of TASKS item 98, R98-3).
fn pulled_cannot_run(com1: &mut Console, proof: &str, cpus: &[CpuIndex]) -> Option<bool> {
    let (why, pass) = if cpus.len() < 2 {
        ("fewer than two processors take part in moves", true)
    } else if pinned::withdrawn() {
        ("moves are withdrawn", false)
    } else if !pull::is_set_up() {
        ("balancing is not set up", false)
    } else {
        return None;
    };
    let _ = writeln!(
        com1,
        "origin-kernel: scheduler: thread migration: {proof}, pulled: {why}: nothing to pull{}",
        if pass { "" } else { ": scheduler failed" }
    );
    Some(pass)
}

/// Integrity's second phase: its threads, declared for balancing and run with interrupts on,
/// pulled by the idle processors (the module documentation, "Pulled"). It passes on the same
/// counts as the first phase, on at least one pull of a thread that had been moved before, and
/// on the balancer's bounds.
fn integrity_by_pulls(flow: Pinned<'_>, com1: &mut Console, online: &Online, tsc_hz: u64) -> bool {
    let cpus = taking_part(online);
    if let Some(pass) = pulled_cannot_run(com1, "integrity", &cpus) {
        return pass;
    }
    let Some((source, threads)) = pulled_start(&cpus) else {
        return true;
    };
    let settled_before = sched::settle(tsc_hz);
    let before = counts();
    let unready_before = sched::unready_switches();
    let shared = Shared::for_pulled(threads, threads / 2, 0, source, tsc_hz);
    let (occupants, occupied) = occupy(&shared, &cpus, source, tsc_hz);
    let spawned = occupied
        && (0..threads).all(|i| {
            sched::spawn_balanced(
                source,
                integrity_thread,
                u64::try_from(i).unwrap_or(u64::MAX),
            )
            .is_ok()
        });
    let total = u64::try_from(threads).unwrap_or(u64::MAX);
    sched::yield_until(
        || shared.started.load(Ordering::Acquire) == total,
        tsc_hz,
        2,
    );
    let mut pulled = Pulled::begin();
    shared.run_for(COVER_SECONDS + SECONDS + PHASE_SECONDS);
    if spawned {
        pull_back_for(flow, &shared, source, online, tsc_hz, &mut pulled, |c| {
            c.again >= 1
        });
    }
    shared.stop.store(true, Ordering::Release);
    for pair in &shared.pairs {
        pair.queue.wake_all();
    }
    sched::yield_until(
        || shared.ended.load(Ordering::Acquire) == shared.started.load(Ordering::Acquire),
        tsc_hz,
        sched::OTHERS_PATIENCE,
    );
    let ended = shared.ended.load(Ordering::Acquire) == shared.started.load(Ordering::Acquire);
    if !ended {
        describe_stuck(&shared, "pulled threads did not end");
    }
    let vacated = vacate(&shared, tsc_hz);
    let failed_swaps = shared.swaps_failed.load(Ordering::Relaxed);
    let changed = shared.canaries_changed.load(Ordering::Relaxed);
    let moved_holding = shared
        .pulled
        .as_ref()
        .map_or(0, |p| p.moved_holding.load(Ordering::Relaxed));
    drop(shared);
    let unready = sched::unready_switches().wrapping_sub(unready_before);
    let (did, bounds) = (pulled.counts(), Bounds::read());
    let (back, unsettled) = bracket_after(settled_before, before, tsc_hz);
    let pinned_mode = pinned::entered();
    let ok = spawned
        && ended
        && vacated
        && failed_swaps == 0
        && changed == 0
        && unready == 0
        && back
        && did.again >= 1
        && bounds.hold()
        && pinned_mode.is_none();
    let _ = writeln!(
        com1,
        "origin-kernel: scheduler: thread migration: integrity, pulled: {threads} threads declared for balancing, run with interrupts on, started on CPU {source} of {} processors, {occupants} of them occupied; {} pulls by idle processors, {} of a thread moved before, {} asks, kicks {} at ticks, {} at reschedule upcalls and {} at wake-ups or spawns, {} moves back by the boot flow ({} refused); threads moved while they held their word {moved_holding}; a second processor in a context {failed_swaps} times, canaries changed {changed} times, threads run before their state existed {unready}; a pull of a thread moved before: {}; {}; spawned {spawned}; ended {ended}; occupants ended {vacated}; heap, frames and stacks back {back}{}{}{}",
        cpus.len(),
        did.pulls,
        did.again,
        did.asks,
        did.kicks_tick,
        did.kicks_upcall,
        did.kicks_here,
        pulled.back,
        pulled.back_refused,
        pulled.reached(tsc_hz),
        bounds.line(tsc_hz),
        match pinned_mode {
            Some((trigger, _)) => alloc::format!("; pinned mode entered: {trigger:?}"),
            None => alloc::string::String::new(),
        },
        unsettled,
        if ok { "" } else { ": scheduler failed" }
    );
    ok
}

/// Delivery's second phase: its receivers, declared for balancing, pulled by the idle processors
/// while one pinned sender on their start processor posts to them (the module documentation,
/// "Pulled"). It passes on every token posted being taken, on at least one wake-up that crossed
/// a pull with its token untaken, and on the balancer's bounds. A failure enters pinned mode, as
/// the first phase's does.
fn delivery_by_pulls(flow: Pinned<'_>, com1: &mut Console, online: &Online, tsc_hz: u64) -> bool {
    let cpus = taking_part(online);
    if let Some(pass) = pulled_cannot_run(com1, "delivery", &cpus) {
        return pass;
    }
    let Some((source, receivers)) = pulled_start(&cpus) else {
        return true;
    };
    let settled_before = sched::settle(tsc_hz);
    let before = counts();
    let shared = Shared::for_pulled(receivers, 0, 1, source, tsc_hz);
    let (occupants, occupied) = occupy(&shared, &cpus, source, tsc_hz);
    let mut spawned = occupied
        && (0..receivers).all(|i| {
            sched::spawn_balanced(source, receiver, u64::try_from(i).unwrap_or(u64::MAX)).is_ok()
        });
    spawned &= sched::spawn(source, sender, 0).is_ok();
    let total = u64::try_from(receivers + 1).unwrap_or(u64::MAX);
    sched::yield_until(
        || shared.started.load(Ordering::Acquire) == total,
        tsc_hz,
        2,
    );
    let mut pulled = Pulled::begin();
    // The sender runs until the boot flow ends its run below.
    shared.run_for(COVER_SECONDS + SECONDS + PHASE_SECONDS);
    if spawned {
        let crossed = || {
            shared
                .pulled
                .as_ref()
                .map_or(0, |p| p.crossed.load(Ordering::Relaxed))
        };
        pull_back_for(flow, &shared, source, online, tsc_hz, &mut pulled, |_| {
            crossed() >= 1
        });
    }
    // The sender stops; then every token posted must be taken.
    shared.deadline.store(0, Ordering::Release);
    sched::yield_until(
        || shared.ended.load(Ordering::Acquire) == 1,
        tsc_hz,
        sched::OTHERS_PATIENCE,
    );
    let all_taken = || {
        shared
            .threads
            .iter()
            .all(|t| t.taken.load(Ordering::Acquire) == t.posted.load(Ordering::Acquire))
    };
    sched::yield_until(all_taken, tsc_hz, DRAIN_SECONDS);
    let delivered = all_taken();
    let short = shared
        .threads
        .iter()
        .filter(|t| t.taken.load(Ordering::Acquire) != t.posted.load(Ordering::Acquire))
        .count();
    let posted: u64 = shared
        .senders
        .iter()
        .map(|s| s.posted.load(Ordering::Relaxed))
        .sum();
    let taken: u64 = shared
        .threads
        .iter()
        .map(|t| t.taken.load(Ordering::Acquire))
        .sum();
    // S20, case 1: undelivered tokens withdraw moves before the receivers are woken a last time.
    let mut entered_here = !delivered && pinned::enter(pinned::Trigger::DeliveryFailed);
    shared.stop.store(true, Ordering::Release);
    for thread in &shared.threads {
        if let Some(name) = thread.name() {
            let _ = sched::wake(name);
        }
        thread.queue.wake_all();
    }
    sched::yield_until(
        || shared.ended.load(Ordering::Acquire) == shared.started.load(Ordering::Acquire),
        tsc_hz,
        sched::OTHERS_PATIENCE,
    );
    let ended = shared.ended.load(Ordering::Acquire) == shared.started.load(Ordering::Acquire);
    if !ended {
        describe_stuck(&shared, "pulled threads did not end");
    }
    let vacated = vacate(&shared, tsc_hz);
    let crossed = shared
        .pulled
        .as_ref()
        .map_or(0, |p| p.crossed.load(Ordering::Relaxed));
    drop(shared);
    let (did, bounds) = (pulled.counts(), Bounds::read());
    let (back, unsettled) = bracket_after(settled_before, before, tsc_hz);
    let entered_before = pinned::entered().is_some() && !entered_here;
    let ok = spawned
        && delivered
        && posted == taken
        && ended
        && vacated
        && back
        && crossed >= 1
        && bounds.hold()
        && !entered_before;
    if !ok && !entered_here {
        entered_here = pinned::enter(pinned::Trigger::DeliveryFailed);
    }
    let pinned_mode = pinned::entered();
    let _ = writeln!(
        com1,
        "origin-kernel: scheduler: thread migration: delivery, pulled: {receivers} receivers declared for balancing and one pinned sender, started on CPU {source} of {} processors, {occupants} of them occupied; {posted} tokens posted, {taken} taken, {short} receivers short; {} pulls by idle processors, {} carrying a pending wake-up, {} of a thread moved before, {} asks, kicks {} at ticks, {} at reschedule upcalls and {} at wake-ups or spawns, {} moves back by the boot flow ({} refused); wake-ups that crossed a pull with their token untaken {crossed}: {}; {}; spawned {spawned}; ended {ended}; occupants ended {vacated}; heap, frames and stacks back {back}{}{}{}",
        cpus.len(),
        did.pulls,
        did.carrying,
        did.again,
        did.asks,
        did.kicks_tick,
        did.kicks_upcall,
        did.kicks_here,
        pulled.back,
        pulled.back_refused,
        pulled.reached(tsc_hz),
        bounds.line(tsc_hz),
        match pinned_mode {
            Some((trigger, _)) => alloc::format!("; pinned mode entered: {trigger:?}"),
            None => alloc::string::String::new(),
        },
        unsettled,
        if ok { "" } else { ": scheduler failed" }
    );
    if entered_here {
        pinned::line(
            pinned::Trigger::DeliveryFailed,
            CpuIndex::BOOT,
            "; the boot goes on with moves withdrawn",
        );
    }
    ok
}
