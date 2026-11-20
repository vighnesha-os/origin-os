//! The boot proof of the kernel stacks' region (ADR-0004 Amendment 1, A2; CLAUDE.md §25, §28;
//! ADR-0013), and what a stack costs. The mechanism is the framework's (`origin_frame::stack`).
//!
//! - **Below every stack is unmapped space.** For a stack of each size the kernel makes, the
//!   page below it, the bottom of its slot and the bottom of the next slot fault, while each page
//!   of the stack is read/write, never executable and global. Each application processor's
//!   interrupt stacks, as its TSS names them, are in the region with the same space below. A
//!   frame larger than a page is probed a page at a time (Rust 1.100.0's inline stack probes on
//!   `x86_64-unknown-none`), so a stack's first access past its end is in that space.
//! - **The direct map is whole:** it still has every 2 MiB leaf `layout::build` made.
//! - **A freed stack is out of every processor's reach:** another processor caches the
//!   translations of a stack's pages; once the stack has been freed and reclaimed, its reads
//!   fault, and the slot is free again, which it is only once the frames have been released.
//! - **A thread's own stack stays in use through its switches** (TASKS item 70; the review of
//!   TASKS item 68, T68-2). A thread on another processor is held in the window of its own
//!   switch-out, after the scheduler has decided the switch and before the trap entry saves its
//!   frame. (Until TASKS item 77 the scheduler had moved that processor's kernel-stack record
//!   there; since then the trap entry moves it only after the frame is saved, so the window holds
//!   the thread's frame unsaved and its stack named by neither the frame nor the record, its home
//!   alone covering it.) It is held again in the window of its exit, when its stack
//!   has become a zombie. Each time the boot flow asks the framework whether the stack is in use,
//!   as a stack's drop would decide, and the answer must be yes. Once the thread has ended and
//!   its processor has reaped it, that drop must have given the stack back, not leaked it.
//! - **Nothing leaks:** once the proof's stacks are given back and reclaimed, the region holds
//!   exactly what it held before.
//! - **What it costs** (CLAUDE.md rule 21: under QEMU, with no budget).
#![forbid(unsafe_code)]

use core::fmt::Write;
use core::hint::black_box;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use origin_frame::once::SetOnce;
use origin_frame::stack::{self, GuardedStack, StackSlot, State};

use crate::console::Console;
use crate::cpu::{self, CpuIndex, Probe, rdtsc};
use crate::paging::PAGE;
use crate::paging::layout::{self, STACK_REGION_SIZE};

/// Stacks the cost measurement makes, then frees.
const ROUNDS: usize = 64;
/// The size it measures: a kernel thread's (`sched`'s `THREAD_STACK`).
const MEASURED: usize = 16 * 1024;
/// The sizes the guard proof makes: the park drill's, a kernel thread's and an interrupt
/// stack's, and an application processor's own.
const SIZES: [usize; 3] = [4 * 1024, 16 * 1024, 64 * 1024];
/// The pages of the stack another processor caches.
const CACHED_PAGES: usize = MEASURED / PAGE as usize;

/// Nanoseconds per operation, for `ticks` of the TSC spent on `count` of them.
fn nanos_each(ticks: u64, count: usize, tsc_hz: u64) -> u64 {
    let nanos = u128::from(ticks) * 1_000_000_000 / u128::from(tsc_hz.max(1));
    u64::try_from(nanos / count.max(1) as u128).unwrap_or(u64::MAX)
}

/// Whether a read of `addr` faults as a page that is not present does, there.
fn faults(addr: u64) -> bool {
    matches!(cpu::probe_read(addr), Probe::Faulted { error: 0, address } if address == addr)
}

/// Whether the page at `addr` is a stack page: read/write, never executable, global and
/// supervisor, a 4 KiB leaf.
fn stack_page(addr: u64) -> bool {
    crate::vm::translate(addr)
        .is_some_and(|t| t.writable && !t.executable && t.global && !t.user && t.size == PAGE)
        && cpu::probe_read(addr) == Probe::Completed
}

