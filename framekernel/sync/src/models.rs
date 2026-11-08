//! Loom models of [`SpinLock`] and of the kernel's wait-queue protocol (CLAUDE.md §73).
//!
//! Built only by `cargo xtask loom`, with `--cfg loom`, for this crate's tests. A model runs the
//! real code, not a copy of it: `SpinLock` with its orderings, `crate::wait` (the protocol
//! `framekernel/src/wait.rs` instantiates) and the real run queue
//! (`framekernel/src/runqueue.rs`, compiled in here), with Loom's atomics and cell in place of
//! `core`'s (`prim` in `lib.rs`). `cargo xtask loom` also applies a negative control for each
//! property to a scratch copy of this source and requires the named model to report the bug
//! (`developer-tools/xtask/src/loom.rs`).
//!
//! # What is proved
//!
//! For the code above, **within the bounds each model prints**, over every thread interleaving
//! (up to Loom's removal of equivalent ones) and every store a `Relaxed` load may see in Loom's
//! model of the C11 memory model:
//!
//! - `SpinLock`: at most one holder at a time; every access to the protected value is ordered
//!   by happens-before with every other (Loom's cell checks this; no data race); each holder
//!   sees all earlier holders' writes, whole. `try_lock`: a failure leaves the holder's lock held
//!   and a success is a real holder.
//! - The wait queue, no lost wake-ups: a thread in `wait_until` or `wait_until_deadline` whose
//!   condition is made true by a thread that then calls `wake_one` does not stay blocked
//!   (a lost wake-up leaves every thread blocked, which Loom reports as a deadlock). This holds
//!   with `Relaxed` on the condition, the weakest ordering a caller could use: the queue's lock
//!   carries it. A wait returns only with its condition true, also after a wake-up that outlived
//!   an earlier wait; a timed wait that gives up leaves no registration, so it cannot swallow
//!   the next waiter's wake-up; and a thread that cannot register (a full queue, or the boot
//!   flow) still sees the condition by polling.
//!
//! # What is not proved
//!
//! - Anything past the bounds: more threads or rounds than the models have (at most three spawned
//!   threads and the model's own thread here; Loom's limit is five in all), more thread preemptions
//!   than the bound where one is printed, or more spin iterations than `Bound::spins` (below).
//! - Anything Loom's memory model omits: it does not model load buffering (a load seeing a store
//!   that its thread only makes later), and its `compare_exchange_weak` never fails spuriously,
//!   so `lock` is not explored taking a spurious failure (it would only loop again).
//! - Interrupts: `without_interrupts` does nothing here, so the rule that no handler may take a
//!   lock interrupted code can hold is not checked. A handler that wakes is modelled as another
//!   thread.
//! - The scheduler around the run queue: the context switch, `sched::on_block`, the reschedule
//!   IPI and the timer are stood in for by [`Cpu`]'s `kick` and the run-queue calls they make.
//!   That a ready thread eventually runs is assumed. Fairness and starvation are not modelled.
//! - The machine: these are host builds against Loom's atomics. The x86-64 code the kernel runs,
//!   the `pause` instruction and TSO are not examined.
//! - `RwLock`, which has no model yet; a timed wait on the boot flow (the poll path with a
//!   deadline); and what callers must uphold themselves: the state changes before `wake_one`, and
//!   one condition per queue (`waker_wakes_before_setting_the_flag` shows the first matters).
//!
//! # What the models add
//!
//! Nothing that orders memory where the real code does not: the models' own counters, tokens
//! and clock are `Relaxed`. `Notify`, the stand-in for what restarts a halted processor,
//! does synchronize, but the resumed thread goes on to take the run queue's lock, which carries
//! the ordering the real resumption gets from that lock anyway.
//!
//! # Bounds
//!
//! Two threads spinning on one lock can take turns for ever, and Loom counts each turn as a new
//! execution whatever the preemption bound. So a thread's spin iterations are explored only up
//! to `Bound::spins`; past that the execution still runs to its end, with every check, but
//! branches no further. A spin iteration changes nothing and can only observe the lock change
//! hands, so the bound limits how many of those changes a thread can wait through.
//! `ORIGIN_LOOM_DEEP` selects each model's larger bound (a long run).

use std::sync::atomic::AtomicU64 as ExecutionCounter;

use core::sync::atomic::Ordering::{self, Acquire, Relaxed, Release};

use loom::sync::atomic::{AtomicBool, AtomicU32, AtomicU64};
use loom::sync::{Arc, Notify};
use loom::thread::{self, JoinHandle};

