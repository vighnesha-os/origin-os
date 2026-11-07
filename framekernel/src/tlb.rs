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
//! - Targets are the processors whose schedulers run ([`mark_online`]), each named by its
//!   x2APIC ID: nothing is broadcast to processors that never started.
//! - The caller must hold no lock that another processor may wait for with interrupts
//!   disabled: that processor could then never take the interrupt. `vm` releases its own
//!   lock before it shoots down.
//!
//! Kernel-half mappings are global (CR4.PGE), so their translations survive the CR3 loads
//! of process switches; a full flush toggles CR4.PGE (`cpu::flush_tlb_all`). PCIDs for
//! user address spaces are not used yet.

use alloc::alloc::{Layout, alloc_zeroed, dealloc};
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering, fence};

use origin_sync::SpinLock;

use crate::console::Console;
use crate::cpu::{self, rdtsc};
use crate::sched::MAX_CPUS;
use crate::timer::X2Apic;

/// The shootdown interrupt's vector: a fixed IPI, above every device vector in use.
pub(crate) const SHOOTDOWN_VECTOR: u8 = 0xF0;
/// A request to flush everything rather than one page.
const ALL: u64 = u64::MAX;
/// Polls before an initiator gives up waiting: on the order of a second.
const WAIT_POLLS: u64 = 1 << 26;
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
static LAST_CYCLES: AtomicU64 = AtomicU64::new(0);
/// Processors that turned on global pages.
static GLOBAL_PAGES: AtomicU64 = AtomicU64::new(0);

fn this_cpu() -> usize {
    usize::try_from(cpu::cpu_id()).unwrap_or(usize::MAX)
}

/// Turns on global pages on this processor and counts it, for the boot report.
///
/// # Safety
///
/// As for `cpu::enable_global_pages`.
pub(crate) unsafe fn enable_global_pages() -> bool {
    // SAFETY: the caller's contract.
    let on = unsafe { cpu::enable_global_pages() };
    if on {
        GLOBAL_PAGES.fetch_add(1, Ordering::Relaxed);
    }
    on
}