/// The guard proof for one stack: in the region, in a slot of its own; the page below it, the
/// bottom of its slot and the bottom of the next slot fault; every page of it is a stack page.
fn guarded(stack: &GuardedStack) -> (bool, u32) {
    let top = stack.top();
    let low = top - stack.bytes() as u64;
    let base = stack::region().base;
    let placed = stack.slot_base() >= base
        && top <= base + STACK_REGION_SIZE
        && top - stack.slot_base() == stack::SLOT;
    let below = [stack.guard_addr(), stack.slot_base(), top]
        .into_iter()
        .filter(|&addr| faults(addr))
        .count() as u32;
    let pages = (low..top).step_by(PAGE as usize).all(stack_page);
    (placed && pages, below)
}

/// The region's slide (ADR-0004 Amendment 2): its base is one the slide can give,
/// 2 MiB-aligned with the whole region in PML4 slots 384–447. When the loader handed over a
/// random value for it (A5.1, decision item 166) the base was drawn from that value, taken
/// once, and so is not the fixed base, which a draw never gives; when the loader handed over
/// none, it is the fixed base. Nothing it prints names the value, the base or an address in the
/// region (A5.3, A6).
fn placed_at_random(com1: &mut Console) -> bool {
    use origin_frame::random::{self, StackValue};
    let region = stack::region();
    let in_window = layout::is_stack_region_base(region.base);
    let fixed = region.base == layout::STACK_REGION;
    // What became of the loader's value: taken exactly when the base was drawn from it, and
    // not there at all when the base is the fixed one. A value left unclaimed beside a fixed
    // base, or a drawn base with no value taken, is wrong.
    let value = random::stack_value();
    let right = in_window
        && if region.drawn {
            !fixed && value == StackValue::Taken
        } else {
            fixed && value == StackValue::None
        };
    let verdict = if right { "" } else { ": kernel stacks failed" };
    let _ = if region.drawn {
        writeln!(
            com1,
            "origin-kernel: kernel stacks: the region's base is 2 MiB-aligned with the whole region in PML4 slots 384-447: {in_window}; drawn from the loader's random value, taken once: {}, so not its fixed base: {}{verdict}",
            value == StackValue::Taken,
            !fixed
        )
    } else {
        writeln!(
            com1,
            "origin-kernel: kernel stacks: the region's base is 2 MiB-aligned with the whole region in PML4 slots 384-447: {in_window}; the loader handed over no random value: {}, and the region is at its fixed base: {fixed}{verdict}",
            value == StackValue::None
        )
    };
    right
}

/// The guard proof for an application processor's interrupt stacks, as its TSS names them:
/// each top is a slot's top in the region, the stack's pages are stack pages, and the page
/// below each stack faults. Returns how many of the four passed.
fn interrupt_stacks_guarded(cpu: CpuIndex) -> u32 {
    let Some(tops) = cpu::interrupt_stack_tops(cpu) else {
        return 0;
    };
    let size = cpu::IST_STACK_SIZE as u64;
    tops.into_iter()
        .filter(|&top| {
            origin_frame::stack::slot_top(top - 1) == Some(top)
                && (top - size..top).step_by(PAGE as usize).all(stack_page)
                && faults(top - size - PAGE)
        })
        .count() as u32
}

/// The pages another processor caches and probes again once their stack is reclaimed.
static PAGES: [AtomicU64; CACHED_PAGES] = [const { AtomicU64::new(0) }; CACHED_PAGES];
/// 1: the reader has cached the pages; 2: they are reclaimed, probe again; 3: it has.
static STAGE: AtomicU64 = AtomicU64::new(0);
/// The reader's probes after the reclaim that faulted, and that read through a stale
/// translation instead.
static FAULTED: AtomicU64 = AtomicU64::new(0);
static STALE: AtomicU64 = AtomicU64::new(0);

