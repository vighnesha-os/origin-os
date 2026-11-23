//! The idle pull and the idle kick (ADR-0020 S7.1, S7.2 and S10; TASKS item 98): how a thread
//! reaches a processor that has nothing to run.
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
//!
//! Only threads their spawner declared for balancing are taken ([`super::spawn_balanced`]; the
//! coordinating session's ruling Q10 on this item's contract). With none on a queue, each upcall
//! below pays one load and one branch for all of this.
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
//! **A thread whose frame is not published is passed over.** With exactly two threads that never
//! yield on a processor, the waiting one's frame is always its table's leaving record's between
//! two ticks (`Contexts::switch` publishes it and takes it in one call), so no pull takes it. An
//! idle processor that asks the source to publish it is ADR-0020's Amendment 1, if the owner
//! accepts it (owner item 355); until then this module only counts those threads.
//!
//! # Locks
//!
//! A pull takes the two run-queue locks in CPU-index order, then the move gate, inside
//! `cpu::move_context`; it takes no other lock, frees nothing and drops no stack, so no TLB
//! shootdown starts under a queue lock (S7, "Locks"). A kick is made under its own processor's
//! queue lock alone. The published elements and the hints are atomics.
//!
//! # Interrupts off
//!
//! A pass is one interrupts-off section that ends in the halt (`sti; hlt`), so no interrupt can
//! fall between "nothing to pull" and the halt: one that arrives meanwhile ends the halt at once.
//! The section holds at most [`TRIES`] looks under a pair of locks and one move. Its longest is
//! kept ([`Counts::longest_pass`]) to print beside budget B18.

use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::fmt::Write;

use origin_frame::once::SetOnce;
use origin_frame::sync::balance::{self as published, Element, Load, Masks, Victim};
use origin_frame::trap::{Here, InterruptsOff, Pinned};

use super::{BALANCE, Placed, QUEUES, RunQueue, Unmoved, pinned};
use crate::balance::{self, CacheFrom, Candidate, Plan};
use crate::console::Console;
use crate::cpu::{self, CpuIndex, ThisCpu, rdtsc};
use crate::runqueue::{CAPACITY, Census};
use crate::timer;

