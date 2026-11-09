//! The boot proof of TLB shootdown and global kernel pages (CLAUDE.md §28, ADR-0004 H5). The
//! mechanism is the framework's (`origin_frame::tlb`).
#![forbid(unsafe_code)]

use core::fmt::Write;
use core::sync::atomic::{AtomicU64, Ordering};

use origin_frame::stack::GuardablePage;
pub(crate) use origin_frame::tlb::{GLOBAL_PAGES, LAST_CYCLES, mark_online};

use crate::console::Console;
use crate::cpu::{self, CpuIndex};

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

/// An application processor's thread: reads the proof pages (`probe_read` both reads a byte
/// and proves it mapped), so its TLB holds their translations; stops touching them; then, once
/// they are unmapped, probes each.
extern "sysv64" fn toucher(_: u64) -> ! {
    let mut touched = 0u64;
    for _ in 0..64 {
        for page in &PAGES {
            if cpu::probe_read(page.load(Ordering::Acquire)) == cpu::Probe::Completed {
                touched += 1;
            }
        }
    }
    core::hint::black_box(touched);
    CACHED.fetch_add(1, Ordering::AcqRel);
    while STAGE.load(Ordering::Acquire) < 2 {
        core::hint::spin_loop();
    }
    for page in &PAGES {
        let addr = page.load(Ordering::Acquire);
        match cpu::probe_read(addr) {
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
pub(crate) fn prove(com1: &mut Console, cpus: &[CpuIndex], bsp: CpuIndex, tsc_hz: u64) -> bool {
    let mut ok = true;
    let census = crate::vm::leaf_census();
    let global = cpus
        .iter()
        .filter(|c| {
            GLOBAL_PAGES
                .get(c.get())
                .is_some_and(|g| g.load(Ordering::Relaxed))
        })
        .count();
    let census_ok =
        census.is_some_and(|c| c.upper_local == 0 && c.lower_global == 0 && c.upper_global > 0);
    ok &= census_ok && global == cpus.len();
    let _ = writeln!(
        com1,
        "origin-kernel: TLB: global pages on {global} of {} processors; kernel's tables: {:?}{}",
        cpus.len(),
        census,
        if ok { "" } else { ": TLB failed" }
    );
    let (Some(mut one), Some(mut two), Some(mut three)) = (
        GuardablePage::new(),
        GuardablePage::new(),
        GuardablePage::new(),
    ) else {
        return false;
    };
    for (slot, page) in PAGES.iter().zip([&one, &two, &three]) {
        slot.store(page.addr(), Ordering::Release);
    }
    let mut aps = 0u64;
    for &id in cpus.iter().filter(|&&id| id != bsp) {
        if crate::sched::spawn(id, toucher, 0).is_err() {
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
    let single = one.unmap();
    let single_cycles = LAST_CYCLES.load(Ordering::Relaxed);
    let batch = GuardablePage::unmap_all(&mut [&mut two, &mut three]);
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
    ok
}
