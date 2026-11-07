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

use origin_sync::SpinLock;

use crate::console::Console;
use crate::cpu::{self, rdtsc};
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

/// Proves the wait queues:
/// - a producer on one processor sends [`ITEMS`] numbers through an 8-slot ring to a
///   consumer on another, each blocking when the ring is full or empty: every item must
///   arrive, in order, within 5 s;
/// - two threads on different processors pass a turn back and forth [`ROUNDS`] times,
///   each waking the other: a round trip must average under 2 ms. Woken only by the
///   other's timer tick (10 ms), a round trip would take milliseconds.
///
/// Runs from the boot flow with every scheduler active.
pub(crate) fn prove(com1: &mut Console, cpus: &[u32], bsp: usize, tsc_hz: u64) -> bool {
    let far = cpus
        .iter()
        .map(|&id| id as usize)
        .find(|&id| id != bsp)
        .unwrap_or(bsp);
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
    let spawned = [sched::spawn(bsp, pinger, 0), sched::spawn(far, ponger, 0)];
    if spawned.iter().any(Result::is_err) {
        let _ = writeln!(
            com1,
            "origin-kernel: wait queues: cannot spawn: wait queues failed"
        );
        return false;
    }
    sched::yield_until(|| FINISHED.load(Ordering::Acquire) == 4, tsc_hz, 5);
    let done = FINISHED.load(Ordering::Acquire) == 4;
    let each = PING_CYCLES.load(Ordering::Relaxed) / ROUNDS / (tsc_hz / 1_000_000).max(1);
    let ping_ok = done && each < 2000;
    let where_ = if far == bsp {
        " (one processor)"
    } else {
        ", woken by IPI"
    };
    if done {
        let _ = writeln!(
            com1,
            "origin-kernel: wait queues: ping-pong between CPU {bsp} and CPU {far}: {ROUNDS} round trips, {each} us each{where_}{}",
            if ping_ok { "" } else { ": wait queues failed" }
        );
    } else {
        let _ = writeln!(
            com1,
            "origin-kernel: wait queues: ping-pong between CPU {bsp} and CPU {far}: {ROUNDS} round trips did not finish within 5 s: wait queues failed"
        );
    }
    transfer_ok && ping_ok
}
