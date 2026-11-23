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
use core::marker::PhantomData;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::bootproof::{Proof, Tally};
use crate::runqueue::{
    PREEMPTION_ROUNDS, QueueError, ReadBack, RunQueue, TickWatch, WINDOW_PAUSE_SECONDS,
    WINDOW_RUNS_SECONDS, WindowVerdict, judge_window, per_mille, share_near_weight, window_again,
};

use crate::console::Console;
use crate::cpu::{self, CpuIndex, ThisCpu, rdtsc};
use crate::process::UserContext;
use crate::threads::{self, Placed};
use crate::timer::{self, X2Apic};
use origin_frame::objref::ObjectRef;
use origin_frame::percpu::PerCpu;
use origin_frame::stack::{FpuArea, GuardedStack, StackSlot, State as SlotState, ThreadStart};
use origin_frame::sync::SpinLock;
use origin_frame::sync::exits::{Exits, Pending};
use origin_frame::sync::threads::{Empty, Location, Met, Outcome, Queues, Record};
use origin_frame::trap::{MintHolder, Pinned, Switch};

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
pub(crate) static QUEUES: PerCpu<SpinLock<RunQueue<Placed>>> = PerCpu::new(
    "framekernel::sched::QUEUES",
    SpinLock::new(RunQueue::empty()),
    || SpinLock::new(RunQueue::empty()),
);
/// Each processor whose scheduler runs.
pub(crate) static ACTIVE: PerCpu<AtomicBool> =
    PerCpu::new("framekernel::sched::ACTIVE", AtomicBool::new(false), || {
        AtomicBool::new(false)
    });
/// Set by the boot processor once every AP's threads are queued.
static GO: AtomicBool = AtomicBool::new(false);

use crate::cpu::{without_interrupts, without_interrupts_here};

/// The scheduler's [`origin_frame::trap::PinMint`] (ADR-0020 S2, S3; TASKS item 93, the
/// coordinating session's ruling Q3). It is kept here, in a `static` no other module can name,
/// from before any thread exists ([`take_pin_mint`]), and is used only by [`PinGuard`], after the
/// running thread's pin is raised.
static PIN_MINT: MintHolder = MintHolder::new();

/// Takes the pin's mint. The kernel's entry calls this once, before any thread exists:
/// - a second take, by anyone, stops the kernel;
/// - so does a spawn or a pin before this (`MintHolder::mint`).
pub(crate) fn take_pin_mint() {
    PIN_MINT.keep(origin_frame::trap::take_pin_mint());
}

/// A pin of the running context to its processor (ADR-0020 S2, "The pin"): until it is dropped,
/// the context never moves, whatever it runs and however often it is switched out.
///
/// It is not the per-call pin of ADR-0005 Amendment 8, which `crate::pin` judges.
///
/// On a thread, it holds the count it raised in the thread's record, and lowers it when dropped.
/// On the boot or idle flow it holds nothing, since a flow never moves (S2, point 4).
///
/// It is not `Send`, so the thread that raised the pin is the one that lowers it. Its witness,
/// [`PinGuard::pinned`], is what a function whose body may be switched out asks for. Only
/// [`pin`] and [`end_with`] make one, each after raising the count.
pub(crate) struct PinGuard {
    /// The running thread's record, whose pin count this raised; `None` on the boot or idle flow.
    record: Option<&'static Record<threads::State>>,
    _not_send: PhantomData<*const ()>,
}

impl PinGuard {
    /// The pin's witness, for as long as the guard is borrowed.
    pub(crate) fn pinned(&self) -> Pinned<'_> {
        PIN_MINT.mint().pinned(self)
    }

    /// Keeps the pin for good, never lowered: an ending thread's (ADR-0020 S2, point 2).
    fn keep(self) -> Pinned<'static> {
        let pinned = PIN_MINT.mint().pinned_for_good();
        core::mem::forget(self);
        pinned
    }
}

impl Drop for PinGuard {
    fn drop(&mut self) {
        if let Some(record) = self.record
            && !record.unpin()
        {
            panic!("a processor pin was lowered that was never raised");
        }
    }
}

/// Pins the running context to its processor until the guard is dropped (ADR-0020 S2). One
/// interrupts-off section: it takes this processor's queue lock, finds the running slot's record,
/// and raises that thread's pin count (two plain moves, CL47). On the boot or idle flow, whose slot
/// has no record, it raises nothing. Not for the trap entry, which has its own witness.
///
/// Only slot 0, the boot or idle flow, runs without a record: every spawn queues one. A thread
/// in any other slot without one would hold a pin's witness that pinned nothing, so the kernel
/// stops (review finding R93-8).
pub(crate) fn pin() -> PinGuard {
    let record = without_interrupts_here(|here| {
        let me = cpu::this_cpu(here);
        let (running, placed) = {
            let q = QUEUES.this(me)?.lock();
            (q.current(), q.record(q.current()))
        };
        let Some(placed) = placed else {
            assert!(
                running == 0,
                "a thread without a record asked for a processor pin"
            );
            return None;
        };
        let record = placed.record();
        raise_pin(record);
        Some(record)
    });
    PinGuard {
        record,
        _not_send: PhantomData,
    }
}

/// Raises `record`'s pin count; the kernel stops at its largest, as a thread that thinks itself
/// pinned when it is not could act on another processor's state.
fn raise_pin(record: &Record<threads::State>) {
    assert!(record.pin(), "a processor pin count at its largest");
}

/// The timer interrupted the context whose frame is at `interrupted`; returns the frame to
/// resume. Processors without an active queue keep running what they were running.
pub(crate) fn on_tick(interrupted: u64, switch: &Switch) -> u64 {
    with_queue(interrupted, switch, |q, here| {
        // The preemption proof's watch (TASKS item 66): the TSC only while it is armed. The read
        // back takes the trap entry's witness by value, which is zero-sized, so the closure is as
        // the function item it replaces (review finding R93-9).
        if q.watching() {
            q.note_tick(rdtsc(), move || timer_read_back(here));
        }
        Some(q.tick(interrupted))
    })
}