/// Makes this processor a shootdown target, then flushes its whole TLB, so it holds no
/// translation older than the first request it is sent. In that order: a request whose
/// initiator does not yet see this processor online was made before the flush.
pub(crate) fn mark_online() {
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
pub(crate) fn service() {
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
pub(crate) fn shootdown(addr: u64) -> u64 {
    shoot(addr & !0xFFF)
}

/// Flushes every TLB entry on every online processor: for changes to many pages at once.
///
/// # Panics
///
/// As for [`shootdown`].
pub(crate) fn shootdown_all() -> u64 {
    shoot(ALL)
}

fn shoot(request: u64) -> u64 {
    let outcome = cpu::without_interrupts(|| {
        let mut polls = 0u64;
        let turn = loop {
            if let Some(turn) = INITIATOR.try_lock() {
                break turn;
            }
            service();
            core::hint::spin_loop();
            polls += 1;
            if polls == WAIT_POLLS {
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
                *target = true;
                // SAFETY: a fixed IPI with the shootdown vector, whose handler only
                // invalidates TLB entries and acknowledges; `id` is an online processor's
                // x2APIC ID.
                unsafe {
                    lapic.send_ipi(id as u32, ICR_ASSERT | u32::from(SHOOTDOWN_VECTOR));
                }
            }
        }
        for (id, (&target, served)) in targets.iter().zip(SERVED.iter()).enumerate() {
            if !target {
                continue;
            }
            let mut polls = 0u64;
            while served.load(Ordering::Acquire) < generation {
                polls += 1;
                if polls == WAIT_POLLS {
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

/// The pages the proof unmaps: one with a single-page shootdown, two with a full flush.
static PAGES: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];
/// 1: cache the pages; 2: probe them.
static STAGE: AtomicU64 = AtomicU64::new(0);
static CACHED: AtomicU64 = AtomicU64::new(0);
static PROBED: AtomicU64 = AtomicU64::new(0);
/// Probes, after the shootdowns, that faulted as they must, and that read a stale
/// translation instead.
static FAULTED: AtomicU64 = AtomicU64::new(0);
static STALE: AtomicU64 = AtomicU64::new(0);

/// An application processor's thread: reads the proof pages, so its TLB holds their
/// translations; stops touching them; then, once they are unmapped, probes each.
extern "sysv64" fn toucher(_: u64) -> ! {
    let mut sum = 0u64;
    for _ in 0..64 {
        for page in &PAGES {
            let p =
                core::ptr::with_exposed_provenance::<u64>(page.load(Ordering::Acquire) as usize);
            // SAFETY: each proof page is a live, mapped heap page until stage 2, which this
            // thread waits for before touching the pages again.
            sum = sum.wrapping_add(unsafe { p.read_volatile() });
        }
    }
    core::hint::black_box(sum);
    CACHED.fetch_add(1, Ordering::AcqRel);
    while STAGE.load(Ordering::Acquire) < 2 {
        core::hint::spin_loop();
    }
    for page in &PAGES {
        let addr = page.load(Ordering::Acquire);
        // SAFETY: the page is RAM (a heap page), not MMIO; the probe only reads it.
        match unsafe { cpu::probe_read(addr) } {
            cpu::Probe::Faulted { error: 0, address } if address == addr => {
                FAULTED.fetch_add(1, Ordering::Relaxed);
            }
            _ => {
                STALE.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    PROBED.fetch_add(1, Ordering::AcqRel);
    crate::sched::exit()
}

/// Proves cross-processor invalidation and global pages:
/// - every processor turned on global pages, and the kernel's tables mark the whole kernel
///   half global and nothing in the lower half;
/// - every application processor caches three pages; the boot processor unmaps one with a
///   single-page shootdown and two with one full flush; every processor's next read of
///   each page must fault.
pub(crate) fn prove(com1: &mut Console, cpus: &[u32], bsp: usize, tsc_hz: u64) -> bool {
    let mut ok = true;
    let census = crate::vm::leaf_census();
    let global = GLOBAL_PAGES.load(Ordering::Relaxed);
    let census_ok =
        census.is_some_and(|c| c.upper_local == 0 && c.lower_global == 0 && c.upper_global > 0);
    ok &= census_ok && global == cpus.len() as u64;
    let _ = writeln!(
        com1,
        "origin-kernel: TLB: global pages on {global} of {} processors; kernel's tables: {:?}{}",
        cpus.len(),
        census,
        if ok { "" } else { ": TLB failed" }
    );
    let Ok(layout) = Layout::from_size_align(4096, 4096) else {
        return false;
    };
    let mut pages = [0u64; 3];
    for (slot, page) in pages.iter_mut().zip(&PAGES) {
        // SAFETY: `layout` has a non-zero size.
        let p = unsafe { alloc_zeroed(layout) };
        if p.is_null() {
            return false;
        }
        *slot = p.expose_provenance() as u64;
        page.store(*slot, Ordering::Release);
    }
    let mut aps = 0u64;
    for &id in cpus.iter().filter(|&&id| id as usize != bsp) {
        if crate::sched::spawn(id as usize, toucher, 0).is_err() {
            let _ = writeln!(
                com1,
                "origin-kernel: TLB: cannot spawn on CPU {id}: TLB failed"
            );
            return false;
        }
        aps += 1;
    }
    STAGE.store(1, Ordering::Release);
    crate::sched::yield_until(|| CACHED.load(Ordering::Acquire) == aps, tsc_hz, 2);
    let [one, two, three] = pages;
    let single = crate::vm::make_guard(one).is_ok();
    let single_cycles = LAST_CYCLES.load(Ordering::Relaxed);
    let batch = crate::vm::make_guards(&[two, three]).is_ok();
    let batch_cycles = LAST_CYCLES.load(Ordering::Relaxed);
    STAGE.store(2, Ordering::Release);
    crate::sched::yield_until(|| PROBED.load(Ordering::Acquire) == aps, tsc_hz, 2);
    let (faulted, stale) = (
        FAULTED.load(Ordering::Relaxed),
        STALE.load(Ordering::Relaxed),
    );
    let probes_ok = single
        && batch
        && PROBED.load(Ordering::Acquire) == aps
        && faulted == 3 * aps
        && stale == 0;
    ok &= probes_ok;
    let _ = writeln!(
        com1,
        "origin-kernel: TLB: {aps} other processors cached 3 pages; after one single-page shootdown ({single_cycles} TSC cycles) and one full flush of two pages ({batch_cycles} cycles), {faulted} of their {} reads faulted and {stale} saw a stale translation{}",
        3 * aps,
        if probes_ok { "" } else { ": TLB failed" }
    );
    for page in pages {
        if crate::vm::unguard(page).is_ok() {
            // SAFETY: `page` came from `alloc_zeroed(layout)` and is mapped again.
            unsafe {
                dealloc(
                    core::ptr::with_exposed_provenance_mut(page as usize),
                    layout,
                )
            };
        }
    }
    ok
}