/// Victims an idle pass looks at under their locks before it gives up and halts.
const TRIES: usize = 3;

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
    /// The cache-hot window, in TSC ticks.
    window: u64,
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
/// Without the domains, or without the memory for the masks, nothing is set up: no processor
/// pulls and none kicks, and the line says why.
pub(crate) fn set_up(com1: &mut Console, online: &crate::smp::Online, tsc_hz: u64) {
    let made = (|| {
        let domains = crate::domains::DOMAINS
            .get()
            .ok_or("the domains were not built")?;
        let count = QUEUES.indexes();
        let masks = Masks::new(count).ok_or("no memory for its masks")?;
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
            window: (tsc_hz / 1_000_000).max(1) * balance::HOT_MICROS,
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

/// What the idle pull and the idle kick have done since the boot began: counts, each raised with
/// a relaxed add off every hot path, and read for a proof's line.
#[derive(Debug, Default)]
pub(crate) struct Counts {
    /// Threads pulled, and how many of them were cache-hot (taken inside their cache).
    pub(crate) pulls: AtomicU64,
    pub(crate) pulls_hot: AtomicU64,
    /// Looks under a victim's lock that took nothing.
    pub(crate) nothing: AtomicU64,
    /// Threads passed over, by reason, each time one was looked at.
    pub(crate) pinned: AtomicU64,
    pub(crate) unsettled: AtomicU64,
    pub(crate) hot: AtomicU64,
    pub(crate) unpublished: AtomicU64,
    /// Moves a pull began that were refused, and passes stopped because moves are withdrawn, the
    /// puller's queue had work or no room, or the table of contexts was full.
    pub(crate) refused: AtomicU64,
    pub(crate) stopped: AtomicU64,
    /// Kicks sent, by where: the tick, the reschedule upcall, a wake-up or a spawn made here.
    pub(crate) kicks_tick: AtomicU64,
    pub(crate) kicks_upcall: AtomicU64,
    pub(crate) kicks_here: AtomicU64,
    /// Kicks not sent: the processor had sent its one for this tick, or nobody was idle.
    pub(crate) kicks_over_rate: AtomicU64,
    pub(crate) kicks_nobody_idle: AtomicU64,
    /// A pull of a thread that was not settled, and of a cache-hot thread by a processor outside
    /// its cache: counted apart from the tests that refuse them, and 0 unless one of those is
    /// broken.
    pub(crate) too_soon: AtomicU64,
    pub(crate) hot_across: AtomicU64,
    /// The longest idle pass, in TSC ticks of the processor that made it, from its start to its
    /// halt or its pull.
    pub(crate) longest_pass: AtomicU64,
}

/// The counts ([`Counts`]).
pub(crate) static COUNTS: Counts = Counts {
    pulls: AtomicU64::new(0),
    pulls_hot: AtomicU64::new(0),
    nothing: AtomicU64::new(0),
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
    too_soon: AtomicU64::new(0),
    hot_across: AtomicU64::new(0),
    longest_pass: AtomicU64::new(0),
};

fn count(counter: &AtomicU64, by: u64) {
    if by != 0 {
        counter.fetch_add(by, Relaxed);
    }
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

/// The idle flow's turn (`run_ap`'s loop): one pass of the idle pull, then the halt, or, with a
/// thread pulled, a yield to it. `_flow` is the idle flow's witness: no thread calls this.
pub(super) fn idle(_flow: Pinned<'_>) {
    let (pulled, owed) = cpu::without_interrupts_off(|off| {
        let me = cpu::this_cpu(off.here());
        let start = rdtsc();
        let mut owed = None;
        let pulled = pass(off, me, &mut owed);
        // Both readings are this processor's.
        COUNTS
            .longest_pass
            .fetch_max(rdtsc().saturating_sub(start), Relaxed);
        if !pulled {
            // With interrupts still off: one that came during the pass ends the halt at once.
            cpu::idle_disabling();
        }
        (pulled, owed)
    });
    if let Some((trigger, on)) = owed {
        pinned::line(trigger, on, "; the thread stays where it is");
    }
    if pulled {
        // The arrival is marked woken and the idle flow gives way to any ready thread.
        super::yield_now();
    }
}

/// One pass of the idle pull on this processor: `true` if it pulled a thread.
fn pass(
    off: InterruptsOff<'_>,
    me: ThisCpu<'_>,
    owed: &mut Option<(pinned::Trigger, CpuIndex)>,
) -> bool {
    let Some(setup) = SETUP.get() else {
        return false;
    };
    let index = me.index().get();
    let (Some(own), Some(levels), Ok(mine)) = (
        BALANCE.this(me),
        setup.levels.get(index),
        u32::try_from(index),
    ) else {
        return false;
    };
    // A processor that takes no part in moves, or that is to be avoided (capacity 0, ADR-0020
    // S9), never says it is idle: nobody kicks it, and it pulls nothing.
    if levels.is_empty()
        || !setup.takes_part(index)
        || crate::capacity::current(index) == 0
        || pinned::withdrawn()
    {
        return false;
    }
    let mut tried = [u32::MAX; TRIES];
    for attempt in 0..TRIES {
        let allowed = |cpu: u32, _: usize| setup.takes_part(cpu as usize) && !tried.contains(&cpu);
        // The first look publishes that this processor is idle, and only then reads who is
        // crowded (`origin_sync::balance`, "Neither misses the other").
        let victim = if attempt == 0 {
            published::idle_then_choose(mine, own, &setup.masks, levels, element, allowed)
        } else {
            published::choose(mine, &setup.masks.crowded, levels, element, allowed)
        };
        let Some(victim) = victim else {
            return false;
        };
        match pull_from(off, me, victim, setup, owed) {
            Pull::Made => {
                own.leave_idle();
                return true;
            }
            Pull::Stop => {
                count(&COUNTS.stopped, 1);
                return false;
            }
            Pull::Nothing => {
                count(&COUNTS.nothing, 1);
                if let Some(slot) = tried.get_mut(attempt) {
                    *slot = victim.cpu;
                }
            }
        }
    }
    false
}

/// What one look at a victim came to.
enum Pull {
    /// A thread was pulled.
    Made,
    /// Nothing there for this processor: try the next victim.
    Nothing,
    /// This pass is over: moves are withdrawn, this queue has work of its own or no room, or a
    /// move was refused.
    Stop,
}

/// Pulls one thread from `victim` to this processor, if it holds one this processor may take.
fn pull_from(
    off: InterruptsOff<'_>,
    me: ThisCpu<'_>,
    victim: Victim,
    setup: &Setup,
    owed: &mut Option<(pinned::Trigger, CpuIndex)>,
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
    // Both queue locks, in index order (`origin_sync::threads`, "Locks"), as `migrate` takes them.
    let (mut low, mut high) = if from.get() < to.get() {
        (source_queue.lock(), destination_queue.lock())
    } else {
        (destination_queue.lock(), source_queue.lock())
    };
    let (source, destination) = if from.get() < to.get() {
        (&mut *low, &mut *high)
    } else {
        (&mut *high, &mut *low)
    };
    // Under the source's lock (S20).
    if pinned::withdrawn() {
        return Pull::Stop;
    }
    // A thread of this processor's own became ready meanwhile, or its queue is full (N13).
    if destination.census().runnable != 0 || !destination.has_room() {
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
        return Pull::Nothing;
    };
    // Counted apart from the tests above: what a proof holds at 0.
    if !source.settled(candidate.slot, step.period) {
        count(&COUNTS.too_soon, 1);
    }
    if !candidate.cold && !step.shares_cache {
        count(&COUNTS.hot_across, 1);
    }
    let moved = super::carry(
        off,
        placed.record(),
        candidate.slot,
        frame,
        (from, &mut *source),
        (to, &mut *destination),
    );
    publish(from.get(), source, setup);
    publish(to.get(), destination, setup);
    drop((low, high));
    let on = to;
    match super::moved_or_why(moved, |trigger| {
        if pinned::enter(trigger) {
            *owed = Some((trigger, on));
        }
    }) {
        Ok(_) => {
            count(&COUNTS.pulls, 1);
            count(&COUNTS.pulls_hot, u64::from(!candidate.cold));
            Pull::Made
        }
        // The frame was published under the source's lock, so neither can be its answer; taken
        // as nothing rather than trusted.
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
/// thread on the queue that balancing may take, that is the rate's renewal, the idle flag and
/// the hints, out of line.
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

/// The tick on a queue that holds a thread balancing may take: the cache-hot judgement around
/// the step, the mark, and the kick.
#[inline(never)]
fn tick_balanced(queue: &mut RunQueue<Placed>, interrupted: u64, here: Here<'_>) -> u64 {
    let now = rdtsc();
    let (quiet, was) = (queue.quiet(now, window()), queue.current());
    let next = queue.tick(interrupted);
    queue.judge(quiet, was);
    queue.mark(now);
    ticked(queue, here);
    if kick(queue, here, was) {
        count(&COUNTS.kicks_tick, 1);
    }
    next
}

/// The reschedule upcall's step (k2): `RunQueue::preempt_for_wake`, and, on a queue that holds a
/// thread balancing may take, the cache-hot judgement and the kick.
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
    let now = rdtsc();
    let (quiet, was) = (queue.quiet(now, window()), queue.current());
    let next = queue.preempt_for_wake(interrupted);
    queue.judge(quiet, was);
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
#[cold]
#[inline(never)]
pub(super) fn made_ready_here(cpu: u32, here: Here<'_>) {
    let me = cpu::this_cpu(here);
    if u32::try_from(me.index().get()) != Ok(cpu) {
        return;
    }
    let Some(queue) = QUEUES.this(me) else {
        return;
    };
    let mut queue = queue.lock();
    if !queue.any_balanced() {
        return;
    }
    let (quiet, was) = (queue.quiet(rdtsc(), window()), queue.current());
    queue.judge(quiet, was);
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
/// **Who is told.** The nearest processor whose idle flow has published that it is idle, that
/// takes part in moves and has capacity; and, if every thread that waits here is cache-hot, one
/// that shares this processor's last-level cache (S10).
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
    let lendable = census.waiting & !out;
    if !queue.busy() || lendable == 0 || !setup.takes_part(index) || pinned::withdrawn() {
        own.publish(index, load, &setup.masks.crowded);
        return false;
    }
    let cold = queue.cold() & lendable != 0;
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
                && (cold
                    || setup
                        .plan
                        .step(index, level)
                        .is_some_and(|step| step.shares_cache))
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
        true
    } else {
        count(&COUNTS.kicks_over_rate, 1);
        false
    }
}
