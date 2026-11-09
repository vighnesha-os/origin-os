//! Application-processor start-up (M3, first step; CLAUDE.md §24 SMP start-up, secondary
//! CPU initialization, per-CPU state).
//!
//! References: Intel SDM Vol. 3A, "Multiple-Processor Management" (the MP initialization
//! protocol: INIT, then two start-up IPIs; real-address mode at `vector << 12`) and
//! "Initializing IA-32e Mode" (PAE, CR3, EFER.LME, then CR0.PG); ACPI 6.6 §5.2.12 (MADT).
//!
//! 1. The processors come from the ACPI MADT (`framekernel::acpi`), so only enabled
//!    processors are started; nothing is broadcast. Each gets a dense CPU index
//!    (`framekernel::cpumap`, ADR-0008): the boot processor 0, the others in table order.
//!    Per-CPU state is indexed by it; the x2APIC ID, which may exceed 255, stays the
//!    processor's address for IPIs and interrupts. The MADT may list far more processors
//!    than are enabled (QEMU lists every possible one), so its list is read onto the heap.
//! 2. A trampoline is copied to [`TRAMPOLINE`] (frames reserved at boot), whose page is
//!    mapped at its identity address read/execute while APs start; its mutable data (a 32/64-bit GDT, CR3, the stack, the entry
//!    and its argument) lives in the separate read/write page [`TRAMPOLINE_DATA`]. The GDT
//!    descriptors are pre-marked accessed, so loading them never writes. So W^X holds even
//!    while an AP starts.
//! 3. For each AP, in turn: a heap stack and heap `CpuTables`, then INIT, 10 ms, start-up
//!    IPI, 200 µs, a second start-up IPI if it is not up yet, and up to 200 ms for its
//!    handshake. The AP runs the trampoline (real → protected → long mode with the
//!    kernel's own page tables), then [`ap_entry`]: its own GDT and TSS, the shared IDT,
//!    the CPU-local area of its index (only if that index's APIC ID is its own), SMEP and
//!    SMAP, its local APIC in x2APIC mode, and the handshake with its APIC ID.
//! 4. The data page is then rewritten for the next AP. An AP that missed its handshake may
//!    only be slow, so before that it must be made unable both to join late and to read
//!    the page again (ADR-0008 C7):
//!    - each start-up is settled once (`framekernel::apstart`): an answer that comes after
//!      the boot processor stopped waiting does not join, and the AP halts;
//!    - every AP that is not online is parked ([`park`]): INIT, which stops it wherever it
//!      is and leaves it in wait-for-SIPI, then the 10 ms the MP protocol gives INIT. Only
//!      a start-up IPI runs it again.
//!
//!    Before the first start-up relies on parking, the park drill proves it on the first
//!    AP ([`drill`]): started into a loop that counts, then parked, its count must stand
//!    still. It waits in wait-for-SIPI for its real start-up.
//! 5. The trampoline page goes back to read/write, never executable.
//!
//! Each AP then enters its own scheduler (`sched::run_ap`).

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::arch::global_asm;
use core::fmt::Write;
use core::ptr;
use core::sync::atomic::{AtomicU64, Ordering, fence};

use framekernel::acpi::{self, Processor};
use framekernel::apstart::{Handshake, Outcome, Refusal};
use framekernel::cpumap::CpuMap;
use framekernel::paging::{Access, PAGE};
use origin_boot_info::{BootInfo, MemoryRegion, RegionKind};

use crate::console::Console;
use crate::cpu::{self, CpuIndex, CpuTables, MAX_CPUS, rdtsc};
use crate::timer::{ApicRead, X2Apic};
use crate::vm;

/// Physical address of the trampoline code; the start-up IPI vector is `TRAMPOLINE >> 12`.
pub(crate) const TRAMPOLINE: u64 = 0x8000;
/// Physical address of the trampoline's data page.
pub(crate) const TRAMPOLINE_DATA: u64 = 0x9000;
/// Each AP's kernel stack.
const AP_STACK_BYTES: usize = 64 * 1024;
/// The park drill's stack: the trampoline's call is all that uses it.
const DRILL_STACK_BYTES: usize = 4096;