/// This processor's timer, read back and compared with the boot processor's registers as its
/// timed proof ran them (TASKS item 66, ruling A on the review's T66-R1; the addendum review's
/// T66-R14). Unread if either cannot be had.
fn timer_read_back(here: origin_frame::trap::Here<'_>) -> ReadBack {
    match (timer::programmed(here), timer::timed_programming()) {
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
    if let Some(table) = threads::table(cpu.get()) {
        without_interrupts(|| {
            let mut t = table.lock();
            if t.idle_fpu.is_none() {
                t.idle_fpu = FpuArea::new();
            }
        });
    }
}

/// What a run-queue slot runs, as a switch sees it: the boot or idle flow (slot 0), whose FPU
/// area is the processor's own; a thread, whose state is in its record, which the slot holds,
/// resolved when the thread was placed ([`Placed`]); or nothing any more, a thread that has just
/// exited, whose slot names no record (its state stays in its record, a zombie's, until its reap,
/// or, when no zombie could be kept, was dropped and its record given back at once; the review of
/// TASKS item 92, R92-14). A non-zero slot that holds no record is `Exited` too, which the kernel
/// never makes: `follow_thread` counts it in `UNREADY_SWITCHES` (R92-18).
#[derive(Clone, Copy)]
enum Running {
    Flow,
    Thread(&'static Record<threads::State>),
    Exited,
}

/// What `slot` of `q` runs ([`Running`]): one read of the slot, no lookup.
#[inline]
fn running(q: &RunQueue<Placed>, slot: usize) -> Running {
    match (slot, q.record(slot)) {
        (0, _) => Running::Flow,
        (_, Some(placed)) => Running::Thread(placed.record()),
        (_, None) => Running::Exited,
    }
}

/// Prepares this processor to resume `new`: if the context changed (`restore`), its FPU/SIMD state
/// is restored, and the page tables are its process's (the kernel's for a kernel thread, and for
/// the boot or idle flow). For a thread, both under one lock of its record, taken after
/// [`with_queue`] released the outgoing context's lock: a switch takes two locks, as the base's
/// took its processor's thread table twice (TASKS item 92; the coordinating session's ruling on
/// its Q6 stop). For the boot or idle flow, the restore is under its processor's table lock and
/// the address space under none; for an exited slot, no lock is taken (the review's R92-11). The stack an interrupt or a system call from user mode lands on is not set here: the
/// framework's trap entry names the incoming context's own stack from its home in the processor's
/// table of contexts, whatever this scheduler answered (TASKS item 77).
#[inline]
fn follow_thread(me: ThisCpu<'_>, new: Running, restore: bool, switch: &Switch) {
    let record = match new {
        Running::Flow => {
            if restore
                && let Some(table) = threads::table_here(me)
                && let Some(area) = table.lock().idle_fpu.as_ref()
            {
                area.restore();
            }
            return crate::vm::activate_kernel_space(switch);
        }
        Running::Thread(record) => record,
        // Every thread but the boot or idle flow has a record with a stack: a ready slot without
        // one means a thread was made runnable before its state was written, which `spawn_with`
        // rules out by writing it under the queue lock. Counted for the boot proof.
        Running::Exited => {
            UNREADY_SWITCHES.fetch_add(1, Ordering::Relaxed);
            return crate::vm::activate_kernel_space(switch);
        }
    };
    let state = record.state().lock();
    if restore && let Some(area) = state.fpu.as_ref() {
        area.restore();
    }
    if state.stack.is_none() {
        UNREADY_SWITCHES.fetch_add(1, Ordering::Relaxed);
    }
    match state.user.as_ref() {
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
///
/// **What "seen" requires** (the review of TASKS item 79, T79-3): the settle is ordered after the
/// count only through the end itself. So `publish` must make the end visible with a Release write
/// (a Release store, an AcqRel read-modify-write, or a write under a lock it then releases), and a
/// thread that waits for the end must read it with Acquire (or under that lock), and settle after
/// that read, in the same thread. A Relaxed end or a Relaxed wait leaves the settle unordered with
/// the count, which the hardware happens to order on x86-64 and the compiler need not.
pub(crate) fn end_with<R>(publish: impl FnOnce() -> R) -> R {
    // The running thread's record, if it has not counted itself already, pinned for good in the
    // same section (ADR-0020 S2, point 2; N5): from here the thread never leaves this processor, so
    // both of its counts are this processor's. No other thread can take the record or its mark
    // while this one runs, so the mark is stored after `publish`.
    let record = without_interrupts_here(|here| {
        let me = cpu::this_cpu(here);
        EXITS.this(me)?;
        let record = {
            let q = QUEUES.this(me)?.lock();
            q.record(q.current())
        }?
        .record();
        if record.state().lock().ending.is_some() {
            return None;
        }
        raise_pin(record);
        Some(record)
    });
    let Some(record) = record else {
        return publish();
    };
    let pinned = PinGuard {
        record: Some(record),
        _not_send: PhantomData,
    }
    .keep();
    let Some(exits) = EXITS.this(cpu::this_cpu(pinned.here())) else {
        // The section found this processor's exits, and the thread has not moved since.
        return publish();
    };
    let (mark, published) = exits.end(publish);
    // A mark not kept is not consumed, so its count stays raised, which every later settle names;
    // it is never lowered early.
    without_interrupts(|| record.state().lock().ending = Some(mark));
    published
}

/// Frees the stacks of threads that exited on **this** processor. Only this processor may
/// do it, and only from thread context: by then its exit handler, which ran on the dead
/// stack, has certainly switched away. Returns how many stacks were freed.
///
/// Then it gives back what retired stacks hold (`stack::reclaim`): here, with no lock held, as
/// the shootdown that must come first waits for every processor (ADR-0004 Amendment 1, A2).
///
/// A caller with no witness of its own: a thread, in a system call's epilogue or a proof. Whether
/// this processor has a zombie is read in a section of its own (TASKS item 93, the ruling's Q8: a
/// stale index there would reap another processor's zombies, which only that processor may do);
/// only if it has one is the thread pinned, for the loop. The boot or idle flow calls
/// [`reap_with`] with its own witness.
pub(crate) fn reap() -> usize {
    let any = without_interrupts_here(|here| {
        EXITS
            .this(cpu::this_cpu(here))
            .is_some_and(|exits| exits.zombies() != 0)
    });
    let freed = if any {
        let pin = pin();
        reap_zombies(pin.pinned())
    } else {
        0
    };
    reclaim_stacks();
    freed
}

/// [`reap`], for a caller that stays on its processor while it runs: the boot or idle flow.
pub(crate) fn reap_with(pinned: Pinned<'_>) -> usize {
    let freed = reap_zombies(pinned);
    reclaim_stacks();
    freed
}

/// Reclaims every retired kernel stack it can (`stack::reclaim`), and says when a stack is given
/// back after a stalled shootdown had withheld it ([`report_given_back`]) and when a shootdown
/// stalls and withholds one ([`report_withheld`]): every reclaim the scheduler makes goes through
/// here, `settle`'s own among them.
fn reclaim_stacks() {
    let reclaimed = origin_frame::stack::reclaim();
    if let Some(back) = reclaimed.given_back {
        report_given_back(back);
    }
    if let Some(withheld) = reclaimed.stalled {
        report_withheld(withheld);
    }
}

/// Says when a kernel stack's shootdown stalls, as it happens: which processor asked, which did
/// not answer within the patience, how long it waited, and how many stalls the boot has had
/// (TASKS item 66). The stack is withheld, with its frames and slot, until a fresh shootdown
/// completes on every processor (TASKS item 73). A report, judged by nothing: the settles and
/// the proofs that count the stacks judge a stack still withheld when they read. It names
/// processors, counts and times only, never an address.
fn report_withheld(withheld: origin_frame::stack::Withheld) {
    let mut out = here_console();
    let millis = |ticks: u64| ticks / (crate::time::tsc_hz() / 1000).max(1);
    // Under the console's 160-byte line (the review's T66-R7). The stall named is the boot's
    // last: another processor's stall can come between this one and this line.
    let what = WithheldWords(withheld.asked_again);
    let _ = match origin_frame::tlb::last_stall() {
        Some(stall) => writeln!(
            out,
            "origin-kernel: stalled shootdown: CPU {} waited {} ms for CPU {} (stall {} of this boot); {what}",
            stall.asker,
            millis(stall.waited),
            stall.target,
            stall.count
        ),
        None => writeln!(
            out,
            "origin-kernel: stalled shootdown: CPU {} did not answer; {what}",
            withheld.stall.cpu
        ),
    };
}

/// What [`report_withheld`] says of the stack: withheld after its first shootdown stalled, or
/// still withheld after this many fresh ones.
struct WithheldWords(u32);

impl core::fmt::Display for WithheldWords {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            0 => f.write_str("a kernel stack withheld until every processor answers"),
            n => write!(
                f,
                "a withheld kernel stack asked again {n} times, still withheld"
            ),
        }
    }
}

/// Says when a kernel stack a stalled shootdown withheld is given back: a fresh shootdown of its
/// pages completed on every processor. How many fresh shootdowns it took, and how long the stack
/// was out of the tables before its frames went back (TASKS item 73). A report, judged by
/// nothing.
fn report_given_back(back: origin_frame::stack::GivenBack) {
    let millis = back.ticks / (crate::time::tsc_hz() / 1000).max(1);
    let _ = writeln!(
        here_console(),
        "origin-kernel: a kernel stack withheld by a stalled shootdown is given back: asked again {} times, {millis} ms after its unmap",
        back.asked_again
    );
}

/// A console for a line printed where the caller may be: the boot flow, which runs with
/// interrupts off and may wait for the UART, gets its own kind, so that the line is not dropped
/// (the review's T66-R7); anywhere else, the ordinary one.
fn here_console() -> Console {
    // The running slot and the processor read in the section that reads the slot (TASKS item 93).
    let boot_flow = || {
        without_interrupts_here(|here| {
            let me = cpu::this_cpu(here);
            // As `current_thread().is_none()` read it: no queue, or no record in the running slot.
            QUEUES.this(me).is_none_or(|queue| {
                let q = queue.lock();
                q.record(q.current()).is_none()
            }) && me.index() == CpuIndex::BOOT
        })
    };
    if !cpu::interrupts_enabled() && boot_flow() {
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
/// processor). Each of those moves the counts on its own schedule: on the 65-processor machine the
/// stacks that earlier proofs' threads left were still being reclaimed, one shootdown at a time,
/// when the IPC proof took its first count, so the count rose during the proof; and a cache of
/// kept stacks that refills during a proof lowers it.
/// A thread's end is seen before it has given anything back: a user thread's process says it has
/// ended, and a kernel thread raises its own done-flag, before the thread leaves its processor as
/// a zombie. So waiting for zombies alone found none for a thread still in that stretch, and the
/// count was read with its stack, FPU area and address space held (TASKS item 79: H1 when an
/// earlier proof's thread was there at a proof's first count, H2 when its own was at its compare).
/// Settled at both reads, the counts change only with what the proof does. Waits up to
/// [`OTHERS_PATIENCE`] seconds for each part; `Err` names what was still pending, and any
/// stack lost while it ran (`settled`), and the processor a stack withheld by a stalled
/// shootdown still waits for.
///
/// **Closed channels** (TASKS item 81). First, every closed channel's undelivered messages are
/// freed: those still listed by this flow's own [`crate::ipc::reap_closed`], and those another
/// processor took off the list by its own, for which it waits. A reap in a system call's epilogue
/// on another processor could otherwise still be freeing what an ended process's messages held
/// when a proof read its first count (the review of TASKS item 79, T79-6). It comes first because
/// a drain can wake a thread that then ends, whose end the exits wait after it then sees; were it
/// last, such an end would come after the exits wait and before the verdict, which would fail on
/// what the settle itself did (the review of TASKS items 75, 77 and 81, R81-2).
///
/// **Passes** (the coordinating session's rulings on R81-2). The drain and its wait, the exits
/// wait, the trim, the reclaim and the retired wait make one pass ([`settle_pass`]). After a pass
/// that left a closed channel pending, or a thread ending or a zombie, or whose own drain freed a
/// closed channel, the settle yields once and opens an interrupt window, so that a thread the
/// drain woke gets the processor, and makes another pass; up to [`crate::settled::MAX_PASSES`] in
/// all ([`crate::settled::again`]), then it judges. That also covers the reverse case: a thread
/// that ends during the exits wait lists and reaps its own channels in its `end_with`, but if
/// another processor had taken one of them first, that processor may still be freeing its
/// messages once the closed wait has passed; the next pass waits for it. A settle that is still
/// not settled after its last pass fails by name, as before.
///
/// Two limits. A thread a drain woke on another processor that has not ended by the next pass's
/// look is an end no one has seen, which a settle never promised to wait for (TASKS item 79
/// covers ends that were seen). And a drain in the third pass is followed by the verdict, with no
/// pass after it.
///
/// **Reaping closed channels here** (the coordinating session's addition (e)). The wait's closure
/// runs on the boot flow, which waits with interrupts off (an interrupt window between rounds),
/// and holds no lock: `wait_for_others` takes none, and `reap_closed` takes the list's lock and
/// each channel's message slot only to take from them, each with interrupts off, and drops the
/// messages with neither held. Dropping them can close channel ends (each end's state, its charge
/// list, the wait queues and port sources it wakes, and the list again), and free processes,
/// their address spaces and memory objects. Each of those takes its own lock only around its own
/// change, with interrupts off, and none waits for another processor: an address space's drop
/// leaks a space a processor still has loaded rather than shoot it down
/// (`origin_frame::userspace::UserSpace`'s `Drop`), and frees one no processor has loaded without
/// a shootdown. So nothing the closure frees waits on a processor that waits on the boot flow, and
/// a processor spinning on a lock the closure takes finds it released without help. Were a drop
/// to start a TLB shootdown, the protocol would still not deadlock here: the boot flow holds no
/// lock while it waits for answers, and a processor waiting for its turn to shoot serves the
/// request in progress (`origin_sync::shootdown`, "How a request travels"). Only the boot flow
/// settles, so no other processor is waiting in a settle meanwhile.
pub(crate) fn settle(tsc_hz: u64) -> Result<(), Unsettled> {
    let lost_before = origin_frame::stack::counts().lost;
    let mut passes = 0;
    loop {
        let drained = settle_pass(tsc_hz);
        passes += 1;
        let pending = crate::ipc::closed_pending() != 0 || !exits_pending().none();
        if !crate::settled::again(passes, pending, drained > 0) {
            break;
        }
        // A thread the drain woke gets the processor before the next pass looks for its end.
        yield_now();
        cpu::interrupt_window();
    }
    let counts = origin_frame::stack::counts();
    let exits = exits_pending();
    crate::settled::judge(
        exits.ending,
        exits.zombies,
        counts.retired,
        origin_frame::stack::waiting().map(|stall| stall.cpu),
        crate::ipc::closed_pending(),
        lost_before,
        counts.lost,
        wakes_followed(),
        threads::unaccounted(),
    )
}

/// One pass of [`settle`]: closed channels drained and their reaps waited for (TASKS item 81),
/// then ending threads and zombies, then the cache of kept stacks trimmed and every retired stack
/// reclaimed and waited for. Each wait up to [`OTHERS_PATIENCE`] seconds. Returns how many closed
/// channels this pass's own drain freed.
fn settle_pass(tsc_hz: u64) -> usize {
    reap();
    // The reap runs on the boot flow, with interrupts off and no lock held between messages, as
    // a proof's own `reap_closed` does; see `settle`'s "Reaping closed channels here".
    let drained = core::cell::Cell::new(0);
    wait_for_others(
        "closed channels' reaping",
        || {
            drained.set(drained.get() + crate::ipc::reap_closed());
            crate::ipc::closed_pending() == 0
        },
        tsc_hz,
        2000,
    );
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
    drained.get()
}

/// [`reap`]'s own processor's exited threads, while `pinned` keeps the caller on it.
fn reap_zombies(pinned: Pinned<'_>) -> usize {
    let me = cpu::this_cpu(pinned.here());
    let (Some(table), Some(exits)) = (threads::table_here(me), EXITS.this(me)) else {
        return 0;
    };
    if exits.zombies() == 0 {
        return 0;
    }
    let mut freed = 0;
    loop {
        // The zombie's state is taken out of its record, and the record given back, under the
        // list's lock; what it held is dropped below with no lock held. So a reap in progress
        // holds no record (`crate::threads`, "Enough").
        let next = without_interrupts(|| {
            let mut t = table.lock();
            let number = t.zombies.iter_mut().find_map(Option::take)?;
            let record = threads::record(number)?;
            let state = core::mem::replace(&mut *record.state().lock(), threads::State::EMPTY);
            // Its location was cleared at its exit, so the block takes it back.
            threads::give_back(number);
            Some(state)
        });
        let Some(threads::State {
            stack, fpu, user, ..
        }) = next
        else {
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
    f: impl FnOnce(&mut RunQueue<Placed>, origin_frame::trap::Here<'_>) -> Option<u64>,
) -> u64 {
    let me = cpu::this_cpu(switch.here());
    match (ACTIVE.this(me), QUEUES.this(me)) {
        (Some(active), Some(queue)) if active.load(Ordering::Acquire) => {
            let mut q = queue.lock();
            let old = q.current();
            let next = f(&mut q, switch.here()).unwrap_or(interrupted);
            // The preemption proof's rounds clause stamps its 15th switch after the step that
            // made it (TASKS item 66, the addendum review's T66-R15).
            if q.watching() {
                q.note_switches(rdtsc());
            }
            let new = q.current();
            // After `f`: an exited thread's slot names no record any more. The outgoing context's
            // FPU/SIMD state is saved, so SIMD registers belong to one thread at a time, under its
            // lock (its record's, or the boot or idle flow's table lock), which is released before
            // `follow_thread` takes the incoming context's, once: never two record locks at once
            // (the lock order of `origin_sync::threads`). An exited thread's state is not saved:
            // it is dead.
            let switched = new != old;
            if switched {
                match running(&q, old) {
                    Running::Flow => {
                        if let Some(table) = threads::table_here(me)
                            && let Some(area) = table.lock().idle_fpu.as_mut()
                        {
                            area.save();
                        }
                    }
                    Running::Thread(record) => {
                        if let Some(area) = record.state().lock().fpu.as_mut() {
                            area.save();
                        }
                    }
                    Running::Exited => {}
                }
            }
            follow_thread(me, running(&q, new), switched, switch);
            next
        }
        _ => interrupted,
    }
}

/// `YIELD_VECTOR`: the running context gives up the processor.
pub(crate) fn on_yield(interrupted: u64, switch: &Switch) -> u64 {
    with_queue(interrupted, switch, |q, _| Some(q.yield_now(interrupted)))
}

/// `SLEEP_VECTOR`: the running thread sleeps for `ticks` timer ticks.
pub(crate) fn on_sleep(interrupted: u64, ticks: u64, switch: &Switch) -> u64 {
    let next = with_queue(interrupted, switch, |q, _| q.sleep(interrupted, ticks).ok());
    // The kernel stacks proof holds one thread here, in its switch-out window (TASKS item 70).
    crate::stacks::hold_in_window(interrupted, next);
    next
}

/// `BLOCK_VECTOR`: the running thread blocks until [`wake`] (or goes on, if a wake-up is
/// already pending).
/// RDI holds the most ticks to wait (0: no limit).
pub(crate) fn on_block(interrupted: u64, ticks: u64, switch: &Switch) -> u64 {
    with_queue(interrupted, switch, |q, _| {
        q.block_for(interrupted, (ticks != 0).then_some(ticks)).ok()
    })
}

/// A device interrupt whose driver may have woken a waiter, or `RESCHEDULE_VECTOR` (another
/// processor made a thread here ready). The interrupted context is charged nothing: this
/// switches only if a thread that was woken should run now
/// ([`RunQueue::preempt_for_wake`]), and otherwise resumes what the interrupt interrupted.
pub(crate) fn on_reschedule(interrupted: u64, switch: &Switch) -> u64 {
    with_queue(interrupted, switch, |q, _| {
        Some(q.preempt_for_wake(interrupted))
    })
}

/// A thread: its record and the record's generation (ADR-0020 S1, TASKS item 92). A wake-up
/// finds the thread through its record wherever it is, and a name that outlives its thread never
/// names the record's next one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ThreadRef {
    record: u32,
    generation: u32,
}

/// The running thread, or `None` for the boot or idle flow (which cannot block).
pub(crate) fn current_thread() -> Option<ThreadRef> {
    without_interrupts_here(|here| {
        let q = QUEUES.this(cpu::this_cpu(here))?.lock();
        // Read under the queue's lock, as `origin_sync::threads::Record::generation` asks.
        let placed = q.record(q.current())?;
        Some(ThreadRef {
            record: placed.number(),
            generation: placed.record().generation(),
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

/// The processors' run queues as `sched::wake` hands them to `origin_sync::threads::wake`, inside
/// the one interrupts-off section `wake` runs in (TASKS item 93, the judge's stop of 2026-11-21 and
/// the coordinating session's ruling; ADR-0018 V1). Each queue is locked in that section, so
/// `locked` opens none of its own, and `wake` reads the processor index under the same section's
/// witness: one flag save, `cli` and restore for a wake-up, as before TASKS item 93.
mod waking {
    use super::{Placed, QUEUES, Queues, RunQueue, without_interrupts_here};
    use origin_frame::trap::Here;

    /// The run queues, lent for the life of the section [`within`] opens. Its field is private to
    /// this module, so only `within` makes one, and its `locked` always runs with interrupts off.
    pub(super) struct KernelQueues<'a> {
        _section: Here<'a>,
    }

    /// Runs `f` in one interrupts-off section, with the run queues as `origin_sync::threads::wake`
    /// takes them and the section's witness.
    pub(super) fn within<T>(f: impl for<'a> FnOnce(&KernelQueues<'a>, Here<'a>) -> T) -> T {
        without_interrupts_here(|here| f(&KernelQueues { _section: here }, here))
    }

    impl Queues for KernelQueues<'_> {
        type Queue = RunQueue<Placed>;

        // Inlined into the location loop, as before TASKS item 96: with the loop also in the
        // out-of-line retry after pinned mode, LLVM would otherwise make it a call on every
        // wake-up's look (the recount turn's figures).
        #[inline(always)]
        fn locked<R>(&self, cpu: u32, f: impl FnOnce(&mut RunQueue<Placed>) -> R) -> Option<R> {
            let queue = QUEUES.get(usize::try_from(cpu).ok()?)?;
            // Interrupts are off: only `within`'s section makes `self`.
            Some(f(&mut queue.lock()))
        }

        #[inline(always)]
        fn holds<S>(
            queue: &RunQueue<Placed>,
            slot: u16,
            _number: u32,
            record: &origin_frame::sync::threads::Record<S>,
        ) -> bool {
            // ADR-0020 S20, trigger 3 (TASKS item 96, the coordinating session's ruling Q6): the
            // slot holds this very record, compared by address with the one the wake-up has, read
            // without the slot's state (`RunQueue::held`). On a wake-up that meets no
            // inconsistency, that load and compare and their branch are all it adds (CL47).
            queue.held(usize::from(slot)).is_some_and(|placed| {
                core::ptr::eq(
                    core::ptr::from_ref(placed.record()).cast::<()>(),
                    core::ptr::from_ref(record).cast::<()>(),
                )
            })
        }

        fn wake_slot(queue: &mut RunQueue<Placed>, slot: u16) -> bool {
            let slot = usize::from(slot);
            let generation = queue.generation(slot);
            queue.wake(slot, generation)
        }

        fn find(queue: &RunQueue<Placed>, number: u32) -> Option<u16> {
            (1..crate::runqueue::CAPACITY)
                .find(|&slot| {
                    queue
                        .record(slot)
                        .is_some_and(|placed| placed.number() == number)
                })
                .and_then(|slot| u16::try_from(slot).ok())
        }

        fn count(&self) -> u32 {
            u32::try_from(QUEUES.indexes()).unwrap_or(u32::MAX)
        }
    }
}

/// ADR-0020 S20's pinned mode (TASKS item 96; the contract's §2): once an inconsistency is met
/// that a move or a wake-up can stop safely at, moves are withdrawn for the rest of the boot.
/// Nothing leaves it. Every mover reads [`withdrawn`] under its source's queue lock, and the
/// first trigger is named on one line, by whoever entered it, after its locks are released, with
/// interrupts as its caller had them. Later triggers are counted.
///
/// The kernel stops only for what S20 keeps the stop for: an inconsistency that could have let
/// two processors run one context, or given back a stack in use. A wake-up exhausted a second
/// time, with moves withdrawn, means the flag itself failed, and stops too (the ruling's Q17).
pub(crate) mod pinned {
    use core::fmt::Write;
    use core::sync::atomic::{AtomicU8, AtomicU64, Ordering};

    use origin_frame::sync::threads::{Met, Outcome};

    use crate::cpu::CpuIndex;

    /// What entered pinned mode (S20's triggers; the contract's §2).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Trigger {
        /// "thread migration: delivery" failed (trigger 1).
        DeliveryFailed,
        /// A move's `lend` found no frame where its checks had found it published (trigger 2).
        LendLost,
        /// A location named a queue whose slot did not hold its thread (trigger 3).
        Misplaced,
        /// A wake-up followed its thread `WAKE_TRIES` times (trigger 4).
        Exhausted,
    }

    impl Trigger {
        const fn code(self) -> u8 {
            match self {
                Self::DeliveryFailed => 1,
                Self::LendLost => 2,
                Self::Misplaced => 3,
                Self::Exhausted => 4,
            }
        }

        const fn from_code(code: u8) -> Option<Self> {
            match code {
                1 => Some(Self::DeliveryFailed),
                2 => Some(Self::LendLost),
                3 => Some(Self::Misplaced),
                4 => Some(Self::Exhausted),
                _ => None,
            }
        }

        /// What the line calls it, with no address (ADR-0004 Amendment 2, A6). Short enough that
        /// every line, with its longest `then`, fits the console's line of 160 bytes (the review
        /// of TASKS item 96, R96-9): at most 153 with a CPU of three digits.
        const fn named(self) -> &'static str {
            match self {
                Self::DeliveryFailed => "thread migration: delivery failed",
                Self::LendLost => "a move's lend found no published frame",
                Self::Misplaced => "a wake-up's location named the wrong slot",
                Self::Exhausted => "a wake-up followed its thread 64 times",
            }
        }
    }

    impl From<Met> for Trigger {
        fn from(met: Met) -> Self {
            match met {
                Met::Exhausted => Self::Exhausted,
                Met::Misplaced => Self::Misplaced,
            }
        }
    }

    /// 0 while moves are on; otherwise the first trigger's code.
    static FIRST: AtomicU8 = AtomicU8::new(0);
    /// Triggers met after the first.
    static LATER: AtomicU64 = AtomicU64::new(0);

    /// Enters pinned mode for `trigger`: `true` if this call entered it, and so owes the line
    /// ([`line`]); `false` if it was entered already, and the trigger is counted. A
    /// compare-and-swap from 0, so the first trigger is the one named.
    pub(crate) fn enter(trigger: Trigger) -> bool {
        if FIRST
            .compare_exchange(0, trigger.code(), Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            true
        } else {
            LATER.fetch_add(1, Ordering::Relaxed);
            false
        }
    }

    /// Whether moves are withdrawn. A mover reads it under its source's queue lock, and holds that
    /// lock to its move's end (`sched::migrate`).
    pub(crate) fn withdrawn() -> bool {
        FIRST.load(Ordering::Acquire) != 0
    }

    /// The first trigger, if pinned mode was entered, and the triggers met after it.
    pub(crate) fn entered() -> Option<(Trigger, u64)> {
        Trigger::from_code(FIRST.load(Ordering::Acquire))
            .map(|trigger| (trigger, LATER.load(Ordering::Relaxed)))
    }

    /// The line that names the first trigger, by the caller that entered pinned mode, after the
    /// queue locks it took are released. `then` says what it did next: for a misplaced location,
    /// what the one search found.
    ///
    /// A wake-up writes it inside its interrupts-off section, out of line (a change from TASKS item
    /// 96's contract, §2, which had it after the section; ruled by the coordinating session): the
    /// console queues a line written with interrupts off and pumps the UART once, as it does for
    /// an interrupt handler's line, so nothing waits. A full queue drops it, as it drops any line
    /// written so, and counts the bytes it dropped (`console`'s `note_lost`; R96-9): the line is
    /// kept short, within the console's line, so it goes in one send.
    ///
    /// **Nesting** (the ruling's first condition). No run-queue lock is held here:
    /// `origin_sync::threads::wake` calls its `report` hook after its retry or its search has
    /// returned, and each queue lock is taken and released inside one `Queues::locked` call. The
    /// console's lock (`console::SINKS`) is a leaf:
    /// - every holder takes it with interrupts off, so no interrupt handler on the same
    ///   processor runs while it is held;
    /// - every holder does only the console's own work under it (its queues, its UARTs, its
    ///   framebuffer), and never takes a run-queue lock or calls `wake` there. That covers the
    ///   ordinary writers, the urgent writer of panics and exception reports, and `init`, the
    ///   SPCR move and the loader's UART, which run before any thread exists.
    ///
    /// So neither lock is ever taken inside the other, in either order.
    ///
    /// **The writer.** `Console::new`, never `here_console`: on the boot flow with interrupts off,
    /// `here_console` gives the boot flow's writer, which may open an interrupt window between a
    /// line's sections, and that must not happen inside the wake-up's section.
    ///
    /// **A panic right after** (the ruling's second condition). The panic handler's urgent writer
    /// hands its bytes to the same queue behind this line and pumps the UART until its own bytes
    /// are out, so this line goes out first. Only a holder stuck with the lock makes it write
    /// around the queue.
    #[cold]
    #[inline(never)]
    pub(crate) fn line(trigger: Trigger, on: CpuIndex, then: &str) {
        let _ = writeln!(
            crate::console::Console::new(),
            "origin-kernel: scheduler: pinned mode: moves withdrawn: {} on CPU {on}{then}",
            trigger.named()
        );
    }

    /// What a wake-up that met `met` did next, for its line: where it reached its thread, if it did.
    pub(crate) fn then_woke(met: Met, outcome: Outcome) -> &'static str {
        match (met, outcome) {
            (Met::Misplaced, Outcome::Reached { .. }) => "; the search woke it in its slot",
            (Met::Misplaced, Outcome::Stale) => "; the search found its record reused",
            (Met::Misplaced, _) => "; the search found its record in no queue",
            (Met::Exhausted, Outcome::Reached { .. }) => {
                "; with moves withdrawn it reached the thread"
            }
            (Met::Exhausted, Outcome::Exhausted) => {
                "; 64 more with moves withdrawn: the kernel stops"
            }
            // The second loop met a misplaced location, and the search found the record nowhere
            // (R96-10).
            (Met::Exhausted, Outcome::Misplaced) => {
                "; with moves withdrawn its record was in no queue"
            }
            (Met::Exhausted, Outcome::Stale | Outcome::Gone) => {
                "; with moves withdrawn its thread had gone"
            }
        }
    }
}

/// Moves a wake-up followed of threads not declared movable: times it found, under the lock of the
/// queue it had read, its thread's location changed to another slot with the same generation.
/// Such a thread never moves (TASKS item 96: only a movable thread does), so a follow of one is an
/// inconsistency, and the IPC proof and every settle require 0 (TASKS item 92, the coordinating
/// session's ruling Q5; TASKS item 96's Q8). Follows of movable threads are counted by the waker's
/// processor (`threads::Follows`).
static WAKES_FOLLOWED: AtomicU64 = AtomicU64::new(0);

/// How many moves wake-ups have followed ([`WAKES_FOLLOWED`]).
pub(crate) fn wakes_followed() -> u64 {
    WAKES_FOLLOWED.load(Ordering::Relaxed)
}

/// Moves wake-ups followed of threads that ended, their records going to new threads, before the
/// follows were counted ([`followed`]): neither a movable thread's nor another's, since only the
/// followed thread's own generation tells which it was (the review of TASKS item 96, R96-5). The
/// delivery proof reports them; no settle judges them.
static FOLLOWS_UNCLASSED: AtomicU64 = AtomicU64::new(0);

/// How many follows were of threads gone before they were counted ([`FOLLOWS_UNCLASSED`]).
pub(crate) fn follows_unclassed() -> u64 {
    FOLLOWS_UNCLASSED.load(Ordering::Relaxed)
}

/// Makes `thread` ready, wherever it is (`origin_sync::threads::wake`). If it was blocked on
/// another processor, that processor is sent a reschedule IPI (to its APIC ID), so it switches to
/// the thread now rather than at its next timer tick. Returns whether an IPI was sent, so a
/// wake-up across processors can be counted.
///
/// An inconsistency it meets enters pinned mode ([`pinned`]; ADR-0020 S20): a location that named
/// a slot not holding its thread (trigger 3), or 64 follows (trigger 4), after which it looks again
/// with moves withdrawn; a second exhaustion stops the kernel. A wake-up that meets none pays the
/// record check alone (CL47); the rest is out of line.
///
/// **Interrupts off** (ADR-0020 S1's note of 2026-11-22; the coordinating session's ruling Q18):
/// the wake-up, the compare and the IPI run in one interrupts-off section. Each move it follows
/// adds about 2.5 µs on the budgets' figures (the rest of one move, B20, and one look). Paced as
/// `migrate` requires, a wake-up follows at most one; a caller that broke that rule could make it
/// follow 64, about 160 µs, which exceeds B18's 50 µs. The delivery proof measures the longest.
pub(crate) fn wake(thread: ThreadRef) -> bool {
    let Some(record) = threads::record(thread.record) else {
        return false;
    };
    // The wake-up, the compare and the IPI in one section (TASKS item 93, the ruling's Q8, and
    // the judge's stop): with a stale index, a waker that had left `cpu` would send no IPI, and
    // the woken thread would wait for that processor's next tick. The queues are locked in the
    // same section (`waking`), so a wake-up opens one section, as before TASKS item 93.
    waking::within(|queues, here| {
        // Only the location loop is on this path (TASKS item 96, the coordinating session's
        // ruling, ADR-0018 V1): an inconsistency goes on out of line, in `wake_inconsistent`, so
        // that no answer of the out-of-line paths merges into this one's.
        // The record's number goes to `follow` and no further: the kernel's queues compare the
        // record by address (`KernelQueues::holds`), and the out-of-line path finds the number
        // from the record, so no register keeps it through the loop.
        let woke =
            origin_frame::sync::threads::follow(queues, record, thread.record, thread.generation);
        match woke.outcome {
            Outcome::Exhausted => {
                return wake_inconsistent(
                    queues,
                    record,
                    thread.generation,
                    Met::Exhausted,
                    woke.retries,
                    here,
                );
            }
            Outcome::Misplaced => {
                return wake_inconsistent(
                    queues,
                    record,
                    thread.generation,
                    Met::Misplaced,
                    woke.retries,
                    here,
                );
            }
            Outcome::Reached { .. } | Outcome::Stale | Outcome::Gone => {}
        }
        if woke.retries != 0 {
            followed(record, thread.generation, woke.retries, here, true);
        }
        match woke.outcome {
            Outcome::Reached { ready: true, cpu } => ipi_if_elsewhere(cpu, here),
            Outcome::Reached { ready: false, .. }
            | Outcome::Stale
            | Outcome::Gone
            | Outcome::Exhausted
            | Outcome::Misplaced => false,
        }
    })
}

/// The reschedule IPI to processor `cpu`, which a wake-up made a thread ready on, if it is not
/// this one: whether one was sent.
#[inline(always)]
fn ipi_if_elsewhere(cpu: u32, here: origin_frame::trap::Here<'_>) -> bool {
    let Some(cpu) = usize::try_from(cpu).ok().and_then(CpuIndex::new) else {
        return false;
    };
    let me = cpu::this_cpu(here);
    if cpu != me.index()
        && let Some(apic_id) = cpu.apic_id()
    {
        timer::send_reschedule_ipi(me, apic_id);
        return true;
    }
    false
}

/// [`wake`] after its location loop met an inconsistency (ADR-0020 S20, triggers 3 and 4), out of
/// line and in its section: pinned mode's two hooks, the retry or the search
/// (`origin_sync::threads::after_inconsistency`), the moves followed, and the IPI. The line that
/// names the trigger is written there, inside the section, as an interrupt handler's line is
/// (`pinned::line`). A second exhaustion, with moves withdrawn, means the flag itself failed, and
/// the kernel stops (the ruling's Q17).
///
/// It is given the record and the generation, and finds the record's number from the record
/// (`threads::number_of`), so that [`wake`]'s location loop keeps no register for the number. A
/// record is always one block's, since [`wake`] has it from `threads::record`; one that is not
/// stops the kernel.
#[cold]
#[inline(never)]
fn wake_inconsistent<'a>(
    queues: &waking::KernelQueues<'a>,
    record: &Record<threads::State>,
    generation: u32,
    met: Met,
    retries: u32,
    here: origin_frame::trap::Here<'a>,
) -> bool {
    let Some(number) = threads::number_of(record) else {
        panic!("a wake-up's record is in no processor's block");
    };
    let woke = origin_frame::sync::threads::after_inconsistency(
        queues,
        record,
        number,
        generation,
        met,
        retries,
        |met| pinned::enter(met.into()),
        |met, outcome| {
            pinned::line(
                met.into(),
                cpu::this_cpu(here).index(),
                pinned::then_woke(met, outcome),
            );
        },
    );
    if woke.retries != 0 {
        followed(record, generation, woke.retries, here, false);
    }
    match woke.outcome {
        Outcome::Reached { ready: true, cpu } => ipi_if_elsewhere(cpu, here),
        Outcome::Exhausted => panic!(
            "a wake-up followed its thread {} times, {} of them with moves withdrawn",
            woke.retries,
            origin_frame::sync::threads::WAKE_TRIES
        ),
        Outcome::Reached { ready: false, .. }
        | Outcome::Stale
        | Outcome::Gone
        | Outcome::Misplaced => false,
    }
}

/// The moves a wake-up of the thread `record` and `generation` name followed, out of line (TASKS
/// item 96, the coordinating session's rulings Q8 and Q13): of a movable thread, by this
/// processor's count (`threads::Follows`), which the delivery proof reports; of any other,
/// [`WAKES_FOLLOWED`], an inconsistency the settles judge. In the wake-up's section, under its
/// witness.
///
/// **Whose flag** (the review of TASKS item 96, R96-5). Read after the location loop, with no
/// lock, the movable flag may already be a new thread's: the followed one may have ended and its
/// record gone to another. So the generation is read before the flag and after it, and both must
/// be the name's. `Block::take` stores a new thread's flag with Release after its new generation,
/// and `Record::movable` loads it with Acquire, so a flag read from a new thread comes with the new
/// generation. Follows whose thread is gone are counted apart ([`FOLLOWS_UNCLASSED`]).
///
/// **Pacing.** On a wake-up's common path (`paced`), a debug build also stops on more than 8
/// follows of a movable thread, which pacing rules out; every build reports them. Not after an
/// inconsistency, where the loop has run its `WAKE_TRIES` and S20 goes on pinned (R96-7).
#[cold]
#[inline(never)]
fn followed(
    record: &Record<threads::State>,
    generation: u32,
    retries: u32,
    here: origin_frame::trap::Here<'_>,
    paced: bool,
) {
    let movable = (record.generation() == generation)
        .then(|| record.movable())
        .filter(|_| record.generation() == generation);
    match movable {
        Some(true) => {
            debug_assert!(
                !paced || retries <= 8,
                "a wake-up followed a movable thread {retries} times: moves were not paced"
            );
            if let Some(follows) = threads::follows_here(cpu::this_cpu(here)) {
                follows.note(retries);
            }
        }
        Some(false) => {
            WAKES_FOLLOWED.fetch_add(u64::from(retries), Ordering::Relaxed);
        }
        None => {
            FOLLOWS_UNCLASSED.fetch_add(u64::from(retries), Ordering::Relaxed);
        }
    }
}

/// Why [`migrate`] moved nothing (TASKS item 96; the contract's §1.1 and §1.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unmoved {
    /// The thread is on the destination already.
    AlreadyThere,
    /// Its record's location is clear: it has exited, or was never queued.
    Gone,
    /// The record holds another thread: the name's has been reaped.
    Stale,
    /// Moves are withdrawn (pinned mode), or the thread may not move: not declared movable, or its
    /// pin count is above 0 (ADR-0020 S2, S20).
    Pinned,
    /// It is running, asleep, blocked or exiting: only a ready thread that is not running moves
    /// (the review of TASKS item 95, R95-5).
    NotReady,
    /// A processor does not take part in moves: not online, or its FPU format differs (ADR-0020
    /// S6, N9).
    FpuFormat,
    /// The destination's run queue has no free slot (N13).
    QueueFull,
    /// The destination's table of contexts had no free slot: the frame went back to its source.
    TableFull,
    /// Its frame is still being switched out (`NotYet` or `Leaving`; ADR-0020 S5): try later.
    TryLater,
    /// Its location changed at each of `WAKE_TRIES` looks, another mover moving it: try later.
    Moving,
    /// An inconsistency (ADR-0020 S20, triggers 2 and 3): pinned mode was entered, and the thread
    /// stays where it is.
    Inconsistent,
}

/// What a [`migrate`] did, for the proofs' counts.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Moved {
    /// The processor it left.
    pub(crate) from: CpuIndex,
    /// Whether the thread carried a pending wake-up (N1).
    pub(crate) carried_wake: bool,
    /// The move's critical section in TSC ticks, from before its first queue lock to after its
    /// last unlock (B20, a hypothesis of 2 µs; measured by the integrity proof).
    pub(crate) ticks: u64,
}

/// Whether processor `cpu`'s run queue has a free slot: the integrity proof's capacity phase
/// fills a queue and checks it is full (ADR-0020 N13; the review of TASKS item 96, R96-13).
pub(crate) fn has_room(cpu: CpuIndex) -> bool {
    QUEUES
        .get(cpu.get())
        .is_some_and(|queue| without_interrupts(|| queue.lock().has_room()))
}

/// The state of the thread `thread` names, for a migration proof's line when it fails (TASKS
/// item 96; the coordinating session's ruling on the gates' stop of 2026-11-22): its record's
/// number and generation against the name's, whether it is movable, its pin count, its location,
/// and its slot as the queue there holds it (`RunQueue::describe`), with whether that slot holds
/// this very record (`RunQueue::held`, compared by address as `KernelQueues::holds` compares it):
/// "held matches", "held is" another record's number, or "held none". That tells a line read while
/// a hand-off is in flight from a thread that is stuck where it is (the coordinating session's
/// ruling on the second stop, 2026-11-22). No address. Read only, under the queue's lock; no path
/// but a failing proof's calls it.
pub(crate) fn describe_thread(thread: ThreadRef) -> alloc::string::String {
    let Some(record) = threads::record(thread.record) else {
        return alloc::format!("record {} none", thread.record);
    };
    let at = record.location();
    let mut out = alloc::format!(
        "record {} generation {} (named {}), movable {}, pins {}",
        thread.record,
        record.generation(),
        thread.generation,
        record.movable(),
        record.pins()
    );
    match at {
        Some(at) => {
            let slot = QUEUES
                .get(usize::try_from(at.cpu()).unwrap_or(usize::MAX))
                .map(|queue| {
                    without_interrupts(|| {
                        let queue = queue.lock();
                        let slot = usize::from(at.slot());
                        (queue.describe(slot), queue.held(slot))
                    })
                });
            match slot {
                Some((s, held)) => {
                    out.push_str(&alloc::format!(
                        ", on CPU {} slot {}: {}, pending {}, woken {}, pass {}, floor {}, current slot {}",
                        at.cpu(),
                        at.slot(),
                        s.state,
                        s.pending,
                        s.woken,
                        s.pass,
                        s.floor,
                        s.current
                    ));
                    match held {
                        Some(placed) if core::ptr::eq(placed.record(), record) => {
                            out.push_str(", held matches");
                        }
                        Some(placed) => {
                            out.push_str(&alloc::format!(", held is {}", placed.number()));
                        }
                        None => out.push_str(", held none"),
                    }
                }
                None => out.push_str(&alloc::format!(", on CPU {} (no such queue)", at.cpu())),
            }
        }
        None => out.push_str(", located nowhere"),
    }
    out
}

/// Every processor's run queue, for the same lines ([`describe_thread`]): its ready slots, its
/// current slot, whether slot 0 is its idle flow, and the contexts it holds. Read only.
pub(crate) fn describe_cpus() -> alloc::string::String {
    let mut out = alloc::string::String::new();
    for cpu in 0..QUEUES.indexes() {
        if let Some(queue) = QUEUES.get(cpu) {
            let (ready, current, idle, live) = without_interrupts(|| queue.lock().describe_queue());
            out.push_str(&alloc::format!(
                "{}CPU {cpu}: {ready} ready, current slot {current}, idle {idle}, {live} contexts",
                if out.is_empty() { "" } else { "; " }
            ));
        }
    }
    out
}

/// Whether the thread `thread` names is alive and queued on processor `cpu`: the capacity phase's
/// check that a refused move left it where it was (R96-13).
pub(crate) fn queued_on(thread: ThreadRef, cpu: CpuIndex) -> bool {
    threads::record(thread.record).is_some_and(|record| {
        record.generation() == thread.generation
            && record
                .location()
                .is_some_and(|at| usize::try_from(at.cpu()).ok() == Some(cpu.get()))
    })
}

/// Whether the user thread `thread` is blocked on processor `cpu`: a slot of that processor's run
/// queue holds it and is blocked, read under the queue's lock. That is the one state in which a
/// wake-up makes the thread ready and, from another processor, sends `cpu` a reschedule IPI
/// ([`wake`]). A thread that has only entered a wait, or registered for it, is not there yet: a
/// wake-up that finds it so is kept by its queue, and no IPI is sent.
///
/// For a boot proof that starts a waker only once its peer has blocked, so that whether the
/// wake-up crosses processors does not depend on the schedule (TASKS item 134; ADR-0013 R1(b)).
/// It holds for a thread that stays on `cpu`: one not declared movable. Read only. The record's
/// state is locked under the queue's lock, the order a spawn takes them in. No switch, wake-up or
/// block calls it.
pub(crate) fn user_thread_blocked(
    cpu: CpuIndex,
    thread: &ObjectRef<crate::process::ThreadObject>,
) -> bool {
    QUEUES.get(cpu.get()).is_some_and(|queue| {
        without_interrupts(|| {
            let queue = queue.lock();
            (1..crate::runqueue::CAPACITY).any(|slot| {
                queue.is_blocked(slot)
                    && queue.record(slot).is_some_and(|placed| {
                        placed
                            .record()
                            .state()
                            .lock()
                            .user
                            .as_ref()
                            .is_some_and(|user| ObjectRef::same(&user.thread, thread))
                    })
            })
        })
    })
}

/// Moves `thread` to processor `to` (ADR-0020 S6; TASKS item 96): a movable thread, ready and not
/// running, whose pin count is 0, between two processors that take part in moves (`online`).
///
/// **The order** (S6; the contract's §1.1), in one interrupts-off section whose token the move gate
/// requires: the location read; both queue locks, in index order; the location read again (a move
/// in between is followed, at most `WAKE_TRIES` times); pinned mode checked under the source's
/// lock; eligibility; the gate (`cpu::move_context`), under which the frame is lent from the
/// source's table of contexts, adopted by the destination's, the thread departs its source queue
/// with its lag and pending wake-up, arrives at the destination's, and its location follows, and
/// the source forgets the home; the locks released; one reschedule IPI to a destination that is
/// not this processor (the ruling's Q11). Under the gate it takes no lock and drops no stack.
///
/// **Why the reads under the source's lock suffice** (the review of TASKS item 93, R93-7): the pin
/// count and the movable flag are written only by the running thread and by its spawner before its
/// first push, and the mover acts only on a thread ready and not running under that lock, whose
/// last write came before the switch-out its scheduler made under the same lock.
///
/// **Pacing** (S1, S10; the coordinating session's rulings Q13 and Q18): a caller moves a thread at
/// most once per tick of the processor it runs on. S1's bound of 64 follows on a wake-up rests on
/// that rate: paced, a wake-up follows at most one move, about 2.5 µs with interrupts off; a
/// caller that broke the rule could make one follow 64, about 160 µs, over B18's 50 µs. TASKS item
/// 96 gives `migrate` no rate of its own; its only callers, the migration proofs, pace themselves.
///
/// An inconsistency met (S20, triggers 2 and 3) enters pinned mode, and its line is printed after
/// the section; `NotRestored`, a frame that could not be put back, stops the kernel (the ruling's
/// Q7: two processors could have run one context). Called from thread context, with interrupts on,
/// or from the boot flow, with them off: its section saves and restores them.
pub(crate) fn migrate(
    thread: ThreadRef,
    to: CpuIndex,
    online: &crate::smp::Online,
) -> Result<Moved, Unmoved> {
    let Some(record) = threads::record(thread.record) else {
        return Err(Unmoved::Gone);
    };
    let (result, owed) = cpu::without_interrupts_off(|off| {
        let mut owed = None;
        let mut result = Err(Unmoved::Moving);
        for _ in 0..origin_frame::sync::threads::WAKE_TRIES {
            match move_once(off, record, thread, to, online, &mut owed) {
                Err(Unmoved::Moving) => {}
                done => {
                    result = done;
                    break;
                }
            }
        }
        if result.is_ok() {
            let me = cpu::this_cpu(off.here());
            if to != me.index()
                && let Some(apic_id) = to.apic_id()
            {
                timer::send_reschedule_ipi(me, apic_id);
            }
        }
        (result, owed)
    });
    if let Some((trigger, on)) = owed {
        pinned::line(trigger, on, "; the thread stays where it is");
    }
    result
}

/// One try of [`migrate`]: `Moving` if the location changed between its read and the locks.
fn move_once(
    off: origin_frame::trap::InterruptsOff<'_>,
    record: &'static Record<threads::State>,
    thread: ThreadRef,
    to: CpuIndex,
    online: &crate::smp::Online,
    owed: &mut Option<(pinned::Trigger, CpuIndex)>,
) -> Result<Moved, Unmoved> {
    use origin_frame::sync::contexts::{AdoptRefused, LendRefused, MoveRefused};

    let me = cpu::this_cpu(off.here()).index();
    let mut enter = |trigger| {
        if pinned::enter(trigger) {
            *owed = Some((trigger, me));
        }
        Err(Unmoved::Inconsistent)
    };
    let at = record.location().ok_or(Unmoved::Gone)?;
    let from = usize::try_from(at.cpu())
        .ok()
        .and_then(CpuIndex::new)
        .ok_or(Unmoved::Gone)?;
    if from == to {
        return Err(Unmoved::AlreadyThere);
    }
    let to_index = u32::try_from(to.get()).map_err(|_| Unmoved::Gone)?;
    let (source_queue, destination_queue) = (
        QUEUES.get(from.get()).ok_or(Unmoved::Gone)?,
        QUEUES.get(to.get()).ok_or(Unmoved::Gone)?,
    );
    let start = rdtsc();
    // Both queue locks, in index order (`origin_sync::threads`, "Locks").
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
    if record.location() != Some(at) {
        return Err(Unmoved::Moving);
    }
    if record.generation() != thread.generation {
        return Err(Unmoved::Stale);
    }
    // Under the source's lock (S20; the contract's §1.1, step 4).
    if pinned::withdrawn() {
        return Err(Unmoved::Pinned);
    }
    let slot = usize::from(at.slot());
    // (a) The slot holds this thread: otherwise S20's trigger 3.
    if source.record(slot).map(Placed::number) != Some(thread.record) {
        return enter(pinned::Trigger::Misplaced);
    }
    // (b) Ready and not running (R95-5): its frame, which the source's scheduler cannot have named.
    let Some(frame) = source.frame(slot) else {
        return Err(Unmoved::NotReady);
    };
    // (c) Declared movable, and not pinned (S2; R93-7).
    if !record.movable() || record.pins() != 0 {
        return Err(Unmoved::Pinned);
    }
    // (d) Both processors take part (N9).
    if !(online.takes_part_in_moves(from) && online.takes_part_in_moves(to)) {
        return Err(Unmoved::FpuFormat);
    }
    // (e) The destination has room (N13), so `arrive` cannot be refused once the frame is adopted.
    if !destination.has_room() {
        return Err(Unmoved::QueueFull);
    }
    let moved = cpu::move_context(off, from, to, frame, &mut |_home| {
        // Under both queue locks and the gate: nothing has changed since the checks above.
        let Some(departed) = source.depart(slot) else {
            panic!("a thread checked ready under its queue's lock could not depart");
        };
        let carried = departed.wake_pending();
        let Ok(arrived) = destination.arrive(departed) else {
            panic!("a thread could not arrive at a queue checked to have room");
        };
        let Ok(arrived) = u16::try_from(arrived) else {
            panic!("a run-queue slot past a location's 16 bits");
        };
        record.set_location(Location::new(to_index, arrived));
        carried
    });
    drop((low, high));
    let ticks = rdtsc().saturating_sub(start);
    match moved {
        Some(Ok((_, carried_wake))) => Ok(Moved {
            from,
            carried_wake,
            ticks,
        }),
        Some(Err(MoveRefused::Lend(LendRefused::NotYet | LendRefused::Leaving))) => {
            Err(Unmoved::TryLater)
        }
        Some(Err(MoveRefused::Adopt(AdoptRefused::Full))) => Err(Unmoved::TableFull),
        // ADR-0020 S20, trigger 2: its checks found the frame published, and `lend` did not.
        // `SameTable` and `None` (no table for a processor) cannot happen, since `from` and `to`
        // differ and every processor has a table; were one met, it is answered as the same safe
        // stop rather than a panic, though the line then names trigger 2 for it (the review of
        // TASKS item 96, R96-16).
        Some(Err(
            MoveRefused::Lend(LendRefused::NotHere | LendRefused::BootFlow)
            | MoveRefused::Adopt(AdoptRefused::SameTable),
        ))
        | None => enter(pinned::Trigger::LendLost),
        // The ruling's Q7: the frame is in no table, and two processors could have run it.
        Some(Err(MoveRefused::NotRestored)) => {
            panic!("a lent frame could not be put back in its source's table")
        }
    }
}

/// `EXIT_VECTOR`: the running thread ends. Its record, holding its stack, becomes a zombie, which
/// [`reap`] empties and gives back later on this processor.
pub(crate) fn on_exit(interrupted: u64, switch: &Switch) -> u64 {
    let me = cpu::this_cpu(switch.here());
    let next = with_queue(interrupted, switch, |q, _| {
        let placed = q.record(q.current());
        let next = q.exit(interrupted).ok()?;
        if let (Some(table), Some(placed)) = (threads::table_here(me), placed) {
            let (number, record) = (placed.number(), placed.record());
            let mut t = table.lock();
            let mut state = record.state().lock();
            // In the critical section that freed its slot: from here no wake-up finds it
            // (`origin_sync::threads`, "The location").
            record.clear_location();
            // Taken whatever happens below, so that the record's next thread starts unmarked.
            let mark = state.ending.take();
            match t.zombies.iter_mut().find(|z| z.is_none()) {
                Some(free) if state.stack.is_some() => {
                    *free = Some(number);
                    // The zombie counts before the thread stops counting as ending, so that a
                    // `settle` never finds it in neither (TASKS item 79; `Exits::queued`).
                    // Every processor has its counts. A mark not consumed keeps its count raised:
                    // never lowered early.
                    if let Some(exits) = EXITS.this(me) {
                        exits.queued(mark);
                    }
                }
                // No zombie of it can be kept. Its references are leaked deliberately: this
                // processor is still in the process's address space, and dropping the last
                // reference here would free the tables it is running on. Its stack and FPU area
                // go first (the stack is leaked, as the processor is still on it), and its record
                // goes back at once. Then nothing more it holds will be given back and it is
                // leaving the processor, so a `settle` stops waiting for it: what it leaked is the
                // compare's to find (TASKS item 79, case (a); the review's T79-4).
                _ => {
                    core::mem::forget(state.user.take());
                    drop(state.stack.take());
                    drop(state.fpu.take());
                    drop(state);
                    threads::give_back(number);
                    if let Some(mark) = mark {
                        mark.left();
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
/// whatever tells other threads it has ended (a done-flag or a count), and exits. `signal` writes
/// with Release (or as an AcqRel read-modify-write), and whoever waits for it reads with Acquire,
/// as [`end_with`] requires. Every kernel
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
    without_interrupts_here(|here| {
        let placed = {
            let q = QUEUES.this(cpu::this_cpu(here))?.lock();
            q.record(q.current())
        };
        placed?.record().state().lock().user.clone()
    })
}

/// This CPU's tick count, as its run queue sees it. A caller that reads it before and after a
/// sleep or a wait holds a pin across both (TASKS item 93): the two counts are then one processor's.
pub(crate) fn queue_ticks(me: ThisCpu<'_>) -> u64 {
    QUEUES
        .this(me)
        .map_or(0, |q| without_interrupts(|| q.lock().ticks()))
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

/// As [`spawn`], declaring the thread movable (ADR-0020 S2, point 1; TASKS item 96, the coordinating
/// session's ruling Q3): [`migrate`] may move it, while its pin count is 0. Nothing else declares a
/// thread movable: in TASKS item 96 only the migration proofs' threads are.
pub(crate) fn spawn_movable(
    cpu: CpuIndex,
    entry: extern "sysv64" fn(u64) -> !,
    arg: u64,
) -> Result<usize, QueueError> {
    spawn_with_parts(
        cpu,
        ThreadStart::Kernel { entry, arg },
        crate::runqueue::DEFAULT_WEIGHT,
        None,
        true,
        FpuArea::new,
        GuardedStack::publish_first_frame,
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
    spawn_with_parts(
        cpu,
        start,
        weight,
        user,
        false,
        FpuArea::new,
        GuardedStack::publish_first_frame,
    )
}

/// [`spawn_with`], with `fpu_area` the source of the thread's FPU area and `publish` what
/// publishes its first frame to `cpu`'s table of contexts (the boot proof gives sources that
/// refuse: an FPU area that is not there, and a publication refused as a full table refuses one).
/// A thread is created whole or not at all: every way this fails gives back the stack, its guard
/// page and the FPU area, through their `Drop`s, and leaves the run queue and the processor's
/// block of records as they were, but for the generation of a record taken and given back when
/// the queue is full (the review of TASKS item 92, R92-7).
///
/// **The order** (TASKS item 75). The first frame is published before the thread is queued, so
/// that no run queue ever names a frame its processor's table does not hold: the trap entry
/// refuses such a frame with a panic. Nothing can pick a thread that is not queued, so the
/// publication needs no lock. A refused publication returns at once, having touched neither the
/// queue nor the thread table. A queue that then refuses the thread (it is full) has the
/// publication withdrawn before the stack is dropped: otherwise the stack's home would still be in
/// the table, and its drop would leak it. Every spawn holds a slot of the table between its
/// publication and its push or withdrawal, with interrupts on, so the spawns in that stretch share
/// the table's 15 slots to spare: more would have a publication refused, which fails that spawn
/// safely, and would halt a processor whose boot or idle flow had not yet switched out. Today's
/// boot reaches neither (`origin_sync::contexts`, "Capacity"; the review of TASKS items 75, 77
/// and 81, R75-2).
fn spawn_with_parts(
    cpu: CpuIndex,
    start: ThreadStart,
    weight: u32,
    user: Option<ObjectRef<UserContext>>,
    movable: bool,
    fpu_area: impl FnOnce() -> Option<FpuArea>,
    publish: impl FnOnce(&mut GuardedStack, CpuIndex) -> bool,
) -> Result<usize, QueueError> {
    let queue = QUEUES.get(cpu.get()).ok_or(QueueError::Full)?;
    let index = u32::try_from(cpu.get()).map_err(|_| QueueError::Full)?;
    let mut stack = GuardedStack::new(THREAD_STACK).ok_or(QueueError::Full)?;
    let guard = stack.guard_addr();
    let frame_at = stack.build_first_frame(start).ok_or(QueueError::Full)?;
    // A thread without its own FPU save area would run on the previous thread's registers
    // (`follow_thread` then has nothing to restore), which may be another process's data; it
    // is refused instead.
    let fpu_area = fpu_area().ok_or(QueueError::Full)?;
    if !publish(&mut stack, cpu) {
        return Err(QueueError::Full);
    }
    let mut parts = Some((stack, fpu_area, user));
    let spawned = without_interrupts(|| {
        // The queue lock is held until the thread's state is complete and its record located: the
        // moment the slot is Ready the target processor can pick it, and `follow_thread` needs
        // the state. The record comes from the target's block (`crate::threads`), taken here so
        // that no spawn holds one it has not queued.
        let mut queue = queue.lock();
        // Resolved once, here, for the slot to hold (`threads::Placed`).
        let placed = threads::take(cpu).ok_or(QueueError::Full)?;
        // Before the push, while no thread runs on the record (`origin_sync::threads`, "Movable").
        if movable {
            placed.record().set_movable();
        }
        let slot = match queue.push_weighted(frame_at, weight, Some(placed)) {
            Ok(slot) => slot,
            Err(refused) => {
                threads::give_back(placed.number());
                return Err(refused);
            }
        };
        let record = placed.record();
        // A slot fits a location (`threads`'s assertion): neither `if` below can fail (the review
        // of TASKS item 92, R92-5).
        if let Some((stack, fpu_area, user)) = parts.take() {
            let mut state = record.state().lock();
            state.stack = Some(stack);
            state.fpu = Some(fpu_area);
            state.user = user;
            state.ending = None;
            drop(state);
            // After the push, under the same lock (`origin_sync::threads`, "The location").
            if let Ok(slot) = u16::try_from(slot) {
                record.set_location(Location::new(index, slot));
            }
        }
        Ok(slot)
    });
    match spawned {
        Ok(_) => {
            let _ = FIRST_GUARD.compare_exchange(0, guard, Ordering::Relaxed, Ordering::Relaxed);
        }
        // Never queued, so never resumed: the withdrawal finds the frame. Were it refused, the
        // home would stay and the drop below would leak the stack, which is the safe failure.
        Err(_) => {
            if let Some((mut stack, _, _)) = parts {
                let _ = stack.withdraw_first_frame();
            }
        }
    }
    spawned
}

/// An application processor's scheduler: waits until the boot processor has queued its
/// threads, then starts its own timer and idles with interrupts on; the timer preempts the
/// idle loop to run the queue's threads.
pub(crate) fn run_ap(flow: Pinned<'static>, lapic: &X2Apic) -> ! {
    // This processor's idle flow, which never moves (ADR-0020 S2, point 4): its witness, made
    // in `smp::ap_entry` with interrupts off and passed down (TASKS item 93).
    let me = cpu::this_cpu(flow.here());
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
    if let Some(queue) = QUEUES.this(me) {
        without_interrupts(|| queue.lock().set_idle(true));
    }
    if let Some(active) = ACTIVE.this(me) {
        active.store(true, Ordering::Release);
    }
    timer::start_periodic(lapic);
    loop {
        reap_with(flow);
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
/// The CPU index each worker last ran on, plus one (0: never ran), as [`PROGRESS`]: read by the
/// worker under its own section once it stops, which is where it last ran.
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
        if let Some(p) = worker_entry(&PROGRESS, slot)
            && RUN.load(Ordering::Relaxed) == run
            && step < limit
        {
            p.fetch_add(step, Ordering::Relaxed);
        }
        core::hint::spin_loop();
    }
    // Where it stopped, which is where it last ran: the processor's own index, read once, in a
    // section of its own, after the measured window (TASKS item 93: the judge's stop of
    // 2026-11-21 found a section in every iteration lowered the window's switches, and the delta
    // review's D93-1 asked for the processor's own view, not the scheduler's record).
    if let Some(r) = worker_entry(&RAN_ON, slot)
        && RUN.load(Ordering::Relaxed) == run
    {
        without_interrupts_here(|here| {
            r.store(
                cpu::this_cpu(here).index().get() as u64 + 1,
                Ordering::Relaxed,
            );
        });
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
    // Pinned across the sleep, so both counts are this processor's (TASKS item 93).
    let pin = pin();
    let me = cpu::this_cpu(pin.pinned().here());
    let before = queue_ticks(me);
    sleep(SLEEP_TICKS);
    SLEPT.store(queue_ticks(me) - before + 1, Ordering::Relaxed);
    drop(pin);
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
    // The proof before this one (`prove_simd`) ends when its workers have counted themselves
    // finished, which each does before it exits: wait for them to leave the queue too, as below
    // (the delta review of TASKS item 92's G92-1, R92-20). An empty queue costs no wait.
    wait_for_others(
        "the earlier proofs' threads to leave the queue",
        || live() == 1,
        tsc_hz,
        2000,
    );
    if live() != 1 {
        let _ = writeln!(
            com1,
            "origin-kernel: CPU {bsp} still has {} after the wait: scheduler failed",
            Contexts(live().saturating_sub(1))
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
    let first_left = || FINISHED.load(Ordering::Acquire) == 3 && live() == 1;
    yield_until(first_left, tsc_hz, 3);
    let first_waited = first_left();
    let reused = spawn(bsp, quitter, 0);
    let quitter_left = || FINISHED.load(Ordering::Acquire) == 4 && live() == 1;
    yield_until(quitter_left, tsc_hz, 1);
    let quitter_waited = quitter_left();
    let measured = measure_switch(bsp);
    if let Some(cycles) = measured {
        let _ = writeln!(
            com1,
            "origin-kernel: scheduler: {cycles} TSC cycles ({} ns) per context switch, yield ping-pong of 3 contexts",
            cycles * 1_000_000_000 / tsc_hz.max(1)
        );
    }
    // A bench thread counts itself finished before it exits (`finish`), so `measure_switch` can
    // return with one still in its slot: the proof waits for every thread whose end it has seen
    // to leave the queue before it reads its verdict, as the settles wait (TASKS item 92's gates,
    // G92-1; TASKS item 79). A thread that never leaves still fails it, once the wait is spent.
    wait_for_others(
        "the lifecycle proof's threads to leave the queue",
        || live() == 1,
        tsc_hz,
        2000,
    );
    let alternations = ALTERNATIONS.load(Ordering::Relaxed);
    let slept = SLEPT.load(Ordering::Relaxed).saturating_sub(1);
    let queued = live().saturating_sub(1);
    let words = LifecycleWords {
        alternated: alternations >= 2 * TURNS - 5,
        slept_right: (SLEEP_TICKS..=10 * SLEEP_TICKS).contains(&slept),
        reused: reused == Ok(1),
        queued,
        quitter_waited,
        first_waited,
        measured: measured.is_some(),
    };
    let _ = writeln!(
        com1,
        "origin-kernel: scheduler: yield alternated {alternations} of {} turns; sleep({SLEEP_TICKS}) took {slept} ticks; exited threads freed their slots (slot {:?} reused){words}",
        2 * TURNS,
        reused,
    );
    words.ok()
}

/// What [`prove_lifecycle`]'s line adds: nothing when it holds, otherwise ": scheduler failed"
/// and each condition that failed, so that a failure says which way it came (TASKS item 92's
/// gates, G92-1, as TASKS item 80 did for the IPC refusals). The counts themselves are on the line
/// already. The verdict is [`LifecycleWords::ok`], derived from the same conditions, so no failure
/// can name none (the delta review's R92-29).
struct LifecycleWords {
    alternated: bool,
    slept_right: bool,
    reused: bool,
    queued: usize,
    quitter_waited: bool,
    /// Said on a failure, not judged: the pingers' and the sleeper's 3 s wait gave up, which may
    /// be why the quitter's wait did (the delta review's R92-23).
    first_waited: bool,
    /// Said on a failure, not judged: the switch benchmark could not spawn its threads, which
    /// may be why one is still queued (R92-24).
    measured: bool,
}

impl LifecycleWords {
    /// The proof's verdict.
    fn ok(&self) -> bool {
        self.alternated
            && self.slept_right
            && self.reused
            && self.queued == 0
            && self.quitter_waited
    }
}

impl core::fmt::Display for LifecycleWords {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.ok() {
            return Ok(());
        }
        f.write_str(": scheduler failed")?;
        if !self.alternated {
            f.write_str("; too few alternations")?;
        }
        if !self.slept_right {
            f.write_str("; the sleep's ticks out of bounds")?;
        }
        if !self.reused {
            f.write_str("; the exited thread's slot not reused")?;
        }
        if self.queued > 0 {
            write!(f, "; {} still queued after the wait", Contexts(self.queued))?;
        }
        if !self.quitter_waited {
            f.write_str("; the quitter's wait gave up after 1 s")?;
        }
        if !self.first_waited {
            f.write_str("; the first threads' wait gave up after 3 s")?;
        }
        if !self.measured {
            f.write_str("; the switch benchmark could not spawn")?;
        }
        Ok(())
    }
}

/// `n` contexts, in words: "1 context", "2 contexts" (the delta review's R92-25).
struct Contexts(usize);

impl core::fmt::Display for Contexts {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let plural = if self.0 == 1 { "" } else { "s" };
        write!(f, "{} context{plural}", self.0)
    }
}

/// A spawn that cannot be completed leaves nothing behind: refused for want of an FPU area
/// (without one the thread would inherit another's SIMD registers), because its first frame's
/// publication is refused (as a full table of contexts refuses one), or because the queue is
/// full, it hands back the stack, its guard page and the FPU area, queues nothing, and loses no
/// stack (TASKS item 75: a publication the queue then refused is withdrawn, or its stack would be
/// leaked). The processor must hold only its boot context when this runs. (Anything a refusal
/// leaks would also fail the final check in [`prove`], which holds the heap to the byte.)
fn prove_spawn_refused(com1: &mut Console, bsp: CpuIndex, tsc_hz: u64) -> bool {
    let live = || {
        QUEUES
            .get(bsp.get())
            .map_or(0, |q| without_interrupts(|| q.lock().live()))
    };
    let in_use = || crate::memory::stats().map_or(0, |(h, _)| h.bytes_in_use);
    // A refused stack's own slot, read as soon as the spawn returns: a leak, which is what a
    // publication left behind would cause, makes it `Lost` at once. Not the region's count of
    // lost slots, which a stalled shootdown of any processor's reclaim also raises meanwhile
    // (the review of TASKS items 75, 77 and 81, R75-1). Reused already by another spawn, it is
    // `Live`: not leaked either.
    let not_leaked = |slot: Option<StackSlot>| {
        slot.is_some_and(|slot| origin_frame::stack::slot_state(slot) != Some(SlotState::Lost))
    };
    let started = FINISHED.load(Ordering::Acquire);
    let contexts = live();
    let before = in_use();
    let quitting = || ThreadStart::Kernel {
        entry: quitter,
        arg: 0,
    };
    let no_area = spawn_with_parts(
        bsp,
        quitting(),
        crate::runqueue::DEFAULT_WEIGHT,
        None,
        false,
        || None,
        GuardedStack::publish_first_frame,
    );
    let no_area_clean =
        no_area == Err(QueueError::Full) && in_use() <= before && live() == contexts;
    let unpublished_slot = core::cell::Cell::new(None);
    let unpublished = spawn_with_parts(
        bsp,
        quitting(),
        crate::runqueue::DEFAULT_WEIGHT,
        None,
        false,
        FpuArea::new,
        |stack, _| {
            unpublished_slot.set(Some(stack.slot()));
            false
        },
    );
    let unpublished_clean = unpublished == Err(QueueError::Full)
        && not_leaked(unpublished_slot.get())
        && in_use() <= before
        && live() == contexts;
    let free = crate::runqueue::CAPACITY.saturating_sub(contexts);
    let mut filled = 0;
    for _ in 0..free {
        if spawn(bsp, quitter, 0).is_ok() {
            filled += 1;
        }
    }
    let full_use = in_use();
    // Published, then refused by the full queue: withdrawn before its stack drops.
    let full_slot = core::cell::Cell::new(None);
    let full = spawn_with_parts(
        bsp,
        quitting(),
        crate::runqueue::DEFAULT_WEIGHT,
        None,
        false,
        FpuArea::new,
        |stack, cpu| {
            full_slot.set(Some(stack.slot()));
            stack.publish_first_frame(cpu)
        },
    );
    let full_clean = full == Err(QueueError::Full)
        && not_leaked(full_slot.get())
        && in_use() <= full_use
        && live() == contexts + free;
    let finished = started + filled as u64;
    yield_until(
        || FINISHED.load(Ordering::Acquire) == finished && live() == contexts,
        tsc_hz,
        3,
    );
    reap();
    let ok = no_area_clean
        && unpublished_clean
        && full_clean
        && filled == free
        && live() == contexts
        && FINISHED.load(Ordering::Acquire) == finished;
    let _ = writeln!(
        com1,
        "origin-kernel: scheduler: spawns refused for want of an FPU area ({no_area:?}), at their publication ({unpublished:?}) and on a full queue ({full:?}) give back the stack, its guard page and the FPU area, queue nothing and lose no stack{}",
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
pub(crate) fn prove(
    flow: Pinned<'_>,
    com1: &mut Console,
    online: &crate::smp::Online,
    tsc_hz: u64,
    proofs: &mut Tally,
) {
    let cpus = online.cpus();
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
    // The boot flow, which never moves: its witness serves the whole proof (TASKS item 93).
    let me = cpu::this_cpu(flow.here());
    let Some(lapic) = X2Apic::here(me) else {
        let _ = writeln!(
            com1,
            "origin-kernel: the local APIC is not in x2APIC mode: scheduler failed"
        );
        proofs.record(Proof::Preemption, false);
        return;
    };
    let bsp = me.index();
    GO.store(true, Ordering::Release);
    if let Some(active) = ACTIVE.this(me) {
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
    // Moves between processors (TASKS item 96; the contract's §3, the ruling's Q10), last: their
    // threads are the only movable ones, and every one has ended and been reaped before the next
    // proof starts. The processors online and their FPU flags come with `online` (Q16).
    proofs.record(
        Proof::MigrationIntegrity,
        crate::migration_proof::prove_integrity(flow, com1, online, tsc_hz),
    );
    proofs.record(
        Proof::MigrationDelivery,
        crate::migration_proof::prove_delivery(flow, com1, online, tsc_hz),
    );
}

/// Stops the boot processor's scheduler and timer once the boot proofs are done, so the
/// boot flow can halt.
pub(crate) fn stop_boot_processor(flow: Pinned<'_>) {
    let me = cpu::this_cpu(flow.here());
    if let Some(lapic) = X2Apic::here(me) {
        lapic.stop();
    }
    if let Some(active) = ACTIVE.this(me) {
        active.store(false, Ordering::Release);
    }
}
