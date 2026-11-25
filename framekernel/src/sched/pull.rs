//! The idle pull, the idle kick and the ask (ADR-0020 S7.1, S7.2 and S10, and its Amendment 1;
//! TASKS item 98): how a thread reaches a processor that has nothing to run.
//!
//! Balancing in version 1 moves a thread only by a pull: the processor that will run it is the
//! one that moves it (S7). This module is that mover and what tells it to look:
//!
//! - **The idle pull** ([`idle`]). An application processor's idle flow, before it halts,
//!   publishes that it is idle, chooses the nearest processor that has a thread waiting
//!   (`origin_sync::balance`, over the levels of [`crate::balance::Plan`]), and under both run
//!   queues' locks takes one thread it may take. It then yields to it.
//! - **The idle kick** ([`kick`]). A processor that finds, at one of its own upcalls, that a
//!   thread balancing may take is ready beside what it runs, tells the nearest idle processor
//!   with one reschedule interrupt, at most one in each of its ticks
//!   (`timer::send_balance_ipi`). The idle flow that takes it makes its pass.
//! - **The ask** (Amendment 1), below.
//!
//! Only threads their spawner declared for balancing are taken ([`super::spawn_balanced`]; the
//! coordinating session's ruling Q10 on this item's contract).
//!
//! # What keeps a processor from idling beside a thread it may take
//!
//! Two things, and the first is the one it rests on. **Every idle application processor makes a
//! pass at each of its own ticks**: the timer's interrupt ends its halt, and its idle flow looks
//! again before it halts again. The kick and the ask only shorten the wait to less than a tick.
//! So a kick that is not sent (the rate was spent, the hint was stale) costs at most a tick.
//!
//! A pass looks at up to [`TRIES`] victims. One that gave it nothing, or that it asked, is not
//! looked at again until every other crowded processor has had its turn: each idle flow keeps a
//! mask of them, a bit a processor (`Setup::held`), so no number of processors it can never take
//! from keeps it from one it can (the reviews' R98-2 and D98-1). When every crowded processor is
//! in the mask, the mask is cleared and they are all looked at again, in the same pass if it has
//! looks left; so a processor kicked by one it has just asked reaches it in that pass.
//!
//! A processor's own queue is looked at only beside a victim's, under the two locks of a look. A
//! thread spawned onto an idle processor by another, which sends no interrupt, waits for that
//! processor's next tick, as it did before this item (the review's D98-10).
//!
//! # What a pull takes
//!
//! A thread that is, under its source's lock: declared for balancing; ready and not running;
//! not pinned by count; settled for the level the pull is made at (S10's move rate,
//! `RunQueue::settled`); not cache-hot, unless the puller shares the source's last-level cache
//! (S10, `RunQueue::cold`); and published in the source's table of contexts
//! (`cpu::frame_published`), so that a move is begun only for a frame that can be lent. Both
//! processors take part in moves (N9), the puller's capacity is above 0 (S9's "avoid"), its
//! queue has room (N13) and nothing of its own to run, and moves are not withdrawn (S20). Of
//! those threads, one that is not cache-hot before one that is, then the lowest pass
//! (`balance::pick`).
//!
//! # The ask (ADR-0020 Amendment 1; owner item 355)
//!
//! A thread whose frame is not published cannot be lent. With exactly two threads that never
//! yield on a processor, the waiting one's frame is always its table's leaving record's between
//! two ticks (`Contexts::switch` publishes it and takes it in one call), so no pull would ever
//! take it. So an idle processor that finds a thread it could otherwise take, unpublished, sends
//! its source one reschedule interrupt, the **ask**, and halts. The source's reschedule upcall
//! runs its step as any other (it switches only if a thread was woken since its last choice);
//! then it publishes its leaving frame and kicks the nearest idle processor ([`kick`], as at any
//! of its upcalls), whose idle flow then pulls the published frame. Every step waits on an
//! event, none on a time.
//!
//! Its bounds: a processor sends at most one balancing interrupt a tick, kicks and asks
//! together (`timer::send_balance_ipi`); a processor takes at most one ask in each of its ticks,
//! however many are idle (`RunQueue::take_ask`, a mark an asker sets under the source's queue
//! lock, which it holds already, and the source's next tick ends); and an ask is sent only for a
//! thread that every other check of the pull would take. With moves withdrawn, none is sent.
//!
//! **What it does not reach.** A processor outside the source's last-level cache takes only a
//! thread that is not cache-hot. Where a processor has exactly two runnable contexts of equal
//! stride that alternate at every tick (two balanced threads; one balanced and one pinned; on
//! the boot processor, the boot flow and one balanced thread), the waiting one has always run
//! within the last tick when a tick comes. It is in fact not cache-hot from one window after
//! the tick until the next tick, but nothing judges then: only its own processor judges, at its
//! own upcalls, the idle processors' ticks fall with it, and an ask sent at a tick finds the
//! thread still hot. So that one thread is not taken across caches (the reviews' R98-1 and
//! D98-5; a limit, recorded with the item).
//!
//! # Locks
//!
//! A pull takes the two run-queue locks in CPU-index order, then the move gate, inside
//! `cpu::move_context`; it takes no other lock, frees nothing and drops no stack, so no TLB
//! shootdown starts under a queue lock (S7, "Locks"). It does not wait long for either queue
//! lock: after [`LOCK_TRIES`] tries it leaves that victim for this pass, so that many idle
//! processors that look at one crowded one at the same tick do not queue on its lock with
//! interrupts off (the review's R98-7). A kick is made under its own processor's queue lock
//! alone. The published elements and the hints are atomics.
//!
//! # Interrupts off
//!
//! Each victim a pass tries is its own interrupts-off section: the search, both queue locks, the
//! checks and at most one move. Interrupts are on between them (the contract's §2; the
//! coordinating session's ruling of 2026-11-25, which found the pass first built as one section).
//! The last section publishes that the processor is idle, reads who is crowded once more, looks
//! at what it found, and ends in the halt (`sti; hlt`), so no interrupt can fall between that
//! read and the halt: one that arrives meanwhile ends the halt at once. Each section's time is
//! kept against budget B18, the pass's whole length as a trend ([`Counts::longest_pass`]). The
//! idle flow runs with interrupts on outside its sections, as it did before this item: its reap is
//! preemptible (`run_ap`; the review's D98-2).
//!
//! # What it costs where no thread is declared for balancing
//!
//! - **A switch:** nothing in `with_queue`, `RunQueue::switch` and `follow_thread`, which are
//!   not edited. But the run queue's steps that lead to one pay for the cache-hot rule whether or
//!   not a thread is balanced: **a block, a sleep and a wake-up's preemption** each set one bit
//!   in a word of the queue (`RunQueue::ran`), under the lock they hold already. The static
//!   counts and the IPC ping-pong against the base are this item's evidence for it (the review's
//!   D98-7).
//! - **A spawn and an exit:** a spawn writes the slot's balancing state (six words of the
//!   queue), an exit two.
//! - **A tick:** one test of the queue's `balanced` bits, and [`ticked`], out of line: the
//!   rate's renewal, the idle flag, and this processor's two hint bits, about ten loads, two of
//!   them of the shared hint words.
//! - **A reschedule upcall:** the same test, and [`left_idle`]: one load, and one store when the
//!   processor was idle.
//! - **A wake-up:** in its local branch only, one load of the woken record's declaration.
//! - **An idle pass:** the idle flag's store, a fence, and a read of the crowded hints' words.

