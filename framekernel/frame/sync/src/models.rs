//! Loom models of [`SpinLock`], [`RwLock`] and the kernel's wait-queue protocol (CLAUDE.md §73).
//!
//! Built only by `cargo xtask loom`, with `--cfg loom`, for this crate's tests, in a scratch
//! workspace that alone has Loom as a dependency (`developer-tools/xtask/src/loom.rs`). That tool
//! also applies a negative control for each property to a scratch copy of this source and
//! requires the named model to report the bug.
//!
//! # What is the real code, and what is a copy of it
//!
//! Real, and compiled in unchanged: the two locks with their orderings (`lib.rs`), `crate::wait`
//! (the protocol `framekernel/src/wait.rs` instantiates), the run queue
//! (`framekernel/src/runqueue.rs`) and the monotonic clock's floor (`floor.rs`, which
//! `origin_frame::monotonic` reads through). What differs under `--cfg loom` is `prim` in `lib.rs`, which
//! swaps `core`'s atomics, cell and spin hint for Loom's, and that the constructors are not
//! `const`.
//!
//! The TLB shootdown protocol (`shootdown.rs`, which `origin_frame::tlb` instantiates) is real
//! too. What stands in for the machine there is `TlbCpu`: a boolean for what its TLB holds, a
//! relaxed load for a page walk, `Notify` for the interrupt, and a clock that ticks once each
//! time it is read, so that an initiator gives up after `PATIENCE` polls.
//!
//! Copies, written by hand here, standing in for kernel code; nothing compares them with it:
//!
//! - `ThreadName` is `sched::ThreadRef`, and `OnCpu::current_thread` is `sched::current_thread`.
//! - `OnCpu::wake` is the logic of `sched::wake`: `RunQueue::wake` under the queue's lock, then
//!   a kick (the reschedule IPI) only if the thread it woke is on another processor.
//! - `OnCpu::block` and `OnCpu::block_until` make the run-queue calls `sched::on_block` makes, and
//!   `sched::block_until` turns a deadline into its tick count; the copy takes one tick to any
//!   reachable deadline and `u64::MAX` ticks to `UNREACHABLE`.
//! - `OnCpu::halt_until_ready` makes the calls `sched::on_reschedule` (`RunQueue::preempt_for_wake`) and
//!   `sched::on_tick` (`RunQueue::tick`) make for a processor halted in its idle context. What
//!   else those handlers do around them (`with_queue`, the FPU and TSS switch) is not here.
//! - `Cpu::new` leaves a real run queue as `sched` has it while a kernel thread runs: the idle
//!   context in slot 0 and that thread current.
//! - `wake_all` is the loop of `WaitQueue::wake_all` in `framekernel/src/wait.rs`.
//! - `monotonic_nanos` stands for `time::monotonic_nanos`, and `yield_now` for `sched::yield_now`
//!   (to Loom, a spin).
//!
//! # What is proved
//!
//! For the real code, **within the bounds each model prints**, over every thread interleaving
//! (up to Loom's removal of equivalent ones) and every store a `Relaxed` load may see in Loom's
//! model of the C11 memory model:
//!
//! - `SpinLock`: at most one holder at a time; every dereference of a guard is ordered by
//!   happens-before with every other (Loom's cell checks each one; no data race at a dereference:
//!   see "not proved"); each holder sees all earlier holders' writes, whole. `try_lock`: a failure
//!   leaves the holder's lock held and a success is a real holder.
//! - `RwLock`: a writer overlaps nobody; readers can hold it together; a reader sees a writer's
//!   update whole or not at all; and every dereference of a guard is ordered by happens-before
//!   with every conflicting one (a reader's with the next writer's, a writer's with the next
//!   reader's or writer's).
//! - The wait queue, no lost wake-ups: a thread in `wait_until`, or in `wait_until_deadline` with
//!   a deadline the clock never reaches (`UNREACHABLE`, so that only a wake-up can end its block),
//!   whose condition is made true by a thread that then calls `wake_one` does not stay blocked (a
//!   lost wake-up leaves every thread blocked, which Loom reports as a deadlock). This holds with
//!   `Relaxed` on the condition, the weakest ordering a caller could use: the queue's lock carries
//!   it. It holds whether the waker is on another processor, which kicks the waiter's with a
//!   reschedule IPI, or on the waiter's own, which sends none: the thread is made ready, and that
//!   processor's next timer tick runs it (`OnCpu::wake` has `sched::wake`'s no-IPI rule).
//! - Wake-all (`take_all`, each waiter then woken): it loses no wake-up against a waiter
//!   registering at the same moment, which either is taken or sees the condition. It takes every
//!   waiter, leaves none registered, and does not erase a registration made as it runs.
//! - A wait returns only with its condition true, also after a wake-up that outlived an earlier
//!   wait; a timed wait that gives up leaves no registration, so it cannot swallow the next
//!   waiter's wake-up; and a thread that cannot register (a full queue, or the boot flow) still
//!   sees the condition by polling. A timed wait on the boot flow polls, never blocks or registers,
//!   and ends with the condition or at the deadline.
//! - The clock's floor: the readings are constants that disagree, so a processor whose counter is
//!   behind reads the floor. A call that happens after another (through the release/acquire
//!   handoff a caller supplies, here standing for a lock or a wake-up) returns at least as much,
//!   also along a chain of processors, and each thread's own results never go down. Racing
//!   readers lose no update: the floor ends at the largest reading. The floor's own `Relaxed`
//!   orderings are enough for all of this. What carries the ordering between the two calls is the
//!   caller's handoff; a negative control weakens the handoff to `Relaxed` and the model then sees
//!   a stale floor taken at face value.
//! - TLB shootdown: when `shoot` returns, every online processor that held the translation has
//!   dropped it, also one coming online at that moment; the invalidation it does is the one the
//!   initiator asked for; and two initiators shooting at each other both finish. A request for
//!   one address space asks a processor in another for nothing, and still leaves no stale
//!   translation on a processor entering or leaving that space as it runs.
//! - The deferred free (`Shootdown::retire`): every access through a translation that was cached
//!   when the page left the tables happens before the frame is released (Loom's cell reports any
//!   pair that is not ordered), and an unmap that shoots down releases exactly once, also for a
//!   frame mapped in two spaces and unmapped from each by a different processor (`RefCount`).
//!
//! # What is not proved
//!
//! - Anything past the bounds: more threads or rounds than the models have (at most three spawned
//!   threads and the model's own thread here; Loom's limit is five in all), more thread preemptions
//!   than the bound where one is printed, or a schedule in which a thread spins past
//!   `Bound::spins`: from that point on its whole execution is no longer explored (below).
//! - Anything Loom's memory model omits: it does not model load buffering (a load seeing a store
//!   that its thread only makes later), its `compare_exchange_weak` never fails spuriously, so
//!   `lock` is not explored taking a spurious failure (it would only loop again), and it keeps
//!   only the last 7 stores of each atomic (`MAX_ATOMIC_HISTORY`), so a load is never explored
//!   reading an older one.
//! - Later uses of a guard's reference: Loom's cell checks each `Deref` and `DerefMut` of a guard
//!   as it runs (`with` and `with_mut` in `lib.rs`), and does not follow the reference it hands
//!   back. "Every dereference" above means those moments, not each use of the reference. The code
//!   the models run does nothing more to a Loom atomic or cell between the two, but no other
//!   callers of these locks are modelled.
//! - Interrupts: `without_interrupts` does nothing here, so the rule that no handler may take a
//!   lock interrupted code can hold is not checked. A handler that wakes is modelled as another
//!   thread, which can overlap the thread it interrupted; on the machine it cannot.
//! - The scheduler around the run queue: the context switch, the reschedule IPI and the timer
//!   are stood in for by [`Cpu`]'s `kick` and the run-queue calls they make (the copies above).
//!   That a ready thread eventually runs is assumed, and so is the timer: one kick after a
//!   same-processor wake stands for the next tick, which the real timer always brings. Fairness
//!   and starvation are not modelled.
//! - The machine: these are host builds against Loom's atomics. The x86-64 code the kernel runs,
//!   the `pause` instruction and TSO are not examined. In the shootdown models the page walker is
//!   a relaxed load at the points a model calls `walk`, where a real one may walk at any moment;
//!   a TLB is a boolean; and speculative use of a stale translation is not modelled.
//! - A processor that does not answer, beyond the models' own use of it: an initiator gives up
//!   after `PATIENCE` polls and the model fails with `NO_ANSWER`, which is how a bug that stops
//!   an answer shows (the control `shootdown_waiting_initiator_does_not_serve`). No correct
//!   execution gets there (`PATIENCE` says why). The leak that follows a `Stalled` is tested on
//!   the host only.
//! - `RwLock` writer starvation, which its documentation accepts; a timed waiter that finds the
//!   queue full (the other way to reach `Step::Poll`, the same loop as the boot flow's); and what
//!   callers must uphold themselves: the state changes before `wake_one`, and one condition per
//!   queue (`waker_wakes_before_setting_the_flag` shows the first matters). A wait with a deadline
//!   that does arrive is not shown to keep its wake-up: the timer ends it either way.
//!
//! # What the models add
//!
//! Nothing that orders memory where the real code does not: the models' own counters, tokens
//! and clock are `Relaxed`. `Notify`, the stand-in for what restarts a halted processor,
//! does synchronize, but the resumed thread goes on to take the run queue's lock, which carries
//! the ordering the real resumption gets from that lock anyway. The floor's models hand a value
//! over with release and acquire, because that handoff is the caller's ordering, which the
//! property is about.
//!
//! # Bounds
//!
//! Two threads spinning on one lock can take turns for ever, and Loom counts each turn as a new
//! execution whatever the preemption bound. So spinning is cut off: once any one thread of an
//! execution has spun more than `Bound::spins` times, Loom's `skip_branch` stops the exploration
//! of that whole execution, for all its threads, from there on. The count is per thread; the
//! cut-off is not. The execution still runs to its end, with every check, along one schedule.
//! A spin iteration changes nothing and can only observe the lock change hands, so the bound
//! limits how many of those changes a thread can wait through while its execution is explored.
//! `ORIGIN_LOOM_DEEP` selects each model's larger bound (a long run, `cargo xtask loom` only).

