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
use framekernel::serial::Uart16550;

use crate::Com1;
use crate::cpu::{self, rdtsc};
use crate::sync::SpinLock;
use crate::timer::{self, ApicRead, X2Apic};

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

/// This processor's x2APIC ID, as a queue index.
fn this_cpu() -> usize {
    // SAFETY: only called where x2APIC mode is on (timer interrupts and code after the
    // timer bring-up); the ID register has no read side effect.
    (unsafe { cpu::rdmsr(framekernel::apic::X2APIC_ID) }) as usize
}

/// Runs `f` with interrupts disabled on this processor, restoring the previous state.
fn without_interrupts<T>(f: impl FnOnce() -> T) -> T {
    let flags: u64;
    // SAFETY: reads RFLAGS and disables interrupts; CPL 0. Restored below.
    unsafe { asm!("pushfq", "pop {}", "cli", out(reg) flags, options(nomem, preserves_flags)) };
    let result = f();
    if flags & (1 << 9) != 0 {
        // SAFETY: interrupts were enabled on entry, so re-enabling restores that state.
        unsafe { asm!("sti", options(nomem, nostack)) };
    }
    result
}

/// The timer interrupted the context whose frame is at `interrupted`; returns the frame to
/// resume. Processors without an active queue keep running what they were running.
pub(crate) fn on_tick(interrupted: u64) -> u64 {
    let id = this_cpu();
    match (ACTIVE.get(id), QUEUES.get(id)) {
        (Some(active), Some(queue)) if active.load(Ordering::Acquire) => {
            queue.lock().tick(interrupted)
        }
        _ => interrupted,
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

/// Creates a kernel thread on processor `cpu` (an x2APIC ID) that runs `entry(arg)`.
pub(crate) fn spawn(
    cpu: usize,
    entry: extern "sysv64" fn(u64) -> !,
    arg: u64,
) -> Result<(), QueueError> {
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
    without_interrupts(|| queue.lock().push(frame_at))
}

/// An application processor's scheduler: waits until the boot processor has queued its
/// threads, then starts its own timer and idles with interrupts on; the timer preempts the
/// idle loop to run the queue's threads.
pub(crate) fn run_ap(lapic: &X2Apic) -> ! {
    while !GO.load(Ordering::Acquire) {
        core::hint::spin_loop();
    }
    // The boot processor split large pages and unmapped guard pages while this processor
    // waited: drop any stale translation before running threads.
    crate::vm::flush_tlb();
    if let Some(active) = ACTIVE.get(this_cpu()) {
        active.store(true, Ordering::Release);
    }
    timer::start_periodic(lapic);
    loop {
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
    loop {
        // SAFETY: as in `run_ap`: an idle wait with interrupts on.
        unsafe { asm!("sti", "hlt", options(nomem, nostack)) };
    }
}

/// Proves preemptive multitasking on every online processor: two non-yielding workers per
/// processor run for 500 ms while the boot flow (itself one of the boot processor's
/// contexts) waits. On each processor, both workers must have run there, with shares
/// within a factor of two, over at least five full rounds of context switches. Returns
/// whether the proof passed.
pub(crate) fn prove(com1: &mut Uart16550<Com1>, cpus: &[u32], tsc_hz: u64) -> bool {
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
    STOP.store(true, Ordering::Relaxed);
    lapic.stop();
    if let Some(active) = ACTIVE.get(bsp) {
        active.store(false, Ordering::Release);
    }
    let mut ok = true;
    for (i, &id) in cpus.iter().enumerate() {
        let switches = QUEUES
            .get(id as usize)
            .map_or(0, |q| without_interrupts(|| q.lock().switches()));
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
    ok
}
