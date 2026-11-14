//! Preemptive kernel threads on every processor (M3, step 2; CLAUDE.md §25 true preemption
//! and parallel execution, §26 per-CPU run queues and preemption).
//!
//! A thread's saved context is its interrupt frame on its own stack (the layout
//! `cpu::exception_common` pushes). Each processor's timer interrupt asks that processor's
//! [`RunQueue`] which frame to resume, so switching is the common stub loading another
//! stack pointer. A new thread starts from a frame built here: its registers zero except
//! RDI (the argument), RIP at its entry, RFLAGS with interrupts enabled, RSP at the top of
//! its own heap stack.
//!
//! Locking: each queue has its own spin lock, taken by that processor's timer interrupt and
//! by [`spawn`] from any processor. `spawn` holds it with interrupts disabled, so the
//! processor's own timer can never spin on a lock it interrupted.
//!
//! Queues are indexed by CPU index (`cpu::CpuIndex`, ADR-0008), never by APIC ID; a
//! reschedule IPI goes to the APIC ID recorded for the target's index.
#![forbid(unsafe_code)]

use core::fmt::Write;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::bootproof::{Proof, Tally};
use crate::runqueue::{QueueError, RunQueue, per_mille, share_near_weight};

use crate::console::Console;
use crate::cpu::{self, CpuIndex, rdtsc};
use crate::process::UserContext;
use crate::timer::{self, X2Apic};
use origin_frame::objref::ObjectRef;
use origin_frame::percpu::PerCpu;
use origin_frame::stack::{FpuArea, GuardedStack, ThreadStart};
use origin_frame::sync::SpinLock;
use origin_frame::trap::Switch;

/// Each kernel thread's stack.
pub(crate) const THREAD_STACK: usize = 16 * 1024;

/// Each processor's run queue, by CPU index (ADR-0008 Amendment 1, D1: sized at discovery).
pub(crate) static QUEUES: PerCpu<SpinLock<RunQueue>> = PerCpu::new(
    "framekernel::sched::QUEUES",
    SpinLock::new(RunQueue::new()),
    || SpinLock::new(RunQueue::new()),
);
/// Each processor whose scheduler runs.
pub(crate) static ACTIVE: PerCpu<AtomicBool> =
    PerCpu::new("framekernel::sched::ACTIVE", AtomicBool::new(false), || {
        AtomicBool::new(false)
    });
/// Set by the boot processor once every AP's threads are queued.
static GO: AtomicBool = AtomicBool::new(false);

/// This processor's CPU index, as a queue index (from its GS-based CPU-local area).
fn this_cpu() -> usize {
    cpu::this_cpu().get()
}

use crate::cpu::without_interrupts;

/// The timer interrupted the context whose frame is at `interrupted`; returns the frame to
/// resume. Processors without an active queue keep running what they were running.
pub(crate) fn on_tick(interrupted: u64, switch: &Switch) -> u64 {
    with_queue(interrupted, switch, |q| Some(q.tick(interrupted)))
}

/// Makes sure CPU `cpu`'s boot/idle context (slot 0) has an FPU area, so the first switch
/// away from it can save its state. Called before the processor's scheduler runs.
pub(crate) fn init_cpu_fpu(cpu: CpuIndex) {
    if let Some(threads) = THREADS.get(cpu.get()) {
        without_interrupts(|| {
            let mut t = threads.lock();
            if t.fpu[0].is_none() {
                t.fpu[0] = FpuArea::new();
            }
        });
    }
}

/// Saves the outgoing context's FPU/SIMD state and restores the incoming one's, so SIMD
/// registers belong to one thread at a time. An exiting thread's area is already gone (moved
/// to its zombie), so its dead state is not saved.
fn switch_fpu(id: usize, old: usize, new: usize) {
    let Some(threads) = THREADS.get(id) else {
        return;
    };
    let mut t = threads.lock();
    if let Some(area) = t.fpu.get_mut(old).and_then(Option::as_mut) {
        area.save();
    }
    if let Some(area) = t.fpu.get(new).and_then(Option::as_ref) {
        area.restore();
    }
}

/// Prepares CPU `id` to resume the thread in `slot`: TSS.RSP0 points at its kernel stack, so
/// an interrupt from user mode always lands on the running thread's own stack (the boot or
/// idle flow, with no stack of its own, never runs in user mode), and the page tables are its
/// process's (the kernel's for a kernel thread).
fn follow_thread(id: usize, slot: usize, switch: &Switch) {
    let Some(threads) = THREADS.get(id) else {
        return;
    };
    let t = threads.lock();
    match t.stacks.get(slot).and_then(Option::as_ref) {
        Some(stack) => cpu::set_kernel_stack(stack),
        // Every thread but the boot or idle flow has a stack: a slot without one means a
        // thread was made runnable before its state was written, which `spawn_with` rules
        // out by writing it under the queue lock. Counted for the boot proof.
        None if slot != 0 => {
            UNREADY_SWITCHES.fetch_add(1, Ordering::Relaxed);
        }
        // The boot or idle flow has no stack of its own: no thread's stack is current here
        // any more, and an exited thread's stack can be freed.
        None => cpu::clear_kernel_stack(),
    }
    match t.user.get(slot).and_then(Option::as_ref) {
        Some(user) => {
            // ADR-0017 Z3: a barrier, if this processor last ran another process in ring 3.
            cpu::entering_process(user.process.id(), switch);
            user.space.activate(switch);
        }
        None => crate::vm::activate_kernel_space(switch),
    }
}

/// The guard page of the first thread `spawn` created, for the boot proof.
static FIRST_GUARD: AtomicU64 = AtomicU64::new(0);

/// A thread that has exited, awaiting [`reap`]: its stack, FPU area and, for a user
/// thread, the references that keep its process and address space until this processor has left
/// them.
struct Zombie {
    stack: GuardedStack,
    fpu: Option<FpuArea>,
    user: Option<ObjectRef<UserContext>>,
}

/// Per CPU: each slot's stack, and the stacks of threads that exited and await
/// [`reap`]. Locked after the run queue, always with interrupts disabled.
pub(crate) struct Threads {
    stacks: [Option<GuardedStack>; crate::runqueue::CAPACITY],
    /// Exited threads awaiting [`reap`].
    zombies: [Option<Zombie>; crate::runqueue::CAPACITY],
    /// Each slot's FPU/SIMD save area; slot 0 is the boot or idle context.
    fpu: [Option<FpuArea>; crate::runqueue::CAPACITY],
    /// What each slot's thread runs in (`None`: a kernel thread).
    user: [Option<ObjectRef<UserContext>>; crate::runqueue::CAPACITY],
}

impl Threads {
    /// No thread.
    const fn new() -> Self {
        Self {
            stacks: [const { None }; crate::runqueue::CAPACITY],
            zombies: [const { None }; crate::runqueue::CAPACITY],
            fpu: [const { None }; crate::runqueue::CAPACITY],
            user: [const { None }; crate::runqueue::CAPACITY],
        }
    }
}