/// ICR command for INIT, level assert (Intel SDM Vol. 3A, "Interrupt Command Register":
/// delivery mode 101b, level 1). Processors since the Pentium 4 take no INIT de-assert.
const INIT_ASSERT: u32 = 0x4500;
/// ICR command for a start-up IPI (delivery mode 110b) whose vector is the trampoline's
/// page number: the AP starts at [`TRAMPOLINE`].
const STARTUP: u32 = 0x4600 | (TRAMPOLINE >> 12) as u32;
/// The time the MP initialization protocol gives a processor to complete INIT.
const INIT_MICROS: u64 = 10_000;
/// How long the boot processor waits for an AP after its second start-up IPI.
const HANDSHAKE_MICROS: u64 = 200_000;
/// How long the park drill watches the count of a processor it parked.
const STILL_MICROS: u64 = 1_000;

// Data-page layout, shared with the trampoline below.
const GDT: u64 = 0x00;
const GDTR: u64 = 0x20;
const CR3: u64 = 0x28;
const STACK: u64 = 0x30;
const ENTRY: u64 = 0x38;
const ARG: u64 = 0x40;

// The trampoline. It is assembled for TRAMPOLINE and TRAMPOLINE_DATA (0x8000 and 0x9000);
// every address in it is a constant, so it has no relocations.
global_asm!(
    ".pushsection .text.origin_ap_trampoline, \"ax\", @progbits",
    ".balign 16",
    ".globl origin_ap_trampoline_start",
    ".hidden origin_ap_trampoline_start",
    ".globl origin_ap_trampoline_end",
    ".hidden origin_ap_trampoline_end",
    ".code16",
    "origin_ap_trampoline_start:",
    "cli",
    "cld",
    "xorw %ax, %ax",
    "movw %ax, %ds",
    "movw %ax, %es",
    "movw %ax, %ss",
    "lgdtl 0x9020",
    "movl %cr0, %eax",
    "orl $1, %eax",
    "movl %eax, %cr0",
    "ljmp $0x08, $(origin_ap_trampoline_32 - origin_ap_trampoline_start + 0x8000)",
    ".code32",
    "origin_ap_trampoline_32:",
    "movw $0x10, %ax",
    "movw %ax, %ds",
    "movw %ax, %es",
    "movw %ax, %ss",
    // CR4.PAE, then the kernel's PML4.
    "movl %cr4, %eax",
    "orl $0x20, %eax",
    "movl %eax, %cr4",
    "movl 0x9028, %eax",
    "movl %eax, %cr3",
    // EFER.LME and EFER.NXE (the kernel's tables use NX).
    "movl $0xC0000080, %ecx",
    "rdmsr",
    "orl $0x900, %eax",
    "wrmsr",
    // CR0: clear CD and NW (INIT sets them), set PG and WP.
    "movl %cr0, %eax",
    "andl $0x9FFFFFFF, %eax",
    "orl $0x80010000, %eax",
    "movl %eax, %cr0",
    "ljmp $0x18, $(origin_ap_trampoline_64 - origin_ap_trampoline_start + 0x8000)",
    ".code64",
    "origin_ap_trampoline_64:",
    "movw $0x10, %ax",
    "movw %ax, %ds",
    "movw %ax, %es",
    "movw %ax, %ss",
    "movq 0x9030, %rsp",
    "movq 0x9040, %rdi",
    "movq 0x9038, %rax",
    "xorl %ebp, %ebp",
    "callq *%rax",
    "1:",
    "cli",
    "hlt",
    "jmp 1b",
    "origin_ap_trampoline_end:",
    ".popsection",
    options(att_syntax),
);

unsafe extern "C" {
    static origin_ap_trampoline_start: u8;
    static origin_ap_trampoline_end: u8;
}

/// What the boot processor hands one AP, and how its start-up was settled.
struct ApStart {
    /// Settled once, by the AP's answer or refusal or by the boot processor's deadline,
    /// whichever comes first.
    handshake: Handshake,
    /// The AP's own GDT, TSS and interrupt stacks.
    tables: *mut CpuTables,
    /// The CPU index the AP takes; discovery assigned it the AP's APIC ID.
    index: CpuIndex,
    /// What the AP found it lacks, set before it refuses (`Refusal` has room only for its
    /// x2APIC), so the boot processor can name it: 0, [`LACKS_SMEP_SMAP`] or
    /// [`LACKS_BASELINE`].
    lacks: AtomicU64,
}