use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::fmt::Write;

use origin_frame::once::SetOnce;
use origin_frame::sync::balance::{self as published, Element, Hints, Load, Masks, Victim};
use origin_frame::sync::{SpinGuard, SpinLock};
use origin_frame::trap::{Here, InterruptsOff, Pinned};

use super::{BALANCE, Placed, QUEUES, RunQueue, Unmoved, pinned};
use crate::balance::{self, CacheFrom, Candidate, Plan};
use crate::console::Console;
use crate::cpu::{self, CpuIndex, ThisCpu, rdtsc};
use crate::runqueue::{CAPACITY, Census};
use crate::timer;

/// Victims an idle pass looks at under their locks before it halts.
const TRIES: usize = 3;
/// Tries at one queue lock before a pass leaves that victim (the module documentation, "Locks").
const LOCK_TRIES: u32 = 64;
/// More levels than a processor's plan can have: one for each kind of scheduling domain, and one
/// to spare ([`kick`]'s table of them).
const LEVELS: usize = 8;

/// What balancing is set up with, once, before any thread balancing may take exists.
struct Setup {
    masks: Masks,
    /// Each processor's levels and their periods. Leaked: made once, never freed, as the
    /// per-processor tables are.
    plan: &'static Plan,
    /// For each CPU index, its groups, narrowest first, as `origin_sync::balance` takes them.
    levels: Vec<Vec<&'static [u32]>>,
    /// For each CPU index, whether it takes part in moves (`smp::Online::takes_part_in_moves`).
    takes_part: Vec<bool>,
    /// For each CPU index, the victims its idle flow passes over for now, a bit a processor (the
    /// module documentation). A row is set, read and cleared by its own processor's idle flow
    /// alone.
    held: Vec<Hints>,
    /// The cache-hot window, in TSC ticks.
    window: u64,
    /// Budget B18's 50 us, in TSC ticks: what [`Counts::over_b18_quiet`] counts passes against.
    b18: u64,
}

impl Setup {
    fn takes_part(&self, cpu: usize) -> bool {
        self.takes_part.get(cpu).copied().unwrap_or(false)
    }
}

static SETUP: SetOnce<Setup> = SetOnce::new();

/// Sets balancing up from the scheduling domains (`domains::DOMAINS`) and says so on one line.
/// The boot flow, once, before the first thread balancing may take is spawned and before any
/// count of the heap is taken: what it allocates is kept for good.
///
/// Without the domains, without the memory for the masks, or without a TSC rate the cache-hot
/// window can be counted in (a rate below 1 MHz, or none measured, would make every waiting thread
/// look cold), nothing is set up: no processor pulls and none kicks, and the line says why. The
/// plan and the lists beside it are allocated as the rest of the boot's tables are: a heap that
/// cannot hold them stops the kernel.
pub(crate) fn set_up(com1: &mut Console, online: &crate::smp::Online, tsc_hz: u64) {
    let made = (|| {
        let domains = crate::domains::DOMAINS
            .get()
            .ok_or("the domains were not built")?;
        if tsc_hz < 1_000_000 {
            return Err("the TSC's rate is not known");
        }
        let count = QUEUES.indexes();
        let masks = Masks::new(count).ok_or("no memory for its masks")?;
        let held = (0..count)
            .map(|_| Hints::new(count))
            .collect::<Option<Vec<_>>>()
            .ok_or("no memory for its masks")?;
        let shapes = balance::shapes(&domains.tree);
        let plan: &'static Plan = Box::leak(Box::new(Plan::build(
            &shapes,
            domains.tree.cpus(),
            count,
            domains.caches_reported,
            domains.node_order.len(),
        )));
        let takes_part: Vec<bool> = (0..count)
            .map(|cpu| CpuIndex::new(cpu).is_some_and(|c| online.takes_part_in_moves(c)))
            .collect();
        Ok::<_, &'static str>(Setup {
            masks,
            plan,
            levels: (0..count).map(|cpu| plan.levels(cpu)).collect(),
            takes_part,
            held,
            window: (tsc_hz / 1_000_000).saturating_mul(balance::HOT_MICROS),
            b18: (tsc_hz / 1_000_000).saturating_mul(50),
        })
    })();
    match made {
        Ok(setup) => {
            let taking = setup.takes_part.iter().filter(|&&t| t).count();
            let cache = match setup.plan.cache_from {
                CacheFrom::Reported => "the processors' reports",
                CacheFrom::Package => "the package, since no processor reports a cache",
                CacheFrom::Nobody => "nothing: no two processors share one",
            };
            let _ = writeln!(
                com1,
                "origin-kernel: balancing: idle pull and idle kick set up: {taking} of {} processors take part in moves; a cache-hot thread (ran within {} us) stays in its last-level cache, read from {cache}; a thread moves again after more than 1, 4 or 16 ticks, by the widest level its move crosses",
                online.cpus().len(),
                balance::HOT_MICROS,
            );
            let _ = SETUP.set(setup);
        }
        Err(why) => {
            let _ = writeln!(
                com1,
                "origin-kernel: balancing: not set up ({why}): no processor pulls or kicks"
            );
        }
    }
}