/// Each processor's threads, by CPU index.
pub(crate) static THREADS: PerCpu<SpinLock<Threads>> = PerCpu::new(
    "framekernel::sched::THREADS",
    SpinLock::new(Threads::new()),
    || SpinLock::new(Threads::new()),
);

/// Times a processor picked a thread whose state was not complete (never, if the queue
/// lock is held as it must be; the boot proof requires 0).
static UNREADY_SWITCHES: AtomicU64 = AtomicU64::new(0);

/// How many switches have found a thread with no recorded stack.
pub(crate) fn unready_switches() -> u64 {
    UNREADY_SWITCHES.load(Ordering::Relaxed)
}

/// Threads that exited on each processor and await [`reap`], so a processor can tell cheaply
/// that it has none.
pub(crate) static ZOMBIES: PerCpu<AtomicU64> =
    PerCpu::new("framekernel::sched::ZOMBIES", AtomicU64::new(0), || {
        AtomicU64::new(0)
    });

/// Exited threads that await [`reap`] on any processor: for a proof that needs every stack
/// given back before it counts them (`stacks`).
pub(crate) fn zombies() -> u64 {
    ZOMBIES.iter().map(|z| z.load(Ordering::Acquire)).sum()
}

/// Frees the stacks of threads that exited on **this** processor. Only this processor may
/// do it, and only from thread context: by then its exit handler, which ran on the dead
/// stack, has certainly switched away. Returns how many stacks were freed.
///
/// Then it gives back what retired stacks hold (`stack::reclaim`): here, with no lock held, as
/// the shootdown that must come first waits for every processor (ADR-0004 Amendment 1, A2).
pub(crate) fn reap() -> usize {
    let freed = reap_zombies();
    origin_frame::stack::reclaim();
    freed
}

/// [`reap`]'s own processor's exited threads.
fn reap_zombies() -> usize {
    let Some(threads) = THREADS.get(this_cpu()) else {
        return 0;
    };
    if ZOMBIES
        .get(this_cpu())
        .is_some_and(|z| z.load(Ordering::Acquire) == 0)
    {
        return 0;
    }
    let mut freed = 0;
    loop {
        let next = without_interrupts(|| threads.lock().zombies.iter_mut().find_map(Option::take));
        let Some(Zombie { stack, fpu, user }) = next else {
            return freed;
        };
        if let Some(z) = ZOMBIES.get(this_cpu()) {
            z.fetch_sub(1, Ordering::AcqRel);
        }
        drop(fpu);
        drop(stack);
        if let Some(user) = user {
            let process = user.process.clone();
            // Possibly the last reference to the address space, which frees it: this processor
            // left it when the thread exited, and no processor still runs in it.
            drop(user);
            // After the memory, so that whoever sees `TERMINATED` knows it has gone (P11).
            process.thread_reaped();
        }
        freed += 1;
    }
}

/// The running thread's queue request, made with interrupts disabled (the gate cleared IF).
fn with_queue(
    interrupted: u64,
    switch: &Switch,
    f: impl FnOnce(&mut RunQueue) -> Option<u64>,
) -> u64 {
    let id = this_cpu();
    match (ACTIVE.get(id), QUEUES.get(id)) {
        (Some(active), Some(queue)) if active.load(Ordering::Acquire) => {
            let mut q = queue.lock();
            let old = q.current();
            let next = f(&mut q).unwrap_or(interrupted);
            let new = q.current();
            if new != old {
                switch_fpu(id, old, new);
            }
            follow_thread(id, new, switch);
            next
        }
        _ => interrupted,
    }
}

/// `YIELD_VECTOR`: the running context gives up the processor.
pub(crate) fn on_yield(interrupted: u64, switch: &Switch) -> u64 {
    with_queue(interrupted, switch, |q| Some(q.yield_now(interrupted)))
}

/// `SLEEP_VECTOR`: the running thread sleeps for `ticks` timer ticks.
pub(crate) fn on_sleep(interrupted: u64, ticks: u64, switch: &Switch) -> u64 {
    with_queue(interrupted, switch, |q| q.sleep(interrupted, ticks).ok())
}

/// `BLOCK_VECTOR`: the running thread blocks until [`wake`] (or goes on, if a wake-up is
/// already pending).
/// RDI holds the most ticks to wait (0: no limit).
pub(crate) fn on_block(interrupted: u64, ticks: u64, switch: &Switch) -> u64 {
    with_queue(interrupted, switch, |q| {
        q.block_for(interrupted, (ticks != 0).then_some(ticks)).ok()
    })
}

/// A device interrupt whose driver may have woken a waiter, or `RESCHEDULE_VECTOR` (another
/// processor made a thread here ready). The interrupted context is charged nothing: this
/// switches only if a thread that was woken should run now
/// ([`RunQueue::preempt_for_wake`]), and otherwise resumes what the interrupt interrupted.
pub(crate) fn on_reschedule(interrupted: u64, switch: &Switch) -> u64 {
    with_queue(interrupted, switch, |q| {
        Some(q.preempt_for_wake(interrupted))
    })
}

/// A kernel thread: its processor (CPU index), run-queue slot, and the slot's generation,
/// so a reference that outlives its thread never names the slot's next one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ThreadRef {
    cpu: CpuIndex,
    slot: u32,
    generation: u32,
}

/// The running kernel thread, or `None` for the boot or idle flow (which cannot block).
pub(crate) fn current_thread() -> Option<ThreadRef> {
    let cpu = cpu::this_cpu();
    let queue = QUEUES.get(cpu.get())?;
    without_interrupts(|| {
        let q = queue.lock();
        let slot = q.current();
        (slot != 0).then(|| ThreadRef {
            cpu,
            slot: slot as u32,
            generation: q.generation(slot),
        })
    })
}

/// Blocks the running kernel thread until [`wake`]. Returns at once if a wake-up arrived
/// first. Use it through `wait::WaitQueue`, which makes sure the wake-up comes.
pub(crate) fn block() {
    cpu::thread_block(0);
}

/// Nanoseconds per timer tick.
const TICK_NANOS: u64 = 1_000_000_000 / crate::timer::HZ;

/// As [`block`], but also returns once the monotonic clock reaches `deadline` (in
/// nanoseconds), to within one timer tick (10 ms). The caller tells the two apart by
/// checking what it waited for.
pub(crate) fn block_until(deadline: u64) {
    let left = deadline.saturating_sub(crate::time::monotonic_nanos());
    let ticks = left.div_ceil(TICK_NANOS).max(1);
    cpu::thread_block(ticks);
}

/// Makes `thread` ready. If it was blocked on another processor, that processor is sent a
/// reschedule IPI (to its APIC ID), so it switches to the thread now rather than at its
/// next timer tick. Returns whether an IPI was sent, so a wake-up across processors can be
/// counted.
pub(crate) fn wake(thread: ThreadRef) -> bool {
    let Some(queue) = QUEUES.get(thread.cpu.get()) else {
        return false;
    };
    let woke = without_interrupts(|| queue.lock().wake(thread.slot as usize, thread.generation));
    if woke
        && thread.cpu != cpu::this_cpu()
        && let Some(apic_id) = thread.cpu.apic_id()
    {
        timer::send_reschedule_ipi(apic_id);
        return true;
    }
    false
}

