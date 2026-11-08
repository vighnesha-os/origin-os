//! Kernel wait queues (CLAUDE.md §25 thread lifecycle, §26 thread wake-ups, §30 wait
//! objects): a kernel thread blocks until a condition holds, and whoever makes it hold
//! wakes it, on any processor.
//!
//! [`WaitQueue::wait_until`] checks the condition under the queue's lock and registers the
//! thread there before it blocks. A waker changes the state first, then takes the same
//! lock to pick a waiter. So either the waiter sees the change, or it is registered by the
//! time the waker looks; and a wake-up that reaches the thread just before it blocks is
//! kept by its run queue (`runqueue`), so the block returns at once. No wake-up is lost.
//!
//! A thread blocked on another processor is woken with a reschedule IPI (`sched::wake`),
//! so it runs within microseconds instead of at that processor's next timer tick (10 ms).
//!
//! Lock order: a wait queue's lock, then whatever its condition reads. A waker never holds
//! a wait queue's lock while it takes another one.
//!
//! These queues are kernel-internal. The user-visible waiting of ADR-0005 P9 builds on them
//! later.

use core::fmt::Write;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use framekernel::runqueue::{RoundTrips, judge_round_trips, per_mille};
use origin_sync::SpinLock;

use crate::console::Console;
use crate::cpu::{self, CpuIndex, rdtsc};
use crate::sched::{self, ThreadRef};

/// Threads one queue can hold. A thread that finds it full polls by yielding instead.
const WAITERS: usize = 16;

/// Threads waiting for one condition.
pub(crate) struct WaitQueue {
    waiters: SpinLock<[Option<ThreadRef>; WAITERS]>,
}

impl WaitQueue {
    pub(crate) const fn new() -> Self {
        Self {
            waiters: SpinLock::new([None; WAITERS]),
        }
    }

    /// Returns once `condition()` holds, blocking the running kernel thread meanwhile.
    /// `condition` runs under this queue's lock, with interrupts disabled. Returns how often
    /// the thread blocked. The boot or idle flow, which cannot block, polls by yielding.
    pub(crate) fn wait_until(&self, condition: impl Fn() -> bool) -> u64 {
        let me = sched::current_thread();
        let mut blocked = 0;
        loop {
            let registered = cpu::without_interrupts(|| {
                let mut waiters = self.waiters.lock();
                if condition() {
                    // Leave no registration behind (a wake-up that raced with the
                    // condition becoming true is harmless: the next block consumes it).
                    if let Some(slot) = waiters.iter_mut().find(|w| **w == me && me.is_some()) {
                        *slot = None;
                    }
                    return None;
                }
                let Some(thread) = me else {
                    return Some(false);
                };
                if waiters.contains(&Some(thread)) {
                    return Some(true);
                }
                match waiters.iter_mut().find(|w| w.is_none()) {
                    Some(free) => {
                        *free = Some(thread);
                        Some(true)
                    }
                    None => Some(false),
                }
            });
            match registered {
                None => return blocked,
                Some(true) => {
                    sched::block();
                    blocked += 1;
                }
                Some(false) => sched::yield_now(),
            }
        }
    }

    /// As [`WaitQueue::wait_until`], but gives up when the monotonic clock reaches
    /// `deadline` (nanoseconds) with the condition still false: `Err(TimedOut)`. The
    /// deadline is kept to within one timer tick (10 ms).
    pub(crate) fn wait_until_deadline(
        &self,
        condition: impl Fn() -> bool,
        deadline: u64,
    ) -> Result<u64, TimedOut> {
        let me = sched::current_thread();
        let mut blocked = 0;
        loop {
            let step = cpu::without_interrupts(|| {
                let mut waiters = self.waiters.lock();
                let done = condition();
                if done || crate::time::monotonic_nanos() >= deadline {
                    if let Some(slot) = waiters.iter_mut().find(|w| **w == me && me.is_some()) {
                        *slot = None;
                    }
                    return Step::Finished(done);
                }
                let Some(thread) = me else {
                    return Step::Poll;
                };
                if waiters.contains(&Some(thread)) {
                    return Step::Block;
                }
                match waiters.iter_mut().find(|w| w.is_none()) {
                    Some(free) => {
                        *free = Some(thread);
                        Step::Block
                    }
                    None => Step::Poll,
                }
            });
            match step {
                Step::Finished(true) => return Ok(blocked),
                Step::Finished(false) => return Err(TimedOut),
                Step::Block => {
                    sched::block_until(deadline);
                    blocked += 1;
                }
                Step::Poll => sched::yield_now(),
            }
        }
    }