/// What the idle pull, the idle kick and the ask have done since the boot began: counts, each
/// raised with a relaxed add off every hot path, and read for a proof's line.
#[derive(Debug, Default)]
pub(crate) struct Counts {
    /// Threads pulled; how many of them were cache-hot (taken inside their cache); how many
    /// carried a wake-up that had come before they blocked (N1); and how many had arrived on
    /// their source by a move before (a thread moved, and then pulled).
    pub(crate) pulls: AtomicU64,
    pub(crate) pulls_hot: AtomicU64,
    pub(crate) pulls_carrying: AtomicU64,
    pub(crate) pulls_again: AtomicU64,
    /// Idle passes made; those that found no crowded processor to look at; and those that found
    /// a thread of the processor's own ready.
    pub(crate) passes: AtomicU64,
    pub(crate) no_victim: AtomicU64,
    pub(crate) own: AtomicU64,
    /// Looks under a victim's lock that took nothing, and looks given up because a queue lock
    /// was held.
    pub(crate) nothing: AtomicU64,
    pub(crate) contended: AtomicU64,
    /// Threads passed over, by reason, each time one was looked at.
    pub(crate) pinned: AtomicU64,
    pub(crate) unsettled: AtomicU64,
    pub(crate) hot: AtomicU64,
    pub(crate) unpublished: AtomicU64,
    /// Moves a pull began that were refused, and passes stopped because moves are withdrawn, the
    /// puller's queue had no room, or the table of contexts was full.
    pub(crate) refused: AtomicU64,
    pub(crate) stopped: AtomicU64,
    /// Kicks sent, by where: the tick, the reschedule upcall, a wake-up or a spawn made here.
    pub(crate) kicks_tick: AtomicU64,
    pub(crate) kicks_upcall: AtomicU64,
    pub(crate) kicks_here: AtomicU64,
    /// Kicks not sent: the processor had sent its one for this tick, or nobody was idle.
    pub(crate) kicks_over_rate: AtomicU64,
    pub(crate) kicks_nobody_idle: AtomicU64,
    /// Asks sent (ADR-0020 Amendment 1), and asks not sent: the source had been asked in its
    /// present tick, or this processor had sent its one balancing interrupt for this tick.
    pub(crate) asks: AtomicU64,
    pub(crate) asks_not_first: AtomicU64,
    pub(crate) asks_over_rate: AtomicU64,
    /// A pull of a thread that was not settled, and of a cache-hot thread by a processor outside
    /// its cache. Each asks the run queue again after the choice, so it is 0 unless the test that
    /// refuses such a thread has been taken out; it does not show that test to be right, which
    /// the run queue's host tests and its randomized model do (the review's R98-10).
    pub(crate) too_soon: AtomicU64,
    pub(crate) hot_across: AtomicU64,
    /// The longest idle pass, in TSC ticks of the processor that made it, from its start to its
    /// halt or its pull, its sections and the gaps between them (a trend); the longest of its
    /// interrupts-off sections that made no move, and of those that made one; the longest move a
    /// section made (`sched::carry`, under both queue locks); and the sections longer than budget
    /// B18's 50 us, without a move and with one. So a boot tells a section's own work from a
    /// move's, and both from the host's stalls (the coordinating session's rulings on the first
    /// boots' 1.05 ms pass).
    pub(crate) longest_pass: AtomicU64,
    pub(crate) longest_quiet: AtomicU64,
    pub(crate) longest_moving: AtomicU64,
    pub(crate) longest_move: AtomicU64,
    pub(crate) over_b18_quiet: AtomicU64,
    pub(crate) over_b18_moving: AtomicU64,
}

/// The counts ([`Counts`]).
pub(crate) static COUNTS: Counts = Counts {
    pulls: AtomicU64::new(0),
    pulls_hot: AtomicU64::new(0),
    pulls_carrying: AtomicU64::new(0),
    pulls_again: AtomicU64::new(0),
    passes: AtomicU64::new(0),
    no_victim: AtomicU64::new(0),
    own: AtomicU64::new(0),
    nothing: AtomicU64::new(0),
    contended: AtomicU64::new(0),
    pinned: AtomicU64::new(0),
    unsettled: AtomicU64::new(0),
    hot: AtomicU64::new(0),
    unpublished: AtomicU64::new(0),
    refused: AtomicU64::new(0),
    stopped: AtomicU64::new(0),
    kicks_tick: AtomicU64::new(0),
    kicks_upcall: AtomicU64::new(0),
    kicks_here: AtomicU64::new(0),
    kicks_over_rate: AtomicU64::new(0),
    kicks_nobody_idle: AtomicU64::new(0),
    asks: AtomicU64::new(0),
    asks_not_first: AtomicU64::new(0),
    asks_over_rate: AtomicU64::new(0),
    too_soon: AtomicU64::new(0),
    hot_across: AtomicU64::new(0),
    longest_pass: AtomicU64::new(0),
    longest_quiet: AtomicU64::new(0),
    longest_moving: AtomicU64::new(0),
    longest_move: AtomicU64::new(0),
    over_b18_quiet: AtomicU64::new(0),
    over_b18_moving: AtomicU64::new(0),
};