/// A thread on another processor: reads every page of the stack, so its TLB holds their
/// translations; then, once the stack is reclaimed, probes each page again. It spins between
/// the two, so nothing else runs on its processor and no CR3 load drops a translation.
extern "sysv64" fn reader(_: u64) -> ! {
    let mut read = 0u64;
    for _ in 0..16 {
        for page in &PAGES {
            if cpu::probe_read(page.load(Ordering::Acquire)) == Probe::Completed {
                read += 1;
            }
        }
    }
    core::hint::black_box(read);
    STAGE.store(1, Ordering::Release);
    while STAGE.load(Ordering::Acquire) < 2 {
        core::hint::spin_loop();
    }
    for page in &PAGES {
        if faults(page.load(Ordering::Acquire)) {
            FAULTED.fetch_add(1, Ordering::Relaxed);
        } else {
            STALE.fetch_add(1, Ordering::Relaxed);
        }
    }
    crate::sched::finish(|| STAGE.store(3, Ordering::Release))
}

/// Every retired stack reclaimed, here or by another processor: the others reclaim in their idle
/// loops (`sched::reap`), so this waits for the reclaims they have under way too.
fn reclaimed(tsc_hz: u64) {
    stack::reclaim();
    crate::sched::yield_until(|| stack::counts().retired == 0, tsc_hz, 2);
}

/// Proves the region; see the module documentation. Returns whether every part held.
pub(crate) fn prove(com1: &mut Console, cpus: &[CpuIndex], bsp: CpuIndex, tsc_hz: u64) -> bool {
    // What the region holds when the proof begins, settled (`sched::settle`, TASKS item 67): every
    // exited thread's stack given back, nothing kept for reuse, retired or being reclaimed, so the
    // counts that follow change only with what the proof does.
    let settled_before = crate::sched::settle(tsc_hz);
    let before = stack::counts();
    let mut ok = settled_before.is_ok();

    // Where the region is, said without its base.
    ok &= placed_at_random(com1);

    // The guards.
    let mut placed = true;
    let mut below = 0;
    for size in SIZES {
        let Some(made) = GuardedStack::new(size) else {
            placed = false;
            continue;
        };
        let (fine, faulted) = guarded(&made);
        placed &= fine;
        below += faulted;
    }
    let aps: u32 = cpus
        .iter()
        .filter(|&&c| c != bsp)
        .map(|&c| interrupt_stacks_guarded(c))
        .sum();
    let ap_count = cpus.len() as u32 - 1;
    let guards_ok = placed && below == 3 * SIZES.len() as u32 && aps == 4 * ap_count;
    ok &= guards_ok;
    let _ = writeln!(
        com1,
        "origin-kernel: kernel stacks: stacks of 4, 16 and 64 KiB each in a slot of its own in the region, every page read/write, not executable and global: {placed}; the page below each, the bottom of its slot and the bottom of the next faulted: {below} of {}; the application processors' interrupt stacks, as their TSSs name them, in the region with the page below each faulting: {aps} of {}{}",
        3 * SIZES.len(),
        4 * ap_count,
        if guards_ok {
            ""
        } else {
            ": kernel stacks failed"
        }
    );

    // The direct map.
    let whole = match crate::vm::direct_map_leaves() {
        Some((now, boot)) => {
            let whole = now.large == boot.large && now.huge == boot.huge;
            let _ = writeln!(
                com1,
                "origin-kernel: kernel stacks: the direct map has every 2 MiB page it was built with: {whole} ({} 2 MiB and {} 4 KiB leaves; {} and {} when the kernel's tables went live){}",
                now.large,
                now.small,
                boot.large,
                boot.small,
                if whole { "" } else { ": kernel stacks failed" }
            );
            whole
        }
        None => {
            let _ = writeln!(
                com1,
                "origin-kernel: kernel stacks: the direct map's leaves cannot be counted: kernel stacks failed"
            );
            false
        }
    };
    ok &= whole;

    // A freed stack, cached by another processor.
    ok &= reclaimed_out_of_reach(com1, cpus, bsp, tsc_hz);

    // A thread's own stack in the windows of its switch-out and its exit (TASKS item 70).
    ok &= switch_windows(com1, cpus, bsp, tsc_hz);

    // What it costs.
    ok &= cost(com1, tsc_hz);

    // Nothing leaks: the reader's stack is reaped on its own processor, then all is reclaimed.
    let settled_after = crate::sched::settle(tsc_hz);
    let after = stack::counts();
    let clean = after == before && settled_after.is_ok();
    ok &= clean;
    let _ = write!(
        com1,
        "origin-kernel: kernel stacks: every stack the proof made is given back: {after:?}, as before: {clean}{}",
        crate::sched::not_settled(settled_before, settled_after)
    );
    if !clean {
        let _ = write!(com1, " ({before:?} before): kernel stacks failed");
    }
    let _ = writeln!(com1);
    ok
}