    /// Wakes one waiter, if there is one; returns whether there was.
    pub(crate) fn wake_one(&self) -> bool {
        let thread =
            cpu::without_interrupts(|| self.waiters.lock().iter_mut().find_map(Option::take));
        if let Some(t) = thread {
            sched::wake(t);
        }
        thread.is_some()
    }
}

/// A timed wait whose deadline passed with its condition still false.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TimedOut;

/// What one round of a timed wait decided.
enum Step {
    /// The condition held (`true`), or the deadline passed (`false`).
    Finished(bool),
    Block,
    Poll,
}

/// The ring the producer fills and the consumer drains.
struct Ring {
    items: [u64; RING],
    head: usize,
    len: usize,
}

const RING: usize = 8;
/// Items the producer sends.
const ITEMS: u64 = 10_000;
/// Round trips of the ping-pong.
const ROUNDS: u64 = 1000;

static RING_STATE: SpinLock<Ring> = SpinLock::new(Ring {
    items: [0; RING],
    head: 0,
    len: 0,
});
static NOT_EMPTY: WaitQueue = WaitQueue::new();
static NOT_FULL: WaitQueue = WaitQueue::new();
static PRODUCER_BLOCKS: AtomicU64 = AtomicU64::new(0);
static CONSUMER_BLOCKS: AtomicU64 = AtomicU64::new(0);
static RECEIVED: AtomicU64 = AtomicU64::new(0);
static IN_ORDER: AtomicBool = AtomicBool::new(false);
static FINISHED: AtomicU64 = AtomicU64::new(0);

fn ring_len() -> usize {
    cpu::without_interrupts(|| RING_STATE.lock().len)
}

extern "sysv64" fn producer(_: u64) -> ! {
    for item in 0..ITEMS {
        let blocks = NOT_FULL.wait_until(|| ring_len() < RING);
        PRODUCER_BLOCKS.fetch_add(blocks, Ordering::Relaxed);
        cpu::without_interrupts(|| {
            let mut r = RING_STATE.lock();
            let at = (r.head + r.len) % RING;
            if let Some(slot) = r.items.get_mut(at) {
                *slot = item;
            }
            r.len += 1;
        });
        NOT_EMPTY.wake_one();
    }
    FINISHED.fetch_add(1, Ordering::AcqRel);
    sched::exit()
}

extern "sysv64" fn consumer(_: u64) -> ! {
    let mut in_order = true;
    for expected in 0..ITEMS {
        let blocks = NOT_EMPTY.wait_until(|| ring_len() > 0);
        CONSUMER_BLOCKS.fetch_add(blocks, Ordering::Relaxed);
        let item = cpu::without_interrupts(|| {
            let mut r = RING_STATE.lock();
            let item = r.items.get(r.head).copied().unwrap_or(u64::MAX);
            r.head = (r.head + 1) % RING;
            r.len -= 1;
            item
        });
        NOT_FULL.wake_one();
        in_order &= item == expected;
        RECEIVED.fetch_add(1, Ordering::Relaxed);
    }
    IN_ORDER.store(in_order, Ordering::Relaxed);
    FINISHED.fetch_add(1, Ordering::AcqRel);
    sched::exit()
}

