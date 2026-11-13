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
//! - **Nothing leaks:** once the proof's stacks are given back and reclaimed, the region holds
//!   exactly what it held before.
//! - **What it costs** (CLAUDE.md rule 21: under QEMU, with no budget).
#![forbid(unsafe_code)]

use core::fmt::Write;
use core::sync::atomic::{AtomicU64, Ordering};

use origin_frame::stack::{self, GuardedStack};

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
    STAGE.store(3, Ordering::Release);
    crate::sched::exit()
}

/// Every retired stack reclaimed, here or by another processor: the others reclaim in their idle
/// loops (`sched::reap`), so this waits for the reclaims they have under way too.
fn reclaimed(tsc_hz: u64) {
    stack::reclaim();
    crate::sched::yield_until(|| stack::counts().retired == 0, tsc_hz, 2);
}

/// Every exited thread's stack given back, on every processor, then nothing kept for reuse,
/// retired or being reclaimed: so the counts that follow change only with what the proof does.
fn settle(tsc_hz: u64) {
    crate::sched::reap();
    crate::sched::yield_until(|| crate::sched::zombies() == 0, tsc_hz, 2);
    stack::trim();
    reclaimed(tsc_hz);
}

/// Proves the region; see the module documentation. Returns whether every part held.
pub(crate) fn prove(com1: &mut Console, cpus: &[CpuIndex], bsp: CpuIndex, tsc_hz: u64) -> bool {
    // What the region holds when the proof begins.
    settle(tsc_hz);
    let before = stack::counts();
    let mut ok = true;

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

    // What it costs.
    ok &= cost(com1, tsc_hz);

    // Nothing leaks: the reader's stack is reaped on its own processor, then all is reclaimed.
    settle(tsc_hz);
    let after = stack::counts();
    let clean = after == before;
    ok &= clean;
    let _ = write!(
        com1,
        "origin-kernel: kernel stacks: every stack the proof made is given back: {after:?}, as before: {clean}"
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
