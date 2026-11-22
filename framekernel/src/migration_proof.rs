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
//! **Integrity.** Two movable threads per processor that takes part run for 2 s. Each holds six
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
//! the senders stop, and every token posted must be taken within 2 s more.
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
    /// The TSC at which the threads' loops end on their own.
    deadline: u64,
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
    /// The TSC rate, for the receivers' spin to the next tick.
    tsc_hz: u64,
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
    /// Refusals by reason, in [`REASONS`]' order.
    refused: [u64; REASONS.len()],
    /// Each move's critical section, in TSC ticks (B20).
    ticks: Vec<u64>,
    /// By processor index: whether it was a source, and a destination, at least once.
    source: Vec<bool>,
    destination: Vec<bool>,
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
/// processor of `cpus` chosen by `choice`, at most once per tick of its own processor.
fn move_for(
    flow: Pinned<'_>,
    shared: &Shared,
    cpus: &[CpuIndex],
    online: &Online,
    tsc_hz: u64,
    moves: &mut Moves,
) {
    let me = cpu::this_cpu(flow.here());
    let mut choice = Choice(0x2545_f491_4f6c_dd1d);
    let start = rdtsc();
    while rdtsc().wrapping_sub(start) < tsc_hz.saturating_mul(SECONDS) {
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
            match sched::migrate(name, to, online) {
                Ok(moved) => {
                    moves.made += 1;
                    moves.carried_wake += u64::from(moved.carried_wake);
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
            let ending = shared.stop.load(Ordering::Acquire) || rdtsc() >= shared.deadline;
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
                            || rdtsc() >= shared.deadline
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
        deadline: rdtsc().saturating_add(tsc_hz.saturating_mul(SECONDS + 1)),
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
    let mut moves = Moves::new(online.cpus().iter().map(|c| c.get() + 1).max().unwrap_or(0));
    if spawned {
        move_for(flow, &shared, &cpus, online, tsc_hz, &mut moves);
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
        if reached { "yes" } else { "no" },
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
        let period = shared.tsc_hz / crate::timer::HZ;
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
                // in which it can be preempted, moved and woken before it blocks (N1).
                let start = rdtsc();
                while rdtsc().wrapping_sub(start) < period {
                    core::hint::spin_loop();
                }
                if me.posted.load(Ordering::Acquire) == taken
                    && !shared.stop.load(Ordering::Acquire)
                {
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
    while let Some(me) = me
        && !shared.stop.load(Ordering::Acquire)
        && rdtsc() < shared.deadline
    {
        let Some(to) = shared.threads.get(choice.next(shared.threads.len())) else {
            break;
        };
        let Some(name) = to.name() else {
            sched::yield_now();
            continue;
        };
        to.posted.fetch_add(1, Ordering::Release);
        me.posted.fetch_add(1, Ordering::Relaxed);
        // The wake-up in an interrupts-off section of the sender's own: the span holds `wake`'s
        // whole section, and the count's change on this processor is this wake-up's follows
        // (D93-2; the contract's "Additions", B).
        cpu::without_interrupts_here(|here| {
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
        });
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
/// module documentation. On a failure it enters pinned mode (S20, case 1; trigger 1), so the
/// boot goes on with moves withdrawn, and wakes every receiver again so that they end; an entry
/// into pinned mode before it, during the boot's proofs, fails it too, naming the trigger (the
/// ruling's Q5).
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
        deadline: rdtsc().saturating_add(tsc_hz.saturating_mul(SECONDS)),
        threads: (0..receivers).map(|_| Movable::new()).collect(),
        pairs: Vec::new(),
        senders: (0..cpus.len())
            .map(|_| Sender {
                posted: AtomicU64::new(0),
                longest_followed: AtomicU64::new(0),
                longest_followed_follows: AtomicU64::new(0),
                longest_plain: AtomicU64::new(0),
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
    let mut moves = Moves::new(online.cpus().iter().map(|c| c.get() + 1).max().unwrap_or(0));
    if spawned {
        move_for(flow, &shared, &cpus, online, tsc_hz, &mut moves);
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
    // S20, case 1: a failure withdraws moves for the rest of the boot.
    let entered_here = !delivered && pinned::enter(pinned::Trigger::DeliveryFailed);
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
    drop(shared);
    let (follows, most, over_eight) = {
        let (a, m, o) = crate::threads::follows();
        (
            a.wrapping_sub(follows_before.0),
            m,
            o.wrapping_sub(follows_before.2),
        )
    };
    let reached = moves.carried_wake != 0;
    let (made, carried, refused) = (moves.made, moves.carried_wake, moves.refused);
    drop(moves);
    let (back, unsettled) = bracket_after(settled_before, before, tsc_hz);
    let refusals = refusals(&refused);
    let pinned_mode = pinned::entered();
    let ok = spawned
        && delivered
        && posted == taken
        && ended
        && back
        && reached
        && pinned_mode.is_none();
    let _ = writeln!(
        com1,
        "origin-kernel: scheduler: thread migration: delivery: {receivers} movable receivers and {} pinned senders, {posted} tokens posted, {taken} taken, {short} receivers short; spawned {spawned}; ended {ended}; heap, frames and stacks back {back}; {made} moves, {carried} carrying a pending wake-up, refused: {refusals}; wake-ups followed {follows} moves of movable threads, the most one followed {most}, {over_eight} followed more than 8; a sender's wake-up that followed a move: longest {} ticks ({} ns) with {} follows; one that followed none: longest {} ticks ({} ns){}{}{}",
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