fn count(counter: &AtomicU64, by: u64) {
    if by != 0 {
        counter.fetch_add(by, Relaxed);
    }
}

/// The bounds, as every processor's run queue counted them against its own ticks
/// (`RunQueue::sent`, `RunQueue::asks`): the balancing interrupts sent in all and the most one
/// processor sent in one of its ticks, then the asks taken in all and the most one processor took
/// in one of its ticks. Each "most" is 1, or 0 with none. Each queue is read under its lock.
pub(crate) fn bounds() -> ((u64, u32), (u64, u32)) {
    (0..QUEUES.indexes())
        .filter_map(|cpu| QUEUES.get(cpu))
        .map(|queue| {
            cpu::without_interrupts(|| {
                let queue = queue.lock();
                (queue.sent(), queue.asks())
            })
        })
        .fold(((0, 0), (0, 0)), |(sent, asks), (s, a)| {
            (
                (sent.0.saturating_add(s.0), sent.1.max(s.1)),
                (asks.0.saturating_add(a.0), asks.1.max(a.1)),
            )
        })
}

/// Whether balancing is set up ([`set_up`]): without it no processor pulls.
pub(crate) fn is_set_up() -> bool {
    SETUP.get().is_some()
}

/// One line of every count since the boot began, report only: what a boot's reader needs to
/// see why pulls were or were not made (the review's D98-6). Written after each pulled phase of
/// the migration proofs. `tsc_hz` turns the longest pass into nanoseconds.
pub(crate) fn report(com1: &mut Console, tsc_hz: u64) {
    let of = |counter: &AtomicU64| counter.load(Relaxed);
    let c = &COUNTS;
    let nanos =
        |counter: &AtomicU64| u128::from(of(counter)) * 1_000_000_000 / u128::from(tsc_hz.max(1));
    let _ = writeln!(
        com1,
        "origin-kernel: balancing: counts since boot: idle passes {}, {} with no crowded processor to look at, {} with a thread of their own ready; pulls {} ({} cache-hot, {} carrying a pending wake-up, {} of a thread moved before); looks that took nothing {}, given up at a held queue lock {}; threads passed over: pinned {}, moved too recently {}, cache-hot {}, not published {}; moves refused {}, passes stopped {}; kicks {} at ticks, {} at reschedule upcalls, {} at wake-ups or spawns, not sent: {} over the rate, {} with nobody idle; asks {} sent, not sent: {} with the source asked already, {} over the rate; longest idle pass {} ns; its interrupts-off sections: the longest without a move {} ns, with one {} ns, the move itself {} ns; sections over B18's 50 us: {} without a move, {} with one",
        of(&c.passes),
        of(&c.no_victim),
        of(&c.own),
        of(&c.pulls),
        of(&c.pulls_hot),
        of(&c.pulls_carrying),
        of(&c.pulls_again),
        of(&c.nothing),
        of(&c.contended),
        of(&c.pinned),
        of(&c.unsettled),
        of(&c.hot),
        of(&c.unpublished),
        of(&c.refused),
        of(&c.stopped),
        of(&c.kicks_tick),
        of(&c.kicks_upcall),
        of(&c.kicks_here),
        of(&c.kicks_over_rate),
        of(&c.kicks_nobody_idle),
        of(&c.asks),
        of(&c.asks_not_first),
        of(&c.asks_over_rate),
        nanos(&c.longest_pass),
        nanos(&c.longest_quiet),
        nanos(&c.longest_moving),
        nanos(&c.longest_move),
        of(&c.over_b18_quiet),
        of(&c.over_b18_moving),
    );
}

/// The census of processor `cpu`'s queue as it is published: its counts, and its weights scaled
/// by the processor's capacity as it is now (ADR-0020 S9).
fn load_of(cpu: usize, census: &Census) -> Load {
    Load {
        runnable: u8::try_from(census.runnable).unwrap_or(u8::MAX),
        waiting: u8::try_from(census.waiting.count_ones()).unwrap_or(u8::MAX),
        load: balance::scaled(census.weight, crate::capacity::current(cpu)),
    }
}

/// Processor `cpu`'s element, for a search.
fn element(cpu: u32) -> Option<&'static Element> {
    BALANCE.get(cpu as usize)
}

/// The idle flow's turn (`run_ap`'s loop): one pass of the idle pull, which halts the processor
/// when it finds nothing to run, or, with a thread to run, a yield to it. `flow` is the idle
/// flow's witness: no thread calls this. The idle flow calls it with interrupts on, and goes on
/// with them on.
pub(super) fn idle(flow: Pinned<'_>) {
    // The idle flow's own witness: it never moves, so its processor's index holds across the
    // pass's sections and the interrupts between them.
    let me = cpu::this_cpu(flow.here());
    let start = rdtsc();
    let mut owed = None;
    let run = match SETUP.get() {
        Some(setup) => pass(me, setup, start, &mut owed),
        None => {
            halt();
            false
        }
    };
    if let Some((trigger, on)) = owed {
        pinned::line(trigger, on, "; the thread stays where it is");
    }
    if run {
        // An arrival is marked woken, and the idle flow gives way to any ready thread.
        super::yield_now();
    }
}

