//! "load balancing" (Report; TASKS item 98, the coordinating session's ruling R2 on its diagnostic
//! round, 2026-11-25): what the idle pull, the idle kick and the ask (ADR-0020 S7.1, S7.2 and
//! Amendment 1) do with threads declared for balancing, on whatever machine the kernel boots. It
//! runs after the migration proofs, whose pulled phases hold the balancer's bounds.
//!
//! Three cases, each with threads declared for balancing (`sched::spawn_balanced`), each ended
//! and reaped before the next:
//!
//! - **Spread.** Threads that never yield, started on the first processor that takes part: two
//!   for each such processor, as many as its queue holds at a time. **Condition:** every
//!   processor that takes part holds one, judged only once every thread spawned is counted on some
//!   processor (a thread in the middle of a move is on none), within [`PATIENCE`]. Reported: how
//!   long that took, how they lie then and after [`SHARE_SECONDS`], and each thread's share of a
//!   processor. That busy processors also even out ("each held two") is the periodic pull's,
//!   TASKS item 99's, not this item's.
//! - **Two alone** (three or more processors take part). Two such threads on one application
//!   processor, pinned by count until both have run there, then left to balancing. **Condition:**
//!   another processor takes one, within [`PATIENCE`]: the case only the ask makes pass (ADR-0020
//!   Amendment 1). Reported: how long it took. Its limit (R98-1, ADR-0020 S7's dated note): where
//!   the idle processors are beyond the two threads' last-level cache, a thread that ran within the
//!   cache-hot window is not taken, and two threads that alternate always have; QEMU reports no
//!   cache, so every processor here counts as sharing the package's.
//! - **Kick.** A thread that never yields, on the first processor that takes part, wakes
//!   [`WAKEES`] threads declared for balancing, one at a time, [`BETWEEN_WAKES`] apart. A wakee is
//!   spawned unpinned and pins itself when it first runs, so a spawn's own kick can let an idle
//!   processor take one first: that one waits on its new processor, and its wake-up there is a
//!   remote one, which sends the reschedule interrupt and no kick. So the wake-ups the case judges
//!   are those made on the waker's processor, counted from where each wakee blocked.
//!   **Conditions:** every such wake-up sends its kick (`pull::made_ready_here`: the kicks at
//!   wake-ups, counted from when every wakee has blocked, equal them), at least one was made, and
//!   at least one of their threads ran on another processor. Reported: how many ran elsewhere, and
//!   from each wake-up to its thread running, on the monotonic clock (budget B21's path).
//!
//! With fewer than two processors taking part, or with moves withdrawn, it says so and passes. A
//! failure ends its line ": scheduler failed", as the migration proofs' do; no machine forgives it.
#![forbid(unsafe_code)]

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use origin_frame::sync::SpinLock;

use crate::console::Console;
use crate::cpu::{self, CpuIndex, rdtsc};
use crate::sched::{self, ThreadRef, pull};
use crate::smp::Online;
use crate::wait::WaitQueue;

/// Threads the kick case wakes.
const WAKEES: usize = 8;
/// How long each case may wait for what it requires, in seconds: a budget, not a bound (ADR-0013
/// R1 (b)). Each wait ends as soon as it holds.
const PATIENCE: u64 = 30;
/// How long shares are measured, in seconds.
const SHARE_SECONDS: u64 = 2;
/// A step of a thread's own clock longer than this many microseconds is time it did not run.
const STEP_MICROS: u64 = 3000;
/// Between two wake-ups of the kick case, in nanoseconds: three ticks, so that each wake-up's kick
/// is its processor's first balancing interrupt of its tick.
const BETWEEN_WAKES: u64 = 30_000_000;

/// One thread of a case.
struct Thread {
    name: SpinLock<Option<ThreadRef>>,
    /// TSC ticks it saw itself run, each step measured on one processor.
    ran: AtomicU64,
    /// How often it saw itself on another processor.
    moved: AtomicU64,
    /// Kick: where it waits, the processor index plus 1 it blocked on, whether it was woken, when
    /// (monotonic nanoseconds), and what it found when it ran: the monotonic time and the processor
    /// index plus 1.
    queue: WaitQueue,
    parked_on: AtomicU64,
    woken: AtomicBool,
    woken_at: AtomicU64,
    ran_at: AtomicU64,
    ran_on: AtomicU64,
}