/// An AP's [`ApStart::lacks`]: no SMEP or no SMAP, which ADR-0003 requires on every CPU.
const LACKS_SMEP_SMAP: u64 = 1;
/// An AP's [`ApStart::lacks`]: below the x86-64-v3 baseline (CLAUDE.md §14), or without the
/// AVX state XCR0 must hold.
const LACKS_BASELINE: u64 = 2;

/// The processors online, by CPU index, the boot processor first.
pub(crate) struct Online {
    cpus: [CpuIndex; MAX_CPUS],
    count: usize,
    /// Every processor the MADT marks enabled is online: none can still be running
    /// firmware code.
    complete: bool,
}

impl Online {
    fn one() -> Self {
        Self {
            cpus: [CpuIndex::BOOT; MAX_CPUS],
            count: 1,
            complete: false,
        }
    }

    fn add(&mut self, index: CpuIndex) {
        if let Some(slot) = self.cpus.get_mut(self.count) {
            *slot = index;
            self.count += 1;
        }
    }

    /// Whether every enabled processor came online.
    pub(crate) const fn complete(&self) -> bool {
        self.complete
    }

    /// Their CPU indexes, the boot processor first: where threads are spawned.
    pub(crate) fn cpus(&self) -> &[CpuIndex] {
        self.cpus.get(..self.count).unwrap_or(&[])
    }

    /// Their x2APIC IDs, in the same order: for topology and NUMA affinity.
    pub(crate) fn apic_ids(&self) -> Vec<u32> {
        self.cpus().iter().filter_map(|c| c.apic_id()).collect()
    }
}