/// Another processor caches the translations of a stack's pages; the stack is freed and
/// reclaimed; that processor's reads of them must then fault, and the slot be free again. With
/// one processor, this one's own reads must fault.
fn reclaimed_out_of_reach(
    com1: &mut Console,
    cpus: &[CpuIndex],
    bsp: CpuIndex,
    tsc_hz: u64,
) -> bool {
    let Some(made) = GuardedStack::new(MEASURED) else {
        let _ = writeln!(
            com1,
            "origin-kernel: kernel stacks: no memory for the reclaimed stack: kernel stacks failed"
        );
        return false;
    };
    let low = made.top() - MEASURED as u64;
    for (slot, page) in PAGES.iter().zip((low..).step_by(PAGE as usize)) {
        slot.store(page, Ordering::Release);
    }
    let other = cpus.iter().copied().find(|&c| c != bsp);
    if let Some(cpu) = other {
        if crate::sched::spawn(cpu, reader, 0).is_err() {
            let _ = writeln!(
                com1,
                "origin-kernel: kernel stacks: cannot spawn on CPU {cpu}: kernel stacks failed"
            );
            return false;
        }
        crate::sched::yield_until(|| STAGE.load(Ordering::Acquire) >= 1, tsc_hz, 5);
    }
    let cached = other.is_none() || STAGE.load(Ordering::Acquire) >= 1;
    // Freed, then retired with everything kept for reuse, then reclaimed: a shootdown each.
    // Another processor may reclaim it first, in its idle loop: this waits for whoever does, on
    // the stack's own slot. Its top is the base of the next slot, which may be free whatever
    // happens to this one: a wait on that slot ended at once while another processor was still
    // reclaiming the stack, and the reader read through translations it still held. So the slot
    // waited on must be the one the stack's last byte is in, on every boot.
    let slot = made.slot();
    let own = stack::slot_holding(made.top() - 1) == Some(slot);
    drop(made);
    stack::trim();
    stack::reclaim();
    // The slot is free only once its frames have been released (`Slots::reclaim`); the frames
    // themselves may be someone else's again by now, so they are not looked at.
    crate::sched::yield_until(|| stack::slot_is_free(slot), tsc_hz, 5);
    let free = stack::slot_is_free(slot);
    let (faulted, stale, reader) = match other {
        Some(cpu) => {
            STAGE.store(2, Ordering::Release);
            crate::sched::yield_until(|| STAGE.load(Ordering::Acquire) >= 3, tsc_hz, 5);
            (
                FAULTED.load(Ordering::Relaxed),
                STALE.load(Ordering::Relaxed),
                cpu.named(),
            )
        }
        None => {
            let faulted = PAGES
                .iter()
                .filter(|p| faults(p.load(Ordering::Acquire)))
                .count() as u64;
            (
                faulted,
                CACHED_PAGES as u64 - faulted,
                cpu::this_cpu().named(),
            )
        }
    };
    let pages = CACHED_PAGES as u64;
    let ok = cached && own && free && faulted == pages && stale == 0;
    let _ = writeln!(
        com1,
        "origin-kernel: kernel stacks: a stack's {pages} pages read on {reader}, then the stack freed and reclaimed: {faulted} of {pages} reads there faulted and {stale} saw the old pages; its slot, the one its last byte was in: {own}, free again: {free}{}",
        if ok { "" } else { ": kernel stacks failed" }
    );
    ok
}