use crate::SpinLock;
use crate::runqueue::RunQueue;
use crate::wait::{Scheduler, WaitQueue};

/// How far one model's search goes.
#[derive(Clone, Copy)]
struct Bound {
    /// At most this many thread preemptions; `None` is the exhaustive search.
    preemptions: Option<usize>,
    /// Spin iterations per thread that are explored (see `prim::spin_loop`).
    spins: u32,
}

/// The spin iterations every model explores, unless it says otherwise.
const SPINS: u32 = 4;

const EXHAUSTIVE: Bound = Bound {
    preemptions: None,
    spins: SPINS,
};

const fn preempted(preemptions: usize, spins: u32) -> Bound {
    Bound {
        preemptions: Some(preemptions),
        spins,
    }
}

/// Explores every execution of `model` within `quick`, or within `deep` if `ORIGIN_LOOM_DEEP`
/// is set, and prints how many executions there were.
///
/// For experiments, `LOOM_MAX_PREEMPTIONS` and `ORIGIN_LOOM_SPINS` override the bound, and
/// `LOOM_LOG=trace` traces a failure through Loom's own runner (which uses the preemption bound
/// from the environment only).
fn check(name: &str, quick: Bound, deep: Bound, model: impl Fn() + Send + Sync + 'static) {
    let bound = if std::env::var_os("ORIGIN_LOOM_DEEP").is_some() {
        deep
    } else {
        quick
    };
    let spins = std::env::var("ORIGIN_LOOM_SPINS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(bound.spins);
    crate::prim::SPIN_BOUND.with(|bound| bound.set(spins));
    if std::env::var_os("LOOM_LOG").is_some() {
        return loom::model(model);
    }
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = builder.preemption_bound.or(bound.preemptions);
    // A model that stops early would prove less than it says.
    builder.max_permutations = None;
    builder.max_duration = None;
    let executions = std::sync::Arc::new(ExecutionCounter::new(0));
    let counted = std::sync::Arc::clone(&executions);
    let started = std::time::Instant::now();
    builder.check(move || {
        counted.fetch_add(1, Relaxed);
        model();
    });
    let search = builder
        .preemption_bound
        .map_or("exhaustive".to_owned(), |b| {
            format!("at most {b} preemptions")
        });
    println!(
        "loom model {name}: {} executions, {search}, {spins} spins per thread, {} ms",
        executions.load(Relaxed),
        started.elapsed().as_millis()
    );
}

// ---------------------------------------------------------------- SpinLock

/// `threads` threads each take the lock `rounds` times. Every holder does a non-atomic
/// read-modify-write through its guard and checks that no one else is inside.
fn holders_exclude_each_other(threads: u64, rounds: u64) {
    let lock = Arc::new(SpinLock::new(0u64));
    let inside = Arc::new(AtomicU64::new(0));
    let holders: Vec<_> = (0..threads)
        .map(|_| {
            let (lock, inside) = (Arc::clone(&lock), Arc::clone(&inside));
            thread::spawn(move || {
                for _ in 0..rounds {
                    let mut held = lock.lock();
                    assert_eq!(inside.fetch_add(1, Relaxed), 0, "two holders at once");
                    let value = *held;
                    *held = value + 1;
                    assert_eq!(inside.fetch_sub(1, Relaxed), 1, "two holders at once");
                }
            })
        })
        .collect();
    for holder in holders {
        holder.join().unwrap();
    }
    assert_eq!(*lock.lock(), threads * rounds, "a lost update");
}

#[test]
fn spin_lock_two_threads_two_rounds() {
    check(
        "spin_lock_two_threads_two_rounds",
        EXHAUSTIVE,
        EXHAUSTIVE,
        || {
            holders_exclude_each_other(2, 2);
        },
    );
}

#[test]
fn spin_lock_three_threads() {
    check(
        "spin_lock_three_threads",
        preempted(3, SPINS),
        preempted(4, SPINS),
        || {
            holders_exclude_each_other(3, 1);
        },
    );
}

/// Writes made under the lock reach the next holder whole, through `lock`.
#[test]
fn spin_lock_passes_writes_to_the_next_holder() {
    check(
        "spin_lock_passes_writes_to_the_next_holder",
        EXHAUSTIVE,
        EXHAUSTIVE,
        || {
            let lock = Arc::new(SpinLock::new((0u64, 0u64)));
            let writer = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    let mut held = lock.lock();
                    held.0 = 1;
                    held.1 = 3;
                })
            };
            let seen = *lock.lock();
            assert!(seen == (0, 0) || seen == (1, 3), "a torn update: {seen:?}");
            writer.join().unwrap();
            assert_eq!(*lock.lock(), (1, 3));
        },
    );
}