/// Brings up every enabled AP the MADT lists. Returns the processors online, the boot
/// processor included.
pub(crate) fn start(
    com1: &mut Console,
    info: &BootInfo,
    regions: &[MemoryRegion],
    tsc_hz: u64,
) -> Online {
    // SAFETY: CPL 0, boot processor, interrupts disabled, x2APIC supported (the timer
    // bring-up checked it and already switched this APIC; the transition is then a no-op).
    let lapic = unsafe { X2Apic::enable() };
    let bsp = lapic.read(ApicRead::Id) as u32;
    let mut online = Online::one();
    if cpu::this_cpu() != CpuIndex::BOOT || CpuIndex::BOOT.apic_id() != Some(bsp) {
        let _ = writeln!(
            com1,
            "origin-kernel: the CPU-local area names {}, not APIC ID {bsp}: AP start-up failed",
            cpu::this_cpu().named()
        );
        return online;
    }
    cpu::record_cpu_state();
    // The boot processor's interrupt stacks are in the kernel's image; nothing could guard
    // them before the kernel's own tables were live.
    let bsp_guards = guard_interrupt_stacks(cpu::bsp_tables())
        .map(|proven| INTERRUPT_STACK_GUARDS.store(proven, Ordering::Relaxed));
    let map = match discover(info.acpi_rsdp, bsp) {
        Ok(map) => map,
        Err(e) => {
            let _ = writeln!(com1, "origin-kernel: ACPI processor discovery failed: {e}");
            return online;
        }
    };
    let _ = write!(
        com1,
        "origin-kernel: ACPI MADT: {} processors, {} enabled (largest APIC ID listed {}); CPU index 0 for the boot processor (APIC ID {bsp})",
        map.listed(),
        map.enabled(),
        map.highest().unwrap_or(bsp),
    );
    match map.indexed() {
        1 => {}
        2 => {
            let _ = write!(com1, ", 1 for the other enabled processor");
        }
        n => {
            let _ = write!(
                com1,
                ", 1-{} for the other enabled processors in table order",
                n - 1
            );
        }
    }
    if !map.boot_listed() {
        let _ = write!(com1, "; the MADT does not list the boot processor");
    }
    if map.unindexed() > 0 {
        let _ = write!(
            com1,
            "; {} enabled processors beyond the kernel's {MAX_CPUS} CPU indexes stay in firmware",
            map.unindexed()
        );
    }
    if map.invalid() > 0 {
        let _ = write!(
            com1,
            "; {} with the broadcast APIC ID refused",
            map.invalid()
        );
    }
    let _ = writeln!(com1);
    // Every AP's APIC ID is on record before any AP starts: each checks its own.
    for (i, apic_id) in map.iter().skip(1) {
        if let Some(index) = CpuIndex::new(i) {
            cpu::assign(index, apic_id);
        }
    }
    if let Err(why) = prepare(regions) {
        let _ = writeln!(com1, "origin-kernel: AP start-up not possible: {why}");
        return online;
    }
    // Every start-up below relies on parking an AP that misses its handshake: the drill
    // proves that first, on the first AP. If INIT did not stop it, no AP is started.
    let mut parks = true;
    if let Some((i, apic_id)) = map.iter().nth(1)
        && let Some(index) = CpuIndex::new(i)
    {
        parks = drill_and_report(com1, &lapic, index, apic_id, tsc_hz);
    }
    for (i, apic_id) in map.iter().skip(1).filter(|_| parks) {
        let Some(index) = CpuIndex::new(i) else {
            continue;
        };
        match start_one(&lapic, index, apic_id, tsc_hz) {
            Ok(micros) => {
                online.add(index);
                let _ = writeln!(
                    com1,
                    "origin-kernel: CPU {index} (APIC ID {apic_id}) online after {micros} us: own GDT, TSS and IDT, x2APIC",
                );
            }
            Err(why) => {
                let _ = writeln!(
                    com1,
                    "origin-kernel: CPU {index} (APIC ID {apic_id}): {why}: AP start-up failed"
                );
            }
        }
    }
    // The identity mappings go: the lower half belongs to user space (ADR-0004 H1).
    if let Err(e) = vm::unmap(TRAMPOLINE).and_then(|_| vm::unmap(TRAMPOLINE_DATA)) {
        let _ = writeln!(
            com1,
            "origin-kernel: trampoline mappings not removed: {e:?}: AP start-up failed"
        );
    }
    online.complete = online.count == map.indexed() && map.covers_enabled();
    let leftover = vm::is_mapped(TRAMPOLINE) || vm::is_mapped(TRAMPOLINE_DATA);
    let _ = writeln!(
        com1,
        "origin-kernel: SMP: {} of {} enabled processors online; trampoline identity pages removed {}{}",
        online.count,
        map.enabled(),
        !leftover,
        if leftover { ": AP start-up failed" } else { "" }
    );
    // Each processor's interrupt stacks end at a guard page that faults, and each put its own
    // control state where the kernel's rules need it.
    let proven = INTERRUPT_STACK_GUARDS.load(Ordering::Relaxed);
    let expected = online.count as u64 * 4;
    if bsp_guards.is_ok() && proven == expected {
        let _ = writeln!(
            com1,
            "origin-kernel: page protection: interrupt-stack guard pages: {proven} of {expected} faulted (4 per processor)"
        );
    } else {
        let _ = writeln!(
            com1,
            "origin-kernel: page protection violated: interrupt-stack guard pages: {proven} of {expected} faulted ({})",
            bsp_guards.err().unwrap_or("an application processor's")
        );
    }
    let holding = cpu::cpu_states_holding(online.cpus());
    let _ = writeln!(
        com1,
        "origin-kernel: CPU state: machine checks raise #MC, x87 errors raise #MF, ring 3 has no RDPMC, FSGSBASE or SYSENTER, no debug breakpoint armed: {holding} of {} processors{}",
        online.count,
        if holding == online.count {
            ""
        } else {
            ": CPU baseline failed"
        }
    );
    online
}

/// Guard pages proven unmapped, for the boot report.
static INTERRUPT_STACK_GUARDS: AtomicU64 = AtomicU64::new(0);

/// Unmaps the guard page below each interrupt stack in `tables` (an AP has yet to start; on the
/// boot processor each guard page lies below anything those stacks have used so far), proves
/// that each one faults, and returns how many did.
fn guard_interrupt_stacks(tables: *const CpuTables) -> Result<u64, &'static str> {
    let guards = cpu::interrupt_stack_guards(tables);
    vm::make_guards(&guards).map_err(|_| "cannot unmap the guard pages")?;
    let mut proven = 0;
    for guard in guards {
        // SAFETY: the page is RAM of `tables`, not MMIO; the probe only reads it.
        match unsafe { cpu::probe_read(guard) } {
            cpu::Probe::Faulted { error: 0, address } if address == guard => {
                proven += 1;
            }
            _ => return Err("a guard page did not fault"),
        }
    }
    Ok(proven)
}

