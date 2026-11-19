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
use crate::runqueue::{
    PREEMPTION_ROUNDS, QueueError, ReadBack, RunQueue, TickWatch, WINDOW_PAUSE_SECONDS,
    WINDOW_RUNS_SECONDS, WindowVerdict, judge_window, per_mille, share_near_weight, window_again,
};

use crate::console::Console;
use crate::cpu::{self, CpuIndex, rdtsc};
use crate::process::UserContext;
use crate::timer::{self, X2Apic};
use origin_frame::objref::ObjectRef;
use origin_frame::percpu::PerCpu;
use origin_frame::stack::{FpuArea, GuardedStack, ThreadStart};
use origin_frame::sync::SpinLock;
use origin_frame::sync::exits::{Exits, Pending};
use origin_frame::trap::Switch;

/// Each kernel thread's stack.
pub(crate) const THREAD_STACK: usize = 16 * 1024;

// A processor's table of contexts (`origin_frame::sync::contexts`, TASKS item 70) holds one per
// run-queue slot, the boot or idle flow's among them, and one ended and not yet retired: the
// queue's capacity must be less than the table's.
const _: () = assert!(
    crate::runqueue::CAPACITY < origin_frame::sync::contexts::SLOTS,
    "every context of a full run queue, and one ended, fits a processor's table"
);

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
    with_queue(interrupted, switch, |q| {
        // The preemption proof's watch (TASKS item 66): the TSC only while it is armed.
        if q.watching() {
            q.note_tick(rdtsc(), timer_read_back);
        }
        Some(q.tick(interrupted))
    })
}

/// This processor's timer, read back and compared with the boot processor's registers as its
/// timed proof ran them (TASKS item 66, ruling A on the review's T66-R1; the addendum review's
/// T66-R14). Unread if either cannot be had.
fn timer_read_back() -> ReadBack {
    match (timer::programmed(), timer::timed_programming()) {
        (Some(read), Some(timed)) => match read.first_difference(&timed) {
            None => ReadBack::AsTimed,
            Some((register, got, timed)) => ReadBack::Otherwise(register, got, timed),
        },
        _ => ReadBack::Unread,
    }
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
    /// Whether each slot's thread has counted itself as ending ([`end_with`]), so that its exit
    /// lowers that count once, and only for a thread that raised it (TASKS item 79).
    ending: [bool; crate::runqueue::CAPACITY],
}