/// A failed `try_lock` leaves the holder's lock held; a successful one is a real holder.
#[test]
fn try_lock_leaves_a_held_lock_held() {
    check(
        "try_lock_leaves_a_held_lock_held",
        EXHAUSTIVE,
        EXHAUSTIVE,
        || {
            let lock = Arc::new(SpinLock::new(0u64));
            let holder = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    let mut held = lock.lock();
                    let value = *held;
                    *held = value + 1;
                })
            };
            let prober = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    // Taken at once or after a failed try: the increment must be exclusive either
                    // way, and a failed try must not have released the holder's lock.
                    let mut held = match lock.try_lock() {
                        Some(held) => held,
                        None => lock.lock(),
                    };
                    let value = *held;
                    *held = value + 1;
                })
            };
            holder.join().unwrap();
            prober.join().unwrap();
            assert_eq!(*lock.lock(), 2, "a lost update");
        },
    );
}

/// A successful `try_lock` sees the previous holder's writes whole.
#[test]
fn try_lock_passes_writes_to_the_next_holder() {
    check(
        "try_lock_passes_writes_to_the_next_holder",
        EXHAUSTIVE,
        EXHAUSTIVE,
        || {
            let lock = Arc::new(SpinLock::new((0u64, 0u64)));
            let writer = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    let mut held = lock.lock();
                    held.0 = 1;
                    held.1 = 3;
                })
            };
            if let Some(held) = lock.try_lock() {
                let seen = *held;
                assert!(seen == (0, 0) || seen == (1, 3), "a torn update: {seen:?}");
            }
            writer.join().unwrap();
        },
    );
}

// ---------------------------------------------------------------- the kernel, around the queue

/// The frame the model's kernel thread resumes at.
const THREAD_FRAME: u64 = 0x2000;
/// The frame of the processor's idle context.
const IDLE_FRAME: u64 = 0x1000;
/// Slots in a wait queue, as in the kernel (`WAITERS`).
const WAITERS: usize = 16;
/// The timed models' deadline, on the model's clock (which starts at 0).
const DEADLINE: u64 = 1;

/// A processor: its real run queue, holding one kernel thread and the idle context.
struct Cpu {
    queue: SpinLock<RunQueue>,
    /// Stands for the reschedule IPI or timer tick that makes the halted processor look at its
    /// queue again. Loom's `Notify` may also return spuriously, like a tick that readies
    /// nothing: the queue, not this, decides what runs.
    kick: Notify,
    slot: usize,
    generation: u32,
}

impl Cpu {
    /// A processor already running its kernel thread.
    fn new() -> Self {
        let mut queue = RunQueue::new();
        queue.set_idle(true);
        let slot = queue.push(THREAD_FRAME).unwrap();
        let generation = queue.generation(slot);
        assert_eq!(queue.tick(IDLE_FRAME), THREAD_FRAME);
        Self {
            queue: SpinLock::new(queue),
            kick: Notify::new(),
            slot,
            generation,
        }
    }
}

/// The processors, and the monotonic clock they share.
struct World {
    cpus: Vec<Cpu>,
    clock: AtomicU64,
}

/// A handle on the [`World`] that makes kernel threads in it.
struct Kernel(Arc<World>);

impl Kernel {
    /// A kernel with `cpus` processors, its clock at 0.
    fn with_cpus(cpus: usize) -> Self {
        Self(Arc::new(World {
            cpus: (0..cpus).map(|_| Cpu::new()).collect(),
            clock: AtomicU64::new(0),
        }))
    }

    /// The context of the kernel thread running on `cpu`.
    fn thread_on(&self, cpu: usize) -> OnCpu {
        OnCpu {
            world: Arc::clone(&self.0),
            cpu,
            can_block: true,
        }
    }

    /// The context of `cpu`'s boot or idle flow, which cannot block.
    fn boot_flow_on(&self, cpu: usize) -> OnCpu {
        OnCpu {
            can_block: false,
            ..self.thread_on(cpu)
        }
    }

    /// Spawns a kernel thread on `cpu`.
    fn spawn<R: 'static>(
        &self,
        cpu: usize,
        body: impl FnOnce(OnCpu) -> R + 'static,
    ) -> JoinHandle<R> {
        let me = self.thread_on(cpu);
        thread::spawn(move || body(me))
    }
}