/// 0: the pinger's turn; 1: the ponger's; 2: done.
static TURN: AtomicU64 = AtomicU64::new(0);
static PING: WaitQueue = WaitQueue::new();
static PONG: WaitQueue = WaitQueue::new();
static PING_CYCLES: AtomicU64 = AtomicU64::new(0);
/// Runs of the ping-pong [`prove_ping_pong`] may make: an extra one only after a run over
/// the bound that shows a disturbance (`judge_round_trips`). A generous budget costs
/// nothing where it is not used: an undisturbed boot pays for one run, and every run has
/// the same bound to meet.
const PING_ATTEMPTS: u32 = 8;

extern "sysv64" fn pinger(_: u64) -> ! {
    let start = rdtsc();
    for _ in 0..ROUNDS {
        TURN.store(1, Ordering::Release);
        PONG.wake_one();
        PING.wait_until(|| TURN.load(Ordering::Acquire) == 0);
    }
    PING_CYCLES.store(rdtsc().wrapping_sub(start), Ordering::Relaxed);
    TURN.store(2, Ordering::Release);
    PONG.wake_one();
    FINISHED.fetch_add(1, Ordering::AcqRel);
    sched::exit()
}

extern "sysv64" fn ponger(_: u64) -> ! {
    loop {
        PONG.wait_until(|| TURN.load(Ordering::Acquire) != 0);
        if TURN.load(Ordering::Acquire) == 2 {
            break;
        }
        TURN.store(0, Ordering::Release);
        PING.wake_one();
    }
    FINISHED.fetch_add(1, Ordering::AcqRel);
    sched::exit()
}

/// Nobody signals this queue: a timed wait on it can only time out.
static NEVER: WaitQueue = WaitQueue::new();
static NEVER_SET: AtomicBool = AtomicBool::new(false);
/// Signalled by the boot flow, well before the second waiter's deadline.
static SOON: WaitQueue = WaitQueue::new();
static SOON_SET: AtomicBool = AtomicBool::new(false);
/// Each timed waiter's result: milliseconds waited, plus 1 if it timed out, times 2
/// (0: not finished).
static TIMED: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];
const SHORT_MS: u64 = 50;
const LONG_MS: u64 = 2000;

extern "sysv64" fn timed_waiter(which: u64) -> ! {
    let start = crate::time::monotonic_nanos();
    let (queue, flag, ms) = if which == 0 {
        (&NEVER, &NEVER_SET, SHORT_MS)
    } else {
        (&SOON, &SOON_SET, LONG_MS)
    };
    let result = queue.wait_until_deadline(|| flag.load(Ordering::Acquire), start + ms * 1_000_000);
    let waited = (crate::time::monotonic_nanos() - start) / 1_000_000;
    if let Some(slot) = TIMED.get(which as usize) {
        slot.store(
            2 * waited + u64::from(result.is_err()) + 2,
            Ordering::Release,
        );
    }
    FINISHED.fetch_add(1, Ordering::AcqRel);
    sched::exit()
}

