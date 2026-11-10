//! The kernel's monotonic clock (CLAUDE.md §68): nanoseconds since kernel entry, read from the
//! TSC on any processor, once the boot has chosen the TSC's rate (`time`, above the
//! framework, chooses and publishes it; the framework reads it for its own timeouts).
//!
//! Neither Intel nor AMD promises that two processors' counters agree, so every reading passes
//! through a floor shared by all processors ([`MonotonicFloor`], ADR-0012): no caller on any
//! processor can be given less than a caller has been given before, whatever the TSCs do.
#![forbid(unsafe_code)]

use core::sync::atomic::{AtomicU64, Ordering};

use crate::clock::{Scale, SharedScale};
use crate::cpu::rdtsc;
use crate::sync::floor::MonotonicFloor;

/// The TSC when the kernel was entered: the clock's zero.
static ENTRY_TSC: AtomicU64 = AtomicU64::new(0);
/// TSC ticks to nanoseconds, once the rate has been published.
static SCALE: SharedScale = SharedScale::new();
/// The latest time any caller has been given, on any processor (ADR-0012).
static FLOOR: MonotonicFloor = MonotonicFloor::new();

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
///
/// Never less than any value this function has returned earlier, on any processor, whatever
/// the processors' TSCs say (ADR-0012). Two calls can return the same value. Safe in any
/// context, interrupt and NMI handlers included.
pub fn monotonic_nanos() -> u64 {
    monotonic_reading().map_or(0, |(_, given)| given)
}

/// What this processor's TSC says in nanoseconds since kernel entry, and what the clock gives
/// out for that: the same, or the floor if the floor is above it. `None` before the rate has
/// been published. For the boot proof, which counts the readings the floor held back; everyone
/// else wants [`monotonic_nanos`].
#[inline]
pub fn monotonic_reading() -> Option<(u64, u64)> {
    let scale = SCALE.get()?;
    let counted = scale.nanos(rdtsc().saturating_sub(ENTRY_TSC.load(Ordering::Relaxed)));
    Some((counted, FLOOR.advance(counted)))
}

/// The published rate, or `None` before it has been published. For measuring what the floor
/// adds to a read: a caller that used it to read the clock would bypass the floor.
pub fn published_scale() -> Option<Scale> {
    SCALE.get()
}

/// The chosen TSC rate in Hz (1 before the rate is known), for spin-wait budgets.
pub fn tsc_hz() -> u64 {
    SCALE.get().map_or(1, |s| s.hz())
}
