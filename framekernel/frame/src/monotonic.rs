//! The kernel's monotonic clock (CLAUDE.md §68): nanoseconds since kernel entry, read from the
//! TSC on any processor, once the boot has chosen the TSC's rate (`time`, above the
//! framework, chooses and publishes it; the framework reads it for its own timeouts).

use core::sync::atomic::{AtomicU64, Ordering};

use crate::clock::{Scale, SharedScale};
use crate::cpu::rdtsc;

/// The TSC when the kernel was entered: the clock's zero.
static ENTRY_TSC: AtomicU64 = AtomicU64::new(0);
/// TSC ticks to nanoseconds, once the rate has been published.
static SCALE: SharedScale = SharedScale::new();

/// Records the clock's zero: the first thing `_start` does.
pub fn mark_entry() {
    ENTRY_TSC.store(rdtsc(), Ordering::Relaxed);
}

/// The TSC when the kernel was entered.
pub fn entry_tsc() -> u64 {
    ENTRY_TSC.load(Ordering::Relaxed)
}

/// Publishes the chosen TSC rate; the clock reads 0 until this has run.
pub fn publish_scale(scale: Scale) {
    SCALE.publish(scale);
}

/// Nanoseconds since kernel entry; 0 until the TSC rate has been published.
pub fn monotonic_nanos() -> u64 {
    let Some(scale) = SCALE.get() else {
        return 0;
    };
    scale.nanos(rdtsc().saturating_sub(ENTRY_TSC.load(Ordering::Relaxed)))
}

/// The chosen TSC rate in Hz (1 before the rate is known), for spin-wait budgets.
pub fn tsc_hz() -> u64 {
    SCALE.get().map_or(1, |s| s.hz())
}