/// The halt with nothing to pull: `sti; hlt; cli` in a stretch with interrupts off, so that an
/// interrupt that came before it ends it at once; interrupts on again after it.
fn halt() {
    cpu::without_interrupts(cpu::idle_disabling);
}

/// Keeps one section's time, `took` TSC ticks, of which `carried` in a move
/// ([`Counts::longest_quiet`]).
fn sectioned(took: u64, carried: u64) {
    let c = &COUNTS;
    let over = SETUP.get().is_some_and(|setup| took > setup.b18);
    if carried == 0 {
        c.longest_quiet.fetch_max(took, Relaxed);
        count(&c.over_b18_quiet, u64::from(over));
    } else {
        c.longest_moving.fetch_max(took, Relaxed);
        c.longest_move.fetch_max(carried, Relaxed);
        count(&c.over_b18_moving, u64::from(over));
    }
}

/// What one section of a pass came to.
enum Step {
    /// No crowded processor this flow may look at.
    None,
    /// A look at a victim, and what it came to.
    Looked(u32, Pull),
}

/// One pass of the idle pull on this processor: `true` if the idle flow is to run a thread. It
/// halts the processor itself when there is none, unless a pinned-mode line is owed.
///
/// **Its sections** (the contract's §2; the coordinating session's ruling of 2026-11-25). Each
/// victim tried is its own interrupts-off section: the search, both queue locks, the checks and
/// at most one move. Interrupts are on between them, so a pass holds them off for one look at a
/// time, which is what budget B18 bounds. The last step is one more short section: this
/// processor publishes that it is idle, reads who is crowded once more
/// (`origin_sync::balance::idle_then_choose`, "Neither misses the other"), looks at what that
/// read found, if anything, and halts in the same stretch, so no interrupt falls between that
/// read and the halt. The pass's whole length, sections and gaps, is kept as a trend
/// ([`Counts::longest_pass`]).
fn pass(
    me: ThisCpu<'_>,
    setup: &Setup,
    start: u64,
    owed: &mut Option<(pinned::Trigger, CpuIndex)>,
) -> bool {
    let index = me.index().get();
    let (Some(own), Some(levels), Some(held), Ok(mine)) = (
        BALANCE.this(me),
        setup.levels.get(index),
        setup.held.get(index),
        u32::try_from(index),
    ) else {
        halt();
        return false;
    };
    // A processor that takes no part in moves, or that is to be avoided (capacity 0, ADR-0020
    // S9), never says it is idle: nobody kicks it, and it pulls nothing.
    if levels.is_empty()
        || !setup.takes_part(index)
        || crate::capacity::current(index) == 0
        || pinned::withdrawn()
    {
        halt();
        return false;
    }
    count(&COUNTS.passes, 1);
    let mut tried = [u32::MAX; TRIES];
    let (mut looked, mut cleared) = (0, false);
    // A look at the victim `choose` finds, in a section of its own.
    let look = |tried: &[u32; TRIES], owed: &mut Option<(pinned::Trigger, CpuIndex)>| {
        cpu::without_interrupts_off(|off| {
            let began = rdtsc();
            let allowed = |cpu: u32, _: usize| {
                setup.takes_part(cpu as usize) && !tried.contains(&cpu) && !held.test(cpu as usize)
            };
            let Some(victim) =
                published::choose(mine, &setup.masks.crowded, levels, element, allowed)
            else {
                return Step::None;
            };
            let mut carried = 0;
            let pulled = pull_from(off, me, victim, setup, owed, &mut carried);
            sectioned(rdtsc().saturating_sub(began), carried);
            Step::Looked(victim.cpu, pulled)
        })
    };
    while looked < TRIES {
        let (cpu, pulled) = match look(&tried, owed) {
            Step::Looked(cpu, pulled) => (cpu, pulled),
            Step::None => {
                // Every crowded processor left is one this flow passes over: they all get
                // another turn, once in a pass, and in this pass if it has looks left.
                if !cleared && held.any() {
                    held.clear_all();
                    cleared = true;
                    continue;
                }
                count(&COUNTS.no_victim, u64::from(looked == 0));
                break;
            }
        };
        if let Some(slot) = tried.get_mut(looked) {
            *slot = cpu;
        }
        looked += 1;
        match settle_look(cpu, pulled, own, held) {
            Some(run) => return finish(start, run, owed),
            None => continue,
        }
    }
    // The last section: publish idle, read who is crowded once more, look at what it found, and
    // halt, in one stretch with interrupts off.
    cpu::without_interrupts(|| {
        let (run, done) = cpu::without_interrupts_off(|off| {
            let began = rdtsc();
            let allowed = |cpu: u32, _: usize| {
                setup.takes_part(cpu as usize) && !tried.contains(&cpu) && !held.test(cpu as usize)
            };
            let found =
                published::idle_then_choose(mine, own, &setup.masks, levels, element, allowed);
            let mut carried = 0;
            let settled = found.map(|victim| {
                let pulled = pull_from(off, me, victim, setup, owed, &mut carried);
                settle_look(victim.cpu, pulled, own, held)
            });
            sectioned(rdtsc().saturating_sub(began), carried);
            match settled.flatten() {
                Some(true) => (true, true),
                _ => (false, owed.is_some()),
            }
        });
        COUNTS
            .longest_pass
            .fetch_max(rdtsc().saturating_sub(start), Relaxed);
        if !done {
            // With interrupts still off: one that came since the read ends the halt at once.
            cpu::idle_disabling();
        }
        run
    })
}