/// A timed wait that nobody ends must time out at its deadline (within two ticks), and one
/// that is signalled first must return then, long before its deadline.
fn prove_timed(com1: &mut Console, far: CpuIndex, tsc_hz: u64) -> bool {
    let before = FINISHED.load(Ordering::Acquire);
    let spawned = [
        sched::spawn(far, timed_waiter, 0),
        sched::spawn(far, timed_waiter, 1),
    ];
    if spawned.iter().any(Result::is_err) {
        let _ = writeln!(
            com1,
            "origin-kernel: wait queues: cannot spawn: wait queues failed"
        );
        return false;
    }
    // Signal the second waiter after about 20 ms.
    sched::yield_until(|| false, tsc_hz / 50, 1);
    SOON_SET.store(true, Ordering::Release);
    SOON.wake_one();
    sched::yield_until(|| FINISHED.load(Ordering::Acquire) == before + 2, tsc_hz, 4);
    let read = |i: usize| TIMED.get(i).map_or(0, |t| t.load(Ordering::Acquire));
    let (short, long) = (read(0), read(1));
    let finished = short != 0 && long != 0;
    let (short_ms, short_timed_out) = (short.saturating_sub(2) / 2, short & 1 == 1);
    let (long_ms, long_timed_out) = (long.saturating_sub(2) / 2, long & 1 == 1);
    let ok = finished
        && short_timed_out
        && (SHORT_MS..=SHORT_MS + 30).contains(&short_ms)
        && !long_timed_out
        && long_ms < LONG_MS / 4;
    if finished {
        let _ = writeln!(
            com1,
            "origin-kernel: wait queues: an unsignalled {SHORT_MS} ms timed wait timed out {short_timed_out} after {short_ms} ms; a {LONG_MS} ms timed wait signalled early returned in {long_ms} ms, timed out {long_timed_out}{}",
            if ok { "" } else { ": wait queues failed" }
        );
    } else {
        let _ = writeln!(
            com1,
            "origin-kernel: wait queues: timed waits did not return within 4 s (the {SHORT_MS} ms one {}, the {LONG_MS} ms one {}): wait queues failed",
            if short == 0 {
                "still waiting"
            } else {
                "returned"
            },
            if long == 0 {
                "still waiting"
            } else {
                "returned"
            }
        );
    }
    ok
}

/// One run of the ping-pong, as the boot flow measured it.
struct PingPongRun {
    /// Whether both threads finished within 5 s.
    done: bool,
    /// The mean round trip, in microseconds.
    mean: u64,
    /// How much of the run the boot flow went without a turn, in parts per thousand.
    away: u64,
}

/// Starts the two threads of the ping-pong on `bsp` and `far`, waits at most 5 s for them
/// and returns what the run measured, or `None` if a thread could not be spawned.
fn run_ping_pong(bsp: CpuIndex, far: CpuIndex, tsc_hz: u64) -> Option<PingPongRun> {
    TURN.store(0, Ordering::Release);
    let before = FINISHED.load(Ordering::Acquire);
    sched::spawn(bsp, pinger, 0).ok()?;
    sched::spawn(far, ponger, 0).ok()?;
    let finished = || FINISHED.load(Ordering::Acquire) == before + 2;
    let start = rdtsc();
    let away = sched::yield_until_away(finished, tsc_hz, 5);
    let elapsed = rdtsc().wrapping_sub(start);
    Some(PingPongRun {
        done: finished(),
        mean: PING_CYCLES.load(Ordering::Relaxed) / ROUNDS / (tsc_hz / 1_000_000).max(1),
        away: per_mille(away, elapsed),
    })
}