/// Makes a stack of [`MEASURED`] bytes in each of `stacks`; returns the TSC ticks it took.
fn make(stacks: &mut [Option<GuardedStack>]) -> u64 {
    let start = rdtsc();
    for stack in stacks.iter_mut() {
        *stack = GuardedStack::new(MEASURED);
    }
    rdtsc().wrapping_sub(start)
}

/// Frees every stack in `stacks`; returns the TSC ticks it took.
fn free(stacks: &mut [Option<GuardedStack>]) -> u64 {
    let start = rdtsc();
    for stack in stacks.iter_mut() {
        drop(stack.take());
    }
    rdtsc().wrapping_sub(start)
}

/// What a kernel thread's stack costs: made from fresh frames, and from one kept for reuse;
/// freed; and reclaimed alone, which waits for a shootdown on every processor. Each phase starts
/// with no reclaim under way on any processor, so no shootdown interrupts it. Returns whether
/// every stack could be made.
fn cost(com1: &mut Console, tsc_hz: u64) -> bool {
    stack::trim();
    reclaimed(tsc_hz);
    let mut stacks: [Option<GuardedStack>; ROUNDS] = [const { None }; ROUNDS];
    let fresh = make(&mut stacks);
    let all = stacks.iter().all(Option::is_some);
    // The first `IDLE` stacks freed are kept for reuse, the rest retired.
    let freed = free(&mut stacks);
    reclaimed(tsc_hz);
    let reuse = stacks.get_mut(..stack::IDLE).unwrap_or_default();
    let kept = make(reuse);
    let reused = reuse.iter().all(Option::is_some);
    free(reuse);
    stack::trim();
    reclaimed(tsc_hz);
    // One stack at a time, freed, retired and reclaimed here; a round in which another
    // processor reclaimed it first, in its idle loop, is not counted.
    let (mut alone, mut reclaiming, mut interrupts) = (0, 0, 0);
    for _ in 0..stack::IDLE {
        drop(GuardedStack::new(MEASURED));
        stack::trim();
        let start = rdtsc();
        let done = stack::reclaim();
        let took = rdtsc().wrapping_sub(start);
        if done.stacks == 1 {
            alone += 1;
            reclaiming += took;
            interrupts += done.interrupts;
        }
    }
    reclaimed(tsc_hz);
    let ok = all && reused && alone > 0;
    let _ = writeln!(
        com1,
        "origin-kernel: kernel stacks: {ROUNDS} stacks of {MEASURED} bytes made and freed: all made {ok}; making one took {} ns from fresh frames, {} ns from one kept for reuse; freeing one {} ns; reclaiming one {} ns with {} interrupts ({alone} of {} reclaimed here) (QEMU, no budget)",
        nanos_each(fresh, ROUNDS, tsc_hz),
        nanos_each(kept, stack::IDLE, tsc_hz),
        nanos_each(freed, ROUNDS, tsc_hz),
        nanos_each(reclaiming, alone, tsc_hz),
        interrupts / alone.max(1) as u32,
        stack::IDLE
    );
    ok
}

// ---------------------------------------------------------------- the switch windows

/// The budget of every wait in the switch windows made with interrupts on, each a wait for a
/// step another processor takes: the boot flow's for the held thread to be ready, for its
/// processor to be held, and for its processor's reap, and the thread's for the boot flow's word.
/// Seconds, since the other processor may first have to be scheduled (ADR-0013 R1(b); the
/// addendum to the review of TASKS item 70, T70-8): TASKS item 66's R7 budget for a wait on
/// another processor, `sched::OTHERS_PATIENCE`, set when the two met.
const WINDOW_WAIT_SECONDS: u64 = crate::sched::OTHERS_PATIENCE;
/// The most the held processor spins with interrupts off for the boot flow's answer: below the
/// TLB shootdown's one-second patience (`tlb.rs`, `WAIT_SECONDS`), with margin, so that a
/// shootdown another processor sends meanwhile is answered in time (the review of TASKS item 70,
/// T70-5). A hold that reaches it gives up and fails the clause. Only this wait is capped: the
/// boot flow answers microseconds after it sees the hold, and its own wait for the hold takes
/// interrupts (T70-8). The longest hold seen in `boot-matrix` was 154 µs.
const HOLD_MILLIS: u64 = 500;