/// What a look at victim `cpu` that came to `pulled` means for the pass: `Some(true)` to run a
/// thread, `Some(false)` to end the pass and halt, `None` to look at the next victim.
fn settle_look(cpu: u32, pulled: Pull, own: &Element, held: &Hints) -> Option<bool> {
    match pulled {
        Pull::Made => {
            own.leave_idle();
            held.clear_all();
            Some(true)
        }
        Pull::Own => {
            count(&COUNTS.own, 1);
            own.leave_idle();
            Some(true)
        }
        Pull::Stop => {
            count(&COUNTS.stopped, 1);
            Some(false)
        }
        // The source was asked to publish: this processor idles, and the source's kick wakes it
        // (ADR-0020 Amendment 1, step 1: "it then idles"). The source is passed over until the
        // others have had their turn, so that a processor whose asks another always answers
        // first still reaches the rest (the review's D98-1).
        Pull::Asked => {
            held.set(cpu as usize);
            Some(false)
        }
        Pull::Nothing => {
            count(&COUNTS.nothing, 1);
            held.set(cpu as usize);
            None
        }
        Pull::Later => None,
    }
}

/// Ends a pass before its last section: keeps its length and, unless it is to run a thread or a
/// pinned-mode line is owed, halts.
fn finish(start: u64, run: bool, owed: &Option<(pinned::Trigger, CpuIndex)>) -> bool {
    COUNTS
        .longest_pass
        .fetch_max(rdtsc().saturating_sub(start), Relaxed);
    if !run && owed.is_none() {
        halt();
    }
    run
}

/// What one look at a victim came to.
enum Pull {
    /// A thread was pulled.
    Made,
    /// Nothing there for this processor: the next victim, and this one after the others
    /// (`Setup::held`).
    Nothing,
    /// Not now: a queue lock was held for longer than a pass waits, or an ask was held back by
    /// one of its bounds. The next victim, and this one again at the next pass.
    Later,
    /// A thread of this processor's own became ready meanwhile: it runs that.
    Own,
    /// This pass is over: moves are withdrawn, this queue has no room, or a move was refused.
    Stop,
    /// A thread this processor could otherwise take is not published, and its source was asked
    /// to publish it (ADR-0020 Amendment 1). This pass is over.
    Asked,
}

/// Takes `lock` if it comes free within [`LOCK_TRIES`] tries; counts a look given up if not.
fn take<T>(lock: &SpinLock<T>) -> Option<SpinGuard<'_, T>> {
    let guard = (0..LOCK_TRIES).find_map(|_| {
        let guard = lock.try_lock();
        if guard.is_none() {
            core::hint::spin_loop();
        }
        guard
    });
    if guard.is_none() {
        count(&COUNTS.contended, 1);
    }
    guard
}

/// Pulls one thread from `victim` to this processor, if it holds one this processor may take.
fn pull_from(
    off: InterruptsOff<'_>,
    me: ThisCpu<'_>,
    victim: Victim,
    setup: &Setup,
    owed: &mut Option<(pinned::Trigger, CpuIndex)>,
    carried: &mut u64,
) -> Pull {
    let to = me.index();
    let (Some(from), Some(step)) = (
        CpuIndex::new(victim.cpu as usize),
        setup.plan.step(to.get(), victim.level),
    ) else {
        return Pull::Nothing;
    };
    let (Some(source_queue), Some(destination_queue)) =
        (QUEUES.get(from.get()), QUEUES.get(to.get()))
    else {
        return Pull::Nothing;
    };
    if from == to {
        return Pull::Nothing;
    }
    // Both queue locks, in index order (`origin_sync::threads`, "Locks"), as `migrate` takes
    // them, but without a long wait for either.
    let (mut source_guard, mut destination_guard) = if from.get() < to.get() {
        let Some(source) = take(source_queue) else {
            return Pull::Later;
        };
        let Some(destination) = take(destination_queue) else {
            return Pull::Later;
        };
        (source, destination)
    } else {
        let Some(destination) = take(destination_queue) else {
            return Pull::Later;
        };
        let Some(source) = take(source_queue) else {
            return Pull::Later;
        };
        (source, destination)
    };
    let (source, destination) = (&mut *source_guard, &mut *destination_guard);
    // Under the source's lock (S20).
    if pinned::withdrawn() {
        return Pull::Stop;
    }
    // A thread of this processor's own became ready meanwhile.
    if destination.census().runnable != 0 {
        return Pull::Own;
    }
    // Its queue is full of threads that are not ready (N13).
    if !destination.has_room() {
        return Pull::Stop;
    }
    let (waiting, cold) = (source.census().waiting, source.cold());
    let (mut pinned_by_count, mut unsettled, mut hot, mut unpublished) = (0, 0, 0, 0);
    let candidates = (1..CAPACITY)
        .filter(|&slot| waiting & (1 << slot) != 0)
        .filter_map(|slot| {
            let placed = source.record(slot)?;
            if placed.record().pins() != 0 {
                pinned_by_count += 1;
                return None;
            }
            if !source.settled(slot, step.period) {
                unsettled += 1;
                return None;
            }
            let is_cold = cold & (1 << slot) != 0;
            if !is_cold && !step.shares_cache {
                hot += 1;
                return None;
            }
            if !cpu::frame_published(from, source.frame(slot)?) {
                unpublished += 1;
                return None;
            }
            Some(Candidate {
                slot,
                pass: source.pass_of(slot),
                cold: is_cold,
            })
        });
    let picked = balance::pick(candidates, step.shares_cache);
    count(&COUNTS.pinned, pinned_by_count);
    count(&COUNTS.unsettled, unsettled);
    count(&COUNTS.hot, hot);
    count(&COUNTS.unpublished, unpublished);
    let chosen = picked.and_then(|candidate| {
        let placed = source.record(candidate.slot)?;
        Some((candidate, placed, source.frame(candidate.slot)?))
    });
    let Some((candidate, placed, frame)) = chosen else {
        // What the victim's queue holds now, so that a word gone stale stops naming it.
        publish(from.get(), source, setup);
        if unpublished == 0 {
            return Pull::Nothing;
        }
        // The ask (ADR-0020 Amendment 1): a thread passed every other check and is not
        // published. This processor's own rate first, so that the source is not marked as asked
        // by a processor that then cannot send; then the source's mark, under its lock.
        let Some(apic_id) = from.apic_id() else {
            return Pull::Nothing;
        };
        if timer::balance_spent(me) {
            count(&COUNTS.asks_over_rate, 1);
            return Pull::Later;
        }
        if !source.take_ask() {
            count(&COUNTS.asks_not_first, 1);
            return Pull::Later;
        }
        source.note_ask();
        // The source takes its own queue's lock in the upcall the ask causes: it is let go first.
        drop(source_guard);
        if !timer::send_balance_ipi(me, apic_id) {
            return Pull::Later;
        }
        destination.note_sent();
        count(&COUNTS.asks, 1);
        return Pull::Asked;
    };
    // Counted apart from the tests above ([`Counts::too_soon`]).
    if !source.settled(candidate.slot, step.period) {
        count(&COUNTS.too_soon, 1);
    }
    if !candidate.cold && !step.shares_cache {
        count(&COUNTS.hot_across, 1);
    }
    let again = source.since_arrival(candidate.slot).is_some();
    let began = rdtsc();
    let moved = super::carry(
        off,
        placed.record(),
        candidate.slot,
        frame,
        (from, &mut *source),
        (to, &mut *destination),
    );
    *carried = carried.saturating_add(rdtsc().saturating_sub(began));
    publish(from.get(), source, setup);
    publish(to.get(), destination, setup);
    drop((source_guard, destination_guard));
    let on = to;
    match super::moved_or_why(moved, |trigger| {
        if pinned::enter(trigger) {
            *owed = Some((trigger, on));
        }
    }) {
        Ok(carried_wake) => {
            count(&COUNTS.pulls, 1);
            count(&COUNTS.pulls_hot, u64::from(!candidate.cold));
            count(&COUNTS.pulls_carrying, u64::from(carried_wake));
            count(&COUNTS.pulls_again, u64::from(again));
            Pull::Made
        }
        // The frame was published under the source's lock (`Contexts::published` reads the
        // leaving record first, as `lend` does), so `lend` answers this only where the source
        // is between the two stores of a later publication; taken as nothing.
        Err(Unmoved::TryLater) => Pull::Nothing,
        Err(_) => {
            count(&COUNTS.refused, 1);
            Pull::Stop
        }
    }
}