/// RSDP → XSDT → MADT → the processors it lists → their CPU indexes, the boot processor
/// (APIC ID `bsp`) first. The MADT lists every processor the platform may ever have, enabled
/// or not (QEMU lists all of `maxcpus`, up to 4096 on q35), so the list is sized from the
/// table, on the heap: each processor structure is at least 8 bytes long.
fn discover(rsdp_addr: u64, bsp: u32) -> Result<CpuMap, &'static str> {
    let listed = madt_listing(rsdp_addr)?;
    Ok(CpuMap::new(bsp, &listed))
}

/// Every processor the MADT lists, enabled or not. The list is sized from the table, on the
/// heap: each processor structure is at least 8 bytes long.
fn madt_listing(rsdp_addr: u64) -> Result<Vec<Processor>, &'static str> {
    let madt = crate::firmware::acpi_table(rsdp_addr, b"APIC").map_err(|e| {
        if e == "table not listed in the XSDT" {
            "no MADT"
        } else {
            e
        }
    })?;
    let mut listed = alloc::vec![Processor::default(); madt.len() / 8];
    let count = acpi::madt_processors(madt, &mut listed).map_err(|_| "invalid MADT")?;
    listed.truncate(count);
    Ok(listed)
}

/// The x2APIC ID of every processor the MADT lists, enabled or not: the topology proof
/// checks the CPUID layout against them (CLAUDE.md §24, §27).
pub(crate) fn listed_apic_ids(rsdp_addr: u64) -> Result<Vec<u32>, &'static str> {
    Ok(madt_listing(rsdp_addr)?.iter().map(|p| p.apic_id).collect())
}

/// Writes `value` at `offset` in the trampoline data page.
fn poke(offset: u64, value: u64) {
    let p = ptr::with_exposed_provenance_mut::<u64>(
        crate::phys::to_virt(TRAMPOLINE_DATA + offset) as usize
    );
    // SAFETY: the data page is a frame reserved at boot for this purpose (`prepare`
    // checked it is usable RAM), mapped read/write by the direct map, and no AP reads it
    // while the boot processor writes it: every AP started before is online (it answered
    // after reading its stack, entry and argument) or parked by INIT (`park`).
    unsafe { p.write_volatile(value) };
}

/// Copies the trampoline, makes its page read/execute, and writes the GDT and CR3.
fn prepare(regions: &[MemoryRegion]) -> Result<(), &'static str> {
    let usable = |addr: u64| {
        regions.iter().any(|r| {
            r.kind == RegionKind::Usable as u32
                && addr >= r.base
                && addr + PAGE <= r.base + r.pages * PAGE
        })
    };
    if !usable(TRAMPOLINE) || !usable(TRAMPOLINE_DATA) {
        return Err("the trampoline frames are not usable RAM");
    }
    let root = vm::root().ok_or("no kernel address space")?;
    if root > u64::from(u32::MAX) {
        return Err("the PML4 is above 4 GiB, out of reach of 32-bit code");
    }
    let start = (&raw const origin_ap_trampoline_start).addr();
    let end = (&raw const origin_ap_trampoline_end).addr();
    let len = end
        .checked_sub(start)
        .filter(|&l| l as u64 <= PAGE)
        .ok_or("trampoline too large")?;
    // SAFETY: the source is the trampoline's bytes in the kernel's executable image
    // (readable); the destination is the reserved trampoline frame at its direct-map
    // address (read/write, never executable); they do not overlap.
    unsafe {
        ptr::copy_nonoverlapping(
            ptr::with_exposed_provenance::<u8>(start),
            ptr::with_exposed_provenance_mut::<u8>(crate::phys::to_virt(TRAMPOLINE) as usize),
            len,
        );
    }
    // While APs start, the two frames also appear at their identity addresses, where the
    // trampoline runs as paging comes on: code read/execute, data read/write.
    vm::map_identity(TRAMPOLINE, Access::ReadExecute)
        .and_then(|()| vm::map_identity(TRAMPOLINE_DATA, Access::ReadWrite))
        .map_err(|_| "cannot map the trampoline")?;
    // Null, 32-bit code, data, 64-bit code; all pre-marked accessed (type 0x9B / 0x93).
    for (i, d) in [
        0,
        0x00CF_9B00_0000_FFFF_u64,
        0x00CF_9300_0000_FFFF,
        0x00AF_9B00_0000_FFFF,
    ]
    .into_iter()
    .enumerate()
    {
        poke(GDT + 8 * i as u64, d);
    }
    poke(GDTR, (TRAMPOLINE_DATA << 16) | 31);
    poke(CR3, root);
    Ok(())
}