impl Thread {
    fn new() -> Self {
        Self {
            name: SpinLock::new(None),
            ran: AtomicU64::new(0),
            moved: AtomicU64::new(0),
            queue: WaitQueue::new(),
            parked_on: AtomicU64::new(0),
            woken: AtomicBool::new(false),
            woken_at: AtomicU64::new(0),
            ran_at: AtomicU64::new(0),
            ran_on: AtomicU64::new(0),
        }
    }

    fn name(&self) -> Option<ThreadRef> {
        cpu::without_interrupts(|| *self.name.lock())
    }
}

/// What a case's threads share with the boot flow, through [`SHARED`].
struct Shared {
    stop: AtomicBool,
    threads: Vec<Thread>,
    started: AtomicU64,
    ended: AtomicU64,
    /// Two alone: threads that have run, and the release of their pins.
    ran_once: AtomicU64,
    unpin: AtomicBool,
    /// Kick: wakees about to block.
    parked: AtomicU64,
    /// A step longer than this, in TSC ticks, is not counted as run.
    step: u64,
}

static SHARED: SpinLock<Option<Arc<Shared>>> = SpinLock::new(None);

fn shared() -> Option<Arc<Shared>> {
    cpu::without_interrupts(|| SHARED.lock().clone())
}

fn set_shared(value: Option<Arc<Shared>>) {
    cpu::without_interrupts(|| *SHARED.lock() = value);
}

fn new_shared(threads: usize, tsc_hz: u64) -> Arc<Shared> {
    let shared = Arc::new(Shared {
        stop: AtomicBool::new(false),
        threads: (0..threads).map(|_| Thread::new()).collect(),
        started: AtomicU64::new(0),
        ended: AtomicU64::new(0),
        ran_once: AtomicU64::new(0),
        unpin: AtomicBool::new(true),
        parked: AtomicU64::new(0),
        step: (tsc_hz / 1_000_000).max(1) * STEP_MICROS,
    });
    set_shared(Some(Arc::clone(&shared)));
    shared
}

/// This processor's index and its TSC, read together, so that both are one processor's.
fn here_and_now() -> (u64, u64) {
    cpu::without_interrupts_here(|here| {
        (
            u64::try_from(cpu::this_cpu(here).index().get()).unwrap_or(u64::MAX - 1),
            rdtsc(),
        )
    })
}

/// Runs until told to stop, adding up the time it sees itself run: only steps between two
/// readings on one processor, so no two processors' counters are subtracted (N10).
fn hog_until_stop(shared: &Shared, me: &Thread) {
    let (mut cpu, mut last) = here_and_now();
    while !shared.stop.load(Ordering::Relaxed) {
        let (at, now) = here_and_now();
        if at == cpu {
            let step = now.wrapping_sub(last);
            if step < shared.step {
                me.ran.fetch_add(step, Ordering::Relaxed);
            }
        } else {
            me.moved.fetch_add(1, Ordering::Relaxed);
            cpu = at;
        }
        last = now;
        core::hint::spin_loop();
    }
}

/// A thread that never yields. In the two-alone case it starts pinned by count and stays so until
/// the boot flow has seen both run.
extern "sysv64" fn hog(index: u64) -> ! {
    let Some(shared) = shared() else {
        sched::finish(|| {});
    };
    let Some(me) = shared
        .threads
        .get(usize::try_from(index).unwrap_or(usize::MAX))
    else {
        sched::finish(|| {});
    };
    let named = sched::current_thread();
    cpu::without_interrupts(|| *me.name.lock() = named);
    if shared.unpin.load(Ordering::Acquire) {
        shared.started.fetch_add(1, Ordering::AcqRel);
    } else {
        let pin = sched::pin();
        shared.started.fetch_add(1, Ordering::AcqRel);
        shared.ran_once.fetch_add(1, Ordering::AcqRel);
        while !shared.unpin.load(Ordering::Acquire) && !shared.stop.load(Ordering::Relaxed) {
            core::hint::spin_loop();
        }
        drop(pin);
    }
    hog_until_stop(&shared, me);
    sched::finish(move || {
        shared.ended.fetch_add(1, Ordering::AcqRel);
        drop(shared);
    })
}