/// `EXIT_VECTOR`: the running thread ends. Its stack becomes a zombie, freed later by
/// [`reap`] on this processor.
pub(crate) fn on_exit(interrupted: u64, switch: &Switch) -> u64 {
    let id = this_cpu();
    with_queue(interrupted, switch, |q| {
        let slot = q.current();
        let next = q.exit(interrupted).ok()?;
        if let Some(threads) = THREADS.get(id) {
            let mut t = threads.lock();
            let stack = t.stacks.get_mut(slot).and_then(Option::take);
            let fpu = t.fpu.get_mut(slot).and_then(Option::take);
            let user = t.user.get_mut(slot).and_then(Option::take);
            match (t.zombies.iter_mut().find(|z| z.is_none()), stack) {
                (Some(free), Some(stack)) => {
                    *free = Some(Zombie { stack, fpu, user });
                    if let Some(z) = ZOMBIES.get(id) {
                        z.fetch_add(1, Ordering::AcqRel);
                    }
                }
                // No record of it can be kept. Its references are leaked deliberately: this
                // processor is still in the process's address space, and dropping the last
                // reference here would free the tables it is running on.
                _ => core::mem::forget(user),
            }
        }
        Some(next)
    })
}

/// Gives up the processor to the next ready context on this CPU.
pub(crate) fn yield_now() {
    cpu::thread_yield();
}

/// Sleeps for `ticks` timer ticks of this CPU (at least one).
pub(crate) fn sleep(ticks: u64) {
    cpu::thread_sleep(ticks);
}

/// Ends the running thread.
pub(crate) fn exit() -> ! {
    cpu::thread_exit()
}

/// What the running thread runs in, or `None` for a kernel thread.
pub(crate) fn current_user() -> Option<ObjectRef<UserContext>> {
    let id = this_cpu();
    let (queue, threads) = (QUEUES.get(id)?, THREADS.get(id)?);
    without_interrupts(|| {
        let slot = queue.lock().current();
        threads.lock().user.get(slot).cloned().flatten()
    })
}

/// This CPU's tick count, as its run queue sees it.
fn queue_ticks() -> u64 {
    queue_ticks_of(this_cpu())
}

/// Processor `cpu`'s tick count, as its run queue sees it.
fn queue_ticks_of(cpu: usize) -> u64 {
    QUEUES
        .get(cpu)
        .map_or(0, |q| without_interrupts(|| q.lock().ticks()))
}

/// Creates a kernel thread on processor `cpu` that runs `entry(arg)`, and returns its
/// run-queue slot.
pub(crate) fn spawn(
    cpu: CpuIndex,
    entry: extern "sysv64" fn(u64) -> !,
    arg: u64,
) -> Result<usize, QueueError> {
    spawn_with(
        cpu,
        ThreadStart::Kernel { entry, arg },
        crate::runqueue::DEFAULT_WEIGHT,
        None,
    )
}

/// Creates a user thread in `user` on processor `cpu`: it enters ring 3 at `rip` with the
/// stack pointer `rsp` and `rdi` in RDI, every other register zero, the first time it is
/// scheduled (there is no kernel half to the thread until it enters the kernel).
pub(crate) fn spawn_user(
    cpu: CpuIndex,
    user: ObjectRef<UserContext>,
    rip: u64,
    rsp: u64,
    rdi: u64,
) -> Result<usize, QueueError> {
    spawn_with(
        cpu,
        ThreadStart::User { rip, rsp, rdi },
        crate::runqueue::DEFAULT_WEIGHT,
        Some(user),
    )
}

/// As [`spawn`], with a scheduling weight (stride scheduling: the thread's share of its
/// processor is its share of the weights there).
pub(crate) fn spawn_weighted(
    cpu: CpuIndex,
    entry: extern "sysv64" fn(u64) -> !,
    arg: u64,
    weight: u32,
) -> Result<usize, QueueError> {
    spawn_with(cpu, ThreadStart::Kernel { entry, arg }, weight, None)
}

fn spawn_with(
    cpu: CpuIndex,
    start: ThreadStart,
    weight: u32,
    user: Option<ObjectRef<UserContext>>,
) -> Result<usize, QueueError> {
    spawn_with_fpu(cpu, start, weight, user, FpuArea::new)
}

/// [`spawn_with`], with `fpu_area` the source of the thread's FPU area (the boot proof gives
/// it one that has none). A thread is created whole or not at all: every way this fails
/// gives back the stack, its guard page and the FPU area, through their `Drop`s.
fn spawn_with_fpu(
    cpu: CpuIndex,
    start: ThreadStart,
    weight: u32,
    user: Option<ObjectRef<UserContext>>,
    fpu_area: impl FnOnce() -> Option<FpuArea>,
) -> Result<usize, QueueError> {
    let queue = QUEUES.get(cpu.get()).ok_or(QueueError::Full)?;
    let mut stack = GuardedStack::new(THREAD_STACK).ok_or(QueueError::Full)?;
    let guard = stack.guard_addr();
    let frame_at = stack.build_first_frame(start).ok_or(QueueError::Full)?;
    // A thread without its own FPU save area would run on the previous thread's registers
    // (`switch_fpu` then has nothing to restore), which may be another process's data; it
    // is refused instead.
    let fpu_area = fpu_area().ok_or(QueueError::Full)?;
    let spawned = without_interrupts(|| {
        // The queue lock is held until the thread's state is complete: the moment the slot
        // is Ready the target processor can pick it, and `follow_thread` needs the state.
        let mut queue = queue.lock();
        let mut stack = stack;
        let slot = queue.push_weighted(frame_at, weight)?;
        // Published before the slot can be picked: the lock is held until the thread is whole.
        if !stack.publish_first_frame(cpu) {
            return Err(QueueError::Full);
        }
        if let Some(threads) = THREADS.get(cpu.get()) {
            let mut t = threads.lock();
            if let Some(s) = t.stacks.get_mut(slot) {
                *s = Some(stack);
            }
            if let Some(f) = t.fpu.get_mut(slot) {
                *f = Some(fpu_area);
            }
            if let Some(u) = t.user.get_mut(slot) {
                *u = user;
            }
        }
        Ok(slot)
    });
    if spawned.is_ok() {
        let _ = FIRST_GUARD.compare_exchange(0, guard, Ordering::Relaxed, Ordering::Relaxed);
    }
    spawned
}