impl Threads {
    /// No thread.
    const fn new() -> Self {
        Self {
            stacks: [const { None }; crate::runqueue::CAPACITY],
            zombies: [const { None }; crate::runqueue::CAPACITY],
            fpu: [const { None }; crate::runqueue::CAPACITY],
            user: [const { None }; crate::runqueue::CAPACITY],
            ending: [false; crate::runqueue::CAPACITY],
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

/// Each processor's ending threads and the zombies that await [`reap`] there
/// (`origin_frame::sync::exits`, TASKS item 79): what [`settle`] waits on, and a cheap test that
/// a processor has no zombie.
pub(crate) static EXITS: PerCpu<Exits> =
    PerCpu::new("framekernel::sched::EXITS", Exits::new(), Exits::new);

/// What every processor's exits still hold: threads whose end may have been seen that have not
/// yet left their processor, then zombies not yet reaped, read in that order.
fn exits_pending() -> Pending {
    origin_frame::sync::exits::pending(|| EXITS.iter())
}

/// The running thread is ending: counts it as ending on this processor, once, then runs
/// `publish`, which makes its end visible, and returns what `publish` returns (TASKS item 79).
/// Called before anything about the end can be seen: by `Process::thread_ended` for a user thread
/// and by [`finish`] for a kernel thread. Its exit lowers the count once the thread has left the
/// processor as a zombie, so a [`settle`] that has seen the end waits until then. The boot or idle
/// flow, which never exits, is not counted.
pub(crate) fn end_with<R>(publish: impl FnOnce() -> R) -> R {
    let id = this_cpu();
    let first = match (QUEUES.get(id), THREADS.get(id)) {
        (Some(queue), Some(threads)) => without_interrupts(|| {
            let slot = queue.lock().current();
            let mut t = threads.lock();
            match t.ending.get_mut(slot) {
                Some(marked) if slot != 0 && !*marked => {
                    *marked = true;
                    true
                }
                _ => false,
            }
        }),
        _ => false,
    };
    match EXITS.get(id) {
        Some(exits) if first => exits.end(publish),
        _ => publish(),
    }
}

/// Frees the stacks of threads that exited on **this** processor. Only this processor may
/// do it, and only from thread context: by then its exit handler, which ran on the dead
/// stack, has certainly switched away. Returns how many stacks were freed.
///
/// Then it gives back what retired stacks hold (`stack::reclaim`): here, with no lock held, as
/// the shootdown that must come first waits for every processor (ADR-0004 Amendment 1, A2).
pub(crate) fn reap() -> usize {
    let freed = reap_zombies();
    reclaim_stacks();
    freed
}

/// Reclaims every retired kernel stack (`stack::reclaim`), and says where any was lost to a
/// stalled shootdown ([`report_lost_stacks`]): every reclaim the scheduler makes goes through
/// here, `settle`'s own among them.
fn reclaim_stacks() {
    let reclaimed = origin_frame::stack::reclaim();
    if reclaimed.lost > 0 {
        report_lost_stacks(reclaimed.lost);
    }
}

/// Says where kernel stacks were lost to a stalled shootdown, as it happens: which processor
/// asked, which did not answer within the patience, how long it waited, and how many stalls the
/// boot has had (TASKS item 66). A report, judged by nothing: the proofs that count the stacks
/// judge the loss. It names processors and times only, never an address.
fn report_lost_stacks(lost: u32) {
    let mut out = here_console();
    let millis = |ticks: u64| ticks / (crate::time::tsc_hz() / 1000).max(1);
    // Under the console's 160-byte line (the review's T66-R7). The stall named is the boot's
    // last: another processor's stall can come between the loss and this line.
    let _ = match origin_frame::tlb::last_stall() {
        Some(stall) => writeln!(
            out,
            "origin-kernel: stalled shootdown: CPU {} waited {} ms for CPU {} (stall {} of this boot); {lost} kernel stacks withheld, with their frames",
            stall.asker,
            millis(stall.waited),
            stall.target,
            stall.count
        ),
        None => writeln!(
            out,
            "origin-kernel: stalled shootdown: {lost} kernel stacks withheld, with their frames"
        ),
    };
}

/// A console for a line printed where the caller may be: the boot flow, which runs with
/// interrupts off and may wait for the UART, gets its own kind, so that the line is not dropped
/// (the review's T66-R7); anywhere else, the ordinary one.
fn here_console() -> Console {
    if !cpu::interrupts_enabled() && current_thread().is_none() && cpu::this_cpu().get() == 0 {
        Console::boot_flow()
    } else {
        Console::new()
    }
}

/// Waits, as [`yield_until`], up to [`OTHERS_PATIENCE`] seconds for what only a thread on another
/// processor can do, and returns whether `done()` holds. A wait longer than `before_ms`, the
/// bound it had before TASKS item 66, is said on a line of its own naming `what`, so that a
/// reader of a machine without a hypervisor sees every extension (the review's T66-R3).
pub(crate) fn wait_for_others(
    what: &str,
    done: impl Fn() -> bool,
    tsc_hz: u64,
    before_ms: u64,
) -> bool {
    let start = rdtsc();
    yield_until(&done, tsc_hz, OTHERS_PATIENCE);
    let waited_ms = rdtsc().wrapping_sub(start) / (tsc_hz / 1000).max(1);
    let held = done();
    if waited_ms > before_ms {
        let _ = writeln!(
            here_console(),
            "origin-kernel: waited {waited_ms} ms for {what}, past the former {before_ms} ms{}",
            if held { "" } else { ", in vain" }
        );
    }
    held
}

pub(crate) use crate::settled::{Unsettled, not_settled};

/// Settles the machine before a proof reads the whole machine's heap or free frames to compare
/// them (TASKS item 67): every thread whose end may have been seen gone from its processor and
/// reaped there (TASKS item 79), every stack kept for reuse retired (`stack::trim`), and every
/// retired stack reclaimed (`stack::reclaim`, one shootdown each, which waits for every
/// processor). Each of those moves the counts on its own
/// schedule: on the 65-processor machine the stacks that earlier proofs' threads left were still
/// being reclaimed, one shootdown at a time, when the IPC proof took its first count, so the
/// count rose during the proof; and a cache of kept stacks that refills during a proof lowers it.
/// A thread's end is seen before it has given anything back: a user thread's process says it has
/// ended, and a kernel thread raises its own done-flag, before the thread leaves its processor as
/// a zombie. So waiting for zombies alone found none for a thread still in that stretch, and the
/// count was read with its stack, FPU area and address space held (TASKS item 79: H1 when an
/// earlier proof's thread was there at a proof's first count, H2 when its own was at its compare).
/// Settled at both reads, the counts change only with what the proof does. Waits up to
/// [`OTHERS_PATIENCE`] seconds for each part; `Err` names what was still pending, and any
/// stack lost to a stalled shootdown while it ran (`settled`: safe by design, 4 frames each).
pub(crate) fn settle(tsc_hz: u64) -> Result<(), Unsettled> {
    let lost_before = origin_frame::stack::counts().lost;
    reap();
    wait_for_others(
        "ending threads' exits and reaping",
        || exits_pending().none(),
        tsc_hz,
        2000,
    );
    origin_frame::stack::trim();
    reclaim_stacks();
    wait_for_others(
        "retired stacks' reclaim",
        || origin_frame::stack::counts().retired == 0,
        tsc_hz,
        2000,
    );
    let counts = origin_frame::stack::counts();
    let exits = exits_pending();
    crate::settled::judge(
        exits.ending,
        exits.zombies,
        counts.retired,
        lost_before,
        counts.lost,
    )
}

/// [`reap`]'s own processor's exited threads.
fn reap_zombies() -> usize {
    let (Some(threads), Some(exits)) = (THREADS.get(this_cpu()), EXITS.get(this_cpu())) else {
        return 0;
    };
    if exits.zombies() == 0 {
        return 0;
    }
    let mut freed = 0;
    loop {
        let next = without_interrupts(|| threads.lock().zombies.iter_mut().find_map(Option::take));
        let Some(Zombie { stack, fpu, user }) = next else {
            return freed;
        };
        // The zombie is counted gone only after `release` has run: a `settle` that sees no zombie
        // left then knows that everything this one held has been given back, its stack in the
        // cache of kept stacks among it, before it trims that cache (the review of TASKS item 70,
        // T70-7; `Exits::reap`).
        exits.reap(|| {
            drop(fpu);
            drop(stack);
            if let Some(user) = user {
                let process = user.process.clone();
                // Possibly the last reference to the address space, which frees it: this
                // processor left it when the thread exited, and no processor still runs in it.
                drop(user);
                // After the memory, so that whoever sees `TERMINATED` knows it has gone (P11).
                process.thread_reaped();
            }
        });
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
            // The preemption proof's rounds clause stamps its 15th switch after the step that
            // made it (TASKS item 66, the addendum review's T66-R15).
            if q.watching() {
                q.note_switches(rdtsc());
            }
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
    let next = with_queue(interrupted, switch, |q| q.sleep(interrupted, ticks).ok());
    // The kernel stacks proof holds one thread here, in its switch-out window (TASKS item 70).
    crate::stacks::hold_in_window(interrupted, next);
    next
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
    let next = with_queue(interrupted, switch, |q| {
        let slot = q.current();
        let next = q.exit(interrupted).ok()?;
        if let Some(threads) = THREADS.get(id) {
            let mut t = threads.lock();
            let stack = t.stacks.get_mut(slot).and_then(Option::take);
            let fpu = t.fpu.get_mut(slot).and_then(Option::take);
            let user = t.user.get_mut(slot).and_then(Option::take);
            // Taken whatever happens below, so that the slot's next thread starts unmarked.
            let marked = t.ending.get_mut(slot).is_some_and(core::mem::take);
            match (t.zombies.iter_mut().find(|z| z.is_none()), stack) {
                (Some(free), Some(stack)) => {
                    *free = Some(Zombie { stack, fpu, user });
                    // The zombie counts before the thread stops counting as ending, so that a
                    // `settle` never finds it in neither (TASKS item 79; `Exits::queued`).
                    if let Some(exits) = EXITS.get(id) {
                        exits.queued(marked);
                    }
                }
                // No record of it can be kept. Its references are leaked deliberately: this
                // processor is still in the process's address space, and dropping the last
                // reference here would free the tables it is running on. It has left the
                // processor all the same, so a `settle` stops waiting for it: what it leaked is
                // the compare's to find (TASKS item 79, case (a)).
                _ => {
                    core::mem::forget(user);
                    if let Some(exits) = EXITS.get(id) {
                        exits.not_queued(marked);
                    }
                }
            }
        }
        Some(next)
    });
    // The kernel stacks proof holds one thread here, in its exit's window (TASKS item 70).
    crate::stacks::hold_in_window(interrupted, next);
    next
}

/// Gives up the processor to the next ready context on this CPU.
pub(crate) fn yield_now() {
    cpu::thread_yield();
}

/// Sleeps for `ticks` timer ticks of this CPU (at least one).
pub(crate) fn sleep(ticks: u64) {
    cpu::thread_sleep(ticks);
}

/// Ends the running kernel thread: counts it as ending ([`end_with`]), runs `signal`, which is
/// whatever tells other threads it has ended (a done-flag or a count), and exits. Every kernel
/// thread ends here, so that a [`settle`] made by a thread that has seen `signal` waits until
/// this one has left its processor and been reaped (TASKS item 79). There is no other way for a
/// kernel thread to end: a done-flag raised before an exit that does not count it would let a
/// settle pass while the thread still held its stack and FPU area.
pub(crate) fn finish(signal: impl FnOnce()) -> ! {
    end_with(signal);
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
pub(crate) fn queue_ticks() -> u64 {
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
/// The preemption proof's run, from 1. Each worker carries its run in the upper half of its
/// argument, so that a worker of an earlier run counts nothing for a later one (TASKS item 66,
/// the addendum review's T66-R11).
static RUN: AtomicU64 = AtomicU64::new(1);
/// The run in the upper half, and in the lower half its workers that have stopped and exited.
static EXITED: AtomicU64 = AtomicU64::new(1 << 32);

/// A worker's argument: its run and its slot.
const fn worker_arg(run: u64, slot: usize) -> u64 {
    (run << 32) | slot as u64
}

/// A worker that never yields: until [`STOP`] it adds up the time it sees itself run, so it
/// only ever gives up its processor when the timer preempts it.
extern "sysv64" fn worker(arg: u64) -> ! {
    let (run, slot) = (arg >> 32, (arg & 0xFFFF_FFFF) as usize);
    let limit = RUNNING_STEP.load(Ordering::Relaxed);
    let mut last = rdtsc();
    while !STOP.load(Ordering::Relaxed) {
        let now = rdtsc();
        let step = now.wrapping_sub(last);
        last = now;
        if let (Some(p), Some(r)) = (worker_entry(&PROGRESS, slot), worker_entry(&RAN_ON, slot))
            && RUN.load(Ordering::Relaxed) == run
        {
            if step < limit {
                p.fetch_add(step, Ordering::Relaxed);
            }
            r.store(this_cpu() as u64 + 1, Ordering::Relaxed);
        }
        core::hint::spin_loop();
    }
    // Counted only for its own run.
    finish(|| {
        let _ = EXITED.try_update(Ordering::AcqRel, Ordering::Acquire, |exited| {
            (exited >> 32 == run).then_some(exited + 1)
        });
    })
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
    // AcqRel, read with Acquire: a waiter that settles after seeing it is ordered after
    // `finish`'s count of this thread as ending (TASKS item 79).
    finish(|| {
        FINISHED.fetch_add(1, Ordering::AcqRel);
    })
}

/// Sleeps [`SLEEP_TICKS`] and records how long that took in ticks.
extern "sysv64" fn sleeper(_: u64) -> ! {
    let before = queue_ticks();
    sleep(SLEEP_TICKS);
    SLEPT.store(queue_ticks() - before + 1, Ordering::Relaxed);
    // AcqRel, read with Acquire: a waiter that settles after seeing it is ordered after
    // `finish`'s count of this thread as ending (TASKS item 79).
    finish(|| {
        FINISHED.fetch_add(1, Ordering::AcqRel);
    })
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
    // AcqRel, read with Acquire: a waiter that settles after seeing it is ordered after
    // `finish`'s count of this thread as ending (TASKS item 79).
    finish(|| {
        FINISHED.fetch_add(1, Ordering::AcqRel);
    })
}

/// TSC cycles per context switch: the boot flow and two threads that only yield, so each
/// boot-flow yield is three switches (boot → A → B → boot).
fn measure_switch(bsp: CpuIndex) -> Option<u64> {
    let before = FINISHED.load(Ordering::Acquire);
    spawn(bsp, bencher, 0).ok()?;
    spawn(bsp, bencher, 1).ok()?;
    yield_now(); // let both start
    let start = rdtsc();
    for _ in 0..BENCH_ROUNDS {
        yield_now();
    }
    let cycles = rdtsc().wrapping_sub(start);
    BENCH_STOP.store(true, Ordering::Relaxed);
    while FINISHED.load(Ordering::Acquire) < before + 2 {
        yield_now();
    }
    reap();
    Some(cycles / (3 * BENCH_ROUNDS))
}

/// Exits at once.
extern "sysv64" fn quitter(_: u64) -> ! {
    // AcqRel, read with Acquire: a waiter that settles after seeing it is ordered after
    // `finish`'s count of this thread as ending (TASKS item 79).
    finish(|| {
        FINISHED.fetch_add(1, Ordering::AcqRel);
    })
}

/// How long, in seconds, a Refuse proof waits for what only a thread on another processor can
/// do: a prober running, an exited thread reaped, a retired stack reclaimed. The clock proofs'
/// turn-takers starting and finishing wait as long. A budget, not a bound (ADR-0013 R1(b)):
/// each wait ends as soon as its condition holds, which on a real machine is within
/// microseconds, and its verdict does not change. Under QEMU's software processor a
/// 65-processor machine with a little outside load left one processor's prober unrun past
/// the 2 s this was before TASKS item 66, and in another boot its turn-takers.
pub(crate) const OTHERS_PATIENCE: u64 = 10;

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
    yield_until_away_for(done, tsc_hz, tsc_hz.saturating_mul(seconds))
}

/// As [`yield_until_away`], for at most `duration` TSC ticks: a step of the loop counts as away
/// from [`RUNNING_STEP_MICROS`] of the real rate `tsc_hz`, whatever the duration (the review's
/// T66-R2: a short wait passed as a rate of its own made 60 µs count as away).
pub(crate) fn yield_until_away_for(done: impl Fn() -> bool, tsc_hz: u64, duration: u64) -> u64 {
    let limit = (tsc_hz / 1_000_000).max(1) * RUNNING_STEP_MICROS;
    let start = rdtsc();
    let mut last = start;
    let mut away = 0;
    while !done() && rdtsc().wrapping_sub(start) < duration {
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
    finish(|| {
        SIMD_EXITED.fetch_add(1, Ordering::AcqRel);
    })
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
        || SIMD_EXITED.load(Ordering::Acquire) == SIMD_WORKERS as u64,
        tsc_hz,
        2,
    );
    let corrupt: u64 = SIMD_CORRUPT.iter().map(|c| c.load(Ordering::Relaxed)).sum();
    let iters: u64 = SIMD_ITERS.iter().map(|c| c.load(Ordering::Relaxed)).sum();
    let switches = QUEUES
        .get(bsp.get())
        .map_or(0, |q| without_interrupts(|| q.lock().switches()));
    let ok =
        SIMD_EXITED.load(Ordering::Acquire) == SIMD_WORKERS as u64 && corrupt == 0 && iters > 0;
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
    finish(|| {
        WEIGHTED_EXITED.fetch_add(1, Ordering::AcqRel);
    })
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
    yield_until(|| WEIGHTED_EXITED.load(Ordering::Acquire) == 3, tsc_hz, 2);
    Some(WeightedRun {
        ran: core::array::from_fn(|i| WEIGHTED[i].load(Ordering::Relaxed)),
        exited: WEIGHTED_EXITED.load(Ordering::Acquire) == 3,
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
        || FINISHED.load(Ordering::Acquire) == 3 && live() == 1,
        tsc_hz,
        3,
    );
    let reused = spawn(bsp, quitter, 0);
    yield_until(
        || FINISHED.load(Ordering::Acquire) == 4 && live() == 1,
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
    let started = FINISHED.load(Ordering::Acquire);
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
        || FINISHED.load(Ordering::Acquire) == finished && live() == contexts,
        tsc_hz,
        3,
    );
    reap();
    let ok = no_area_clean
        && full_clean
        && filled == free
        && live() == contexts
        && FINISHED.load(Ordering::Acquire) == finished;
    let _ = writeln!(
        com1,
        "origin-kernel: scheduler: spawns refused for want of an FPU area ({no_area:?}) and on a full queue ({full:?}) give back the stack, its guard page and the FPU area{}",
        if ok { "" } else { ": scheduler failed" }
    );
    ok
}

/// The preemption window, in each processor's own timer ticks: what 500 ms holds at 100 Hz.
const WINDOW_TICKS: u64 = 50;
/// How long the preemption window may last, in seconds, while some processor has not yet
/// taken its [`WINDOW_TICKS`]: a budget, not a bound (ADR-0013 R1(b)). On a real machine the
/// ticks take 500 ms. Under QEMU's software processor a virtual processor the host leaves
/// unscheduled misses ticks (QEMU merges a periodic timer's expiries it was away for), and on
/// the 65-processor machine a 500 ms window of the host's time held only 12 to 14 switches on
/// one processor (TASKS item 66).
const WINDOW_BUDGET_SECONDS: u64 = 10;

/// One run of the preemption proof's window, as the boot flow measured it.
struct WindowRun {
    /// How long the window lasted.
    elapsed_ms: u64,
    /// Each processor's ticks over the window, and its context switches, in the proof's order.
    watches: alloc::vec::Vec<TickWatch>,
    switches: alloc::vec::Vec<u64>,
    /// Whether every worker exited after the window.
    exited: bool,
}

/// One run of the window: every processor's ticks watched from now, until each has taken
/// [`WINDOW_TICKS`] or [`WINDOW_BUDGET_SECONDS`] pass; then the workers are told to stop and
/// waited for.
fn run_window(cpus: &[CpuIndex], tsc_hz: u64, workers: u64, run: u64) -> WindowRun {
    let period = tsc_hz / timer::HZ;
    let start = rdtsc();
    let switches_before: alloc::vec::Vec<u64> = cpus
        .iter()
        .map(|id| {
            QUEUES.get(id.get()).map_or(0, |q| {
                without_interrupts(|| {
                    let mut q = q.lock();
                    q.arm_watch(start, period);
                    q.switches()
                })
            })
        })
        .collect();
    let watch_of = |id: &CpuIndex| {
        QUEUES
            .get(id.get())
            .map(|q| without_interrupts(|| q.lock().watch()))
    };
    // A processor whose timer reads back otherwise than timed ends the window at once: no wait
    // can change that verdict.
    let settled = || {
        cpus.iter()
            .all(|id| watch_of(id).is_none_or(|w| w.ticks >= WINDOW_TICKS))
            || cpus.iter().any(|id| {
                watch_of(id).is_some_and(|w| matches!(w.read_back, ReadBack::Otherwise(..)))
            })
    };
    let budget = tsc_hz.saturating_mul(WINDOW_BUDGET_SECONDS);
    while !settled() && rdtsc().wrapping_sub(start) < budget {
        // Interrupts on only while halted: the timer may switch to a worker here, and this
        // flow resumes after `hlt` when its turn comes round again.
        cpu::idle_disabling();
    }
    let elapsed_ms = rdtsc().wrapping_sub(start) / (tsc_hz / 1000).max(1);
    let (watches, switches) = cpus
        .iter()
        .zip(&switches_before)
        .map(|(id, &before)| {
            QUEUES.get(id.get()).map_or((TickWatch::new(), 0), |q| {
                without_interrupts(|| {
                    let mut q = q.lock();
                    q.disarm_watch();
                    (q.watch(), q.switches().wrapping_sub(before))
                })
            })
        })
        .unzip();
    drop(switches_before);
    STOP.store(true, Ordering::Relaxed);
    // Every worker now exits; the boot flow yields so the boot processor's workers run.
    let exited = wait_for_others(
        "the preemption workers' exit",
        || EXITED.load(Ordering::Acquire) == (run << 32) | workers,
        tsc_hz,
        2000,
    );
    WindowRun {
        elapsed_ms,
        watches,
        switches,
        exited,
    }
}

/// Starts run `run`'s workers, two on every processor, with their counts from zero; the CPU
/// that refused one, if any. Only after every worker of the run before has exited (its
/// [`WindowRun::exited`]), so that nothing of it is left to run (the addendum review's T66-R11).
fn respawn_workers(cpus: &[CpuIndex], run: u64) -> Result<(), CpuIndex> {
    RUN.store(run, Ordering::Relaxed);
    EXITED.store(run << 32, Ordering::Release);
    for slot in 0..2 * cpus.len() {
        for table in [&PROGRESS, &RAN_ON] {
            if let Some(entry) = worker_entry(table, slot) {
                entry.store(0, Ordering::Relaxed);
            }
        }
    }
    STOP.store(false, Ordering::Release);
    for (i, &id) in cpus.iter().enumerate() {
        for k in 0..2 {
            spawn(id, worker, worker_arg(run, 2 * i + k)).map_err(|_| id)?;
        }
    }
    Ok(())
}

/// What processor `id`'s two workers, `2 * i` and `2 * i + 1`, did in the last run: their
/// counts, and whether both last ran on `id`.
fn workers_on(i: usize, id: CpuIndex) -> (u64, u64, bool) {
    let (a, b) = (2 * i, 2 * i + 1);
    let count = |w: usize| worker_entry(&PROGRESS, w).map_or(0, |p| p.load(Ordering::Relaxed));
    let place = |w: usize| worker_entry(&RAN_ON, w).map_or(0, |r| r.load(Ordering::Relaxed));
    let here = id.get() as u64 + 1;
    (count(a), count(b), place(a) == here && place(b) == here)
}

/// The run's correctness clauses, judged on every run (the contract's invariant (3); the
/// addendum review's T66-R11): every worker exited, and on every processor both workers ran,
/// and ran there.
fn run_correct(run: &WindowRun, cpus: &[CpuIndex]) -> bool {
    run.exited
        && cpus.iter().enumerate().all(|(i, &id)| {
            let (a, b, right_cpu) = workers_on(i, id);
            right_cpu && a.min(b) > 0
        })
}

/// The run's verdict from its processors' ticks: the first that fails ([`WindowVerdict::Short`],
/// [`WindowVerdict::WrongRate`] or [`WindowVerdict::Late`]) decides, else the first stalled
/// one, else every processor is in bounds. With the processor and its watch, for the line.
fn judge_run(
    run: &WindowRun,
    cpus: &[CpuIndex],
    tsc_hz: u64,
) -> (WindowVerdict, Option<(CpuIndex, TickWatch)>) {
    // Today's bound: the rounds clause's 15 rounds within 500 ms. Every processor's timer must
    // read back as the boot processor's timed proof ran it (`timer_read_back`).
    let bound = tsc_hz / 2;
    let judged = || {
        cpus.iter()
            .zip(&run.watches)
            .map(|(&id, w)| (id, *w, judge_window(w, WINDOW_TICKS, bound)))
    };
    // A timer programmed otherwise is named first: it ends the window at once, which leaves
    // other processors short of their ticks for no fault of theirs.
    for wanted in [
        WindowVerdict::WrongRate,
        WindowVerdict::Short,
        WindowVerdict::Late,
    ] {
        if let Some((id, w, v)) = judged().find(|&(_, _, v)| v == wanted) {
            return (v, Some((id, w)));
        }
    }
    if let Some((id, w, v)) = judged().find(|&(_, _, v)| v == WindowVerdict::Stalled) {
        return (v, Some((id, w)));
    }
    (WindowVerdict::InBounds, None)
}

/// The run's line: how long the window lasted, which run it was, and what its processors' ticks
/// showed. A stalled run says so, as on a machine without a hypervisor it is a finding (ADR-0013's
/// note of TASKS item 66); a failing one ends `scheduler failed`, on this line and not on a
/// processor's own, whose wording `xtask`'s fair-share allowances read.
fn say_window(
    com1: &mut Console,
    run: &WindowRun,
    verdict: (WindowVerdict, Option<(CpuIndex, TickWatch)>),
    attempt: u32,
    correct: bool,
    again: bool,
    spent_seconds: u64,
) {
    let ms = |w: &TickWatch, ticks: u64| ticks / (w.period.saturating_mul(timer::HZ) / 1000).max(1);
    let _ = write!(
        com1,
        "origin-kernel: scheduler: window of {} ms, run {attempt}, {spent_seconds} s of {WINDOW_RUNS_SECONDS} spent: ",
        run.elapsed_ms
    );
    let _ = match verdict {
        (WindowVerdict::InBounds, _) => write!(
            com1,
            "every timer as timed, every 15th switch within 500 ms{}",
            if attempt > 1 {
                " (after a stalled run)"
            } else {
                ""
            }
        ),
        (v, Some((id, w))) => {
            let what = match v {
                // A run whose correctness clauses failed is never run again.
                WindowVerdict::Stalled if !correct => "stalled, not run again: scheduler failed",
                WindowVerdict::Stalled if again => "stalled, run again",
                WindowVerdict::Stalled => {
                    "stalled in every run, its budget spent: scheduler failed"
                }
                _ => ": scheduler failed",
            };
            match v {
                WindowVerdict::Short => write!(
                    com1,
                    "CPU {id} took {} of {WINDOW_TICKS} ticks in the window{what}",
                    w.ticks
                ),
                WindowVerdict::WrongRate => match w.read_back {
                    ReadBack::Otherwise(register, got, want) => write!(
                        com1,
                        "CPU {id}'s timer {register} is {got:#x}, not {want:#x} as timed{what}"
                    ),
                    _ => write!(com1, "CPU {id}'s timer could not be read back{what}"),
                },
                _ if w.at_switches == 0 => write!(
                    com1,
                    "CPU {id} switched fewer than {PREEMPTION_ROUNDS} times in the window{what}"
                ),
                _ => write!(
                    com1,
                    "CPU {id}'s 15th switch at {} ms, longest gap {} ms{}{}",
                    ms(&w, w.at_switches.saturating_sub(w.start)),
                    ms(&w, w.longest),
                    if v == WindowVerdict::Stalled {
                        ": "
                    } else {
                        ""
                    },
                    what
                ),
            }
        }
        (_, None) => Ok(()),
    };
    let _ = writeln!(com1);
}

/// Proves preemptive multitasking on every online processor: two non-yielding workers per
/// processor run while the boot flow (itself one of the boot processor's contexts) waits,
/// until every processor has taken [`WINDOW_TICKS`] of its own timer ticks (500 ms on a real
/// machine), within [`WINDOW_BUDGET_SECONDS`]. On each processor, both workers must have run
/// there, with shares within a factor of two, over at least five full rounds of context
/// switches; its timer, read back at its first tick, must be programmed as the boot
/// processor's timed proof ran it (TASKS item 66, the review's T66-R1), and its 15th context
/// switch must come within 500 ms on the TSC, today's bound (the addendum review's T66-R15). A
/// run whose 15th switch came late with a gap of two periods or more between its ticks is
/// measured again after a pause, within [`WINDOW_RUNS_SECONDS`] of wall time and at most
/// `WINDOW_RUNS_MOST` runs ([`window_again`]). Then the rest of the thread proofs; each result goes into
/// `proofs`, and `kernel_main` decides what they mean (ADR-0013). Fairness counts loop
/// iterations, which a processor sharing a physical core skews with nothing wrong, so it is a
/// separate result from preemption.
pub(crate) fn prove(com1: &mut Console, cpus: &[CpuIndex], tsc_hz: u64, proofs: &mut Tally) {
    // Give every online processor's boot/idle context an FPU area before the baseline is
    // measured, so the first switch on it can save its state and the leak check is fair.
    for &id in cpus {
        init_cpu_fpu(id);
    }
    // Settled first, so that only these proofs move the counts (TASKS item 67).
    let settled_before = settle(tsc_hz);
    let heap_before = crate::memory::stats().map(|(h, _)| h.bytes_in_use);
    // The stacks the processors hold for good: their own and their interrupt stacks.
    let stacks_before = origin_frame::stack::counts().live;
    RUNNING_STEP.store(
        (tsc_hz / 1_000_000).max(1) * RUNNING_STEP_MICROS,
        Ordering::Relaxed,
    );
    for (i, &id) in cpus.iter().enumerate() {
        for k in 0..2 {
            if spawn(id, worker, worker_arg(1, 2 * i + k)).is_err() {
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
    // Runs of the window, each against the same verdict: another only after a run whose
    // processors' own ticks show a stall before today's bound (TASKS item 66, T66-R1).
    let budget_start = rdtsc();
    let mut attempts = 0;
    let (run, verdict) = loop {
        attempts += 1;
        if attempts > 1
            && let Err(id) = respawn_workers(cpus, u64::from(attempts))
        {
            let _ = writeln!(
                com1,
                "origin-kernel: cannot spawn on CPU {id}: scheduler failed"
            );
            proofs.record(Proof::Preemption, false);
            return;
        }
        let run = run_window(cpus, tsc_hz, workers as u64, u64::from(attempts));
        let verdict = judge_run(&run, cpus, tsc_hz);
        // Every run is judged on correctness first: a failure there fails at once and is
        // never repeated, and a repeat starts only once every worker of this run has exited
        // (the addendum review's T66-R11).
        let correct = run_correct(&run, cpus);
        // Within a budget of time, not a count: stalls come in episodes of seconds (the
        // coordinating session's ruling of 2026-11-20).
        let elapsed = rdtsc().wrapping_sub(budget_start);
        let again = window_again(verdict.0, correct, attempts, elapsed, tsc_hz);
        say_window(
            com1,
            &run,
            verdict,
            attempts,
            correct,
            again,
            elapsed / tsc_hz.max(1),
        );
        if !again {
            break (run, verdict.0);
        }
        // Every worker of the run has exited: a pause with every processor idle, so that an
        // episode of the host's stalls can pass before the next run.
        let pause = WINDOW_PAUSE_SECONDS.saturating_mul(tsc_hz);
        let paused = rdtsc();
        while rdtsc().wrapping_sub(paused) < pause {
            cpu::idle_disabling();
        }
    };
    let elapsed_ms = run.elapsed_ms;
    let mut ok = run.exited && verdict == WindowVerdict::InBounds;
    if !run.exited {
        let _ = writeln!(
            com1,
            "origin-kernel: workers did not exit: scheduler failed"
        );
    }
    let mut fair_everywhere = true;
    for (i, &id) in cpus.iter().enumerate() {
        let switches = run.switches.get(i).copied().unwrap_or(0);
        let (a, b) = (2 * i, 2 * i + 1);
        let (count_a, count_b, right_cpu) = workers_on(i, id);
        let count = |w: usize| if w == a { count_a } else { count_b };
        // Both workers ran, with comparable shares (within a factor of two).
        let (lo, hi) = (count_a.min(count_b), count_a.max(count_b));
        let fair = lo > 0 && lo * 2 >= hi;
        // At least five full rounds of the three contexts.
        let rounds = switches >= PREEMPTION_ROUNDS;
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
    drop(run);
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
    let settled_after = settle(tsc_hz);
    yield_until(back, tsc_hz, 2);
    let reaped = back() && settled_before.is_ok() && settled_after.is_ok();
    let now = crate::memory::stats().map_or(0, |(h, _)| h.bytes_in_use);
    // After the counts are read, so that the words' own allocation is not in them.
    let unsettled = not_settled(settled_before, settled_after);
    proofs.record(Proof::ThreadsReaped, reaped);
    let _ = writeln!(
        com1,
        "origin-kernel: scheduler: exited threads' stacks reaped on their own CPUs: heap {} KiB, {} KiB before the first spawn; {} stacks held, {stacks_before} before the first spawn{unsettled}{}",
        now >> 10,
        heap_before.unwrap_or(0) >> 10,
        stacks(),
        if reaped { "" } else { ": scheduler failed" }
    );
    // The framework's measured stack in a thread switched out on it (TASKS item 68): after the
    // comparison above has closed with a settle, so that it runs inside no proof's window of
    // stacks or frames (the review of TASKS item 68, T68-5).
    proofs.record(
        Proof::MeasuredStackThread,
        crate::measured_stack::prove_in_thread(com1, bsp),
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