/// The kick case's wakee: pinned by count until it blocks, so that it blocks on the processor it
/// first ran on, which it records.
extern "sysv64" fn wakee(index: u64) -> ! {
    let Some(shared) = shared() else {
        sched::finish(|| {});
    };
    let Some(me) = shared
        .threads
        .get(usize::try_from(index).unwrap_or(usize::MAX))
    else {
        sched::finish(|| {});
    };
    let pin = sched::pin();
    shared.started.fetch_add(1, Ordering::AcqRel);
    cpu::without_interrupts(|| {
        me.parked_on
            .store(here_and_now().0.saturating_add(1), Ordering::Release);
        shared.parked.fetch_add(1, Ordering::AcqRel);
        // With interrupts off nothing switches it out between the pin's end and its block.
        drop(pin);
        me.queue.wait_until(|| me.woken.load(Ordering::Acquire));
        me.ran_at
            .store(crate::time::monotonic_nanos(), Ordering::Release);
    });
    me.ran_on
        .store(here_and_now().0.saturating_add(1), Ordering::Release);
    sched::finish(move || {
        shared.ended.fetch_add(1, Ordering::AcqRel);
        drop(shared);
    })
}

/// The kick case's waker: never yields, and wakes each wakee in turn.
extern "sysv64" fn waker(_: u64) -> ! {
    let Some(shared) = shared() else {
        sched::finish(|| {});
    };
    shared.started.fetch_add(1, Ordering::AcqRel);
    let wait = |nanos: u64| {
        let until = crate::time::monotonic_nanos().saturating_add(nanos);
        while crate::time::monotonic_nanos() < until && !shared.stop.load(Ordering::Relaxed) {
            core::hint::spin_loop();
        }
    };
    for thread in shared.threads.iter().take(WAKEES) {
        wait(BETWEEN_WAKES);
        thread
            .woken_at
            .store(crate::time::monotonic_nanos(), Ordering::Release);
        thread.woken.store(true, Ordering::Release);
        thread.queue.wake_one();
    }
    while !shared.stop.load(Ordering::Relaxed) {
        core::hint::spin_loop();
    }
    sched::finish(move || {
        shared.ended.fetch_add(1, Ordering::AcqRel);
        drop(shared);
    })
}

/// The balancer's counts a case reads twice, for what happened in between.
#[derive(Clone, Copy, Default)]
struct Counted {
    pulls: u64,
    pulls_hot: u64,
    contended: u64,
    kicks_tick: u64,
    kicks_upcall: u64,
    kicks_here: u64,
    over_rate: u64,
    nobody_idle: u64,
    asks: u64,
}

fn counted() -> Counted {
    let c = &pull::COUNTS;
    let r = |a: &AtomicU64| a.load(Ordering::Relaxed);
    Counted {
        pulls: r(&c.pulls),
        pulls_hot: r(&c.pulls_hot),
        contended: r(&c.contended),
        kicks_tick: r(&c.kicks_tick),
        kicks_upcall: r(&c.kicks_upcall),
        kicks_here: r(&c.kicks_here),
        over_rate: r(&c.kicks_over_rate),
        nobody_idle: r(&c.kicks_nobody_idle),
        asks: r(&c.asks),
    }
}

impl Counted {
    /// What happened since `before`, for the case's line (reported, not judged).
    fn since(self, before: Self) -> String {
        alloc::format!(
            "pulls {} ({} cache-hot), looks skipped as busy {}, kicks {} at ticks, {} at reschedule upcalls, {} at wake-ups or spawns, not sent: {} over the rate, {} with nobody idle; asks {}",
            self.pulls.wrapping_sub(before.pulls),
            self.pulls_hot.wrapping_sub(before.pulls_hot),
            self.contended.wrapping_sub(before.contended),
            self.kicks_tick.wrapping_sub(before.kicks_tick),
            self.kicks_upcall.wrapping_sub(before.kicks_upcall),
            self.kicks_here.wrapping_sub(before.kicks_here),
            self.over_rate.wrapping_sub(before.over_rate),
            self.nobody_idle.wrapping_sub(before.nobody_idle),
            self.asks.wrapping_sub(before.asks),
        )
    }
}