/// An application processor's scheduler: waits until the boot processor has queued its
/// threads, then starts its own timer and idles with interrupts on; the timer preempts the
/// idle loop to run the queue's threads.
pub(crate) fn run_ap(lapic: &X2Apic) -> ! {
    while !GO.load(Ordering::Acquire) {
        core::hint::spin_loop();
    }
    // The boot processor split large pages and unmapped guard pages while this processor
    // waited: from now on it is sent every shootdown, and it drops every older translation.
    // Without a record it could not be sent one: it stays out, interrupts off (review finding
    // F6 of 2026-11-14; `smp::ap_entry` refuses its start-up before this).
    if !crate::tlb::mark_online() {
        crate::cpu::halt();
    }
    // This flow is the processor's idle context: it runs only when no thread is ready.
    if let Some(queue) = QUEUES.get(this_cpu()) {
        without_interrupts(|| queue.lock().set_idle(true));
    }
    if let Some(active) = ACTIVE.get(this_cpu()) {
        active.store(true, Ordering::Release);
    }
    timer::start_periodic(lapic);
    loop {
        reap();
        // Halting with interrupts on just waits for the next timer tick, which may switch to
        // a thread.
        cpu::idle();
    }
}

/// The time each worker has seen itself run, in TSC ticks, indexed by worker: every step of
/// the TSC shorter than [`RUNNING_STEP`], like [`weighted_worker`]. Time, not iterations: on a
/// processor sharing a physical core, or an emulated one, how many iterations a slice holds
/// varies with what else is running, whatever the scheduler did.
/// Two workers for each processor, worker `slot` the `slot % 2`nd of the `slot / 2`th.
pub(crate) static PROGRESS: PerCpu<[AtomicU64; 2]> = PerCpu::new(
    "framekernel::sched::PROGRESS",
    [const { AtomicU64::new(0) }; 2],
    || [const { AtomicU64::new(0) }; 2],
);
/// The CPU index each worker last ran on, plus one (0: never ran), as [`PROGRESS`].
pub(crate) static RAN_ON: PerCpu<[AtomicU64; 2]> = PerCpu::new(
    "framekernel::sched::RAN_ON",
    [const { AtomicU64::new(0) }; 2],
    || [const { AtomicU64::new(0) }; 2],
);

/// Worker `slot`'s entry of `table` ([`PROGRESS`] or [`RAN_ON`]).
fn worker_entry(table: &PerCpu<[AtomicU64; 2]>, slot: usize) -> Option<&AtomicU64> {
    table.get(slot / 2)?.get(slot % 2)
}
/// Tells the workers to stop counting.
static STOP: AtomicBool = AtomicBool::new(false);
/// Workers that have stopped and exited.
static EXITED: AtomicU64 = AtomicU64::new(0);

/// A worker that never yields: until [`STOP`] it adds up the time it sees itself run, so it
/// only ever gives up its processor when the timer preempts it.
extern "sysv64" fn worker(slot: u64) -> ! {
    let slot = slot as usize;
    let limit = RUNNING_STEP.load(Ordering::Relaxed);
    let mut last = rdtsc();
    while !STOP.load(Ordering::Relaxed) {
        let now = rdtsc();
        let step = now.wrapping_sub(last);
        last = now;
        if let (Some(p), Some(r)) = (worker_entry(&PROGRESS, slot), worker_entry(&RAN_ON, slot)) {
            if step < limit {
                p.fetch_add(step, Ordering::Relaxed);
            }
            r.store(this_cpu() as u64 + 1, Ordering::Relaxed);
        }
        core::hint::spin_loop();
    }
    EXITED.fetch_add(1, Ordering::Relaxed);
    exit()
}

/// The last pinger to take a turn, and how often the turn changed hands.
static LAST_TURN: AtomicU64 = AtomicU64::new(0);
static ALTERNATIONS: AtomicU64 = AtomicU64::new(0);
/// Lifecycle threads finished.
static FINISHED: AtomicU64 = AtomicU64::new(0);
/// Ticks the sleeper measured across `sleep(SLEEP_TICKS)`, plus one (0: not yet).
static SLEPT: AtomicU64 = AtomicU64::new(0);
const TURNS: u64 = 50;
const SLEEP_TICKS: u64 = 5;

/// Takes [`TURNS`] turns, yielding after each.
extern "sysv64" fn pinger(me: u64) -> ! {
    for _ in 0..TURNS {
        if LAST_TURN.swap(me, Ordering::Relaxed) != me {
            ALTERNATIONS.fetch_add(1, Ordering::Relaxed);
        }
        yield_now();
    }
    FINISHED.fetch_add(1, Ordering::Relaxed);
    exit()
}

/// Sleeps [`SLEEP_TICKS`] and records how long that took in ticks.
extern "sysv64" fn sleeper(_: u64) -> ! {
    let before = queue_ticks();
    sleep(SLEEP_TICKS);
    SLEPT.store(queue_ticks() - before + 1, Ordering::Relaxed);
    FINISHED.fetch_add(1, Ordering::Relaxed);
    exit()
}

/// Tells the benchmark threads to exit.
static BENCH_STOP: AtomicBool = AtomicBool::new(false);
/// Yield round trips the boot flow makes in the benchmark (each passes two threads).
const BENCH_ROUNDS: u64 = 2000;

/// Yields until [`BENCH_STOP`], then exits.
extern "sysv64" fn bencher(_: u64) -> ! {
    while !BENCH_STOP.load(Ordering::Relaxed) {
        yield_now();
    }
    FINISHED.fetch_add(1, Ordering::Relaxed);
    exit()
}

/// TSC cycles per context switch: the boot flow and two threads that only yield, so each
/// boot-flow yield is three switches (boot → A → B → boot).
fn measure_switch(bsp: CpuIndex) -> Option<u64> {
    let before = FINISHED.load(Ordering::Relaxed);
    spawn(bsp, bencher, 0).ok()?;
    spawn(bsp, bencher, 1).ok()?;
    yield_now(); // let both start
    let start = rdtsc();
    for _ in 0..BENCH_ROUNDS {
        yield_now();
    }
    let cycles = rdtsc().wrapping_sub(start);
    BENCH_STOP.store(true, Ordering::Relaxed);
    while FINISHED.load(Ordering::Relaxed) < before + 2 {
        yield_now();
    }
    reap();
    Some(cycles / (3 * BENCH_ROUNDS))
}

/// Exits at once.
extern "sysv64" fn quitter(_: u64) -> ! {
    FINISHED.fetch_add(1, Ordering::Relaxed);
    exit()
}

/// Yields until `done()` or `seconds` pass.
///
/// The boot flow runs with interrupts disabled, so yielding alone would never let a timer
/// interrupt in: when it is the only ready context, it would yield to itself for ever and
/// time (and every sleeper) would stand still. Each round therefore also opens an interrupt
/// window.
pub(crate) fn yield_until(done: impl Fn() -> bool, tsc_hz: u64, seconds: u64) {
    let _ = yield_until_away(done, tsc_hz, seconds);
}

/// As [`yield_until`], and returns how long the boot flow went without a turn while it
/// waited, in TSC ticks: the sum of the steps of this loop of at least
/// [`RUNNING_STEP_MICROS`]. A step that long means the virtual CPU was descheduled (a loaded
/// host) or that another thread on this processor ran for that long, and this loop cannot
/// tell which. So the result is only a hint: it may earn a repeat of a measurement, never
/// a more lenient verdict.
pub(crate) fn yield_until_away(done: impl Fn() -> bool, tsc_hz: u64, seconds: u64) -> u64 {
    let limit = (tsc_hz / 1_000_000).max(1) * RUNNING_STEP_MICROS;
    let start = rdtsc();
    let mut last = start;
    let mut away = 0;
    while !done() && rdtsc().wrapping_sub(start) < tsc_hz.saturating_mul(seconds) {
        yield_now();
        cpu::interrupt_window();
        reap();
        let now = rdtsc();
        let step = now.wrapping_sub(last);
        last = now;
        if step >= limit {
            away += step;
        }
    }
    away
}

