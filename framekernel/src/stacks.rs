//! The kernel stacks (ADR-0004 Amendment 1, A2; CLAUDE.md §25, §28): what a stack costs, and
//! the direct map's page sizes. The mechanism is the framework's (`origin_frame::stack`).
#![forbid(unsafe_code)]

use core::fmt::Write;

use origin_frame::stack::GuardedStack;

use crate::console::Console;
use crate::cpu::rdtsc;

/// Stacks the cost measurement makes, then frees.
const ROUNDS: usize = 64;
/// The size it measures: a kernel thread's (`sched`'s `THREAD_STACK`).
const MEASURED: usize = 16 * 1024;

/// Nanoseconds per operation, for `ticks` of the TSC spent on [`ROUNDS`] of them.
fn nanos_each(ticks: u64, tsc_hz: u64) -> u64 {
    let nanos = u128::from(ticks) * 1_000_000_000 / u128::from(tsc_hz.max(1));
    u64::try_from(nanos / ROUNDS as u128).unwrap_or(u64::MAX)
}

/// Reports the direct map's page sizes, now and when the kernel's tables went live, and what
/// making and freeing a kernel thread's stack costs (CLAUDE.md rule 21: measured under QEMU,
/// with no budget).
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
    let mut stacks: [Option<GuardedStack>; ROUNDS] = [const { None }; ROUNDS];
    let start = rdtsc();
    for stack in &mut stacks {
        *stack = GuardedStack::new(MEASURED);
    }
    let made = rdtsc().wrapping_sub(start);
    let all = stacks.iter().all(Option::is_some);
    let start = rdtsc();
    for stack in &mut stacks {
        drop(stack.take());
    }
    let freed = rdtsc().wrapping_sub(start);
    let _ = writeln!(
        com1,
        "origin-kernel: kernel stacks: {ROUNDS} stacks of {MEASURED} bytes made and freed: all made {all}; making one took {} ns, freeing one {} ns (QEMU, no budget)",
        nanos_each(made, tsc_hz),
        nanos_each(freed, tsc_hz)
    );
}