/// The handshake between the boot flow and the held thread:
/// - `READY`: the thread is about to switch away;
/// - `GO`: the boot flow now waits for the hold, and will answer at once, without yielding,
///   reaping or sending an interrupt;
/// - `HELD`: the thread's processor is held in the window, taken from `GO` by a compare-and-swap,
///   so a processor is held only while the boot flow waits for it;
/// - `ANSWERED`: the boot flow has asked, and the processor may go on;
/// - `GAVE_UP`: the boot flow stopped waiting, taken from `GO` the same way, so no processor is
///   held after it.
const READY: u64 = 1;
const GO: u64 = 2;
const HELD: u64 = 3;
const ANSWERED: u64 = 4;
const GAVE_UP: u64 = 5;
static WINDOW_STAGE: AtomicU64 = AtomicU64::new(0);
/// The slot of the held thread's own stack.
static WINDOW_SLOT: SetOnce<StackSlot> = SetOnce::new();
/// The TSC's frequency, for the held processor's budget.
static WINDOW_TSC_HZ: AtomicU64 = AtomicU64::new(0);
/// How long the processor was held, in TSC ticks, in each window, and whether it gave up.
static HELD_TICKS: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];
static HOLDS: AtomicU64 = AtomicU64::new(0);
static HOLD_GAVE_UP: AtomicBool = AtomicBool::new(false);

/// The TSC ticks in `millis` milliseconds.
fn ticks_in(millis: u64, tsc_hz: u64) -> u64 {
    tsc_hz.saturating_mul(millis) / 1000
}

/// Spins until `done`, for at most `budget` TSC ticks; whether it was done. Takes no lock, sends
/// nothing, and does not yield.
fn spin_until(done: impl Fn() -> bool, budget: u64) -> bool {
    let start = rdtsc();
    while !done() {
        if rdtsc().wrapping_sub(start) >= budget {
            return false;
        }
        core::hint::spin_loop();
    }
    true
}

/// As [`spin_until`], with an interrupt window between polls ([`cpu::interrupt_window`]), so the
/// caller answers shootdowns and takes its ticks while it waits. Still takes no lock, sends
/// nothing, and neither yields nor reaps.
fn wait_until(done: impl Fn() -> bool, budget: u64) -> bool {
    let start = rdtsc();
    while !done() {
        if rdtsc().wrapping_sub(start) >= budget {
            return false;
        }
        cpu::interrupt_window();
        core::hint::spin_loop();
    }
    true
}

/// The held thread, on another processor: it records its own stack's slot, then twice tells the
/// boot flow it is about to switch away, waits for its word, and switches: first by sleeping
/// for a tick, then by ending. Each switch is held in its window by [`hold_in_window`]. A
/// sleep, not a yield: a yield switches only to another ready thread, and the processor's idle
/// context runs only when none is.
extern "sysv64" fn windowed(_: u64) -> ! {
    let here = 0u64;
    if let Some(own) = stack::slot_holding(ptr::from_ref(black_box(&here)).addr() as u64) {
        let _ = WINDOW_SLOT.set(own);
    }
    let tsc_hz = WINDOW_TSC_HZ.load(Ordering::Relaxed);
    // Interrupts are on here, in thread context: this wait holds nothing up.
    let ready = || {
        WINDOW_STAGE.store(READY, Ordering::Release);
        spin_until(
            || WINDOW_STAGE.load(Ordering::Acquire) != READY,
            tsc_hz.saturating_mul(WINDOW_WAIT_SECONDS),
        );
    };
    ready();
    crate::sched::sleep(1);
    ready();
    // Nothing waits for its end but the exit window's own handshake, in the exit itself.
    crate::sched::finish(|| ())
}

