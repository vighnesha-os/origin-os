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

use core::arch::asm;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use framekernel::descriptors::{KERNEL_CODE, KERNEL_DATA};
use framekernel::runqueue::{QueueError, RunQueue};

use crate::console::Console;
use crate::cpu::{self, rdtsc};
use crate::timer::{self, ApicRead, X2Apic};
use origin_sync::SpinLock;

/// Processors with a run queue, indexed by x2APIC ID.
pub(crate) const MAX_CPUS: usize = 64;
/// Each kernel thread's stack.
const THREAD_STACK: usize = 16 * 1024;
/// Words in a saved frame: 15 registers, vector, error code, RIP, CS, RFLAGS, RSP, SS.
const FRAME_WORDS: usize = 22;
/// Index of RDI in a saved frame (r15 is first; see `cpu::ExceptionFrame`).
const RDI: usize = 9;
/// RFLAGS of a new thread: IF (interrupts on) and the always-one bit 1.
const NEW_RFLAGS: u64 = 0x202;

static QUEUES: [SpinLock<RunQueue>; MAX_CPUS] =
    [const { SpinLock::new(RunQueue::new()) }; MAX_CPUS];
static ACTIVE: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];
/// Set by the boot processor once every AP's threads are queued.
static GO: AtomicBool = AtomicBool::new(false);

/// This processor's x2APIC ID, as a queue index (from its GS-based CPU-local area).
fn this_cpu() -> usize {
    usize::try_from(cpu::cpu_id()).unwrap_or(usize::MAX)
}

use crate::cpu::without_interrupts;

/// The timer interrupted the context whose frame is at `interrupted`; returns the frame to
/// resume. Processors without an active queue keep running what they were running.
pub(crate) fn on_tick(interrupted: u64) -> u64 {
    with_queue(interrupted, |q| Some(q.tick(interrupted)))
}

/// Prepares CPU `id` to resume the thread in `slot`: TSS.RSP0 points at its kernel stack,
/// so an interrupt from user mode always lands on the running thread's own stack (the boot
/// or idle flow, with no recorded stack, never runs in user mode), and CR3 holds its
/// address space (the kernel's for kernel threads), loaded only when it differs.
fn follow_thread(id: usize, slot: usize) {
    let Some(threads) = THREADS.get(id) else {
        return;
    };
    let (stack, root) = {
        let t = threads.lock();
        (
            t.stacks.get(slot).copied().flatten(),
            t.roots.get(slot).copied().unwrap_or(0),
        )
    };
    if let Some(stack) = stack {
        cpu::set_kernel_stack((stack.guard + 4096 + stack.bytes as u64) & !0xF);
    }
    let want = if root == 0 {
        crate::vm::kernel_root()
    } else {
        root
    };
    if want != 0 && cpu::read_cr3() != want {
        // SAFETY: `want` is the kernel's PML4 or a process PML4 whose kernel half is the
        // kernel's, so the code, stacks and data this handler is using stay mapped.
        unsafe { cpu::write_cr3(want) };
    }
}

/// A page-aligned heap allocation of a guard page plus `bytes` of stack. The guard (the
/// lowest page) is unmapped, so running off the end of the stack faults instead of
/// overwriting a neighbour. Returns the stack (above the guard) and the guard's address.
pub(crate) fn guarded_stack(bytes: usize) -> Option<(&'static mut [u8], u64)> {
    use alloc::alloc::{Layout, alloc_zeroed};
    let layout = Layout::from_size_align(bytes + 4096, 4096).ok()?;
    // SAFETY: `layout` has a non-zero size; the allocation is never freed (threads do not
    // exit yet), so the slice below is `'static`.
    let base = unsafe { alloc_zeroed(layout) };
    if base.is_null() {
        return None;
    }
    let guard = base.expose_provenance() as u64;
    crate::vm::make_guard(guard).ok()?;
    // SAFETY: `base + 4096 .. base + 4096 + bytes` lies inside the fresh allocation, is
    // still mapped read/write, and nothing else refers to it.
    let stack = unsafe { core::slice::from_raw_parts_mut(base.wrapping_add(4096), bytes) };
    Some((stack, guard))
}

