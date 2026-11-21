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

use origin_sync::shootdown::{
    Asked, Completed, Record, Records, Request, Scope, Shootdown, Stalled, TlbHardware,
};

use crate::cpu::{self, CpuIndex, rdtsc};
use crate::percpu::PerCpu;
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

/// Each processor's part in the shootdown protocol, each on a cache line of its own (ADR-0008
/// Amendment 1, D1).
pub(crate) static RECORDS: PerCpu<Record> =
    PerCpu::new("origin_frame::tlb::RECORDS", Record::new(), Record::new);
/// The initiator's mark for each processor.
pub(crate) static ASKED: PerCpu<Asked> =
    PerCpu::new("origin_frame::tlb::ASKED", Asked::new(), Asked::new);

/// The protocol's per-processor storage: [`RECORDS`] and [`ASKED`], sized together at discovery.
struct Tables;

impl Records for Tables {
    /// Every processor that has either: one with a record and no mark makes a request that must
    /// reach it a stall, never a skip (`origin_sync::shootdown`).
    fn count(&self) -> usize {
        RECORDS.indexes().max(ASKED.indexes())
    }

    fn record(&self, cpu: usize) -> Option<&Record> {
        RECORDS.get(cpu)
    }

    fn asked(&self, cpu: usize) -> Option<&Asked> {
        ASKED.get(cpu)
    }
}

/// The machine's shootdown state.
static SHOOTDOWN: Shootdown<Tables> = Shootdown::new(Tables);
/// TSC cycles the last completed shootdown took, for the boot report.
pub static LAST_CYCLES: AtomicU64 = AtomicU64::new(0);
/// Each CPU index whose processor turned on global pages.
pub static GLOBAL_PAGES: PerCpu<AtomicBool> = PerCpu::new(
    "origin_frame::tlb::GLOBAL_PAGES",
    AtomicBool::new(false),
    || AtomicBool::new(false),
);

/// What the protocol asks of the processor it runs on.
struct Hardware;

impl TlbHardware for Hardware {
    type Here<'a> = crate::trap::Here<'a>;