/// `sched::ThreadRef`: a processor and a run-queue slot's generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ThreadName {
    cpu: usize,
    slot: usize,
    generation: u32,
}

/// One thread's view of the kernel, standing in for `sched` and `cpu`. The run-queue calls are
/// the ones `sched::on_block`, `sched::wake` and `sched::on_tick` make, under the same lock.
struct OnCpu {
    world: Arc<World>,
    cpu: usize,
    can_block: bool,
}

impl OnCpu {
    fn this_cpu(&self) -> &Cpu {
        &self.world.cpus[self.cpu]
    }
}

impl Scheduler for OnCpu {
    type Thread = ThreadName;

    fn current_thread(&self) -> Option<ThreadName> {
        self.can_block.then(|| {
            let cpu = self.this_cpu();
            ThreadName {
                cpu: self.cpu,
                slot: cpu.slot,
                generation: cpu.generation,
            }
        })
    }

    fn without_interrupts<R>(&self, f: impl FnOnce() -> R) -> R {
        // The model has no interrupts: a handler that can wake is another thread here.
        f()
    }

    fn block(&self) {
        let cpu = self.this_cpu();
        let next = cpu.queue.lock().block(THREAD_FRAME).unwrap();
        if next == THREAD_FRAME {
            return; // A wake-up was pending: this thread goes on.
        }
        assert_eq!(next, IDLE_FRAME);
        // Halted in the idle context until a reschedule finds this thread ready.
        loop {
            cpu.kick.wait();
            if cpu.queue.lock().yield_now(IDLE_FRAME) == THREAD_FRAME {
                return;
            }
        }
    }

    fn block_until(&self, deadline: u64) {
        let cpu = self.this_cpu();
        let next = cpu.queue.lock().block_for(THREAD_FRAME, Some(1)).unwrap();
        if next == THREAD_FRAME {
            return; // A wake-up was pending.
        }
        assert_eq!(next, IDLE_FRAME);
        // Halted in the idle context. A timer tick always comes: the clock passes the deadline
        // and the queue makes this thread ready on its own, if no wake-up did first.
        self.world.clock.store(deadline, Relaxed);
        assert_eq!(cpu.queue.lock().tick(IDLE_FRAME), THREAD_FRAME);
    }

    fn yield_now(&self) {
        // The poll of a boot flow or of a thread that found the queue full: a spin to Loom.
        crate::prim::spin_loop();
    }

    fn wake(&self, thread: ThreadName) {
        let cpu = &self.world.cpus[thread.cpu];
        let woke = cpu.queue.lock().wake(thread.slot, thread.generation);
        if woke {
            cpu.kick.notify();
        }
    }

    fn monotonic_nanos(&self) -> u64 {
        self.world.clock.load(Relaxed)
    }
}

type Queue = WaitQueue<ThreadName, WAITERS>;

/// Takes one token if there is one (each waiter's condition in the many-waiter models).
fn take(tokens: &AtomicU32) -> bool {
    tokens
        .fetch_update(Relaxed, Relaxed, |t| t.checked_sub(1))
        .is_ok()
}

// ---------------------------------------------------------------- the wait-queue models

/// One thread waits for a flag; another sets it, then wakes the queue. A lost wake-up leaves
/// the waiter blocked for ever, which Loom reports as a deadlock.
fn one_waiter_one_waker(set: Ordering, read: Ordering) {
    let world = Kernel::with_cpus(2);
    let queue = Arc::new(Queue::new());
    let flag = Arc::new(AtomicBool::new(false));
    let waiter = world.spawn(0, {
        let (queue, flag) = (Arc::clone(&queue), Arc::clone(&flag));
        move |me| {
            queue.wait_until(&me, || flag.load(read));
            assert!(flag.load(read), "returned with the condition false");
        }
    });
    let waker = world.spawn(1, move |me| {
        flag.store(true, set);
        queue.wake_one(&me);
    });
    waiter.join().unwrap();
    waker.join().unwrap();
}

/// The kernel's `TURN` orderings.
#[test]
fn wait_queue_waker_and_waiter_release_acquire() {
    check(
        "wait_queue_waker_and_waiter_release_acquire",
        preempted(6, SPINS),
        EXHAUSTIVE,
        || {
            one_waiter_one_waker(Release, Acquire);
        },
    );
}