/// The guard page of the first thread `spawn` created, for the boot proof.
static FIRST_GUARD: AtomicU64 = AtomicU64::new(0);

/// A thread stack from [`guarded_stack`]: its guard page and its usable bytes.
#[derive(Clone, Copy)]
struct ThreadStack {
    guard: u64,
    bytes: usize,
}

/// Per CPU: each slot's stack, and the stacks of threads that exited and await
/// [`reap`]. Locked after the run queue, always with interrupts disabled.
struct Threads {
    stacks: [Option<ThreadStack>; framekernel::runqueue::CAPACITY],
    zombies: [Option<ThreadStack>; framekernel::runqueue::CAPACITY],
    /// Each slot's address space (its PML4); 0 means the kernel's.
    roots: [u64; framekernel::runqueue::CAPACITY],
}

static THREADS: [SpinLock<Threads>; MAX_CPUS] = [const {
    SpinLock::new(Threads {
        stacks: [None; framekernel::runqueue::CAPACITY],
        zombies: [None; framekernel::runqueue::CAPACITY],
        roots: [0; framekernel::runqueue::CAPACITY],
    })
}; MAX_CPUS];

/// Frees the stacks of threads that exited on **this** processor. Only this processor may
/// do it, and only from thread context: by then its exit handler, which ran on the dead
/// stack, has certainly switched away. Returns how many stacks were freed.
pub(crate) fn reap() -> usize {
    use alloc::alloc::{Layout, dealloc};
    let Some(threads) = THREADS.get(this_cpu()) else {
        return 0;
    };
    let mut freed = 0;
    loop {
        let next = without_interrupts(|| threads.lock().zombies.iter_mut().find_map(Option::take));
        let Some(stack) = next else {
            return freed;
        };
        let Ok(layout) = Layout::from_size_align(stack.bytes + 4096, 4096) else {
            continue;
        };
        // The guard must be mapped again before its frame returns to the heap.
        if crate::vm::unguard(stack.guard).is_err() {
            continue; // leak rather than free memory that is not mapped
        }
        // SAFETY: `guard` is the start of a `guarded_stack` allocation with this layout;
        // its thread exited on this processor and this processor has switched away from
        // it, so nothing uses the memory any more.
        unsafe {
            dealloc(
                core::ptr::with_exposed_provenance_mut(stack.guard as usize),
                layout,
            )
        };
        freed += 1;
    }
}

/// The running thread's queue request, made with interrupts disabled (the gate cleared IF).
fn with_queue(interrupted: u64, f: impl FnOnce(&mut RunQueue) -> Option<u64>) -> u64 {
    let id = this_cpu();
    match (ACTIVE.get(id), QUEUES.get(id)) {
        (Some(active), Some(queue)) if active.load(Ordering::Acquire) => {
            let mut q = queue.lock();
            let next = f(&mut q).unwrap_or(interrupted);
            follow_thread(id, q.current());
            next
        }
        _ => interrupted,
    }
}

/// `YIELD_VECTOR`: the running context gives up the processor.
pub(crate) fn on_yield(interrupted: u64) -> u64 {
    with_queue(interrupted, |q| Some(q.yield_now(interrupted)))
}

/// `SLEEP_VECTOR`: the running thread sleeps for `ticks` timer ticks.
pub(crate) fn on_sleep(interrupted: u64, ticks: u64) -> u64 {
    with_queue(interrupted, |q| q.sleep(interrupted, ticks).ok())
}