/// Publishes processor `cpu`'s census, taken under its queue's lock, which the caller holds.
fn publish(cpu: usize, queue: &RunQueue<Placed>, setup: &Setup) {
    if let Some(own) = BALANCE.get(cpu) {
        own.publish(cpu, load_of(cpu, &queue.census()), &setup.masks.crowded);
    }
}

/// This processor is not idle if it runs a thread: its element says so, for whoever would kick
/// it. One plain load unless it was idle.
#[inline]
fn left_idle(queue: &RunQueue<Placed>, me: ThisCpu<'_>) {
    if queue.busy()
        && let Some(own) = BALANCE.this(me)
    {
        own.leave_idle();
    }
}

/// The tick's step of this processor's queue, with what balancing does at a tick (k1). With no
/// thread on the queue that balancing may take, that is [`ticked`] alone.
#[inline]
pub(super) fn tick(queue: &mut RunQueue<Placed>, interrupted: u64, here: Here<'_>) -> u64 {
    if queue.any_balanced() {
        return tick_balanced(queue, interrupted, here);
    }
    let next = queue.tick(interrupted);
    ticked(queue, here);
    next
}

/// What every tick does for balancing: this processor may send one balancing interrupt again;
/// it is not idle if it runs a thread; and its hints are cleared where they are wrong, a census
/// published first if its word still names a thread that is no longer there.
#[inline(never)]
fn ticked(queue: &RunQueue<Placed>, here: Here<'_>) {
    let me = cpu::this_cpu(here);
    timer::balance_tick(me);
    left_idle(queue, me);
    let (Some(own), Some(setup)) = (BALANCE.this(me), SETUP.get()) else {
        return;
    };
    let index = me.index().get();
    if own.load().waiting != 0 && !queue.any_balanced() {
        own.publish(index, load_of(index, &queue.census()), &setup.masks.crowded);
    }
    own.settle(index, &setup.masks);
}

/// The tick on a queue that holds a thread balancing may take: the cache-hot judgement before
/// the step, so that a thread that waited the whole of the last tick is named; the step; the
/// mark; and the kick.
#[inline(never)]
fn tick_balanced(queue: &mut RunQueue<Placed>, interrupted: u64, here: Here<'_>) -> u64 {
    let now = rdtsc();
    let was = queue.current();
    queue.judge(now, window());
    let next = queue.tick(interrupted);
    queue.mark(now);
    ticked(queue, here);
    if kick(queue, here, was) {
        count(&COUNTS.kicks_tick, 1);
    }
    next
}

/// The reschedule upcall's step (k2): `RunQueue::preempt_for_wake`, and, on a queue that holds a
/// thread balancing may take, the cache-hot judgement and the kick. An ask arrives here: the
/// step switches nothing, and the kick publishes and tells an idle processor.
#[inline]
pub(super) fn rescheduled(queue: &mut RunQueue<Placed>, interrupted: u64, here: Here<'_>) -> u64 {
    if queue.any_balanced() {
        return rescheduled_balanced(queue, interrupted, here);
    }
    let next = queue.preempt_for_wake(interrupted);
    left_idle(queue, cpu::this_cpu(here));
    next
}

