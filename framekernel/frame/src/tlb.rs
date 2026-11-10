//! TLB shootdown on the machine (CLAUDE.md §28; Intel SDM Vol. 3A §4.10.5) and global kernel
//! pages (ADR-0004 H5). The protocol is `origin_sync::shootdown`; this is what it asks of a real
//! processor: `invlpg`, a full flush, an IPI, and the TSC for how long to wait.
//!
//! Every address space shares the kernel half, so when a kernel mapping loses access
//! (unmapped, or a permission removed), every processor that may cache the old
//! translation must drop it before the memory is reused. A mapping that only gains access
//! needs nothing: processors do not cache non-present translations.
//!
//! Targets are the processors whose schedulers run ([`mark_online`]), recorded by CPU index and
//! sent the IPI at the x2APIC ID recorded for that index: nothing is broadcast to processors
//! that never started. The caller must hold no lock that another processor may wait for with
//! interrupts disabled: `vm` releases its own lock before it shoots down.
//!
//! Kernel-half mappings are global (CR4.PGE), so their translations survive the CR3 loads
//! of process switches; a full flush toggles CR4.PGE (`cpu::flush_tlb_all`). PCIDs for
//! user address spaces are not used yet.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use origin_sync::shootdown::{Completed, Request, Scope, Shootdown, Stalled, TlbHardware};

use crate::cpu::{self, CpuIndex, MAX_CPUS, rdtsc};
use crate::timer::X2Apic;

/// The shootdown interrupt's vector: a fixed IPI, above every device vector in use.
pub const SHOOTDOWN_VECTOR: u8 = 0xF0;
/// An initiator gives up waiting for one processor's answer after this many seconds of the
/// TSC: time, not polls, so the bound does not depend on what PAUSE costs on this processor.
const WAIT_SECONDS: u64 = 1;
/// The TSC rate to assume before the clock has measured one (`time::tsc_hz` is then 1, and
/// no other processor is running to wait for).
const MIN_TSC_HZ: u64 = 1_000_000_000;
/// ICR level bit: assert (the only meaningful level for fixed IPIs).
const ICR_ASSERT: u32 = 1 << 14;

/// The machine's shootdown state.
static SHOOTDOWN: Shootdown<MAX_CPUS> = Shootdown::new();
/// TSC cycles the last completed shootdown took, for the boot report.
pub static LAST_CYCLES: AtomicU64 = AtomicU64::new(0);
/// Each CPU index whose processor turned on global pages.
pub static GLOBAL_PAGES: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];

/// What the protocol asks of the processor it runs on.
struct Hardware;

impl TlbHardware for Hardware {
    fn this_cpu(&self) -> usize {
        cpu::this_cpu().get()
    }

    fn invalidate(&self, request: Request) {
        match request {
            Request::Pages { base, pages } => {
                for page in 0..pages {
                    cpu::invlpg(base + page * origin_sync::shootdown::PAGE);
                }
            }
            Request::NonGlobal => cpu::flush_tlb_non_global(),
            Request::Everything => cpu::flush_tlb_all(),
        }
    }

    fn send_ipi(&self, target: usize) {
        // A processor without an APIC ID on record (impossible: `smp` records it before the
        // processor starts) gets no IPI, so the wait times out rather than skipping it.
        let Some(apic_id) = CpuIndex::new(target).and_then(CpuIndex::apic_id) else {
            return;
        };
        // SAFETY: CPL 0; this processor's APIC is in x2APIC mode (it is making a shootdown
        // request, so its scheduler runs, so `timer::init` or `ap_entry` enabled it).
        let lapic = unsafe { X2Apic::assume_enabled() };
        // SAFETY: a fixed IPI with the shootdown vector, whose handler only invalidates TLB
        // entries and acknowledges; `apic_id` is the x2APIC ID recorded for online processor
        // `target`.
        unsafe { lapic.send_ipi(apic_id, ICR_ASSERT | u32::from(SHOOTDOWN_VECTOR)) };
    }