/// `EXIT_VECTOR`: the running thread ends. Its stack becomes a zombie, freed later by
/// [`reap`] on this processor.
pub(crate) fn on_exit(interrupted: u64) -> u64 {
    let id = this_cpu();
    with_queue(interrupted, |q| {
        let slot = q.current();
        let next = q.exit(interrupted).ok()?;
        if let Some(threads) = THREADS.get(id) {
            let mut t = threads.lock();
            let stack = t.stacks.get_mut(slot).and_then(Option::take);
            if let Some(r) = t.roots.get_mut(slot) {
                *r = 0;
            }
            if let Some(free) = t.zombies.iter_mut().find(|z| z.is_none()) {
                *free = stack;
            }
        }
        Some(next)
    })
}

/// Gives up the processor to the next ready context on this CPU.
pub(crate) fn yield_now() {
    // SAFETY: the gate for this vector is installed; the handler saves and restores every
    // register, and returns here when this context is next scheduled.
    unsafe { asm!("int 0x81") };
}

/// Sleeps for `ticks` timer ticks of this CPU (at least one).
pub(crate) fn sleep(ticks: u64) {
    // SAFETY: as for `yield_now`; RDI carries the argument and is restored.
    unsafe { asm!("int 0x82", in("rdi") ticks) };
}

/// Ends the running thread.
pub(crate) fn exit() -> ! {
    loop {
        // SAFETY: as for `yield_now`; the handler frees this thread's slot and never
        // resumes it. If the request is refused (no active queue), it is retried.
        unsafe { asm!("int 0x83") };
    }
}

/// The top of the running thread's kernel stack, or `None` for the boot or idle flow.
pub(crate) fn current_stack_top() -> Option<u64> {
    let id = this_cpu();
    let (queue, threads) = (QUEUES.get(id)?, THREADS.get(id)?);
    without_interrupts(|| {
        let slot = queue.lock().current();
        let stack = threads.lock().stacks.get(slot).copied().flatten()?;
        Some((stack.guard + 4096 + stack.bytes as u64) & !0xF)
    })
}

/// This CPU's tick count, as its run queue sees it.
fn queue_ticks() -> u64 {
    QUEUES
        .get(this_cpu())
        .map_or(0, |q| without_interrupts(|| q.lock().ticks()))
}

/// Creates a kernel thread on processor `cpu` (an x2APIC ID) that runs `entry(arg)`, and
/// returns its run-queue slot.
pub(crate) fn spawn(
    cpu: usize,
    entry: extern "sysv64" fn(u64) -> !,
    arg: u64,
) -> Result<usize, QueueError> {
    spawn_in(cpu, entry, arg, 0)
}

/// As [`spawn`], in the address space whose PML4 is `root` (0: the kernel's). The thread
/// starts in kernel mode; a process thread then enters user mode in that space.
pub(crate) fn spawn_in(
    cpu: usize,
    entry: extern "sysv64" fn(u64) -> !,
    arg: u64,
    root: u64,
) -> Result<usize, QueueError> {
    let queue = QUEUES.get(cpu).ok_or(QueueError::Full)?;
    let (stack, guard) = guarded_stack(THREAD_STACK).ok_or(QueueError::Full)?;
    let _ = FIRST_GUARD.compare_exchange(0, guard, Ordering::Relaxed, Ordering::Relaxed);
    let base = stack.as_ptr().addr() as u64;
    // The thread starts with RSP = top − 8 holding a zero return address, as if `entry` had
    // been called: RSP + 8 is then 16-byte aligned (System V).
    let top = (base + THREAD_STACK as u64) & !0xF;
    let rsp = top - 8;
    let frame_at = rsp - (FRAME_WORDS as u64) * 8;
    let mut frame = [0u64; FRAME_WORDS];
    frame[RDI] = arg;
    frame[17] = (entry as *const ()).addr() as u64; // RIP
    frame[18] = u64::from(KERNEL_CODE); // CS
    frame[19] = NEW_RFLAGS;
    frame[20] = rsp;
    frame[21] = u64::from(KERNEL_DATA); // SS
    let offset = (frame_at - base) as usize;
    for (i, word) in frame.iter().chain(core::iter::once(&0u64)).enumerate() {
        let at = offset + i * 8;
        if let Some(dst) = stack.get_mut(at..at + 8) {
            dst.copy_from_slice(&word.to_le_bytes());
        }
    }
    let record = ThreadStack {
        guard,
        bytes: THREAD_STACK,
    };
    without_interrupts(|| {
        let slot = queue.lock().push(frame_at)?;
        if let Some(threads) = THREADS.get(cpu) {
            let mut t = threads.lock();
            if let Some(s) = t.stacks.get_mut(slot) {
                *s = Some(record);
            }
            if let Some(r) = t.roots.get_mut(slot) {
                *r = root;
            }
        }
        Ok(slot)
    })
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
    crate::tlb::mark_online();
    if let Some(active) = ACTIVE.get(this_cpu()) {
        active.store(true, Ordering::Release);
    }
    timer::start_periodic(lapic);
    loop {
        reap();
        // SAFETY: CPL 0; every vector that can arrive has a gate. Halting with interrupts
        // on just waits for the next timer tick, which may switch to a thread.
        unsafe { asm!("sti", "hlt", options(nomem, nostack)) };
    }
}