#[inline(never)]
fn rescheduled_balanced(queue: &mut RunQueue<Placed>, interrupted: u64, here: Here<'_>) -> u64 {
    let was = queue.current();
    let next = queue.preempt_for_wake(interrupted);
    queue.judge(rdtsc(), window());
    left_idle(queue, cpu::this_cpu(here));
    if kick(queue, here, was) {
        count(&COUNTS.kicks_upcall, 1);
    }
    next
}

/// A thread balancing may take was made ready on processor `cpu`'s queue by this processor, in
/// thread context or from a handler, inside an interrupts-off section (k3: a wake-up; k4: a
/// spawn). If `cpu` is this processor, it looks for an idle one to kick. Out of line: the
/// wake-up's own path tests the record's declaration and nothing else.
///
/// The rate is asked first: a processor that has sent its one balancing interrupt for this tick
/// does none of the rest, and its next tick publishes and kicks (the review's R98-9). So a
/// thread that makes others ready in a loop pays, each time after the first, this processor's
/// index, a compare and one load of its rate's flag.
#[cold]
#[inline(never)]
pub(super) fn made_ready_here(cpu: u32, here: Here<'_>) {
    let me = cpu::this_cpu(here);
    if u32::try_from(me.index().get()) != Ok(cpu) || timer::balance_spent(me) {
        return;
    }
    let Some(queue) = QUEUES.this(me) else {
        return;
    };
    let mut queue = queue.lock();
    if !queue.any_balanced() {
        return;
    }
    let was = queue.current();
    queue.judge(rdtsc(), window());
    if kick(&mut queue, here, was) {
        count(&COUNTS.kicks_here, 1);
    }
}

/// The cache-hot window in TSC ticks; unreachable while balancing is not set up, so nothing is
/// then judged cold.
fn window() -> u64 {
    SETUP.get().map_or(u64::MAX, |setup| setup.window)
}

/// The idle kick (ADR-0020 S7.2), by the processor whose queue holds the thread: publishes this
/// processor's census, and, if it runs something and a thread balancing may take is ready beside
/// it and lendable, sends the nearest idle processor one reschedule interrupt. `true` if one was
/// sent. `queue` is this processor's, locked by the caller; `was` is the slot that ran before the
/// step the caller just made, or the running slot if it made none.
///
/// **Publication first.** This processor's own leaving frame is published before anything is
/// counted (`cpu::publish_leaving_here`): the caller is one of its scheduler's upcalls or a
/// thread of its own with interrupts off, so it has left that frame's stack. After it, every
/// thread that is ready and not running here has its frame in its slot, but the one the step
/// just switched out, which this trap is still on: that one is not counted, since a kick for it
/// would spend the tick's one interrupt on a frame no puller can take.
///
/// **Lendable** is judged level by level, by the tests a pull made at that level would apply
/// (the reviews' R98-8 and D98-8): a waiting thread no count pins, settled for the level's period
/// (S10's move rate), and not cache-hot unless the level shares this processor's last-level
/// cache.
///
/// **Who is told.** The nearest processor whose idle flow has published that it is idle, that
/// takes part in moves and has capacity, at a level at which some thread here is lendable.
///
/// The interrupt is sent under this queue's lock, which the upcall holds for its whole step: one
/// write of a register, which waits for nothing.
fn kick(queue: &mut RunQueue<Placed>, here: Here<'_>, was: usize) -> bool {
    let Some(setup) = SETUP.get() else {
        return false;
    };
    let me = cpu::this_cpu(here);
    let index = me.index().get();
    let (Some(own), Some(levels), Ok(mine)) = (
        BALANCE.this(me),
        setup.levels.get(index),
        u32::try_from(index),
    ) else {
        return false;
    };
    cpu::publish_leaving_here(here);
    let census = queue.census();
    let load = load_of(index, &census);
    let out = if queue.current() == was || was >= CAPACITY {
        0
    } else {
        1u16 << was
    };
    let unpinned = (1..CAPACITY)
        .filter(|&slot| (census.waiting & !out) & (1 << slot) != 0)
        .filter(|&slot| {
            queue
                .record(slot)
                .is_some_and(|placed| placed.record().pins() == 0)
        })
        .fold(0u16, |unpinned, slot| unpinned | (1 << slot));
    let cold = queue.cold();
    // For each level, whether a pull made at it would take a thread from here.
    let mut open = [false; LEVELS];
    for (level, at_level) in open.iter_mut().enumerate().take(levels.len()) {
        if let Some(step) = setup.plan.step(index, level) {
            *at_level = (1..CAPACITY).any(|slot| {
                unpinned & (1 << slot) != 0
                    && queue.settled(slot, step.period)
                    && (step.shares_cache || cold & (1 << slot) != 0)
            });
        }
    }
    if !queue.busy() || !open.contains(&true) || !setup.takes_part(index) || pinned::withdrawn() {
        own.publish(index, load, &setup.masks.crowded);
        return false;
    }
    let target = published::publish_then_find_idle(
        mine,
        own,
        load,
        &setup.masks,
        levels,
        element,
        |cpu, level| {
            setup.takes_part(cpu as usize)
                && crate::capacity::current(cpu as usize) != 0
                && open.get(level).copied().unwrap_or(false)
        },
    );
    let apic_id = target
        .and_then(|cpu| CpuIndex::new(cpu as usize))
        .and_then(CpuIndex::apic_id);
    let Some(apic_id) = apic_id else {
        count(&COUNTS.kicks_nobody_idle, 1);
        return false;
    };
    if timer::send_balance_ipi(me, apic_id) {
        queue.note_sent();
        true
    } else {
        count(&COUNTS.kicks_over_rate, 1);
        false
    }
}
