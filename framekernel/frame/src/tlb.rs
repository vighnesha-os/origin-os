//! TLB shootdown (CLAUDE.md §28; Intel SDM Vol. 3A §4.10.5, "Propagation of
//! Paging-Structure Changes to Multiple Processors") and global kernel pages (ADR-0004 H5).
//!
//! Every address space shares the kernel half, so when a kernel mapping loses access
//! (unmapped, or a permission removed), every processor that may cache the old
//! translation must drop it before the memory is reused. A mapping that only gains access
//! needs nothing: processors do not cache non-present translations.
//!
//! [`shootdown`] invalidates one page on this processor, sends [`SHOOTDOWN_VECTOR`] to
//! every other online processor, and waits until each has invalidated it too
//! ([`shootdown_all`] flushes everything instead):
//! - Requests go one at a time, under an initiator lock. A processor waiting for that lock
//!   serves the request in progress meanwhile, so two initiators never wait on each other.
//! - Each request has a generation. A processor records the last generation it served, and
//!   the initiator waits until every target's record reaches its own.
//! - Targets are the processors whose schedulers run ([`mark_online`]), recorded by CPU
//!   index and sent the IPI at the x2APIC ID recorded for that index: nothing is
//!   broadcast to processors that never started.
//! - The caller must hold no lock that another processor may wait for with interrupts
//!   disabled: that processor could then never take the interrupt. `vm` releases its own
//!   lock before it shoots down.
//!
//! Kernel-half mappings are global (CR4.PGE), so their translations survive the CR3 loads
//! of process switches; a full flush toggles CR4.PGE (`cpu::flush_tlb_all`). PCIDs for
//! user address spaces are not used yet.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering, fence};

use origin_sync::SpinLock;

use crate::cpu::{self, CpuIndex, MAX_CPUS, rdtsc};
use crate::timer::X2Apic;

/// The shootdown interrupt's vector: a fixed IPI, above every device vector in use.
pub const SHOOTDOWN_VECTOR: u8 = 0xF0;
/// A request to flush everything rather than one page.
const ALL: u64 = u64::MAX;
/// An initiator gives up waiting after this many seconds of the TSC (for its turn, and for
/// every other processor's answer): time, not polls, so the bound does not depend on what
/// PAUSE costs on this processor.
const WAIT_SECONDS: u64 = 1;
/// The TSC rate to assume before the clock has measured one (`time::tsc_hz` is then 1, and
/// no other processor is running to wait for).
const MIN_TSC_HZ: u64 = 1_000_000_000;
/// ICR level bit: assert (the only meaningful level for fixed IPIs).
const ICR_ASSERT: u32 = 1 << 14;

/// Processors that take shootdown requests.
static ONLINE: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];
/// Held by the processor whose request is in progress.
static INITIATOR: SpinLock<()> = SpinLock::new(());
/// The request in progress: a page address, or [`ALL`].
static REQUEST: AtomicU64 = AtomicU64::new(0);
/// The latest request's generation (the first is 1).
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// The last generation each processor served.
static SERVED: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
/// TSC cycles the last completed shootdown took, for the boot report.
pub static LAST_CYCLES: AtomicU64 = AtomicU64::new(0);
/// Each CPU index whose processor turned on global pages.
pub static GLOBAL_PAGES: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];

fn this_cpu() -> usize {
    cpu::this_cpu().get()
}

/// Turns on global pages on this processor and marks its CPU index, for the boot report.
/// [`prove`] counts only the processors online, so an AP that got this far but never came
/// online (it was parked, `smp::park`) is not counted.
///
/// # Safety
///
/// As for `cpu::enable_global_pages`.
pub unsafe fn enable_global_pages() -> bool {
    // SAFETY: the caller's contract.
    let on = unsafe { cpu::enable_global_pages() };
    if on && let Some(global) = GLOBAL_PAGES.get(this_cpu()) {
        global.store(true, Ordering::Relaxed);
    }
    on
}

/// Makes this processor a shootdown target, then flushes its whole TLB, so it holds no
/// translation older than the first request it is sent. In that order: a request whose
/// initiator does not yet see this processor online was made before the flush.
pub fn mark_online() {
    if let Some(online) = ONLINE.get(this_cpu()) {
        online.store(true, Ordering::SeqCst);
    }
    fence(Ordering::SeqCst);
    cpu::flush_tlb_all();
}