/// Busy-waits `micros` microseconds on the TSC.
fn delay(micros: u64, tsc_hz: u64) {
    let start = rdtsc();
    let ticks = tsc_hz / 1_000_000 * micros;
    while rdtsc().wrapping_sub(start) < ticks {
        core::hint::spin_loop();
    }
}

/// Polls `ready` for up to `micros` microseconds; returns whether it became true.
fn within(micros: u64, tsc_hz: u64, ready: impl Fn() -> bool) -> bool {
    let begin = rdtsc();
    let ticks = tsc_hz / 1_000_000 * micros;
    while rdtsc().wrapping_sub(begin) < ticks {
        if ready() {
            return true;
        }
        core::hint::spin_loop();
    }
    ready()
}

/// The MP initialization protocol for the AP at `apic_id` (Intel SDM Vol. 3A, MP
/// initialization): INIT, 10 ms, a start-up IPI, 200 µs, a second start-up IPI if `up` is
/// not true yet, then up to 200 ms for `up`. Returns whether `up` became true.
///
/// # Safety
///
/// The trampoline and its data page must be ready for this AP: it runs them. Every AP
/// started before must be online or parked ([`park`]).
unsafe fn wake(lapic: &X2Apic, apic_id: u32, tsc_hz: u64, up: impl Fn() -> bool) -> bool {
    // SAFETY: the caller's contract: INIT, then start-up IPIs, make the AP run the
    // trampoline with the data it was given.
    unsafe { lapic.send_ipi(apic_id, INIT_ASSERT) };
    delay(INIT_MICROS, tsc_hz);
    for wait in [200, HANDSHAKE_MICROS] {
        // SAFETY: as above; the start-up IPI's vector starts the AP at the trampoline.
        unsafe { lapic.send_ipi(apic_id, STARTUP) };
        if within(wait, tsc_hz, &up) {
            return true;
        }
    }
    false
}

/// Parks the AP at `apic_id`: INIT, then the 10 ms the MP protocol gives a processor to
/// complete it. INIT stops the processor wherever it is (the trampoline, `ap_entry`, the
/// halt after a refusal or a late answer, the drill's loop) and leaves it in
/// wait-for-SIPI (Intel SDM Vol. 3A, MP initialization). Only a start-up IPI runs it
/// again, and only [`wake`] sends those, each after writing the data for its own start-up.
/// So once this returns, the trampoline's data page may be rewritten.
///
/// # Safety
///
/// The processor at `apic_id` must not be online: it must never have reached
/// `sched::run_ap`. Until then it takes no lock and runs no thread, so stopping it anywhere
/// leaves nothing that another processor waits for.
unsafe fn park(lapic: &X2Apic, apic_id: u32, tsc_hz: u64) {
    // SAFETY: INIT only resets the processor at `apic_id` into wait-for-SIPI, and that
    // processor is not online (the caller's contract).
    unsafe { lapic.send_ipi(apic_id, INIT_ASSERT) };
    delay(INIT_MICROS, tsc_hz);
}