/// SIMD workers for [`prove_simd`].
const SIMD_WORKERS: usize = 3;
static SIMD_STOP: AtomicBool = AtomicBool::new(false);
static SIMD_CORRUPT: [AtomicU64; SIMD_WORKERS] = [const { AtomicU64::new(0) }; SIMD_WORKERS];
static SIMD_ITERS: [AtomicU64; SIMD_WORKERS] = [const { AtomicU64::new(0) }; SIMD_WORKERS];
static SIMD_EXITED: AtomicU64 = AtomicU64::new(0);

/// A worker that loads four distinct 64-bit values into spread-out XMM registers once,
/// then spins checking they are unchanged. If a context switch did not save and restore
/// SIMD state, another worker's values would appear here.
extern "sysv64" fn simd_worker(index: u64) -> ! {
    let v = [
        index.wrapping_add(0x1111_1111_0000_0001),
        !index,
        index.rotate_left(23) ^ 0xA5A5_A5A5,
        index.wrapping_mul(0x9E37_79B9_7F4A_7C15),
    ];
    cpu::xmm_load(v);
    while !SIMD_STOP.load(Ordering::Relaxed) {
        let [a, b, c, d] = cpu::xmm_read();
        if [a, b, c, d] != v
            && let Some(cnt) = SIMD_CORRUPT.get(index as usize)
        {
            cnt.fetch_add(1, Ordering::Relaxed);
        }
        if let Some(it) = SIMD_ITERS.get(index as usize) {
            it.fetch_add(1, Ordering::Relaxed);
        }
        core::hint::spin_loop();
    }
    SIMD_EXITED.fetch_add(1, Ordering::Relaxed);
    exit()
}

/// Proves that FPU/SIMD state is private to each thread across preemption: [`SIMD_WORKERS`]
/// workers on one processor each hold four XMM values and spin checking them for 300 ms
/// while the timer preempts them. None may ever see another's values.
fn prove_simd(com1: &mut Console, bsp: CpuIndex, tsc_hz: u64) -> bool {
    for i in 0..SIMD_WORKERS {
        if spawn(bsp, simd_worker, i as u64).is_err() {
            let _ = writeln!(
                com1,
                "origin-kernel: scheduler: cannot spawn: scheduler failed"
            );
            return false;
        }
    }
    let start = rdtsc();
    while rdtsc().wrapping_sub(start) < tsc_hz * 3 / 10 {
        yield_now();
        cpu::interrupt_window();
    }
    SIMD_STOP.store(true, Ordering::Relaxed);
    yield_until(
        || SIMD_EXITED.load(Ordering::Relaxed) == SIMD_WORKERS as u64,
        tsc_hz,
        2,
    );
    let corrupt: u64 = SIMD_CORRUPT.iter().map(|c| c.load(Ordering::Relaxed)).sum();
    let iters: u64 = SIMD_ITERS.iter().map(|c| c.load(Ordering::Relaxed)).sum();
    let switches = QUEUES
        .get(bsp.get())
        .map_or(0, |q| without_interrupts(|| q.lock().switches()));
    let ok =
        SIMD_EXITED.load(Ordering::Relaxed) == SIMD_WORKERS as u64 && corrupt == 0 && iters > 0;
    let _ = writeln!(
        com1,
        "origin-kernel: scheduler: {SIMD_WORKERS} SIMD threads kept their XMM state across {switches} context switches: {iters} checks, {corrupt} corruptions{}",
        if ok { "" } else { ": scheduler failed" }
    );
    ok
}

/// The weights of the three workers of [`prove_weights`].
const WEIGHTS: [u32; 3] = [1, 4, 16];
/// TSC ticks each weighted worker saw itself run (see [`weighted_worker`]).
static WEIGHTED: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];
static WEIGHTED_STOP: AtomicBool = AtomicBool::new(false);
static WEIGHTED_EXITED: AtomicU64 = AtomicU64::new(0);
/// A step between two of a worker's clock readings counts as time it ran only if it is
/// shorter than this: a longer one means it was preempted or its virtual CPU was away.
/// Three milliseconds, about a third of a timer tick: a loaded host takes a virtual CPU away for
/// milliseconds many times a second, and a limit much below this would count only the
/// luck of when those stalls fell (measured, 100 µs: twice the noise of 3 ms), while a
/// thread preempted by another's slice is gone for a whole 10 ms tick.
const RUNNING_STEP_MICROS: u64 = 3000;
/// [`RUNNING_STEP_MICROS`] in TSC ticks, set before the workers start.
static RUNNING_STEP: AtomicU64 = AtomicU64::new(0);

/// A worker that never yields. Until told to stop, it adds to `WEIGHTED[index]` the time it
/// sees itself running: every step of the TSC shorter than [`RUNNING_STEP`]. Time, not
/// iterations: on an emulated processor the iterations done in one 10 ms slice vary by an
/// order of magnitude with the host's load, whatever the scheduler did, and the lightest
/// thread's share is the sum of only about ten slices.
extern "sysv64" fn weighted_worker(index: u64) -> ! {
    let limit = RUNNING_STEP.load(Ordering::Relaxed);
    let mut last = rdtsc();
    while !WEIGHTED_STOP.load(Ordering::Relaxed) {
        let now = rdtsc();
        let step = now.wrapping_sub(last);
        last = now;
        if step < limit
            && let Some(ran) = WEIGHTED.get(index as usize)
        {
            ran.fetch_add(step, Ordering::Relaxed);
        }
        core::hint::spin_loop();
    }
    WEIGHTED_EXITED.fetch_add(1, Ordering::Relaxed);
    exit()
}

/// How long the workers of [`prove_weights`] share the processor, in seconds.
const WEIGHTS_SECONDS: u64 = 2;
/// Runs [`prove_weights`] may make: an extra one only after a wrong result from a visibly
/// disturbed run (see [`TICKS_LOST_TO_A_STALL`]).
const WEIGHTS_ATTEMPTS: u32 = 3;
/// A run counts as disturbed if its processor took at least this many ticks fewer than the
/// window holds, because a late timer interrupt coalesced with the next (on a loaded host,
/// the virtual CPU or QEMU's main loop was descheduled); one or two can go missing to the
/// window's phase and the calibration's rounding. It also counts as disturbed if the
/// workers saw themselves run for less than nine tenths of the window: their processor was
/// away for the rest of it.
const TICKS_LOST_TO_A_STALL: u64 = 3;

/// One run of the workers of [`prove_weights`].
struct WeightedRun {
    /// The time each worker saw itself run, in TSC ticks.
    ran: [u64; 3],
    exited: bool,
    /// Timer ticks the workers' processor took in the window.
    ticks: u64,
}