use std::sync::atomic::AtomicU64 as ExecutionCounter;

use core::sync::atomic::Ordering::{self, Acquire, Relaxed, Release};

use loom::cell::UnsafeCell;
use loom::sync::atomic::{AtomicBool, AtomicU32, AtomicU64};
use loom::sync::{Arc, Notify};
use loom::thread::{self, JoinHandle};

use crate::floor::MonotonicFloor;
use crate::refcount::RefCount;
use crate::runqueue::RunQueue;
use crate::shootdown::{Request, Scope, Shootdown, TlbHardware};
use crate::wait::{Scheduler, TimedOut, WaitQueue};
use crate::{RwLock, SpinLock};

/// How far one model's search goes.
#[derive(Clone, Copy)]
struct Bound {
    /// At most this many thread preemptions; `None` is the exhaustive search.
    preemptions: Option<usize>,
    /// The spin iterations one thread may make with its execution still explored; one more, by
    /// any thread, ends the exploration of the whole execution (see `prim::spin_loop`).
    spins: u32,
}

/// Every model's spin bound (`Bound::spins`), unless it says otherwise.
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
/// from the environment only). They reach a model only when `ORIGIN_LOOM_EXPERIMENT` is set on
/// `cargo xtask loom`, which proves nothing: xtask removes them from every other run, so that
/// `cargo xtask check` cannot be weakened by what happens to be exported.
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
        "loom model {name}: {} executions, {search}, an execution stops branching once a thread \
         has spun more than {spins} times, {} ms",
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

// ---------------------------------------------------------------- RwLock

/// What a writer adds to `inside`, where a reader adds 1: any overlap with a writer shows.
const WRITER_INSIDE: u64 = 1 << 32;

/// One writer and two readers. A writer overlaps nobody; a reader sees the writer's update
/// whole or not at all; a reader's reads are ordered before the next writer's writes.
#[test]
fn rwlock_writer_excludes_readers() {
    check(
        "rwlock_writer_excludes_readers",
        preempted(2, SPINS),
        preempted(4, SPINS),
        || {
            let lock = Arc::new(RwLock::new((0u64, 0u64)));
            let inside = Arc::new(AtomicU64::new(0));
            let writer = {
                let (lock, inside) = (Arc::clone(&lock), Arc::clone(&inside));
                thread::spawn(move || {
                    let mut held = lock.write();
                    assert_eq!(
                        inside.fetch_add(WRITER_INSIDE, Relaxed),
                        0,
                        "a writer beside another holder"
                    );
                    held.0 = 1;
                    held.1 = 3;
                    inside.fetch_sub(WRITER_INSIDE, Relaxed);
                })
            };
            let readers = [0, 1].map(|_| {
                let (lock, inside) = (Arc::clone(&lock), Arc::clone(&inside));
                thread::spawn(move || {
                    let held = lock.read();
                    assert!(
                        inside.fetch_add(1, Relaxed) < WRITER_INSIDE,
                        "a reader beside a writer"
                    );
                    let seen = *held;
                    assert!(seen == (0, 0) || seen == (1, 3), "a torn update: {seen:?}");
                    inside.fetch_sub(1, Relaxed);
                })
            });
            writer.join().unwrap();
            for reader in readers {
                reader.join().unwrap();
            }
            assert_eq!(*lock.read(), (1, 3));
        },
    );
}

/// Two writers, each doing a non-atomic read-modify-write through its guard.
#[test]
fn rwlock_writers_exclude_each_other() {
    check(
        "rwlock_writers_exclude_each_other",
        EXHAUSTIVE,
        EXHAUSTIVE,
        || {
            let lock = Arc::new(RwLock::new(0u64));
            let writers = [0, 1].map(|_| {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    let mut held = lock.write();
                    let value = *held;
                    *held = value + 1;
                })
            });
            for writer in writers {
                writer.join().unwrap();
            }
            assert_eq!(*lock.read(), 2, "a lost update");
        },
    );
}