/// Starts the AP with x2APIC ID `apic_id` as CPU `index`; returns microseconds until its
/// handshake. Otherwise the AP is parked before this returns ([`park`]), so the data page
/// may be rewritten for the next AP.
fn start_one(
    lapic: &X2Apic,
    index: CpuIndex,
    apic_id: u32,
    tsc_hz: u64,
) -> Result<u64, &'static str> {
    // SAFETY: an all-zero `CpuTables` is valid (integers only); `init_ap` writes it before
    // the processor uses it.
    let tables = Box::into_raw(unsafe { Box::<CpuTables>::new_zeroed().assume_init() });
    let guards = guard_interrupt_stacks(tables)?;
    let (stack, _guard) =
        crate::sched::guarded_stack(AP_STACK_BYTES).ok_or("no memory for the AP stack")?;
    let stack_top = (stack.as_mut_ptr().addr() + AP_STACK_BYTES) as u64 & !0xF;
    let start: &'static ApStart = Box::leak(Box::new(ApStart {
        handshake: Handshake::new(),
        tables,
        index,
        lacks: AtomicU64::new(0),
    }));
    poke(STACK, stack_top);
    poke(ENTRY, (ap_entry as *const ()).addr() as u64);
    poke(ARG, ptr::from_ref(start).addr() as u64);
    fence(Ordering::SeqCst);

    let t0 = rdtsc();
    // SAFETY: the trampoline and its data page are ready for this AP (above and in
    // `prepare`), so it runs the trampoline into `ap_entry` with `start`; every AP before
    // it is online or parked (below).
    unsafe {
        wake(lapic, apic_id, tsc_hz, || {
            start.handshake.outcome().is_some()
        })
    };
    // Settle the start-up: an answer that came stands; without one, the AP can no longer
    // join.
    let outcome = start.handshake.abandon();
    if outcome == Outcome::Answered(apic_id) {
        // Counted only now: `expected` is four per processor that came online.
        INTERRUPT_STACK_GUARDS.fetch_add(guards, Ordering::Relaxed);
        return Ok(rdtsc().wrapping_sub(t0) / (tsc_hz / 1_000_000).max(1));
    }
    // SAFETY: the AP is not online: its start-up was settled without it joining, so it
    // never reaches `sched::run_ap` (`ap_entry`).
    unsafe { park(lapic, apic_id, tsc_hz) };
    Err(match outcome {
        Outcome::Abandoned => "no answer within 200 ms; parked by INIT",
        Outcome::Refused(Refusal::NoX2Apic) => match start.lacks.load(Ordering::Acquire) {
            LACKS_SMEP_SMAP => "no SMEP or no SMAP; parked by INIT",
            LACKS_BASELINE => "below the x86-64-v3 baseline; parked by INIT",
            _ => "no x2APIC; parked by INIT",
        },
        Outcome::Refused(Refusal::NotMine) | Outcome::Answered(_) => {
            "a different processor answered; parked by INIT"
        }
    })
}

/// Counted by the processor in the park drill while it runs.
static DRILL_COUNT: AtomicU64 = AtomicU64::new(0);

/// The park drill's entry from the trampoline: count until INIT stops this processor.
/// Interrupts stay disabled (the trampoline ran `cli`), and it touches nothing else.
extern "sysv64" fn ap_drill() -> ! {
    loop {
        DRILL_COUNT.fetch_add(1, Ordering::Relaxed);
        core::hint::spin_loop();
    }
}

/// What the park drill saw: the count when INIT was sent, and how far it moved in the
/// [`STILL_MICROS`] after INIT and its 10 ms (0 if the processor was parked).
struct Drill {
    at_init: u64,
    after: u64,
}

/// Proves on this machine, before any start-up relies on it, what [`park`] relies on:
/// INIT stops a running processor within the 10 ms it is given. The AP at `apic_id` is
/// started into [`ap_drill`], which counts; once its count is seen moving it is parked, and
/// the count must then stand still. The AP is left in wait-for-SIPI, from which
/// [`start_one`] starts it for real.
fn drill(lapic: &X2Apic, apic_id: u32, tsc_hz: u64) -> Result<Drill, &'static str> {
    let (stack, _guard) =
        crate::sched::guarded_stack(DRILL_STACK_BYTES).ok_or("no memory for its stack")?;
    let stack_top = (stack.as_mut_ptr().addr() + DRILL_STACK_BYTES) as u64 & !0xF;
    poke(STACK, stack_top);
    poke(ENTRY, (ap_drill as *const ()).addr() as u64);
    poke(ARG, 0);
    fence(Ordering::SeqCst);
    let count = || DRILL_COUNT.load(Ordering::Acquire);
    // SAFETY: the trampoline is ready (`prepare`) and its data page names `ap_drill` and a
    // stack of its own, so the AP runs the trampoline into a loop that only counts. No AP
    // was started before.
    let ran = unsafe { wake(lapic, apic_id, tsc_hz, || count() > 0) };
    // Running now: the count moves on from where it was first seen.
    let first = count();
    let running = ran && within(HANDSHAKE_MICROS, tsc_hz, || count() > first);
    let at_init = count();
    // SAFETY: the AP runs `ap_drill` (or firmware's wait, if it never started); it never
    // reaches `sched::run_ap`.
    unsafe { park(lapic, apic_id, tsc_hz) };
    if !running {
        return Err(if ran {
            "it stopped counting before INIT; parked by INIT"
        } else {
            "it did not run the drill within 200 ms; parked by INIT"
        });
    }
    let parked_at = count();
    delay(STILL_MICROS, tsc_hz);
    Ok(Drill {
        at_init,
        after: count().wrapping_sub(parked_at),
    })
}