/// Proves a thread blocked on another processor is woken at once by its IPI, not at that
/// processor's next timer tick: two threads pass a turn back and forth [`ROUNDS`] times,
/// each waking the other, and a round trip must average under
/// [`framekernel::runqueue::ROUND_TRIP_LIMIT_MICROS`] (woken only by the other's timer
/// tick, it would take milliseconds).
///
/// On a loaded host a virtual CPU is descheduled for milliseconds, and a stall that falls
/// into a round trip lengthens it by as much, so the mean of a run can be the host's tail
/// and not the kernel. Such a run says nothing either way, and is measured again, at most
/// [`PING_ATTEMPTS`] runs in all, each against the same bound. What earns the repeat is the
/// boot flow going without a turn for at least a tenth of the run. A thread on its
/// processor can produce that reading just by running, so it can earn a repeat and never a
/// pass: every run that passes has its mean under the bound. A run over the bound without
/// that reading fails at once, and so does any run on one processor, where the boot flow
/// takes turns with the two threads and the reading says nothing about the host.
fn prove_ping_pong(com1: &mut Console, bsp: CpuIndex, far: CpuIndex, tsc_hz: u64) -> bool {
    let mut attempts = 0;
    let (run, ok) = loop {
        attempts += 1;
        let Some(run) = run_ping_pong(bsp, far, tsc_hz) else {
            let _ = writeln!(
                com1,
                "origin-kernel: wait queues: cannot spawn: wait queues failed"
            );
            return false;
        };
        let away = if far == bsp { 0 } else { run.away };
        let verdict = if run.done {
            judge_round_trips(run.mean, away)
        } else {
            RoundTrips::Fail
        };
        if verdict != RoundTrips::Repeat || attempts == PING_ATTEMPTS {
            break (run, verdict == RoundTrips::Pass);
        }
    };
    let (mean, away) = (run.mean, run.away);
    let failed = if ok { "" } else { ": wait queues failed" };
    if run.done && far == bsp {
        let _ = writeln!(
            com1,
            "origin-kernel: wait queues: ping-pong between CPU {bsp} and CPU {far}: {ROUNDS} round trips, {mean} us each (one processor){failed}"
        );
    } else if run.done {
        let retried = if attempts > 1 {
            " (after a stalled run)"
        } else {
            ""
        };
        let _ = writeln!(
            com1,
            "origin-kernel: wait queues: ping-pong between CPU {bsp} and CPU {far}: {ROUNDS} round trips, {mean} us each, woken by IPI (boot flow away for {away} per mille of the run, run {attempts} of {PING_ATTEMPTS}){retried}{failed}"
        );
    } else {
        let _ = writeln!(
            com1,
            "origin-kernel: wait queues: ping-pong between CPU {bsp} and CPU {far}: {ROUNDS} round trips did not finish within 5 s: wait queues failed"
        );
    }
    ok
}

/// Proves the wait queues:
/// - a producer on one processor sends [`ITEMS`] numbers through an 8-slot ring to a
///   consumer on another, each blocking when the ring is full or empty: every item must
///   arrive, in order, within 5 s;
/// - two threads on different processors pass a turn back and forth [`ROUNDS`] times,
///   each waking the other ([`prove_ping_pong`]);
/// - timed waits time out when nobody signals them and return early when somebody does.
///
/// Runs from the boot flow with every scheduler active.
pub(crate) fn prove(com1: &mut Console, cpus: &[CpuIndex], bsp: CpuIndex, tsc_hz: u64) -> bool {
    let far = cpus.iter().copied().find(|&c| c != bsp).unwrap_or(bsp);
    let spawned = [
        sched::spawn(far, producer, 0),
        sched::spawn(bsp, consumer, 0),
    ];
    if spawned.iter().any(Result::is_err) {
        let _ = writeln!(
            com1,
            "origin-kernel: wait queues: cannot spawn: wait queues failed"
        );
        return false;
    }
    let start = rdtsc();
    sched::yield_until(|| FINISHED.load(Ordering::Acquire) == 2, tsc_hz, 5);
    let micros = rdtsc().wrapping_sub(start) / (tsc_hz / 1_000_000).max(1);
    let received = RECEIVED.load(Ordering::Relaxed);
    let transfer_ok = FINISHED.load(Ordering::Acquire) == 2
        && received == ITEMS
        && IN_ORDER.load(Ordering::Relaxed);
    let _ = writeln!(
        com1,
        "origin-kernel: wait queues: {received} of {ITEMS} items from CPU {far} to CPU {bsp} through a {RING}-slot ring, in order {}, in {} ms; the producer blocked {} times and the consumer {}{}",
        IN_ORDER.load(Ordering::Relaxed),
        micros / 1000,
        PRODUCER_BLOCKS.load(Ordering::Relaxed),
        CONSUMER_BLOCKS.load(Ordering::Relaxed),
        if transfer_ok {
            ""
        } else {
            ": wait queues failed"
        }
    );
    let ping_ok = prove_ping_pong(com1, bsp, far, tsc_hz);
    let timed_ok = prove_timed(com1, far, tsc_hz);
    transfer_ok && ping_ok && timed_ok
}