/// Starts the three weighted workers on `far`, lets them run for [`WEIGHTS_SECONDS`], stops
/// them and returns what they measured, or `None` if a worker could not be spawned.
fn run_weighted(far: CpuIndex, tsc_hz: u64) -> Option<WeightedRun> {
    RUNNING_STEP.store(
        (tsc_hz / 1_000_000).max(1) * RUNNING_STEP_MICROS,
        Ordering::Relaxed,
    );
    for ran in &WEIGHTED {
        ran.store(0, Ordering::Relaxed);
    }
    WEIGHTED_STOP.store(false, Ordering::Relaxed);
    WEIGHTED_EXITED.store(0, Ordering::Relaxed);
    for (i, &w) in WEIGHTS.iter().enumerate() {
        spawn_weighted(far, weighted_worker, i as u64, w).ok()?;
    }
    let ticks_before = queue_ticks_of(far.get());
    let start = rdtsc();
    while rdtsc().wrapping_sub(start) < WEIGHTS_SECONDS * tsc_hz {
        yield_now();
        cpu::interrupt_window();
    }
    let ticks = queue_ticks_of(far.get()).wrapping_sub(ticks_before);
    WEIGHTED_STOP.store(true, Ordering::Relaxed);
    yield_until(|| WEIGHTED_EXITED.load(Ordering::Relaxed) == 3, tsc_hz, 2);
    Some(WeightedRun {
        ran: core::array::from_fn(|i| WEIGHTED[i].load(Ordering::Relaxed)),
        exited: WEIGHTED_EXITED.load(Ordering::Relaxed) == 3,
        ticks,
    })
}

/// Proves proportional shares (stride scheduling): three never-yielding workers with
/// weights 1, 4 and 16 share one processor for 2 s; each one's share of the time they ran
/// must be near its share of the weights (1/21, 4/21, 16/21), and the lightest must not
/// starve.
///
/// On a loaded host a late timer interrupt lets the running worker run on, and that alone
/// can swing the lightest thread's share. Such a run says nothing about the scheduler: if
/// the shares are wrong and the processor lost ticks or was away for over a tenth of the
/// window, it is measured again, within a small budget. A wrong result with the ticks all
/// there and the processor present fails at once.
fn prove_weights(com1: &mut Console, cpus: &[CpuIndex], bsp: CpuIndex, tsc_hz: u64) -> bool {
    let far = cpus.iter().copied().find(|&c| c != bsp).unwrap_or(bsp);
    let window_ticks = WEIGHTS_SECONDS * timer::HZ;
    let mut attempts = 0;
    let (shares, ran_ms, ok) = loop {
        attempts += 1;
        let Some(run) = run_weighted(far, tsc_hz) else {
            let _ = writeln!(
                com1,
                "origin-kernel: scheduler: cannot spawn: scheduler failed"
            );
            return false;
        };
        let total: u64 = run.ran.iter().sum();
        let sum_weights: u32 = WEIGHTS.iter().sum();
        let mut ok = run.exited && total > 0;
        let mut shares = [0u64; 3];
        for (i, (&ran, &w)) in run.ran.iter().zip(&WEIGHTS).enumerate() {
            let share = per_mille(ran, total);
            shares[i] = share;
            ok &= ran > 0 && share_near_weight(share, w, sum_weights);
        }
        let disturbed = run.ticks + TICKS_LOST_TO_A_STALL <= window_ticks
            || total.saturating_mul(10) < (WEIGHTS_SECONDS * tsc_hz).saturating_mul(9);
        if ok || !run.exited || !disturbed || attempts == WEIGHTS_ATTEMPTS {
            break (shares, total / (tsc_hz / 1000).max(1), ok);
        }
    };
    let _ = writeln!(
        com1,
        "origin-kernel: scheduler: weights 1, 4 and 16 on CPU {far} for {WEIGHTS_SECONDS} s: shares {}, {} and {} per mille (ideal 47, 190 and 761) over {ran_ms} ms of running{}{}",
        shares[0],
        shares[1],
        shares[2],
        if attempts > 1 {
            " (after a stalled run)"
        } else {
            ""
        },
        if ok { "" } else { ": scheduler failed" }
    );
    ok
}

/// On the boot processor, with its timer running: yields must alternate two threads, a
/// sleep must last its ticks, and exited threads must free their slots for reuse.
fn prove_lifecycle(com1: &mut Console, bsp: CpuIndex, tsc_hz: u64) -> bool {
    let live = || {
        QUEUES
            .get(bsp.get())
            .map_or(0, |q| without_interrupts(|| q.lock().live()))
    };
    if live() != 1 {
        let _ = writeln!(
            com1,
            "origin-kernel: CPU {bsp} still has threads: scheduler failed"
        );
        return false;
    }
    let spawned = [
        spawn(bsp, pinger, 1),
        spawn(bsp, pinger, 2),
        spawn(bsp, sleeper, 0),
    ];
    if spawned.iter().any(Result::is_err) {
        let _ = writeln!(com1, "origin-kernel: cannot spawn: scheduler failed");
        return false;
    }
    yield_until(
        || FINISHED.load(Ordering::Relaxed) == 3 && live() == 1,
        tsc_hz,
        3,
    );
    let reused = spawn(bsp, quitter, 0);
    yield_until(
        || FINISHED.load(Ordering::Relaxed) == 4 && live() == 1,
        tsc_hz,
        1,
    );
    if let Some(cycles) = measure_switch(bsp) {
        let _ = writeln!(
            com1,
            "origin-kernel: scheduler: {cycles} TSC cycles ({} ns) per context switch, yield ping-pong of 3 contexts",
            cycles * 1_000_000_000 / tsc_hz.max(1)
        );
    }
    let alternations = ALTERNATIONS.load(Ordering::Relaxed);
    let slept = SLEPT.load(Ordering::Relaxed).saturating_sub(1);
    let ok = alternations >= 2 * TURNS - 5
        && (SLEEP_TICKS..=10 * SLEEP_TICKS).contains(&slept)
        && reused == Ok(1)
        && live() == 1;
    let _ = writeln!(
        com1,
        "origin-kernel: scheduler: yield alternated {alternations} of {} turns; sleep({SLEEP_TICKS}) took {slept} ticks; exited threads freed their slots (slot {:?} reused){}",
        2 * TURNS,
        reused,
        if ok { "" } else { ": scheduler failed" }
    );
    ok
}