    fn without_interrupts<R>(&self, f: impl FnOnce() -> R) -> R {
        cpu::without_interrupts(f)
    }

    fn now(&self) -> u64 {
        rdtsc()
    }

    fn patience(&self) -> u64 {
        crate::monotonic::tsc_hz()
            .max(MIN_TSC_HZ)
            .saturating_mul(WAIT_SECONDS)
    }

    fn current_root(&self) -> u64 {
        cpu::read_cr3()
    }
}

fn this_cpu() -> usize {
    cpu::this_cpu().get()
}

/// Turns on global pages on this processor and marks its CPU index, for the boot report.
/// The proof counts only the processors online, so an AP that got this far but never came
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

/// Makes this processor a shootdown target, then flushes its whole TLB.
pub fn mark_online() {
    SHOOTDOWN.mark_online(&Hardware);
}

/// Runs `load` (the CR3 write) as this processor's move to address space `to`, with the record
/// of it, all with interrupts off (`Shootdown::switch_space`).
pub(crate) fn switch_space(to: u64, load: impl FnOnce()) {
    SHOOTDOWN.switch_space(&Hardware, to, load);
}

/// Whether any processor has the address space `root` loaded. A user space's tables are freed
/// only when this is false.
pub(crate) fn root_loaded(root: u64) -> bool {
    SHOOTDOWN.processors_holding(root) != 0
}

/// Serves the request in progress, if this processor has not yet: for a loop that may be
/// holding up the processor that made it.
pub fn service() {
    SHOOTDOWN.service(&Hardware);
}

/// How many shootdown interrupts each processor has taken.
static INTERRUPTS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// The shootdown interrupt.
pub(crate) fn interrupted() {
    if let Some(taken) = INTERRUPTS.get(this_cpu()) {
        taken.fetch_add(1, Ordering::Relaxed);
    }
    service();
}

/// How many shootdown interrupts processor `cpu` has taken: for the proof that a request for
/// one address space leaves the processors outside it alone.
pub fn interrupts_taken(cpu: CpuIndex) -> u64 {
    INTERRUPTS
        .get(cpu.get())
        .map_or(0, |taken| taken.load(Ordering::Relaxed))
}

/// Drops what `request` names from every processor that has the address space `root` loaded,
/// after a change to one of its user mappings. The caller changed the page tables first.
pub(crate) fn shootdown_space(root: u64, request: Request) -> Result<Completed, Stalled> {
    SHOOTDOWN.shoot(&Hardware, request, Scope::Space(root))
}

/// The deferred free of a user mapping: `value` is what the caller took out of `root`'s page
/// tables, and `release` gets it back once no processor can reach it (`Shootdown::retire`).
pub(crate) fn retire_in_space<T>(
    root: u64,
    request: Request,
    value: T,
    release: impl FnOnce(T),
) -> Result<Completed, Stalled> {
    SHOOTDOWN.retire(&Hardware, value, request, Scope::Space(root), release)
}

/// How many processors have the address space `root` loaded now.
pub(crate) fn processors_holding(root: u64) -> u32 {
    SHOOTDOWN.processors_holding(root)
}

/// The result of a kernel request. A processor that never answers is left holding a
/// translation of memory the caller is about to reuse, so the kernel stops.
fn finished(outcome: Result<Completed, Stalled>) -> u64 {
    match outcome {
        Ok(done) => {
            LAST_CYCLES.store(done.ticks, Ordering::Relaxed);
            done.ticks
        }
        Err(Stalled { cpu }) => panic!("TLB shootdown: CPU {cpu} did not answer within a second"),
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
    finished(SHOOTDOWN.shoot(&Hardware, Request::page(addr), Scope::Kernel))
}

/// Flushes every TLB entry on every online processor: for changes to many pages at once.
///
/// # Panics
///
/// As for [`shootdown`].
pub fn shootdown_all() -> u64 {
    finished(SHOOTDOWN.shoot(&Hardware, Request::Everything, Scope::Kernel))
}