/// `Relaxed` on the condition, the weakest any caller could use (the NVMe and NIC conditions
/// read device memory, which no ordering governs). The protocol must hold on the queue's lock
/// alone.
#[test]
fn wait_queue_waker_and_waiter_relaxed() {
    check(
        "wait_queue_waker_and_waiter_relaxed",
        preempted(6, SPINS),
        EXHAUSTIVE,
        || {
            one_waiter_one_waker(Relaxed, Relaxed);
        },
    );
}

/// The boot proof's ping-pong (`pinger` and `ponger` in `framekernel/src/wait.rs`): each thread
/// both waits and wakes, with its orderings.
fn ping_pong(rounds: u64) {
    let world = Kernel::with_cpus(2);
    let turn = Arc::new(AtomicU64::new(0));
    let ping = Arc::new(Queue::new());
    let pong = Arc::new(Queue::new());
    let pinger = world.spawn(0, {
        let (turn, ping, pong) = (Arc::clone(&turn), Arc::clone(&ping), Arc::clone(&pong));
        move |me| {
            for _ in 0..rounds {
                turn.store(1, Release);
                pong.wake_one(&me);
                ping.wait_until(&me, || turn.load(Acquire) == 0);
            }
            turn.store(2, Release);
            pong.wake_one(&me);
        }
    });
    let ponger = world.spawn(1, move |me| {
        loop {
            pong.wait_until(&me, || turn.load(Acquire) != 0);
            if turn.load(Acquire) == 2 {
                break;
            }
            turn.store(0, Release);
            ping.wake_one(&me);
        }
    });
    pinger.join().unwrap();
    ponger.join().unwrap();
}

#[test]
fn wait_queue_ping_pong() {
    check(
        "wait_queue_ping_pong",
        preempted(3, SPINS),
        preempted(4, SPINS),
        || ping_pong(1),
    );
}

/// The boot proof's producer and consumer (`framekernel/src/wait.rs`): each blocks on a full or
/// empty ring, the condition taking the ring's own lock under the queue's lock.
#[test]
fn wait_queue_producer_consumer() {
    check(
        "wait_queue_producer_consumer",
        preempted(3, SPINS),
        preempted(4, SPINS),
        || {
            const CAPACITY: usize = 1;
            const ITEMS: u64 = 2;
            let world = Kernel::with_cpus(2);
            let ring = Arc::new(SpinLock::new(0usize));
            let not_empty = Arc::new(Queue::new());
            let not_full = Arc::new(Queue::new());
            let producer = world.spawn(0, {
                let (ring, not_empty, not_full) = (
                    Arc::clone(&ring),
                    Arc::clone(&not_empty),
                    Arc::clone(&not_full),
                );
                move |me| {
                    for _ in 0..ITEMS {
                        not_full.wait_until(&me, || *ring.lock() < CAPACITY);
                        *ring.lock() += 1;
                        not_empty.wake_one(&me);
                    }
                }
            });
            let consumer = world.spawn(1, {
                let ring = Arc::clone(&ring);
                move |me| {
                    for _ in 0..ITEMS {
                        not_empty.wait_until(&me, || *ring.lock() > 0);
                        *ring.lock() -= 1;
                        not_full.wake_one(&me);
                    }
                }
            });
            producer.join().unwrap();
            consumer.join().unwrap();
            assert_eq!(*ring.lock(), 0);
        },
    );
}

/// Two waiters on one queue, one waker with one token and one wake-up for each.
#[test]
fn wait_queue_two_waiters() {
    check(
        "wait_queue_two_waiters",
        preempted(2, 2),
        preempted(3, 3),
        || {
            let world = Kernel::with_cpus(3);
            let queue = Arc::new(Queue::new());
            let tokens = Arc::new(AtomicU32::new(0));
            let waiters = [0, 1].map(|cpu| {
                let (queue, tokens) = (Arc::clone(&queue), Arc::clone(&tokens));
                world.spawn(cpu, move |me| {
                    queue.wait_until(&me, || take(&tokens));
                })
            });
            let waker = world.spawn(2, move |me| {
                for _ in 0..2 {
                    tokens.fetch_add(1, Relaxed);
                    queue.wake_one(&me);
                }
            });
            for waiter in waiters {
                waiter.join().unwrap();
            }
            waker.join().unwrap();
        },
    );
}