fn taking_part(online: &Online) -> Vec<CpuIndex> {
    online
        .cpus()
        .iter()
        .copied()
        .filter(|&cpu| online.takes_part_in_moves(cpu))
        .collect()
}

/// How many of `shared`'s named threads each processor of `cpus` holds, by their locations.
fn held(shared: &Shared, cpus: &[CpuIndex]) -> Vec<usize> {
    cpus.iter()
        .map(|&cpu| {
            shared
                .threads
                .iter()
                .filter(|thread| {
                    thread
                        .name()
                        .is_some_and(|name| sched::queued_on(name, cpu))
                })
                .count()
        })
        .collect()
}

/// Ends a case's threads and waits for them: whether every one that started ended.
fn end(shared: &Shared, tsc_hz: u64) -> bool {
    shared.unpin.store(true, Ordering::Release);
    shared.stop.store(true, Ordering::Release);
    for thread in &shared.threads {
        thread.woken.store(true, Ordering::Release);
        thread.queue.wake_all();
    }
    let done = || shared.ended.load(Ordering::Acquire) == shared.started.load(Ordering::Acquire);
    sched::yield_until(done, tsc_hz, PATIENCE);
    let ended = done();
    set_shared(None);
    ended
}

fn millis(ticks: u64, tsc_hz: u64) -> u64 {
    u64::try_from(u128::from(ticks) * 1000 / u128::from(tsc_hz.max(1))).unwrap_or(u64::MAX)
}

fn after_ms(ticks: Option<u64>, tsc_hz: u64) -> String {
    ticks.map_or_else(
        || alloc::format!("no, in {PATIENCE} s"),
        |t| alloc::format!("yes, after {} ms", millis(t, tsc_hz)),
    )
}

/// The spread case: whether every processor that takes part held one of its threads.
fn spread(com1: &mut Console, cpus: &[CpuIndex], tsc_hz: u64) -> bool {
    let Some(&first) = cpus.first() else {
        return false;
    };
    // Two for each processor, but no more than can wait on the first processor's queue once every
    // other has taken one.
    let wanted = (2 * cpus.len()).min(cpus.len() - 1 + crate::runqueue::CAPACITY - 2);
    let shared = new_shared(wanted, tsc_hz);
    let before = counted();
    let start = rdtsc();
    let mut spawned = 0;
    let mut every = None;
    let budget = tsc_hz.saturating_mul(PATIENCE);
    loop {
        while spawned < wanted && sched::has_room(first) {
            if sched::spawn_balanced(first, hog, u64::try_from(spawned).unwrap_or(u64::MAX))
                .is_err()
            {
                break;
            }
            spawned += 1;
        }
        let lie = held(&shared, cpus);
        let counted_all = lie.iter().sum::<usize>() == spawned;
        let elapsed = rdtsc().wrapping_sub(start);
        if spawned == wanted && counted_all && lie.iter().all(|&n| n >= 1) {
            every = Some(elapsed);
            break;
        }
        if elapsed > budget {
            break;
        }
        sched::yield_now();
        cpu::interrupt_window();
    }
    let lie = held(&shared, cpus);
    // Shares over a window on the boot processor's clock.
    let at_start: Vec<u64> = shared
        .threads
        .iter()
        .map(|t| t.ran.load(Ordering::Relaxed))
        .collect();
    let window_start = rdtsc();
    sched::yield_until(|| false, tsc_hz, SHARE_SECONDS);
    let window = rdtsc().wrapping_sub(window_start).max(1);
    let shares: Vec<u64> = shared
        .threads
        .iter()
        .zip(&at_start)
        .map(|(t, &was)| {
            t.ran
                .load(Ordering::Relaxed)
                .wrapping_sub(was)
                .saturating_mul(1000)
                / window
        })
        .collect();
    let lie_after = held(&shared, cpus);
    let moved: u64 = shared
        .threads
        .iter()
        .map(|t| t.moved.load(Ordering::Relaxed))
        .sum();
    let (lowest, highest) = (
        shares.iter().copied().min().unwrap_or(0),
        shares.iter().copied().max().unwrap_or(0),
    );
    let after = counted();
    let ended = end(&shared, tsc_hz);
    drop(shared);
    let ok = spawned == wanted && every.is_some() && ended;
    let _ = writeln!(
        com1,
        "origin-kernel: scheduler: load balancing: spread: {spawned} of {wanted} threads that never yield started on CPU {first}, {} processors taking part; every processor held one, with every thread counted: {}; held by processor then {lie:?}, after {SHARE_SECONDS} s {lie_after:?}; shares of a processor over {SHARE_SECONDS} s {lowest} to {highest} per mille; the threads saw themselves on another processor {moved} times; {}; ended {ended}{}",
        cpus.len(),
        after_ms(every, tsc_hz),
        after.since(before),
        if ok { "" } else { ": scheduler failed" }
    );
    ok
}

