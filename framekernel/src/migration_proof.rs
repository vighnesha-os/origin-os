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
//! register canaries, 256 bytes of stack canary and four XMM canaries, and a "running on" word
//! that it takes by compare-and-swap first thing after each return from a yield or block, and
//! releases by compare-and-swap last thing before the next one, running with interrupts off in
//! between, so that nothing switches it out while it holds the word: S23's "set at switch-in,
//! cleared at switch-out", done by the thread itself at no cost to the switch path (the ruling's
//! Q9). A failed swap is a second processor running the same context. The threads pass a turn back
//! and forth in pairs through a wait queue, and yield instead in one iteration in four, so that
//! they block, are woken and yield while the boot flow moves them.
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
use crate::sched::{self, ThreadRef, Unmoved, pinned};
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
    /// The TSC rate, for the senders' measures in ticks.
    tsc_hz: u64,
}

impl Shared {
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
/// stacks held.
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

/// The register canaries of integrity's thread `index`.
fn register_canaries(index: u64) -> [u64; 6] {
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
    let on = cpu::without_interrupts_here(|here| {
        u64::try_from(cpu::this_cpu(here).index().get()).unwrap_or(u64::MAX - 1)
    })
    .saturating_add(1);
    if word
        .compare_exchange(0, on, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        shared.swaps_failed.fetch_add(1, Ordering::Relaxed);
    }
    on
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
    cpu::without_interrupts(|| {
        // The canaries: six values kept across the loop, in registers or spilled; 256 bytes in
        // this frame; four XMM registers, which only this thread's FPU area holds.
        let registers = core::hint::black_box(register_canaries(index));
        let stack: [u8; 256] =
            core::hint::black_box(core::array::from_fn(|k| stack_byte(index, k)));
        cpu::xmm_load(xmm_canaries(index));
        let mut iteration = 0u64;
        loop {
            let on = take_word(&shared, &me.running_on);
            let stack_ok = core::hint::black_box(&stack)
                .iter()
                .enumerate()
                .all(|(k, &b)| b == stack_byte(index, k));
            if core::hint::black_box(registers) != register_canaries(index)
                || !stack_ok
                || cpu::xmm_read() != xmm_canaries(index)
            {
                shared.canaries_changed.fetch_add(1, Ordering::Relaxed);
            }
            iteration += 1;
            let ending = shared.stop.load(Ordering::Acquire) || shared.past();
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
    });
    sched::finish(move || {
        shared.ended.fetch_add(1, Ordering::AcqRel);
        drop(shared);
    })
}

/// "thread migration: integrity" (Refuse; the contract's §3.1). See the module documentation.
pub(crate) fn prove_integrity(
    flow: Pinned<'_>,
    com1: &mut Console,
    online: &Online,
    tsc_hz: u64,
) -> bool {
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
        tsc_hz,
    });
    set_shared(Some(Arc::clone(&shared)));
    let spawned = spawn_movable(&cpus, integrity_thread);
    let total = u64::try_from(threads).unwrap_or(u64::MAX);
    sched::yield_until(
        || shared.started.load(Ordering::Acquire) == total,
        tsc_hz,
        2,
    );
    // Their own end, past the mover's longest run: the mover's stop ends them first.
    shared.run_for(COVER_SECONDS + SECONDS);
    let mut moves = Moves::new(online.cpus().iter().map(|c| c.get() + 1).max().unwrap_or(0));
    if spawned {
        move_for(flow, &shared, &cpus, online, tsc_hz, true, &mut moves);
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
        && pinned_mode.is_none();
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
                if before == taken && !shared.stop.load(Ordering::Acquire) {
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

/// "thread migration: delivery" (Withdraw: "no thread moves"; the contract's §3.2). See the
/// module documentation. On any failure it enters pinned mode (S20, case 1; trigger 1), so the
/// boot goes on with moves withdrawn as its verdict says (ADR-0013 R2): undelivered tokens enter
/// it before the receivers are woken a last time, so that those wake-ups reach them with moves
/// withdrawn, and every other failure once it is known, after the compare (the review of TASKS
/// item 96, R96-1). An entry into pinned mode before it, during the boot's proofs, fails it too,
/// naming the trigger (the ruling's Q5).
pub(crate) fn prove_delivery(
    flow: Pinned<'_>,
    com1: &mut Console,
    online: &Online,
    tsc_hz: u64,
) -> bool {
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
        tsc_hz,
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
        "origin-kernel: scheduler: thread migration: delivery: {receivers} movable receivers and {} pinned senders, {posted} tokens posted, {taken} taken, {short} receivers short; spawned {spawned}; ended {ended}; heap, frames and stacks back {back}; {made} moves, {carried} carrying a pending wake-up, {carried_token} of them with a token not yet taken, refused: {refusals}; wake-ups followed {follows} moves of movable threads, the most one followed {most}, {over_eight} followed more than 8; a sender's wake-up that followed a move: longest {} ticks ({} ns) with {} follows; one that followed none: longest {} ticks ({} ns); senders' wake-up sections: {wakes}; their null sections, each as long as their wake-ups' recent median: {nulls}{}{}{}",
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