/// Called by the scheduler's sleep and exit hooks once the switch is decided (`sched::on_sleep`,
/// `sched::on_exit`): the trap entry has not yet saved the outgoing frame `here`, nor moved the
/// processor's kernel-stack record (TASKS item 77). If the switch is away from the held thread's stack and the
/// boot flow is spinning for it (`GO`), this processor waits here, with interrupts off, until the
/// boot flow has asked about that stack, for at most [`HOLD_MILLIS`]. Otherwise it costs a relaxed
/// load.
pub(crate) fn hold_in_window(here: u64, next: u64) {
    if WINDOW_STAGE.load(Ordering::Relaxed) != GO || next == here {
        return;
    }
    let own = WINDOW_SLOT.get().copied();
    if own.is_none() || stack::slot_holding(here) != own {
        return;
    }
    if WINDOW_STAGE
        .compare_exchange(GO, HELD, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    let start = rdtsc();
    let answered = spin_until(
        || WINDOW_STAGE.load(Ordering::Acquire) == ANSWERED,
        ticks_in(HOLD_MILLIS, WINDOW_TSC_HZ.load(Ordering::Relaxed)),
    );
    if !answered {
        HOLD_GAVE_UP.store(true, Ordering::Relaxed);
    }
    let round = HOLDS.fetch_add(1, Ordering::Relaxed) as usize;
    if let Some(ticks) = HELD_TICKS.get(round) {
        ticks.store(rdtsc().wrapping_sub(start), Ordering::Relaxed);
    }
}

/// What the boot flow learnt in one window.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Asked {
    /// The stack was in use, as a drop would decide, or not.
    InUse(bool),
    /// The thread never said it was about to switch.
    NotReady,
    /// Its processor never reached the window.
    NotHeld,
}

impl core::fmt::Display for Asked {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InUse(in_use) => write!(f, "{in_use}"),
            Self::NotReady => f.write_str("the thread never said it was about to switch"),
            Self::NotHeld => f.write_str("its processor never reached the window"),
        }
    }
}

/// One window, as the boot flow saw it: what it learnt, and how long it waited, in TSC ticks,
/// for the thread to be ready and for its processor to be held (ruling (e)).
struct Window {
    asked: Asked,
    ready_wait: u64,
    held_wait: u64,
}

/// One window, from the boot flow: waits for the held thread to be ready, gives the word, waits
/// for its processor to be held, and asks the framework about the thread's stack. From the word
/// to the answer nothing here yields, reaps, reclaims or sends an interrupt, so nothing here
/// waits for the held processor, which does not answer interrupts. Until the processor is held,
/// this side takes interrupts between polls, for at most [`WINDOW_WAIT_SECONDS`], so a host
/// stall of a second fails nothing (ADR-0013 R1(b); T70-8). Once it is held, this side asks and
/// answers with interrupts off, in microseconds; only the held processor's own hold is capped,
/// at [`HOLD_MILLIS`].
fn ask_in_window(tsc_hz: u64) -> Window {
    let start = rdtsc();
    crate::sched::yield_until(
        || WINDOW_STAGE.load(Ordering::Acquire) == READY,
        tsc_hz,
        WINDOW_WAIT_SECONDS,
    );
    let ready_wait = rdtsc().wrapping_sub(start);
    let window = |asked, held_wait| Window {
        asked,
        ready_wait,
        held_wait,
    };
    if WINDOW_STAGE.load(Ordering::Acquire) != READY {
        return window(Asked::NotReady, 0);
    }
    WINDOW_STAGE.store(GO, Ordering::Release);
    let go = rdtsc();
    let held = wait_until(
        || WINDOW_STAGE.load(Ordering::Acquire) == HELD,
        tsc_hz.saturating_mul(WINDOW_WAIT_SECONDS),
    ) || WINDOW_STAGE
        .compare_exchange(GO, GAVE_UP, Ordering::AcqRel, Ordering::Acquire)
        .is_err_and(|stage| stage == HELD);
    let held_wait = rdtsc().wrapping_sub(go);
    if !held {
        return window(Asked::NotHeld, held_wait);
    }
    let in_use = WINDOW_SLOT
        .get()
        .is_some_and(|slot| stack::slot_in_use(*slot));
    WINDOW_STAGE.store(ANSWERED, Ordering::Release);
    window(Asked::InUse(in_use), held_wait)
}