/// A spawn that cannot be completed leaves nothing behind: refused for want of an FPU area
/// (without one the thread would inherit another's SIMD registers), or because the queue
/// is full, it hands back the stack, its guard page and the FPU area. The processor must
/// hold only its boot context when this runs. (Anything a refusal leaks would also fail the
/// final check in [`prove`], which holds the heap to the byte.)
fn prove_spawn_refused(com1: &mut Console, bsp: CpuIndex, tsc_hz: u64) -> bool {
    let live = || {
        QUEUES
            .get(bsp.get())
            .map_or(0, |q| without_interrupts(|| q.lock().live()))
    };
    let in_use = || crate::memory::stats().map_or(0, |(h, _)| h.bytes_in_use);
    let started = FINISHED.load(Ordering::Relaxed);
    let contexts = live();
    let before = in_use();
    let no_area = spawn_with_fpu(
        bsp,
        ThreadStart::Kernel {
            entry: quitter,
            arg: 0,
        },
        crate::runqueue::DEFAULT_WEIGHT,
        None,
        || None,
    );
    let no_area_clean =
        no_area == Err(QueueError::Full) && in_use() <= before && live() == contexts;
    let free = crate::runqueue::CAPACITY.saturating_sub(contexts);
    let mut filled = 0;
    for _ in 0..free {
        if spawn(bsp, quitter, 0).is_ok() {
            filled += 1;
        }
    }
    let full_use = in_use();
    let full = spawn(bsp, quitter, 0);
    let full_clean =
        full == Err(QueueError::Full) && in_use() <= full_use && live() == contexts + free;
    let finished = started + filled as u64;
    yield_until(
        || FINISHED.load(Ordering::Relaxed) == finished && live() == contexts,
        tsc_hz,
        3,
    );
    reap();
    let ok = no_area_clean
        && full_clean
        && filled == free
        && live() == contexts
        && FINISHED.load(Ordering::Relaxed) == finished;
    let _ = writeln!(
        com1,
        "origin-kernel: scheduler: spawns refused for want of an FPU area ({no_area:?}) and on a full queue ({full:?}) give back the stack, its guard page and the FPU area{}",
        if ok { "" } else { ": scheduler failed" }
    );
    ok
}