/// Runs the park drill on CPU `index` (APIC ID `apic_id`) and reports it. Returns whether
/// parking can be relied on: false only if INIT did not stop the running processor.
fn drill_and_report(
    com1: &mut Console,
    lapic: &X2Apic,
    index: CpuIndex,
    apic_id: u32,
    tsc_hz: u64,
) -> bool {
    match drill(lapic, apic_id, tsc_hz) {
        Ok(d) => {
            let parked = d.after == 0;
            let _ = writeln!(
                com1,
                "origin-kernel: SMP: park drill: {} ran a counting loop from the trampoline (count {} at INIT); 10 ms after INIT, its count moved {} in {STILL_MICROS} us: parked {parked}{}",
                index.named(),
                d.at_init,
                d.after,
                if parked {
                    ""
                } else {
                    "; INIT does not stop a running processor, so no AP is started: AP start-up failed"
                }
            );
            parked
        }
        Err(why) => {
            let _ = writeln!(
                com1,
                "origin-kernel: SMP: park drill: {}: {why}: AP start-up failed",
                index.named()
            );
            true
        }
    }
}

/// Where each AP enters Rust, on its own stack, from the trampoline.
extern "sysv64" fn ap_entry(start: *const ApStart) -> ! {
    // SAFETY: the boot processor leaked `start` for this AP (`start_one`), so it is valid
    // for ever and shared only through its handshake and the tables pointer.
    let start = unsafe { &*start };
    // SAFETY: once per AP (each gets its own `start`), CPL 0, interrupts disabled (the
    // trampoline ran `cli`), after `cpu::init` on the boot processor; the tables are this
    // AP's alone, and so is `start.index` (the boot processor gives each index once).
    unsafe { cpu::init_ap(start.tables, start.index) };
    // SAFETY: CPL 0; the kernel has no user mappings. Every processor enforces SMEP and SMAP.
    let (smep, smap) = unsafe { cpu::enable_smep_smap() };
    // SAFETY: CPL 0; the kernel's tables mark only the kernel half global.
    let _ = unsafe { crate::tlb::enable_global_pages() };
    cpu::check_pat();
    let cpuid = cpu::gather_cpuid();
    // SAFETY: CPL 0, this AP, once, before any SIMD runs on it.
    let xcr0 = unsafe { cpu::enable_simd(&cpuid) };
    cpu::record_cpu_state();
    // Every processor is held to what the boot processor is held to. One that falls short
    // does not come online: it refuses, and the boot processor reports why and parks it.
    let baseline =
        framekernel::features::Features::decode(&cpuid).meets_baseline() && xcr0 & 0b111 == 0b111;
    if !(smep && smap) || !baseline {
        let lacks = if smep && smap {
            LACKS_BASELINE
        } else {
            LACKS_SMEP_SMAP
        };
        start.lacks.store(lacks, Ordering::Release);
        let _ = start.handshake.refuse(Refusal::NoX2Apic);
        crate::halt();
    }
    if !cpu::x2apic_supported() {
        let _ = start.handshake.refuse(Refusal::NoX2Apic);
        crate::halt();
    }
    // SAFETY: CPL 0, this AP only, interrupts disabled, x2APIC supported, and the spurious
    // vector has a gate in the shared IDT.
    let lapic = unsafe { X2Apic::enable() };
    let id = lapic.read(ApicRead::Id) as u32;
    // The CPU-local area (GS) must hold the index this processor was given, and that index
    // must be this processor's APIC ID, or its scheduler would use another processor's
    // queue: otherwise answer with a value the boot processor rejects.
    let mine = cpu::this_cpu() == start.index && start.index.apic_id() == Some(id);
    if !mine {
        // Not the processor this index was given to: refuse, and do nothing more (no
        // timer, no scheduler; interrupts stay disabled) until the boot processor parks it.
        let _ = start.handshake.refuse(Refusal::NotMine);
        crate::halt();
    }
    // This processor's topology, read on itself: its leaves are its own (CLAUDE.md §24, §27).
    cpu::record_topology();
    // Join only if this answer settles the start-up. If the boot processor stopped waiting
    // first, CPU `index` stays offline and this processor is being parked: do nothing more.
    if !start.handshake.answer(id) {
        crate::halt();
    }
    // M3: this processor's scheduler.
    crate::sched::run_ap(&lapic)
}