/// Microseconds for `ticks` of the TSC.
fn micros(ticks: u64, tsc_hz: u64) -> u64 {
    nanos_each(ticks, 1, tsc_hz) / 1000
}

/// The switch windows (TASKS item 70), on the first processor after the boot processor.
fn switch_windows(com1: &mut Console, cpus: &[CpuIndex], bsp: CpuIndex, tsc_hz: u64) -> bool {
    let Some(cpu) = cpus.iter().copied().find(|&c| c != bsp) else {
        let _ = writeln!(
            com1,
            "origin-kernel: kernel stacks: a thread's own stack in its switch windows: not asked, with one processor, where no other can drop it while its own is in a window"
        );
        return true;
    };
    WINDOW_TSC_HZ.store(tsc_hz, Ordering::Relaxed);
    // Nothing kept for reuse and nothing retired before the thread starts (T70-4, T70-5): its
    // stack, once reaped, then has room in the cache of kept stacks, where no reclaim touches it,
    // and no reclaim's shootdown is under way while a processor is held.
    stack::trim();
    reclaimed(tsc_hz);
    if crate::sched::spawn(cpu, windowed, 0).is_err() {
        let _ = writeln!(
            com1,
            "origin-kernel: kernel stacks: cannot spawn on {}: kernel stacks failed",
            cpu.named()
        );
        return false;
    }
    let switched_out = ask_in_window(tsc_hz);
    // Room in the cache of kept stacks, before the thread ends: its drop then keeps its stack
    // there (`Idle`), which no reclaim touches until the proof trims, so a `Lost` state after the
    // reap can only be the drop's own leak (the coordinating session's ruling (d)). Without room
    // the clause cannot judge, and fails (T70-4).
    let room = stack::counts().idle < stack::IDLE;
    let ended = ask_in_window(tsc_hz);
    let slot = WINDOW_SLOT.get().copied();
    let reaped = |slot: StackSlot| stack::slot_state(slot) != Some(State::Live);
    crate::sched::yield_until(|| slot.is_some_and(reaped), tsc_hz, WINDOW_WAIT_SECONDS);
    let state = slot.and_then(stack::slot_state);
    let (given_back, how) = match state {
        _ if !room => (false, "not judged: no room was kept for it"),
        Some(State::Idle) => (true, "kept for reuse"),
        Some(State::Retired | State::Reclaiming | State::Free) => (true, "retired"),
        Some(State::Lost) => (false, "leaked"),
        Some(State::Live) => (false, "not reaped"),
        None => (false, "no slot"),
    };
    let held = |round: usize| {
        HELD_TICKS
            .get(round)
            .map_or(0, |t| micros(t.load(Ordering::Relaxed), tsc_hz))
    };
    let gave_up = HOLD_GAVE_UP.load(Ordering::Relaxed);
    let ok = switched_out.asked == Asked::InUse(true)
        && ended.asked == Asked::InUse(true)
        && given_back
        && !gave_up;
    let _ = writeln!(
        com1,
        "origin-kernel: kernel stacks: a thread's own stack, asked about from {} while {} switched it out (its switch decided, the thread's frame not yet saved): in use: {} (held {} us); while it ended: in use: {} (held {} us); given back by its processor's reap, not leaked: {given_back} ({how}); the boot flow waited {} and {} us for the thread to be ready, then {} and {} us for its processor to be held{}{}",
        bsp.named(),
        cpu.named(),
        switched_out.asked,
        held(0),
        ended.asked,
        held(1),
        micros(switched_out.ready_wait, tsc_hz),
        micros(ended.ready_wait, tsc_hz),
        micros(switched_out.held_wait, tsc_hz),
        micros(ended.held_wait, tsc_hz),
        if gave_up {
            "; a held processor gave up waiting"
        } else {
            ""
        },
        if ok { "" } else { ": kernel stacks failed" }
    );
    ok
}