/// Proves preemptive multitasking on every online processor: two non-yielding workers per
/// processor run for 500 ms while the boot flow (itself one of the boot processor's
/// contexts) waits. On each processor, both workers must have run there, with shares
/// within a factor of two, over at least five full rounds of context switches. Then the
/// rest of the thread proofs; each result goes into `proofs`, and `kernel_main` decides
/// what they mean (ADR-0013). Fairness counts loop iterations, which a processor sharing a
/// physical core skews with nothing wrong, so it is a separate result from preemption.
pub(crate) fn prove(com1: &mut Console, cpus: &[CpuIndex], tsc_hz: u64, proofs: &mut Tally) {
    // Give every online processor's boot/idle context an FPU area before the baseline is
    // measured, so the first switch on it can save its state and the leak check is fair.
    for &id in cpus {
        init_cpu_fpu(id);
    }
    let heap_before = crate::memory::stats().map(|(h, _)| h.bytes_in_use);
    // The stacks the processors hold for good: their own and their interrupt stacks.
    let stacks_before = origin_frame::stack::counts().live;
    RUNNING_STEP.store(
        (tsc_hz / 1_000_000).max(1) * RUNNING_STEP_MICROS,
        Ordering::Relaxed,
    );
    for (i, &id) in cpus.iter().enumerate() {
        for k in 0..2 {
            if spawn(id, worker, (2 * i + k) as u64).is_err() {
                let _ = writeln!(
                    com1,
                    "origin-kernel: cannot spawn on CPU {id}: scheduler failed"
                );
                proofs.record(Proof::Preemption, false);
                return;
            }
        }
    }
    let Some(lapic) = X2Apic::this_cpu() else {
        let _ = writeln!(
            com1,
            "origin-kernel: the local APIC is not in x2APIC mode: scheduler failed"
        );
        proofs.record(Proof::Preemption, false);
        return;
    };
    let bsp = cpu::this_cpu();
    GO.store(true, Ordering::Release);
    if let Some(active) = ACTIVE.get(bsp.get()) {
        active.store(true, Ordering::Release);
    }
    // Index 0's record is in the kernel's image, so this holds (review finding F6).
    if !crate::tlb::mark_online() {
        let _ = writeln!(
            com1,
            "origin-kernel: the boot processor has no TLB shootdown record: scheduler failed"
        );
        proofs.record(Proof::Preemption, false);
        return;
    }
    timer::start_periodic(&lapic);
    // The first thread's guard page must be unmapped. It is in the kernel stacks' region, so
    // these lines name neither it nor anything else there (ADR-0004 Amendment 2, A6.3).
    let guard = FIRST_GUARD.load(Ordering::Relaxed);
    match cpu::probe_read(guard) {
        cpu::Probe::Faulted { error: 0, address } if address == guard => {
            let _ = writeln!(
                com1,
                "origin-kernel: page protection: the first thread's stack guard page faulted (#PF error 0x0)"
            );
        }
        other => {
            let _ = match other {
                cpu::Probe::Faulted { error, address } => writeln!(
                    com1,
                    "origin-kernel: page protection violated: the first thread's stack guard page faulted with #PF error {error:#x}{}",
                    if address == guard {
                        ""
                    } else {
                        ", at another address"
                    }
                ),
                cpu::Probe::Completed => writeln!(
                    com1,
                    "origin-kernel: page protection violated: the first thread's stack guard page was read"
                ),
                cpu::Probe::Refused => writeln!(
                    com1,
                    "origin-kernel: page protection violated: the first thread's stack guard page is in a device window"
                ),
            };
            proofs.record(Proof::Preemption, false);
            return;
        }
    }
    let workers = 2 * cpus.len();
    let start = rdtsc();
    // A fixed window: 500 ms at 100 Hz is 50 ticks per processor, so each of its three
    // contexts (the boot or idle flow and two workers) is preempted and resumed many times.
    let window = tsc_hz / 2;
    while rdtsc().wrapping_sub(start) < window {
        // Interrupts on only while halted: the timer may switch to a worker here, and this
        // flow resumes after `hlt` when its turn comes round again.
        cpu::idle_disabling();
    }
    let elapsed_ms = rdtsc().wrapping_sub(start) / (tsc_hz / 1000).max(1);
    let switches_at_stop: alloc::vec::Vec<u64> = cpus
        .iter()
        .map(|id| {
            QUEUES
                .get(id.get())
                .map_or(0, |q| without_interrupts(|| q.lock().switches()))
        })
        .collect();
    STOP.store(true, Ordering::Relaxed);
    // Every worker now exits; the boot flow yields so the boot processor's workers run.
    yield_until(
        || EXITED.load(Ordering::Relaxed) == workers as u64,
        tsc_hz,
        2,
    );
    let mut ok = EXITED.load(Ordering::Relaxed) == workers as u64;
    if !ok {
        let _ = writeln!(
            com1,
            "origin-kernel: workers did not exit: scheduler failed"
        );
    }
    let mut fair_everywhere = true;
    for (i, &id) in cpus.iter().enumerate() {
        let switches = switches_at_stop.get(i).copied().unwrap_or(0);
        let (a, b) = (2 * i, 2 * i + 1);
        let count = |w: usize| worker_entry(&PROGRESS, w).map_or(0, |p| p.load(Ordering::Relaxed));
        let place = |w: usize| worker_entry(&RAN_ON, w).map_or(0, |r| r.load(Ordering::Relaxed));
        let right_cpu = place(a) == id.get() as u64 + 1 && place(b) == id.get() as u64 + 1;
        // Both workers ran, with comparable shares (within a factor of two).
        let (lo, hi) = (count(a).min(count(b)), count(a).max(count(b)));
        let fair = lo > 0 && lo * 2 >= hi;
        // At least five full rounds of the three contexts.
        let rounds = switches >= 15;
        let passed = right_cpu && fair && rounds;
        ok &= right_cpu && lo > 0 && rounds;
        fair_everywhere &= fair;
        let _ = writeln!(
            com1,
            "origin-kernel: scheduler: CPU {id}: workers {a} and {b} counted {} and {}, {switches} context switches{}",
            count(a),
            count(b),
            if passed { "" } else { ": scheduler failed" }
        );
    }
    // Given back now: the reaping proof below finds the heap where it was before the first
    // spawn.
    drop(switches_at_stop);
    if ok && fair_everywhere {
        let _ = writeln!(
            com1,
            "origin-kernel: preemptive multitasking: {workers} non-yielding threads on {} CPUs all ran, in {elapsed_ms} ms",
            cpus.len()
        );
    }
    proofs.record(Proof::Preemption, ok);
    proofs.record(Proof::Fairness, fair_everywhere);
    proofs.record(Proof::Weights, prove_weights(com1, cpus, bsp, tsc_hz));
    proofs.record(Proof::SimdState, prove_simd(com1, bsp, tsc_hz));
    proofs.record(Proof::ThreadLifecycle, prove_lifecycle(com1, bsp, tsc_hz));
    proofs.record(Proof::SpawnRefusal, prove_spawn_refused(com1, bsp, tsc_hz));
    proofs.record(Proof::UserIsolation, crate::user::prove(com1, bsp, tsc_hz));
    let consistent = crate::time::prove_consistent(com1, cpus, bsp, tsc_hz);
    proofs.record(Proof::TscConsistency, consistent);
    let monotonic = crate::time::prove_monotonic(com1, cpus, bsp, tsc_hz);
    proofs.record(Proof::ClockMonotonic, monotonic);
    proofs.record(
        Proof::ConsoleLines,
        crate::console::prove(com1, cpus, bsp, tsc_hz),
    );
    proofs.record(Proof::Tlb, crate::tlb::prove(com1, cpus, bsp, tsc_hz));
    crate::wait::prove(com1, cpus, bsp, tsc_hz, proofs);
    proofs.record(Proof::Ipc, crate::ipc_proof::prove(com1, cpus, bsp, tsc_hz));
    proofs.record(
        Proof::Objects,
        crate::ipc_proof::prove_objects(com1, cpus, bsp, tsc_hz),
    );
    proofs.record(Proof::KernelHalf, crate::aspace::prove_kernel_half(com1));
    proofs.record(
        Proof::AddressSpaces,
        crate::aspace::prove(com1, cpus, bsp, tsc_hz),
    );
    proofs.record(
        Proof::Identity,
        crate::ipc_proof::prove_identity(com1, cpus, bsp, tsc_hz),
    );
    // The first five launch processes, and a start record goes at a random address (ADR-0005
    // C4), as do a carried program and `origin.core` (tasks S4-1 and S4-2); the sixth maps
    // memory objects at addresses the kernel chooses at random (ADR-0005 D5). With no
    // random-number generator, or with one whose proof failed, randomness is withheld: nothing
    // is launched and no such place can be chosen, so none of the six can run. They are left
    // out of the tally, which the verdict counts as "did not run" (ADR-0016 R8; ADR-0013 R2
    // and R6).
    if origin_frame::random::available() {
        proofs.record(
            Proof::Launch,
            crate::ipc_proof::prove_launch(com1, cpus, bsp, tsc_hz),
        );
        proofs.record(
            Proof::RustProgram,
            crate::program_proof::prove(com1, cpus, bsp, tsc_hz),
        );
        proofs.record(
            Proof::CorePin,
            crate::ipc_proof::prove_core_pin(com1, cpus, bsp, tsc_hz),
        );
        proofs.record(
            Proof::EntryFlags,
            crate::ipc_proof::prove_entry_flags(com1, cpus, bsp, tsc_hz),
        );
        proofs.record(
            Proof::Directory,
            crate::ipc_proof::prove_directory(com1, cpus, bsp, tsc_hz),
        );
        proofs.record(
            Proof::Memory,
            crate::ipc_proof::prove_memory(com1, cpus, bsp, tsc_hz),
        );
    } else {
        let _ = writeln!(
            com1,
            "origin-kernel: start: randomness is withheld, so no process is launched and no mapping is placed at a random address: the process-start, Rust-program, origin.core, entry-flags, service-directory and memory-object proofs did not run"
        );
    }
    // Events and ports, and the clock as ring 3 reads it (task S3-6), need no generator: their
    // processes are built by the kernel at fixed addresses, and nothing is placed at random.
    proofs.record(
        Proof::EventsPorts,
        crate::ipc_proof::prove_events_ports(com1, cpus, bsp, tsc_hz),
    );
    proofs.record(
        Proof::UserClock,
        crate::ipc_proof::prove_user_clock(com1, bsp, tsc_hz),
    );
    // The kernel stacks' region needs no generator: its base is the loader's value or the
    // fixed one, and the proof says which.
    proofs.record(Proof::Stacks, crate::stacks::prove(com1, cpus, bsp, tsc_hz));
    // Every thread has exited: once each processor has reaped its own, the heap is back
    // exactly where it was before the first spawn, and so are the stacks held.
    let stacks = || origin_frame::stack::counts().live;
    let back = || {
        crate::memory::stats().map(|(h, _)| h.bytes_in_use) == heap_before
            && stacks() == stacks_before
    };
    yield_until(back, tsc_hz, 2);
    let reaped = back();
    let now = crate::memory::stats().map_or(0, |(h, _)| h.bytes_in_use);
    proofs.record(Proof::ThreadsReaped, reaped);
    let _ = writeln!(
        com1,
        "origin-kernel: scheduler: exited threads' stacks reaped on their own CPUs: heap {} KiB, {} KiB before the first spawn; {} stacks held, {stacks_before} before the first spawn{}",
        now >> 10,
        heap_before.unwrap_or(0) >> 10,
        stacks(),
        if reaped { "" } else { ": scheduler failed" }
    );
    // The kernel-wide bound (ADR-0005 G5; task S4-3), after every proof that makes a process, so
    // that its root can be seen whole again.
    proofs.record(
        Proof::KernelBound,
        crate::ipc_proof::prove_kernel_bound(com1, cpus, bsp, tsc_hz),
    );
}

/// Stops the boot processor's scheduler and timer once the boot proofs are done, so the
/// boot flow can halt.
pub(crate) fn stop_boot_processor() {
    if let Some(lapic) = X2Apic::this_cpu() {
        lapic.stop();
    }
    if let Some(active) = ACTIVE.get(this_cpu()) {
        active.store(false, Ordering::Release);
    }
}