    fn this_cpu(&self, here: crate::trap::Here<'_>) -> usize {
        cpu::this_cpu(here).index().get()
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

    fn without_interrupts<R>(&self, f: impl for<'a> FnOnce(crate::trap::Here<'a>) -> R) -> R {
        cpu::without_interrupts_here(f)
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

/// Turns on global pages on this processor and marks its CPU index, for the boot report.
/// The proof counts only the processors online, so an AP that got this far but never came
/// online (it was parked, `smp::park`) is not counted. `me` is this processor, read under its
/// start-up flow's witness (ADR-0020 S3; TASKS item 93).
///
/// # Safety
///
/// As for `cpu::enable_global_pages`.
pub unsafe fn enable_global_pages(me: cpu::ThisCpu<'_>) -> bool {
    // SAFETY: the caller's contract.
    let on = unsafe { cpu::enable_global_pages() };
    if on && let Some(global) = GLOBAL_PAGES.this(me) {
        global.store(true, Ordering::Relaxed);
    }
    on
}

/// Makes this processor a shootdown target, then flushes its whole TLB. Returns whether it has a
/// record; one that has none is not made a target and must stay out (review finding F6).
pub fn mark_online() -> bool {
    SHOOTDOWN.mark_online(&Hardware)
}

/// Whether processor `index` has a record in the shootdown, so it can be asked to flush. An AP
/// without one refuses its start-up (`smp::ap_entry`; review finding F6 of 2026-11-14). A length
/// check that cannot panic, unlike a lookup of an index the tables were not sized for (review
/// finding N2): the AP asks this before any other per-processor lookup.
pub fn has_record(index: CpuIndex) -> bool {
    RECORDS.holds(index.get()) && ASKED.holds(index.get())
}

/// Runs `load` (the CR3 write) as this processor's move to address space `to`, with the record
/// of it, all with interrupts off (`Shootdown::switch_space`).
pub(crate) fn switch_space(to: u64, load: impl FnOnce()) {
    SHOOTDOWN.switch_space(&Hardware, to, load);
}

/// Runs `f` with the address space `root` loaded on this processor, then puts back what was
/// there, with interrupts off throughout (`Shootdown::visit`).
pub(crate) fn visit<R>(root: u64, f: impl FnOnce() -> R) -> R {
    SHOOTDOWN.visit(
        &Hardware,
        root,
        |to| {
            // SAFETY: `to` is the root a caller of `visit` vouches for (a complete space that
            // stays alive for the call), or the one this processor was running on before it.
            unsafe { cpu::load_cr3(to) }
        },
        f,
    )
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
    note_answer();
}

/// Shootdowns that gave up waiting for some processor this boot, of every kind, and the last
/// one's processors and wait (TASKS item 66; the record is `stalls`, tested on the host).
static STALLS: crate::stalls::StallLog = crate::stalls::StallLog::new();

/// Notes a shootdown that gave up on a processor, asked at TSC `asked_at`. The asker is read
/// in a section of its own (TASKS item 93, the ruling's Q8): a stale index would name the wrong
/// processor as the asker in the stall's record.
fn note(outcome: &Result<Completed, Stalled>, asked_at: u64) {
    if let Err(Stalled { cpu, .. }) = outcome {
        cpu::without_interrupts_here(|here| {
            STALLS.note(*cpu, cpu::this_cpu(here).index().get(), asked_at, rdtsc());
        });
    }
}

/// On the processor the last stall gave up on, notes when it next served a shootdown. Read in a
/// section of its own (the ruling's Q8): with a stale index, the processor named might be marked
/// as having answered when it had not, or the one that had answered not marked.
fn note_answer() {
    cpu::without_interrupts_here(|here| {
        STALLS.answered(cpu::this_cpu(here).index().get(), rdtsc());
    });
}

pub use crate::stalls::Stall;

/// The last shootdown that stalled, if any has.
pub fn last_stall() -> Option<Stall> {
    STALLS.last()
}

/// How many shootdown interrupts each processor has taken.
pub(crate) static INTERRUPTS: PerCpu<AtomicU64> =
    PerCpu::new("origin_frame::tlb::INTERRUPTS", AtomicU64::new(0), || {
        AtomicU64::new(0)
    });

/// The shootdown interrupt, under the trap entry's witness (ADR-0020 S3; TASKS item 93).
pub(crate) fn interrupted(here: crate::trap::Here<'_>) {
    if let Some(taken) = INTERRUPTS.this(cpu::this_cpu(here)) {
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
    let asked_at = rdtsc();
    let outcome = SHOOTDOWN.shoot(&Hardware, request, Scope::Space(root));
    note(&outcome, asked_at);
    outcome
}

/// How many processors have the address space `root` loaded now.
pub(crate) fn processors_holding(root: u64) -> u32 {
    SHOOTDOWN.processors_holding(root)
}

/// The result of a kernel request. A processor that never answers is left holding a
/// translation of memory the caller is about to reuse, so the kernel stops.
fn finished(outcome: Result<Completed, Stalled>, asked_at: u64) -> u64 {
    note(&outcome, asked_at);
    match outcome {
        Ok(done) => {
            LAST_CYCLES.store(done.ticks, Ordering::Relaxed);
            done.ticks
        }
        Err(Stalled { cpu, .. }) => {
            panic!("TLB shootdown: CPU {cpu} did not answer within a second")
        }
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
    let asked_at = rdtsc();
    finished(
        SHOOTDOWN.shoot(&Hardware, Request::page(addr), Scope::Kernel),
        asked_at,
    )
}

/// Flushes every TLB entry on every online processor: for changes to many pages at once.
///
/// # Panics
///
/// As for [`shootdown`].
pub fn shootdown_all() -> u64 {
    let asked_at = rdtsc();
    finished(
        SHOOTDOWN.shoot(&Hardware, Request::Everything, Scope::Kernel),
        asked_at,
    )
}

/// Drops the translations of the `pages` pages from `base` on every online processor, after the
/// caller took them out of the kernel's tables; more than `MAX_LISTED_PAGES` pages flush every
/// translation. A stall is returned rather than stopping the kernel: the stack region's caller
/// (`stack`, through `origin_sync::slots`) then keeps the pages and what they held until a fresh
/// request for them completes (TASKS item 73).
pub(crate) fn shootdown_kernel_pages(base: u64, pages: u64) -> Result<Completed, Stalled> {
    let request = if (1..=origin_sync::shootdown::MAX_LISTED_PAGES).contains(&pages) {
        Request::Pages { base, pages }
    } else {
        Request::Everything
    };
    let asked_at = rdtsc();
    let outcome = SHOOTDOWN.shoot(&Hardware, request, Scope::Kernel);
    note(&outcome, asked_at);
    if let Ok(done) = outcome {
        LAST_CYCLES.store(done.ticks, Ordering::Relaxed);
    }
    outcome
}

/// Whether the processor `stall` names has served a shootdown since it stalled
/// (`Shootdown::answered`): when asking it again is worth an interrupt. Never proof that it
/// dropped any translation: only a fresh request that completes is (TASKS item 73).
pub(crate) fn answered(stall: Stalled) -> bool {
    SHOOTDOWN.answered(stall)
}