/// Readers do not exclude one another: each holds the lock while it waits for the other to
/// have it too. If they excluded each other, neither would ever get there.
#[test]
fn rwlock_readers_share() {
    check("rwlock_readers_share", EXHAUSTIVE, EXHAUSTIVE, || {
        let lock = Arc::new(RwLock::new(0u64));
        let first_holds = Arc::new(Notify::new());
        let second_holds = Arc::new(Notify::new());
        let first = {
            let (lock, first_holds, second_holds) = (
                Arc::clone(&lock),
                Arc::clone(&first_holds),
                Arc::clone(&second_holds),
            );
            thread::spawn(move || {
                let _held = lock.read();
                first_holds.notify();
                second_holds.wait();
            })
        };
        let second = thread::spawn(move || {
            let _held = lock.read();
            second_holds.notify();
            first_holds.wait();
        });
        first.join().unwrap();
        second.join().unwrap();
    });
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
/// A deadline nothing moves the clock to: only a wake-up can end a block on it.
const UNREACHABLE: u64 = u64::MAX;

/// What restarts a halted processor, and so which handler looks at its queue.
#[derive(Clone, Copy)]
enum Restart {
    /// The reschedule IPI another processor sends: `sched::on_reschedule`.
    Ipi,
    /// This processor's timer: `sched::on_tick`. A wake-up from the processor itself sends no
    /// IPI, so its next tick is what runs the woken thread.
    Tick,
}

/// A processor: its real run queue, holding one kernel thread and the idle context.
struct Cpu {
    queue: SpinLock<RunQueue>,
    /// Stands for the reschedule IPI or timer tick that makes the halted processor look at its
    /// queue again. Loom's `Notify` may also return spuriously, once per execution, like a tick
    /// that readies nothing: the queue, not this, decides what runs.
    kick: Notify,
    restart: Restart,
    slot: usize,
    generation: u32,
}

impl Cpu {
    /// A processor already running its kernel thread.
    fn new(restart: Restart) -> Self {
        let mut queue = RunQueue::new();
        queue.set_idle(true);
        let slot = queue.push(THREAD_FRAME).unwrap();
        let generation = queue.generation(slot);
        assert_eq!(queue.tick(IDLE_FRAME), THREAD_FRAME);
        Self {
            queue: SpinLock::new(queue),
            kick: Notify::new(),
            restart,
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
    /// A kernel with `cpus` processors, each restarted by the reschedule IPI, its clock at 0.
    fn with_cpus(cpus: usize) -> Self {
        Self::new(cpus, Restart::Ipi)
    }

    /// A kernel with one processor, which only its timer can restart, its clock at 0.
    fn with_one_ticking_cpu() -> Self {
        Self::new(1, Restart::Tick)
    }

    fn new(cpus: usize, restart: Restart) -> Self {
        Self(Arc::new(World {
            cpus: (0..cpus).map(|_| Cpu::new(restart)).collect(),
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
/// the ones `sched::on_block`, `sched::wake`, `sched::on_reschedule` and `sched::on_tick` make,
/// under the same lock.
struct OnCpu {
    world: Arc<World>,
    cpu: usize,
    can_block: bool,
}

impl OnCpu {
    fn this_cpu(&self) -> &Cpu {
        &self.world.cpus[self.cpu]
    }

    /// Halted in the idle context until a restart finds this thread ready.
    fn halt_until_ready(&self) {
        let cpu = self.this_cpu();
        loop {
            cpu.kick.wait();
            let next = match cpu.restart {
                Restart::Ipi => cpu.queue.lock().preempt_for_wake(IDLE_FRAME),
                Restart::Tick => cpu.queue.lock().tick(IDLE_FRAME),
            };
            if next == THREAD_FRAME {
                return;
            }
        }
    }

    /// This processor's next timer interrupt, which always comes: it restarts the processor
    /// (`Restart::Tick`).
    fn timer_tick(&self) {
        self.this_cpu().kick.notify();
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
        let next = self.this_cpu().queue.lock().block(THREAD_FRAME).unwrap();
        if next == THREAD_FRAME {
            return; // A wake-up was pending: this thread goes on.
        }
        assert_eq!(next, IDLE_FRAME);
        self.halt_until_ready();
    }

    fn block_until(&self, deadline: u64) {
        let cpu = self.this_cpu();
        let ticks = if deadline == UNREACHABLE { u64::MAX } else { 1 };
        let next = cpu
            .queue
            .lock()
            .block_for(THREAD_FRAME, Some(ticks))
            .unwrap();
        if next == THREAD_FRAME {
            return; // A wake-up was pending.
        }
        assert_eq!(next, IDLE_FRAME);
        if deadline == UNREACHABLE {
            // Nothing but a wake-up can end this block: no timer will.
            return self.halt_until_ready();
        }
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
        let target = &self.world.cpus[thread.cpu];
        let woke = target.queue.lock().wake(thread.slot, thread.generation);
        // `sched::wake`: the IPI goes only to another processor. A thread woken from its own
        // processor runs at that processor's next timer tick.
        if woke && thread.cpu != self.cpu {
            target.kick.notify();
        }
    }

    fn monotonic_nanos(&self) -> u64 {
        self.world.clock.load(Relaxed)
    }
}

type Queue = WaitQueue<ThreadName, WAITERS>;

/// How a model's waiter waits.
#[derive(Clone, Copy)]
enum Wait {
    /// `WaitQueue::wait_until`.
    Untimed,
    /// `WaitQueue::wait_until_deadline`, with a deadline the clock never reaches: nothing but a
    /// wake-up can end its block, so a lost one leaves it blocked, as in `Untimed`.
    Timed,
}

impl Wait {
    fn until(self, queue: &Queue, me: &OnCpu, condition: impl Fn() -> bool) {
        match self {
            Wait::Untimed => {
                queue.wait_until(me, condition);
            }
            Wait::Timed => {
                let waited = queue.wait_until_deadline(me, condition, UNREACHABLE);
                assert!(waited.is_ok(), "gave up before a deadline that never comes");
            }
        }
    }
}

/// `WaitQueue::wake_all` in `framekernel/src/wait.rs`: every waiter, each woken after the
/// queue's lock is released, so that each can re-check its own condition.
fn wake_all(queue: &Queue, me: &OnCpu) {
    for thread in queue.take_all(me).into_iter().flatten() {
        me.wake(thread);
    }
}

/// Takes one token if there is one (each waiter's condition in the many-waiter models).
fn take(tokens: &AtomicU32) -> bool {
    tokens
        .fetch_update(Relaxed, Relaxed, |t| t.checked_sub(1))
        .is_ok()
}

// ---------------------------------------------------------------- the wait-queue models

/// One thread waits for a flag; another sets it, then wakes the queue. A lost wake-up leaves
/// the waiter blocked for ever, which Loom reports as a deadlock.
fn one_waiter_one_waker(set: Ordering, read: Ordering, wait: Wait) {
    let world = Kernel::with_cpus(2);
    let queue = Arc::new(Queue::new());
    let flag = Arc::new(AtomicBool::new(false));
    let waiter = world.spawn(0, {
        let (queue, flag) = (Arc::clone(&queue), Arc::clone(&flag));
        move |me| {
            wait.until(&queue, &me, || flag.load(read));
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
            one_waiter_one_waker(Release, Acquire, Wait::Untimed);
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
            one_waiter_one_waker(Relaxed, Relaxed, Wait::Untimed);
        },
    );
}

/// As `wait_queue_waker_and_waiter_release_acquire`, for a timed wait whose deadline never
/// comes: only a wake-up can end its block.
#[test]
fn wait_queue_timed_waiter_and_waker_release_acquire() {
    check(
        "wait_queue_timed_waiter_and_waker_release_acquire",
        preempted(6, SPINS),
        EXHAUSTIVE,
        || {
            one_waiter_one_waker(Release, Acquire, Wait::Timed);
        },
    );
}

/// As `wait_queue_waker_and_waiter_relaxed`, for a timed wait whose deadline never comes.
#[test]
fn wait_queue_timed_waiter_and_waker_relaxed() {
    check(
        "wait_queue_timed_waiter_and_waker_relaxed",
        preempted(6, SPINS),
        EXHAUSTIVE,
        || {
            one_waiter_one_waker(Relaxed, Relaxed, Wait::Timed);
        },
    );
}

/// The waker runs on the waiter's own processor, so `sched::wake` sends no IPI: nothing kicks
/// the halted processor when the wake-up lands, and the waiter resumes at the processor's next
/// timer tick. Without that tick it would stay blocked (control `same_cpu_wake_gets_no_tick`).
fn same_cpu_wake(wait: Wait) {
    let world = Kernel::with_one_ticking_cpu();
    let queue = Arc::new(Queue::new());
    let flag = Arc::new(AtomicBool::new(false));
    let waiter = world.spawn(0, {
        let (queue, flag) = (Arc::clone(&queue), Arc::clone(&flag));
        move |me| {
            wait.until(&queue, &me, || flag.load(Relaxed));
            assert!(flag.load(Relaxed), "returned with the condition false");
        }
    });
    let waker = world.spawn(0, move |me| {
        flag.store(true, Relaxed);
        queue.wake_one(&me);
        me.timer_tick();
    });
    waiter.join().unwrap();
    waker.join().unwrap();
}

#[test]
fn wait_queue_same_cpu_wake_resumes_on_a_tick() {
    check(
        "wait_queue_same_cpu_wake_resumes_on_a_tick",
        EXHAUSTIVE,
        EXHAUSTIVE,
        || same_cpu_wake(Wait::Untimed),
    );
}

#[test]
fn wait_queue_same_cpu_timed_wake_resumes_on_a_tick() {
    check(
        "wait_queue_same_cpu_timed_wake_resumes_on_a_tick",
        EXHAUSTIVE,
        EXHAUSTIVE,
        || same_cpu_wake(Wait::Timed),
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

/// `take_all` against waiters registering as it runs: one waits untimed and one, as the kernel's
/// `wait_one` does, with a deadline, and the flag has been set before the waker looks. Each
/// waiter either is taken and woken or sees the flag, and none is left registered: once the
/// waker has looked, nobody can register (the flag is visible under the queue's lock).
#[test]
fn wait_queue_wake_all_wakes_every_waiter() {
    check(
        "wait_queue_wake_all_wakes_every_waiter",
        preempted(2, 2),
        preempted(3, 3),
        || {
            let world = Kernel::with_cpus(3);
            let queue = Arc::new(Queue::new());
            let flag = Arc::new(AtomicBool::new(false));
            let waiters = [(0, Wait::Untimed), (1, Wait::Timed)].map(|(cpu, wait)| {
                let (queue, flag) = (Arc::clone(&queue), Arc::clone(&flag));
                world.spawn(cpu, move |me| {
                    wait.until(&queue, &me, || flag.load(Relaxed));
                    assert!(flag.load(Relaxed), "returned with the condition false");
                })
            });
            let waker = world.spawn(2, move |me| {
                flag.store(true, Relaxed);
                wake_all(&queue, &me);
                assert!(
                    queue.take_all(&me).iter().all(Option::is_none),
                    "take_all left a waiter registered"
                );
            });
            for waiter in waiters {
                waiter.join().unwrap();
            }
            waker.join().unwrap();
        },
    );
}

/// A wake-all for some other reason (the kernel wakes a whole channel side on any signal)
/// runs as a thread registers. It must leave that registration standing or wake it: erased, and
/// not woken, the thread would sleep through the wake-up that matters. `take_all` is one
/// critical section, so it does.
fn wake_all_against_a_registration(wait: Wait) {
    let world = Kernel::with_cpus(3);
    let queue = Arc::new(Queue::new());
    let flag = Arc::new(AtomicBool::new(false));
    let waiter = world.spawn(0, {
        let (queue, flag) = (Arc::clone(&queue), Arc::clone(&flag));
        move |me| {
            wait.until(&queue, &me, || flag.load(Relaxed));
            assert!(flag.load(Relaxed), "returned with the condition false");
        }
    });
    let unrelated = world.spawn(1, {
        let queue = Arc::clone(&queue);
        move |me| wake_all(&queue, &me)
    });
    let waker = world.spawn(2, move |me| {
        flag.store(true, Relaxed);
        wake_all(&queue, &me);
    });
    waiter.join().unwrap();
    unrelated.join().unwrap();
    waker.join().unwrap();
}

#[test]
fn wait_queue_wake_all_keeps_a_concurrent_registration() {
    check(
        "wait_queue_wake_all_keeps_a_concurrent_registration",
        preempted(2, 2),
        preempted(3, 3),
        || wake_all_against_a_registration(Wait::Untimed),
    );
}

/// As above, for the timed wait the kernel's `wait_one` uses.
#[test]
fn wait_queue_wake_all_keeps_a_concurrent_timed_registration() {
    check(
        "wait_queue_wake_all_keeps_a_concurrent_timed_registration",
        preempted(2, 2),
        preempted(3, 3),
        || wake_all_against_a_registration(Wait::Timed),
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

/// A timed wait on the boot or idle flow: it cannot block or register, so it polls, and it ends
/// with the condition or at the deadline, whichever it sees first.
#[test]
fn wait_queue_boot_flow_timed_wait() {
    check(
        "wait_queue_boot_flow_timed_wait",
        preempted(3, SPINS),
        EXHAUSTIVE,
        || {
            let kernel = Kernel::with_cpus(3);
            let queue = Arc::new(Queue::new());
            let flag = Arc::new(AtomicBool::new(false));
            let boot = {
                let (queue, flag) = (Arc::clone(&queue), Arc::clone(&flag));
                let me = kernel.boot_flow_on(0);
                thread::spawn(move || {
                    match queue.wait_until_deadline(&me, || flag.load(Acquire), DEADLINE) {
                        Ok(blocked) => {
                            assert_eq!(blocked, 0, "the boot flow must never block");
                        }
                        Err(TimedOut) => {
                            assert!(
                                me.monotonic_nanos() >= DEADLINE,
                                "gave up before the deadline"
                            );
                        }
                    }
                })
            };
            let waker = kernel.spawn(1, {
                let (queue, flag) = (Arc::clone(&queue), Arc::clone(&flag));
                move |me| {
                    flag.store(true, Release);
                    queue.wake_one(&me);
                }
            });
            // The deadline passes whether or not the waker has run.
            let timer = kernel.spawn(2, |me| me.world.clock.store(DEADLINE, Relaxed));
            boot.join().unwrap();
            waker.join().unwrap();
            timer.join().unwrap();
            assert!(
                !queue.wake_one(&kernel.thread_on(1)),
                "the boot flow left a registration"
            );
        },
    );
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
fn stale_wake_up(wait: Wait) {
    let world = Kernel::with_cpus(2);
    let queue = Arc::new(Queue::new());
    let first = Arc::new(AtomicBool::new(false));
    let second = Arc::new(AtomicBool::new(false));
    let waiter = world.spawn(0, {
        let (queue, first, second) = (Arc::clone(&queue), Arc::clone(&first), Arc::clone(&second));
        move |me| {
            let _ = queue.wait_until_deadline(&me, || first.load(Acquire), DEADLINE);
            wait.until(&queue, &me, || second.load(Acquire));
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
}

#[test]
fn wait_queue_stale_wake_up_is_harmless() {
    check(
        "wait_queue_stale_wake_up_is_harmless",
        preempted(3, SPINS),
        preempted(4, SPINS),
        || stale_wake_up(Wait::Untimed),
    );
}

/// As `wait_queue_stale_wake_up_is_harmless`, the second wait timed, with a deadline that never
/// comes: `wait_until_deadline` must re-check too.
#[test]
fn wait_queue_timed_stale_wake_up_is_harmless() {
    check(
        "wait_queue_timed_stale_wake_up_is_harmless",
        preempted(3, SPINS),
        preempted(4, SPINS),
        || stale_wake_up(Wait::Timed),
    );
}

// ---------------------------------------------------------------- The clock's floor

/// Two processors whose counters disagree. One reads 7, then 100, hands over (a lock or a
/// wake-up in the kernel, a release/acquire pair here) and reads 110; the other is far behind
/// and reads 5, then 120. A read after the handoff is given at least what the handoff carried,
/// and neither thread's own results go down. The first reader's 7 exists so that there is an
/// older value of the floor to be stale with: a stale floor at or above a reading is taken at
/// face value, with no compare-exchange to correct it.
#[test]
fn floor_handoff_between_two_processors() {
    check(
        "floor_handoff_between_two_processors",
        preempted(3, SPINS),
        preempted(4, SPINS),
        || {
            let floor = Arc::new(MonotonicFloor::new());
            let handed = Arc::new(AtomicU64::new(0));
            let ahead = {
                let (floor, handed) = (Arc::clone(&floor), Arc::clone(&handed));
                thread::spawn(move || {
                    let older = floor.advance(7);
                    let first = floor.advance(100);
                    assert!(
                        first >= older,
                        "the clock went backwards: {first} after {older}"
                    );
                    handed.store(first + 1, Release);
                    let second = floor.advance(110);
                    assert!(
                        second >= first,
                        "the clock went backwards: {second} after {first}"
                    );
                })
            };
            let seen = handed.load(Acquire);
            let behind = floor.advance(5);
            if seen != 0 {
                let given = seen - 1;
                assert!(
                    behind >= given,
                    "the clock went backwards across a handoff: {behind} after {given}"
                );
            }
            let later = floor.advance(120);
            assert!(
                later >= behind,
                "the clock went backwards: {later} after {behind}"
            );
            ahead.join().unwrap();
            let last = floor.advance(1);
            assert!(
                last >= later,
                "the clock went backwards: {last} after {later}"
            );
        },
    );
}

/// The same across a chain of three processors, each further behind than the last: what one
/// was given binds the next, and so the one after that.
#[test]
fn floor_handoff_chain() {
    check(
        "floor_handoff_chain",
        preempted(3, SPINS),
        preempted(4, SPINS),
        || {
            let floor = Arc::new(MonotonicFloor::new());
            let first_to_second = Arc::new(AtomicU64::new(0));
            let second_to_third = Arc::new(AtomicU64::new(0));
            let first = {
                let (floor, handed) = (Arc::clone(&floor), Arc::clone(&first_to_second));
                thread::spawn(move || {
                    let got = floor.advance(100);
                    handed.store(got + 1, Release);
                })
            };
            let second = {
                let (floor, seen, handed) = (
                    Arc::clone(&floor),
                    Arc::clone(&first_to_second),
                    Arc::clone(&second_to_third),
                );
                thread::spawn(move || {
                    let seen = seen.load(Acquire);
                    let got = floor.advance(50);
                    if seen != 0 {
                        assert!(got >= seen - 1, "the clock went backwards across a handoff");
                    }
                    handed.store(got + 1, Release);
                })
            };
            let seen = second_to_third.load(Acquire);
            let got = floor.advance(10);
            if seen != 0 {
                assert!(got >= seen - 1, "the clock went backwards across a handoff");
            }
            first.join().unwrap();
            second.join().unwrap();
        },
    );
}

/// Readers racing with readings 3, 1 and 2: no update is lost and each is given at least what
/// it read.
#[test]
fn floor_racing_readers_end_at_the_largest_reading() {
    check(
        "floor_racing_readers_end_at_the_largest_reading",
        preempted(3, SPINS),
        preempted(4, SPINS),
        || {
            let floor = Arc::new(MonotonicFloor::new());
            let readers: Vec<_> = [3u64, 1]
                .into_iter()
                .map(|reading| {
                    let floor = Arc::clone(&floor);
                    thread::spawn(move || {
                        let got = floor.advance(reading);
                        assert!(
                            got >= reading,
                            "the clock went backwards: {got} after reading {reading}"
                        );
                    })
                })
                .collect();
            let got = floor.advance(2);
            assert!(got >= 2, "the clock went backwards: {got} after reading 2");
            for reader in readers {
                reader.join().unwrap();
            }
            assert_eq!(floor.advance(0), 3, "the floor is the largest reading");
        },
    );
}

// ---------------------------------------------------------------- TLB shootdown

/// Processors in a shootdown model.
const SHOOT_CPUS: usize = 3;
/// The one page the shootdown models care about.
const THE_PAGE: u64 = 0x40_0000;
/// The page table entry of [`THE_PAGE`] while it is mapped; not mapped, it is 0.
const MAPPED: u64 = 1;
/// What a model's assertion says when a shootdown failed to do its job.
const STALE: &str = "a processor kept a translation the shootdown took away";
/// What a model says when an initiator gave up waiting for an answer: see [`PATIENCE`].
const NO_ANSWER: &str = "a processor the shootdown asked never answered";
/// What a model says when a request for one address space interrupted a processor in another.
const OUTSIDE: &str = "a shootdown for one space reached a processor in another";

/// How many readings of its clock an initiator waits for its answers before it gives up: about
/// one per poll. Far more than an execution can poll while it is explored, which the spin bound
/// ends after [`SPINS`]; after that, Loom runs another thread at each of the initiator's spins,
/// and every target answers within a few of its own steps. So a correct model never gives up,
/// and one in which an answer can never come gives up and fails with [`NO_ANSWER`], instead of
/// spinning until Loom's limit on branches says only that something never ended.
const PATIENCE: u64 = 64;

/// The machine of a shootdown model: a page table entry and each processor's TLB.
struct TlbWorld {
    shootdown: Shootdown<SHOOT_CPUS>,
    /// Whether each processor holds a translation of [`THE_PAGE`].
    tlb: [AtomicBool; SHOOT_CPUS],
    /// The shootdown interrupt.
    kick: [Notify; SHOOT_CPUS],
    /// The model is over: serving processors stop.
    stop: AtomicBool,
    /// [`THE_PAGE`]'s page table entry.
    pte: AtomicU64,
    /// Each processor's CR3: what it runs on.
    cr3: [AtomicU64; SHOOT_CPUS],
    /// Each processor's interrupt flag, for [`IrqCpu`]: held while interrupts are off there.
    interrupts: [SpinLock<()>; SHOOT_CPUS],
    /// A processor that no request of the model is for: interrupting it fails the model with
    /// [`OUTSIDE`], at the moment the interrupt is sent.
    bystander: Option<usize>,
}

/// One processor of a [`TlbWorld`], as one thread of the model sees it.
#[derive(Clone)]
struct TlbCpu {
    id: usize,
    world: Arc<TlbWorld>,
    /// This thread's clock: one tick per reading (see [`PATIENCE`]). Not an atomic, so that
    /// reading it is no step Loom has to interleave.
    clock: core::cell::Cell<u64>,
}

/// Processor `id` of `world`.
fn tlb_cpu(world: &Arc<TlbWorld>, id: usize) -> TlbCpu {
    TlbCpu {
        id,
        world: Arc::clone(world),
        clock: core::cell::Cell::new(0),
    }
}

impl TlbHardware for TlbCpu {
    fn this_cpu(&self) -> usize {
        self.id
    }

    /// Drops the translation if this request names the page. A request for any other page
    /// (a torn or stale one, say) leaves it cached, as `invlpg` would.
    fn invalidate(&self, request: Request) {
        let names_the_page = match request {
            Request::Pages { base, pages } => {
                base <= THE_PAGE && THE_PAGE < base + pages * crate::shootdown::PAGE
            }
            Request::NonGlobal | Request::Everything => true,
        };
        if names_the_page {
            self.world.tlb[self.id].store(false, Relaxed);
        }
    }

    fn send_ipi(&self, cpu: usize) {
        assert_ne!(self.world.bystander, Some(cpu), "{OUTSIDE}");
        self.world.kick[cpu].notify();
    }

    fn without_interrupts<R>(&self, f: impl FnOnce() -> R) -> R {
        f()
    }

    /// The clock moves only as it is read, so an initiator's wait is counted in its polls.
    fn now(&self) -> u64 {
        let now = self.clock.get();
        self.clock.set(now + 1);
        now
    }

    fn patience(&self) -> u64 {
        PATIENCE
    }

    fn current_root(&self) -> u64 {
        self.world.cr3[self.id].load(Relaxed)
    }
}

impl TlbWorld {
    fn new() -> Arc<Self> {
        Self::with_bystander(None)
    }

    /// A world in which sending processor `bystander` an interrupt fails the model.
    fn with_bystander(bystander: Option<usize>) -> Arc<Self> {
        Arc::new(Self {
            shootdown: Shootdown::new(),
            tlb: core::array::from_fn(|_| AtomicBool::new(false)),
            kick: core::array::from_fn(|_| Notify::new()),
            stop: AtomicBool::new(false),
            pte: AtomicU64::new(MAPPED),
            cr3: core::array::from_fn(|_| AtomicU64::new(KERNEL_SPACE)),
            interrupts: core::array::from_fn(|_| SpinLock::new(())),
            bystander,
        })
    }

    /// Ends the model: every serving processor stops.
    fn finish(&self, serving: Vec<(usize, JoinHandle<()>)>) {
        self.stop.store(true, Relaxed);
        for (cpu, handle) in serving {
            self.kick[cpu].notify();
            handle.join().unwrap();
        }
    }
}

impl TlbCpu {
    /// Switches this processor to `to`: its CR3 write also runs `then`.
    fn switch_to(&self, to: u64, then: impl FnOnce()) {
        self.world.shootdown.switch_space(self, to, || {
            self.world.cr3[self.id].store(to, Relaxed);
            then();
        });
    }

    /// A processor with interrupts on: takes the shootdown interrupt whenever it comes.
    fn serve(&self) {
        loop {
            self.world.kick[self.id].wait();
            if self.world.stop.load(Relaxed) {
                return;
            }
            self.world.shootdown.service(self);
        }
    }

    /// `serve`, on a thread of its own.
    fn serving(&self) -> (usize, JoinHandle<()>) {
        let me = self.clone();
        (self.id, thread::spawn(move || me.serve()))
    }

    /// Whether this processor holds the translation.
    fn holds(&self) -> bool {
        self.world.tlb[self.id].load(Relaxed)
    }

    /// A page walk, as far as this model needs one: a present entry fills the TLB.
    fn walk(&self) {
        if self.world.pte.load(Relaxed) == MAPPED {
            self.world.tlb[self.id].store(true, Relaxed);
        }
    }

    /// Takes the page away everywhere `scope` covers, as `vm::unmap` and a space's unmap do:
    /// the entry first, then the shootdown.
    fn take_the_page_away(&self, scope: Scope) {
        self.world.pte.store(0, Relaxed);
        self.world
            .shootdown
            .shoot(self, Request::page(THE_PAGE), scope)
            .expect(NO_ANSWER);
    }
}

/// When `shoot` returns, a processor that was online and held the translation has dropped it.
#[test]
fn shootdown_reaches_an_online_processor() {
    check(
        "shootdown_reaches_an_online_processor",
        preempted(3, SPINS),
        preempted(4, SPINS),
        || {
            let world = TlbWorld::new();
            let (near, far) = (tlb_cpu(&world, 0), tlb_cpu(&world, 1));
            world.shootdown.mark_online(&near);
            world.shootdown.mark_online(&far);
            world.tlb[1].store(true, Relaxed);
            let serving = far.serving();
            near.take_the_page_away(Scope::Kernel);
            assert!(!far.holds(), "{STALE}");
            world.finish(vec![serving]);
        },
    );
}

/// A processor coming online while the shootdown runs ends up without the translation: either
/// the initiator saw it online and waited for it, or its own first flush and the page walks
/// after it see the entry gone. It held the translation from before it came online.
#[test]
fn shootdown_and_a_processor_coming_online() {
    check(
        "shootdown_and_a_processor_coming_online",
        preempted(3, SPINS),
        preempted(4, SPINS),
        || {
            let world = TlbWorld::new();
            let (near, far) = (tlb_cpu(&world, 0), tlb_cpu(&world, 1));
            world.shootdown.mark_online(&near);
            world.tlb[1].store(true, Relaxed);
            let serving = {
                let me = far.clone();
                (
                    1,
                    thread::spawn(move || {
                        me.world.shootdown.mark_online(&me);
                        me.walk();
                        me.serve();
                    }),
                )
            };
            near.take_the_page_away(Scope::Kernel);
            world.finish(vec![serving]);
            assert!(!far.holds(), "{STALE}");
        },
    );
}

/// Two processors each take the page away while the other holds it: each serves the other's
/// request while it waits for its turn, and neither finishes before the other has dropped it.
#[test]
fn shootdown_two_initiators_serve_each_other() {
    check(
        "shootdown_two_initiators_serve_each_other",
        preempted(2, SPINS),
        preempted(3, SPINS),
        || {
            let world = TlbWorld::new();
            let cpus = [tlb_cpu(&world, 0), tlb_cpu(&world, 1)];
            for cpu in &cpus {
                world.shootdown.mark_online(cpu);
                world.tlb[cpu.id].store(true, Relaxed);
            }
            let finished = Arc::new((AtomicU64::new(0), Notify::new()));
            let handles: Vec<_> = cpus
                .iter()
                .map(|cpu| {
                    let (me, finished) = (cpu.clone(), Arc::clone(&finished));
                    let other = 1 - cpu.id;
                    (
                        cpu.id,
                        thread::spawn(move || {
                            me.take_the_page_away(Scope::Kernel);
                            assert!(!me.world.tlb[other].load(Relaxed), "{STALE}");
                            finished.0.fetch_add(1, Release);
                            finished.1.notify();
                            me.serve();
                        }),
                    )
                })
                .collect();
            // Loom's `Notify` can wake a waiter with nothing to report, so the count decides.
            while finished.0.load(Acquire) < 2 {
                finished.1.wait();
            }
            world.finish(handles);
        },
    );
}

/// The address space of the targeted models.
const THE_SPACE: u64 = 0xA000;
/// The kernel's address space: where every model processor starts.
const KERNEL_SPACE: u64 = 0xF000;
/// Another address space: a process's that has nothing to do with this.
const OTHER_SPACE: u64 = 0xC000;

/// A processor in the space holds the translation: it is asked, and it answers. A processor in
/// another space, which also holds a translation of that address of its own, is not asked: it
/// is never sent the interrupt (the model fails at the moment it would be) and keeps it.
#[test]
fn shootdown_for_a_space_asks_only_the_processors_in_it() {
    check(
        "shootdown_for_a_space_asks_only_the_processors_in_it",
        preempted(3, SPINS),
        preempted(4, SPINS),
        || {
            let world = TlbWorld::with_bystander(Some(2));
            let (near, inside, elsewhere) =
                (tlb_cpu(&world, 0), tlb_cpu(&world, 1), tlb_cpu(&world, 2));
            for cpu in [&near, &inside, &elsewhere] {
                world.shootdown.mark_online(cpu);
            }
            inside.switch_to(THE_SPACE, || ());
            world.tlb[1].store(true, Relaxed);
            elsewhere.switch_to(OTHER_SPACE, || ());
            world.tlb[2].store(true, Relaxed);
            let serving = vec![inside.serving()];
            near.take_the_page_away(Scope::Space(THE_SPACE));
            assert!(!inside.holds(), "{STALE}");
            assert!(elsewhere.holds(), "{OUTSIDE}");
            world.finish(serving);
        },
    );
}

/// A processor enters the space while the page is taken away: the initiator either sees it
/// arriving and waits for its answer, or the walks it does on arrival see the entry gone.
#[test]
fn shootdown_and_a_processor_entering_the_space() {
    check(
        "shootdown_and_a_processor_entering_the_space",
        preempted(3, SPINS),
        preempted(4, SPINS),
        || {
            let world = TlbWorld::new();
            let (near, entering) = (tlb_cpu(&world, 0), tlb_cpu(&world, 1));
            world.shootdown.mark_online(&near);
            world.shootdown.mark_online(&entering);
            entering.switch_to(OTHER_SPACE, || ());
            let serving = {
                let me = entering.clone();
                (
                    1,
                    thread::spawn(move || {
                        me.switch_to(THE_SPACE, || me.walk());
                        me.serve();
                    }),
                )
            };
            near.take_the_page_away(Scope::Space(THE_SPACE));
            world.finish(vec![serving]);
            assert!(!entering.holds(), "{STALE}");
        },
    );
}

/// A processor leaves the space while the page is taken away, with the translation still
/// cached until its CR3 write. When `shoot` returns the translation must be gone: either it
/// has left, or the initiator saw it leaving and waited.
#[test]
fn shootdown_and_a_processor_leaving_the_space() {
    check(
        "shootdown_and_a_processor_leaving_the_space",
        preempted(3, SPINS),
        preempted(4, SPINS),
        || {
            let world = TlbWorld::new();
            let (near, leaving) = (tlb_cpu(&world, 0), tlb_cpu(&world, 1));
            world.shootdown.mark_online(&near);
            world.shootdown.mark_online(&leaving);
            leaving.switch_to(THE_SPACE, || ());
            world.tlb[1].store(true, Relaxed);
            let serving = {
                let me = leaving.clone();
                (
                    1,
                    thread::spawn(move || {
                        // The CR3 load drops every non-global translation.
                        me.switch_to(OTHER_SPACE, || me.invalidate(Request::NonGlobal));
                        me.serve();
                    }),
                )
            };
            near.take_the_page_away(Scope::Space(THE_SPACE));
            assert!(!leaving.holds(), "{STALE}");
            world.finish(vec![serving]);
        },
    );
}

/// A processor visits the space (a proof's read through its tables, with interrupts off) and
/// caches the translation there; the page is taken away while it is inside. The visit is in the
/// record, so the shootdown waits for the processor's answer, which comes only once the visit
/// is over and its CR3 write has dropped the translation (review finding 2).
#[test]
fn shootdown_and_a_processor_visiting_the_space() {
    check(
        "shootdown_and_a_processor_visiting_the_space",
        preempted(3, SPINS),
        preempted(4, SPINS),
        || {
            let world = TlbWorld::new();
            let (near, visitor) = (tlb_cpu(&world, 0), tlb_cpu(&world, 1));
            world.shootdown.mark_online(&near);
            world.shootdown.mark_online(&visitor);
            let inside = Arc::new((AtomicBool::new(false), Notify::new()));
            let serving = {
                let (me, inside) = (visitor.clone(), Arc::clone(&inside));
                (
                    1,
                    thread::spawn(move || {
                        let load = |to: u64| {
                            me.world.cr3[1].store(to, Relaxed);
                            if to != THE_SPACE {
                                // Back home: the CR3 load drops every non-global translation.
                                me.invalidate(Request::NonGlobal);
                            }
                        };
                        me.world.shootdown.visit(&me, THE_SPACE, load, || {
                            me.walk();
                            inside.0.store(true, Release);
                            inside.1.notify();
                        });
                        me.serve();
                    }),
                )
            };
            while !inside.0.load(Acquire) {
                inside.1.wait();
            }
            near.take_the_page_away(Scope::Space(THE_SPACE));
            assert!(!visitor.holds(), "{STALE}");
            world.finish(vec![serving]);
        },
    );
}

// ---------------------------------------------------------------- the deferred free

/// What the frame holds once it has been reused: not anything its old mapping should see.
const REUSED: u64 = 0xDEAD;

/// A frame of memory: Loom's cell reports any two accesses that are not ordered.
type ModelFrame = UnsafeCell<u64>;

impl TlbCpu {
    /// A user-mode access through this processor's TLB, if it still holds the translation.
    fn touch(&self, frame: &ModelFrame) {
        if self.holds() {
            // SAFETY: the pointer is the cell's own; Loom's cell checks this access against
            // every other, and panics if they are not ordered, which is the point of the model.
            frame.with_mut(|p| unsafe { *p = 1 });
        }
    }

    /// What `service` does in the shootdown interrupt, followed by the user code it interrupted
    /// going on: if the translation is gone, there is no access.
    fn take_interrupt_then_touch(&self, frame: &ModelFrame) {
        self.world.kick[self.id].wait();
        self.world.shootdown.service(self);
        self.touch(frame);
    }
}

/// The frame is released, and reused at once. Ordered after every access through a stale
/// translation, or Loom says so.
fn reuse(frame: &ModelFrame) {
    // SAFETY: as in `touch`.
    frame.with_mut(|p| unsafe { *p = REUSED });
}

/// A processor in user mode holds the translation and touches the page, before and after the
/// interrupt. The frame is taken out of the tables and retired while it does: the release runs
/// after every access that could use the translation.
#[test]
fn deferred_free_comes_after_the_last_use_of_a_stale_translation() {
    check(
        "deferred_free_comes_after_the_last_use_of_a_stale_translation",
        preempted(3, SPINS),
        preempted(4, SPINS),
        || {
            let world = TlbWorld::new();
            let (near, user) = (tlb_cpu(&world, 0), tlb_cpu(&world, 1));
            world.shootdown.mark_online(&near);
            world.shootdown.mark_online(&user);
            user.switch_to(THE_SPACE, || ());
            world.tlb[1].store(true, Relaxed);
            let frame = Arc::new(ModelFrame::new(0));
            let running = {
                let (me, frame) = (user.clone(), Arc::clone(&frame));
                (
                    1,
                    thread::spawn(move || {
                        me.touch(&frame);
                        me.take_interrupt_then_touch(&frame);
                        me.serve();
                    }),
                )
            };
            world.pte.store(0, Relaxed);
            world
                .shootdown
                .retire(
                    &near,
                    Arc::clone(&frame),
                    Request::page(THE_PAGE),
                    Scope::Space(THE_SPACE),
                    |frame| reuse(&frame),
                )
                .expect(NO_ANSWER);
            assert!(!user.holds(), "{STALE}");
            world.finish(vec![running]);
        },
    );
}

/// A frame shared between two spaces: what each mapping holds of it, counted.
struct SharedModelFrame {
    frame: ModelFrame,
    mappings: RefCount,
    frees: AtomicU64,
}

impl SharedModelFrame {
    /// A mapping is gone: the last one frees the frame.
    fn release(&self) {
        if self.mappings.release() {
            reuse(&self.frame);
            self.frees.fetch_add(1, Relaxed);
        }
    }
}

/// One frame mapped in two address spaces. A processor in the first holds its translation and
/// touches the page; two others unmap it, one from each space, at once. The frame is freed
/// exactly once, after the last access, and only one of the two unmaps can be the one to do it.
#[test]
fn deferred_free_of_a_frame_shared_by_two_spaces() {
    check(
        "deferred_free_of_a_frame_shared_by_two_spaces",
        preempted(3, SPINS),
        preempted(4, SPINS),
        || {
            let world = TlbWorld::new();
            let (first, second, user) =
                (tlb_cpu(&world, 0), tlb_cpu(&world, 1), tlb_cpu(&world, 2));
            for cpu in [&first, &second, &user] {
                world.shootdown.mark_online(cpu);
            }
            user.switch_to(THE_SPACE, || ());
            world.tlb[2].store(true, Relaxed);
            let shared = Arc::new(SharedModelFrame {
                frame: ModelFrame::new(0),
                mappings: RefCount::one(),
                frees: AtomicU64::new(0),
            });
            shared.mappings.retain();
            let running = {
                let (me, shared) = (user.clone(), Arc::clone(&shared));
                (
                    2,
                    thread::spawn(move || {
                        me.touch(&shared.frame);
                        me.take_interrupt_then_touch(&shared.frame);
                        me.serve();
                    }),
                )
            };
            let unmap_second = {
                let (me, shared) = (second.clone(), Arc::clone(&shared));
                thread::spawn(move || {
                    let world = Arc::clone(&me.world);
                    world
                        .shootdown
                        .retire(
                            &me,
                            Arc::clone(&shared),
                            Request::page(THE_PAGE),
                            Scope::Space(OTHER_SPACE),
                            |shared| shared.release(),
                        )
                        .expect(NO_ANSWER);
                })
            };
            world.pte.store(0, Relaxed);
            world
                .shootdown
                .retire(
                    &first,
                    Arc::clone(&shared),
                    Request::page(THE_PAGE),
                    Scope::Space(THE_SPACE),
                    |shared| shared.release(),
                )
                .expect(NO_ANSWER);
            unmap_second.join().unwrap();
            world.finish(vec![running]);
            assert_eq!(
                shared.frees.load(Relaxed),
                1,
                "the frame was not freed once"
            );
        },
    );
}

// ---------------------------------------------------------------- a switch, interrupted

/// A [`TlbCpu`] whose interrupt flag is real: `without_interrupts` holds its processor's lock,
/// and whatever stands for an interrupt on that processor takes the same lock to run. So an
/// interrupt cannot land inside an interrupts-off section, as on the machine.
struct IrqCpu(TlbCpu);

impl TlbHardware for IrqCpu {
    fn this_cpu(&self) -> usize {
        self.0.this_cpu()
    }

    fn invalidate(&self, request: Request) {
        self.0.invalidate(request);
    }

    fn send_ipi(&self, cpu: usize) {
        self.0.send_ipi(cpu);
    }

    fn without_interrupts<R>(&self, f: impl FnOnce() -> R) -> R {
        let _off = self.0.world.interrupts[self.0.id].lock();
        f()
    }

    fn now(&self) -> u64 {
        self.0.now()
    }

    fn patience(&self) -> u64 {
        self.0.patience()
    }

    fn current_root(&self) -> u64 {
        self.0.current_root()
    }
}

/// What the record says when it disagrees with CR3.
const RECORD_WRONG: &str = "the record names a space this processor has left";

/// A thread on processor 1 switches to one space while the scheduler, in an interrupt on the
/// same processor, switches it to another (review finding 1). Whichever ends last decides CR3,
/// and the record must name that space and no other: otherwise a shootdown would pass the
/// processor over, and a space it runs on could be freed.
#[test]
fn a_switch_and_the_scheduler_on_one_processor() {
    check(
        "a_switch_and_the_scheduler_on_one_processor",
        EXHAUSTIVE,
        EXHAUSTIVE,
        || {
            let world = TlbWorld::new();
            let cpu = |to: u64| {
                let me = IrqCpu(tlb_cpu(&world, 1));
                move || {
                    world_switch(&me, to);
                }
            };
            let scheduler = thread::spawn(cpu(OTHER_SPACE));
            cpu(THE_SPACE)();
            scheduler.join().unwrap();
            let now = world.cr3[1].load(Relaxed);
            let left = if now == THE_SPACE {
                OTHER_SPACE
            } else {
                THE_SPACE
            };
            assert_eq!(world.shootdown.processors_holding(now), 1, "{RECORD_WRONG}");
            assert_eq!(
                world.shootdown.processors_holding(left),
                0,
                "{RECORD_WRONG}"
            );
            assert_eq!(
                world.shootdown.processors_holding(KERNEL_SPACE),
                0,
                "{RECORD_WRONG}"
            );
        },
    );
}

/// Switches `me` to `to`, its CR3 write storing the new root.
fn world_switch(me: &IrqCpu, to: u64) {
    me.0.world.shootdown.switch_space(me, to, || {
        me.0.world.cr3[me.0.id].store(to, Relaxed);
    });
}