fn invalidate(request: u64) {
    if request == ALL {
        cpu::flush_tlb_all();
    } else {
        cpu::invlpg(request);
    }
}

/// Serves the request in progress, if this processor has not yet. Runs in the shootdown
/// interrupt, and in initiators waiting for their turn.
pub fn service() {
    let Some(served) = SERVED.get(this_cpu()) else {
        return;
    };
    // Acquire: the request read below is the one this generation published (its initiator
    // stored it before the generation, and the next initiator cannot start until every
    // target, this processor included, has served this one).
    let generation = GENERATION.load(Ordering::Acquire);
    if served.load(Ordering::Relaxed) < generation {
        invalidate(REQUEST.load(Ordering::Relaxed));
        served.store(generation, Ordering::Release);
    }
}

/// Invalidates the page holding `addr` on every online processor, after the caller changed
/// its mapping. Returns the TSC cycles it took.
///
/// # Panics
///
/// If a processor does not answer within about a second. The caller is about to reuse
/// memory that processor may still reach, so carrying on would not be memory-safe.
pub fn shootdown(addr: u64) -> u64 {
    shoot(addr & !0xFFF)
}

/// Flushes every TLB entry on every online processor: for changes to many pages at once.
///
/// # Panics
///
/// As for [`shootdown`].
pub fn shootdown_all() -> u64 {
    shoot(ALL)
}

fn shoot(request: u64) -> u64 {
    let outcome = cpu::without_interrupts(|| {
        let limit = crate::monotonic::tsc_hz()
            .max(MIN_TSC_HZ)
            .saturating_mul(WAIT_SECONDS);
        let waiting_since = rdtsc();
        let turn = loop {
            if let Some(turn) = INITIATOR.try_lock() {
                break turn;
            }
            service();
            core::hint::spin_loop();
            if rdtsc().wrapping_sub(waiting_since) >= limit {
                return Err(usize::MAX);
            }
        };
        let start = rdtsc();
        invalidate(request);
        let me = this_cpu();
        REQUEST.store(request, Ordering::Relaxed);
        let generation = GENERATION.fetch_add(1, Ordering::Release) + 1;
        // The caller's page-table change and this request must be visible before the
        // online set is read: a processor that is not yet online flushes after this.
        fence(Ordering::SeqCst);
        // SAFETY: CPL 0; this processor's APIC is in x2APIC mode (its scheduler runs, so
        // `timer::init` or `ap_entry` enabled it).
        let lapic = unsafe { X2Apic::assume_enabled() };
        let mut targets = [false; MAX_CPUS];
        for (id, (online, target)) in ONLINE.iter().zip(targets.iter_mut()).enumerate() {
            if id != me && online.load(Ordering::SeqCst) {
                // Every online processor is waited for. One without an APIC ID on record
                // (impossible: `smp` records it before the processor starts) gets no IPI,
                // so the wait below times out and panics rather than skipping it.
                *target = true;
                if let Some(apic_id) = CpuIndex::new(id).and_then(CpuIndex::apic_id) {
                    // SAFETY: a fixed IPI with the shootdown vector, whose handler only
                    // invalidates TLB entries and acknowledges; `apic_id` is the x2APIC ID
                    // recorded for online processor `id`.
                    unsafe {
                        lapic.send_ipi(apic_id, ICR_ASSERT | u32::from(SHOOTDOWN_VECTOR));
                    }
                }
            }
        }
        for (id, (&target, served)) in targets.iter().zip(SERVED.iter()).enumerate() {
            if !target {
                continue;
            }
            while served.load(Ordering::Acquire) < generation {
                if rdtsc().wrapping_sub(start) >= limit {
                    // Looked at once more: this processor may have been the one away.
                    if served.load(Ordering::Acquire) >= generation {
                        break;
                    }
                    return Err(id);
                }
                core::hint::spin_loop();
            }
        }
        drop(turn);
        let cycles = rdtsc().wrapping_sub(start);
        LAST_CYCLES.store(cycles, Ordering::Relaxed);
        Ok(cycles)
    });
    match outcome {
        Ok(cycles) => cycles,
        Err(usize::MAX) => panic!("TLB shootdown: no turn within a second"),
        Err(cpu) => panic!("TLB shootdown: CPU {cpu} did not answer within a second"),
    }
}