/// Iterations each worker thread has made, indexed by worker.
static PROGRESS: [AtomicU64; 2 * MAX_CPUS] = [const { AtomicU64::new(0) }; 2 * MAX_CPUS];
/// The x2APIC ID each worker last ran on, plus one (0: never ran).
static RAN_ON: [AtomicU64; 2 * MAX_CPUS] = [const { AtomicU64::new(0) }; 2 * MAX_CPUS];
/// Tells the workers to stop counting.
static STOP: AtomicBool = AtomicBool::new(false);
/// Workers that have stopped and exited.
static EXITED: AtomicU64 = AtomicU64::new(0);

/// A worker that never yields: it counts until [`STOP`], so it only ever gives up its
/// processor when the timer preempts it.
extern "sysv64" fn worker(slot: u64) -> ! {
    let slot = slot as usize;
    while !STOP.load(Ordering::Relaxed) {
        if let (Some(p), Some(r)) = (PROGRESS.get(slot), RAN_ON.get(slot)) {
            p.fetch_add(1, Ordering::Relaxed);
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
fn measure_switch(bsp: usize) -> Option<u64> {
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
    let start = rdtsc();
    while !done() && rdtsc().wrapping_sub(start) < tsc_hz.saturating_mul(seconds) {
        yield_now();
        cpu::interrupt_window();
        reap();
    }
}

/// On the boot processor, with its timer running: yields must alternate two threads, a
/// sleep must last its ticks, and exited threads must free their slots for reuse.
fn prove_lifecycle(com1: &mut Console, bsp: usize, tsc_hz: u64) -> bool {
    let live = || {
        QUEUES
            .get(bsp)
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

/// Proves preemptive multitasking on every online processor: two non-yielding workers per
/// processor run for 500 ms while the boot flow (itself one of the boot processor's
/// contexts) waits. On each processor, both workers must have run there, with shares
/// within a factor of two, over at least five full rounds of context switches. Returns
/// whether the proof passed.
pub(crate) fn prove(com1: &mut Console, cpus: &[u32], tsc_hz: u64) -> bool {
    let heap_before = crate::memory::stats().map(|(h, _)| h.bytes_in_use);
    for (i, &id) in cpus.iter().enumerate() {
        for k in 0..2 {
            if spawn(id as usize, worker, (2 * i + k) as u64).is_err() {
                let _ = writeln!(
                    com1,
                    "origin-kernel: cannot spawn on CPU {id}: scheduler failed"
                );
                return false;
            }
        }
    }
    // SAFETY: the boot processor is in x2APIC mode (the timer bring-up), and `start` was
    // already called on it; the transition is a no-op that yields the handle.
    let lapic = unsafe { X2Apic::enable() };
    let bsp = lapic.read(ApicRead::Id) as usize;
    GO.store(true, Ordering::Release);
    if let Some(active) = ACTIVE.get(bsp) {
        active.store(true, Ordering::Release);
    }
    crate::tlb::mark_online();
    timer::start_periodic(&lapic);
    // The first thread's guard page must be unmapped.
    let guard = FIRST_GUARD.load(Ordering::Relaxed);
    // SAFETY: the guard is RAM of a heap allocation, not MMIO.
    match unsafe { cpu::probe_read(guard) } {
        cpu::Probe::Faulted { error: 0, address } if address == guard => {
            let _ = writeln!(
                com1,
                "origin-kernel: page protection: stack guard page at {guard:#x} faulted (#PF error 0x0)"
            );
        }
        other => {
            let _ = writeln!(
                com1,
                "origin-kernel: page protection violated: stack guard page at {guard:#x}: {other:?}"
            );
            return false;
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
        // SAFETY: as in `run_ap`.
        unsafe { asm!("sti", "hlt", "cli", options(nomem, nostack)) };
    }
    let elapsed_ms = rdtsc().wrapping_sub(start) / (tsc_hz / 1000).max(1);
    let mut switches_at_stop = [0u64; MAX_CPUS];
    for (slot, &id) in switches_at_stop.iter_mut().zip(cpus) {
        *slot = QUEUES
            .get(id as usize)
            .map_or(0, |q| without_interrupts(|| q.lock().switches()));
    }
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
    for (i, &id) in cpus.iter().enumerate() {
        let switches = switches_at_stop.get(i).copied().unwrap_or(0);
        let (a, b) = (2 * i, 2 * i + 1);
        let count = |w: usize| PROGRESS.get(w).map_or(0, |p| p.load(Ordering::Relaxed));
        let place = |w: usize| RAN_ON.get(w).map_or(0, |r| r.load(Ordering::Relaxed));
        let right_cpu = place(a) == u64::from(id) + 1 && place(b) == u64::from(id) + 1;
        // Both workers ran, with comparable shares (within a factor of two).
        let (lo, hi) = (count(a).min(count(b)), count(a).max(count(b)));
        let fair = lo > 0 && lo * 2 >= hi;
        // At least five full rounds of the three contexts.
        let rounds = switches >= 15;
        let passed = right_cpu && fair && rounds;
        ok &= passed;
        let _ = writeln!(
            com1,
            "origin-kernel: scheduler: CPU {id}: workers {a} and {b} counted {} and {}, {switches} context switches{}",
            count(a),
            count(b),
            if passed { "" } else { ": scheduler failed" }
        );
    }
    if ok {
        let _ = writeln!(
            com1,
            "origin-kernel: preemptive multitasking: {workers} non-yielding threads on {} CPUs all ran, in {elapsed_ms} ms",
            cpus.len()
        );
    }
    ok &= prove_lifecycle(com1, bsp, tsc_hz);
    ok &= crate::user::prove(com1, bsp, tsc_hz);
    ok &= crate::time::prove_consistent(com1, cpus, bsp, tsc_hz);
    ok &= crate::console::prove(com1, cpus, bsp, tsc_hz);
    ok &= crate::tlb::prove(com1, cpus, bsp, tsc_hz);
    // Every thread has exited: once each processor has reaped its own, the heap is back
    // exactly where it was before the first spawn.
    let back = || crate::memory::stats().map(|(h, _)| h.bytes_in_use) == heap_before;
    yield_until(back, tsc_hz, 2);
    let reaped = back();
    let now = crate::memory::stats().map_or(0, |(h, _)| h.bytes_in_use);
    ok &= reaped;
    let _ = writeln!(
        com1,
        "origin-kernel: scheduler: exited threads' stacks reaped on their own CPUs: heap {} KiB, {} KiB before the first spawn{}",
        now >> 10,
        heap_before.unwrap_or(0) >> 10,
        if reaped { "" } else { ": scheduler failed" }
    );
    lapic.stop();
    if let Some(active) = ACTIVE.get(bsp) {
        active.store(false, Ordering::Release);
    }
    ok
}