/// A queue with one slot: the second waiter finds it full and polls by yielding, and still
/// sees its token.
#[test]
fn wait_queue_full_queue_polls() {
    check(
        "wait_queue_full_queue_polls",
        preempted(2, 2),
        preempted(3, 3),
        || {
            let world = Kernel::with_cpus(3);
            let queue = Arc::new(WaitQueue::<ThreadName, 1>::new());
            let tokens = Arc::new(AtomicU32::new(0));
            let waiters = [0, 1].map(|cpu| {
                let (queue, tokens) = (Arc::clone(&queue), Arc::clone(&tokens));
                world.spawn(cpu, move |me| {
                    queue.wait_until(&me, || take(&tokens));
                })
            });
            let waker = world.spawn(2, move |me| {
                for _ in 0..2 {
                    tokens.fetch_add(1, Relaxed);
                    queue.wake_one(&me);
                }
            });
            for waiter in waiters {
                waiter.join().unwrap();
            }
            waker.join().unwrap();
        },
    );
}

/// The boot or idle flow cannot block, so it polls by yielding until the condition holds.
#[test]
fn wait_queue_boot_flow_polls() {
    check("wait_queue_boot_flow_polls", EXHAUSTIVE, EXHAUSTIVE, || {
        let world = Kernel::with_cpus(2);
        let queue = Arc::new(Queue::new());
        let flag = Arc::new(AtomicBool::new(false));
        let boot = {
            let (queue, flag) = (Arc::clone(&queue), Arc::clone(&flag));
            let me = world.boot_flow_on(0);
            thread::spawn(move || {
                let blocked = queue.wait_until(&me, || flag.load(Acquire));
                assert_eq!(blocked, 0, "the boot flow must never block");
            })
        };
        let waker = world.spawn(1, move |me| {
            flag.store(true, Release);
            queue.wake_one(&me);
        });
        boot.join().unwrap();
        waker.join().unwrap();
    });
}

/// A timed waiter that gives up must not swallow the wake-up meant for the waiter behind it:
/// one token and one wake-up. If the timed waiter did not end up with the token, the
/// untimed waiter must get it; if it did, the untimed waiter gets a second one.
#[test]
fn wait_queue_timed_wait_gives_way() {
    check(
        "wait_queue_timed_wait_gives_way",
        preempted(2, 2),
        preempted(3, 3),
        || {
            let world = Kernel::with_cpus(4);
            let queue = Arc::new(Queue::new());
            let tokens = Arc::new(AtomicU32::new(0));
            let timed = world.spawn(0, {
                let (queue, tokens) = (Arc::clone(&queue), Arc::clone(&tokens));
                move |me| {
                    queue
                        .wait_until_deadline(&me, || take(&tokens), DEADLINE)
                        .is_ok()
                }
            });
            let untimed = world.spawn(1, {
                let (queue, tokens) = (Arc::clone(&queue), Arc::clone(&tokens));
                move |me| {
                    queue.wait_until(&me, || take(&tokens));
                }
            });
            let waker = world.spawn(2, {
                let (queue, tokens) = (Arc::clone(&queue), Arc::clone(&tokens));
                move |me| {
                    tokens.fetch_add(1, Relaxed);
                    queue.wake_one(&me);
                }
            });
            let got_the_token = timed.join().unwrap();
            waker.join().unwrap();
            if got_the_token {
                tokens.fetch_add(1, Relaxed);
                queue.wake_one(&world.thread_on(3));
            }
            untimed.join().unwrap();
        },
    );
}

/// A wake-up can outlive the wait it was for (it reached a timed waiter that had already been
/// made ready by its deadline) and end the next wait's first block. The wait must re-check.
#[test]
fn wait_queue_stale_wake_up_is_harmless() {
    check(
        "wait_queue_stale_wake_up_is_harmless",
        preempted(3, SPINS),
        preempted(4, SPINS),
        || {
            let world = Kernel::with_cpus(2);
            let queue = Arc::new(Queue::new());
            let first = Arc::new(AtomicBool::new(false));
            let second = Arc::new(AtomicBool::new(false));
            let waiter = world.spawn(0, {
                let (queue, first, second) =
                    (Arc::clone(&queue), Arc::clone(&first), Arc::clone(&second));
                move |me| {
                    let _ = queue.wait_until_deadline(&me, || first.load(Acquire), DEADLINE);
                    queue.wait_until(&me, || second.load(Acquire));
                    assert!(second.load(Acquire), "returned with the condition false");
                }
            });
            let waker = world.spawn(1, move |me| {
                first.store(true, Release);
                queue.wake_one(&me);
                second.store(true, Release);
                queue.wake_one(&me);
            });
            waiter.join().unwrap();
            waker.join().unwrap();
        },
    );
}