/// The two-alone case: whether another processor took one of the two.
fn two_alone(com1: &mut Console, cpus: &[CpuIndex], tsc_hz: u64) -> bool {
    let (Some(&on), true) = (cpus.get(1), cpus.len() >= 3) else {
        let _ = writeln!(
            com1,
            "origin-kernel: scheduler: load balancing: two alone: needs three processors taking part ({}): not run",
            cpus.len()
        );
        return true;
    };
    let shared = new_shared(2, tsc_hz);
    shared.unpin.store(false, Ordering::Release);
    let before = counted();
    let spawned = (0..2).all(|i| sched::spawn_balanced(on, hog, i).is_ok());
    // Both have run on that processor, pinned by count: they alternate at its ticks from here.
    sched::yield_until(
        || shared.ran_once.load(Ordering::Acquire) == 2,
        tsc_hz,
        PATIENCE,
    );
    let both_ran = shared.ran_once.load(Ordering::Acquire) == 2;
    shared.unpin.store(true, Ordering::Release);
    let start = rdtsc();
    let mut apart = None;
    let budget = tsc_hz.saturating_mul(PATIENCE);
    while both_ran && apart.is_none() && rdtsc().wrapping_sub(start) < budget {
        if held(&shared, cpus).iter().filter(|&&n| n >= 1).count() >= 2 {
            apart = Some(rdtsc().wrapping_sub(start));
        }
        sched::yield_now();
        cpu::interrupt_window();
    }
    let lie = held(&shared, cpus);
    let after = counted();
    let ended = end(&shared, tsc_hz);
    drop(shared);
    let ok = spawned && both_ran && apart.is_some() && ended;
    let _ = writeln!(
        com1,
        "origin-kernel: scheduler: load balancing: two alone: two threads that never yield on CPU {on}, spawned {spawned}, both ran there {both_ran}; another processor took one: {} (across a last-level cache it would not, R98-1); held by processor {lie:?}; {}; ended {ended}{}",
        after_ms(apart, tsc_hz),
        after.since(before),
        if ok { "" } else { ": scheduler failed" }
    );
    ok
}

