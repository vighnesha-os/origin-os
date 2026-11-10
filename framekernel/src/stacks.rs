//! The kernel stacks (ADR-0004 Amendment 1, A2; CLAUDE.md §25, §28): what a stack costs, and
//! the direct map's page sizes. The mechanism is the framework's (`origin_frame::stack`).
#![forbid(unsafe_code)]

use core::fmt::Write;

use origin_frame::stack::{self, GuardedStack};

use crate::console::Console;
use crate::cpu::rdtsc;

/// Stacks the cost measurement makes, then frees.
const ROUNDS: usize = 64;
/// The size it measures: a kernel thread's (`sched`'s `THREAD_STACK`).
const MEASURED: usize = 16 * 1024;

/// Nanoseconds per operation, for `ticks` of the TSC spent on `count` of them.
fn nanos_each(ticks: u64, count: usize, tsc_hz: u64) -> u64 {
    let nanos = u128::from(ticks) * 1_000_000_000 / u128::from(tsc_hz.max(1));
    u64::try_from(nanos / count.max(1) as u128).unwrap_or(u64::MAX)
}

/// Every retired stack reclaimed, here or by another processor: the others reclaim in their idle
/// loops (`sched::reap`), so this waits for the reclaims they have under way too.
fn reclaimed(tsc_hz: u64) {
    stack::reclaim();
    crate::sched::yield_until(|| stack::counts().retired == 0, tsc_hz, 2);
}

/// Reports the direct map's page sizes, now and when the kernel's tables went live, and what
/// a kernel thread's stack costs (CLAUDE.md rule 21: measured under QEMU, with no budget).
pub(crate) fn report(com1: &mut Console, tsc_hz: u64) {
    match crate::vm::direct_map_leaves() {
        Some((now, boot)) => {
            let _ = writeln!(
                com1,
                "origin-kernel: kernel stacks: the direct map has {} 2 MiB and {} 4 KiB leaves; {} and {} when the kernel's tables went live",
                now.large, now.small, boot.large, boot.small
            );
        }
        None => {
            let _ = writeln!(
                com1,
                "origin-kernel: kernel stacks: the direct map's leaves cannot be counted"
            );
        }
    }
    cost(com1, tsc_hz);
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