/// The kick case: whether every wake-up made on the waker's processor sent its kick, and one of
/// their threads ran elsewhere.
fn kick(com1: &mut Console, cpus: &[CpuIndex], tsc_hz: u64) -> bool {
    let Some(&on) = cpus.first() else {
        return false;
    };
    let shared = new_shared(WAKEES, tsc_hz);
    let wakees = (0..WAKEES)
        .all(|i| sched::spawn_balanced(on, wakee, u64::try_from(i).unwrap_or(u64::MAX)).is_ok());
    let total = u64::try_from(WAKEES).unwrap_or(u64::MAX);
    sched::yield_until(
        || shared.parked.load(Ordering::Acquire) == total,
        tsc_hz,
        PATIENCE,
    );
    let parked = shared.parked.load(Ordering::Acquire);
    // Counted from here: every wakee has blocked, so no spawn's kick is in them.
    let before = counted();
    let waking = wakees && parked == total && sched::spawn(on, waker, 0).is_ok();
    let ran = || {
        shared
            .threads
            .iter()
            .filter(|t| t.ran_on.load(Ordering::Acquire) != 0)
            .count()
    };
    if waking {
        sched::yield_until(|| ran() == WAKEES, tsc_hz, PATIENCE);
    }
    let after = counted();
    let on_plus = u64::try_from(on.get()).unwrap_or(u64::MAX - 1) + 1;
    let (mut local, mut elsewhere) = (0u64, 0u64);
    let mut latencies: Vec<u64> = Vec::new();
    for thread in &shared.threads {
        let (at, woken, where_, parked_on) = (
            thread.ran_at.load(Ordering::Acquire),
            thread.woken_at.load(Ordering::Acquire),
            thread.ran_on.load(Ordering::Acquire),
            thread.parked_on.load(Ordering::Acquire),
        );
        if parked_on == on_plus {
            local += 1;
            if where_ != 0 && where_ != on_plus {
                elsewhere += 1;
            }
        }
        if where_ != 0 && woken != 0 {
            latencies.push(at.saturating_sub(woken));
        }
    }
    latencies.sort_unstable();
    let (median, largest) = (
        latencies.get(latencies.len() / 2).copied().unwrap_or(0),
        latencies.last().copied().unwrap_or(0),
    );
    let all_ran = ran();
    let kicked = after.kicks_here.wrapping_sub(before.kicks_here);
    let ended = end(&shared, tsc_hz);
    drop(shared);
    let ok =
        waking && all_ran == WAKEES && local >= 1 && kicked == local && elsewhere >= 1 && ended;
    let _ = writeln!(
        com1,
        "origin-kernel: scheduler: load balancing: kick: {WAKEES} threads declared for balancing blocked beside a thread on CPU {on} that never yields (blocked {parked}, {local} of them on CPU {on}, waker started {waking}), woken one at a time: {all_ran} ran; of the {local} wake-ups made on CPU {on}, kicks sent {kicked}, threads that ran on another processor {elsewhere}; from the wake-up to running: median {median} ns, largest {largest} ns; {}; ended {ended}{}",
        after.since(before),
        if ok { "" } else { ": scheduler failed" }
    );
    ok
}

/// The proof, after the migration proofs: `true` if every case's conditions held.
pub(crate) fn prove(com1: &mut Console, online: &Online, tsc_hz: u64) -> bool {
    let cpus = taking_part(online);
    let withdrawn = sched::pinned::withdrawn();
    if cpus.len() < 2 || withdrawn || !pull::is_set_up() {
        let pass = cpus.len() < 2;
        let _ = writeln!(
            com1,
            "origin-kernel: scheduler: load balancing: {} processors take part in moves, moves withdrawn {withdrawn}, balancing set up {}: not run{}",
            cpus.len(),
            pull::is_set_up(),
            if pass { "" } else { ": scheduler failed" }
        );
        return pass;
    }
    let settled = sched::settle(tsc_hz);
    let counts = || {
        (
            crate::memory::stats().map(|(h, f)| (h.bytes_in_use, f)),
            origin_frame::stack::counts().live,
        )
    };
    let before = counts();
    let spread_ok = spread(com1, &cpus, tsc_hz);
    let _ = sched::settle(tsc_hz);
    let alone_ok = two_alone(com1, &cpus, tsc_hz);
    let _ = sched::settle(tsc_hz);
    let kick_ok = kick(com1, &cpus, tsc_hz);
    let settled_after = sched::settle(tsc_hz);
    sched::yield_until(|| counts() == before, tsc_hz, 2);
    let back = counts() == before;
    let ok = spread_ok && alone_ok && kick_ok && back && settled.is_ok() && settled_after.is_ok();
    let _ = writeln!(
        com1,
        "origin-kernel: scheduler: load balancing: spread {spread_ok}, two alone {alone_ok}, kick {kick_ok}; heap, frames and stacks back {back}; settled {} and {}{}",
        settled.is_ok(),
        settled_after.is_ok(),
        if ok { "" } else { ": scheduler failed" }
    );
    ok
}
