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
//! 4. The trampoline page goes back to read/write, never executable.
//!
//! Each AP then enters its own scheduler (`sched::run_ap`).

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::arch::global_asm;
use core::fmt::Write;
use core::ptr;
use core::sync::atomic::{AtomicU32, Ordering, fence};

use framekernel::acpi::{self, Processor};
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

/// What the boot processor hands one AP, and the AP's answer.
struct ApStart {
    /// 0 until the AP is up, then its x2APIC ID + 1 (`u32::MAX` if it has no x2APIC,
    /// `u32::MAX - 1` if it is not the processor `index` was assigned to).
    online: AtomicU32,
    /// The AP's own GDT, TSS and double-fault stack.
    tables: *mut CpuTables,
    /// The CPU index the AP takes; discovery assigned it the AP's APIC ID.
    index: CpuIndex,
}

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
    for (i, apic_id) in map.iter().skip(1) {
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
    online
}

/// RSDP → XSDT → MADT → the processors it lists → their CPU indexes, the boot processor
/// (APIC ID `bsp`) first. The MADT lists every processor the platform may ever have, enabled
/// or not (QEMU lists all of `maxcpus`, up to 4096 on q35), so the list is sized from the
/// table, on the heap: each processor structure is at least 8 bytes long.
fn discover(rsdp_addr: u64, bsp: u32) -> Result<CpuMap, &'static str> {
    let madt = crate::firmware::acpi_table(rsdp_addr, b"APIC").map_err(|e| {
        if e == "table not listed in the XSDT" {
            "no MADT"
        } else {
            e
        }
    })?;
    let mut listed = alloc::vec![Processor::default(); madt.len() / 8];
    let count = acpi::madt_processors(madt, &mut listed).map_err(|_| "invalid MADT")?;
    Ok(CpuMap::new(bsp, listed.get(..count).unwrap_or(&[])))
}

/// Writes `value` at `offset` in the trampoline data page.
fn poke(offset: u64, value: u64) {
    let p = ptr::with_exposed_provenance_mut::<u64>(
        crate::phys::to_virt(TRAMPOLINE_DATA + offset) as usize
    );
    // SAFETY: the data page is a frame reserved at boot for this purpose (`prepare`
    // checked it is usable RAM), mapped read/write by the direct map, and no AP is running
    // the trampoline while the boot processor writes it.
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

/// Starts the AP with x2APIC ID `apic_id` as CPU `index`; returns microseconds until its
/// handshake.
fn start_one(
    lapic: &X2Apic,
    index: CpuIndex,
    apic_id: u32,
    tsc_hz: u64,
) -> Result<u64, &'static str> {
    // SAFETY: an all-zero `CpuTables` is valid (integers only); `init_ap` writes it before
    // the processor uses it.
    let tables = Box::into_raw(unsafe { Box::<CpuTables>::new_zeroed().assume_init() });
    let (stack, _guard) =
        crate::sched::guarded_stack(AP_STACK_BYTES).ok_or("no memory for the AP stack")?;
    let stack_top = (stack.as_mut_ptr().addr() + AP_STACK_BYTES) as u64 & !0xF;
    let start: &'static ApStart = Box::leak(Box::new(ApStart {
        online: AtomicU32::new(0),
        tables,
        index,
    }));
    poke(STACK, stack_top);
    poke(ENTRY, (ap_entry as *const ()).addr() as u64);
    poke(ARG, ptr::from_ref(start).addr() as u64);
    fence(Ordering::SeqCst);

    let vector = (TRAMPOLINE >> 12) as u32;
    let t0 = rdtsc();
    // SAFETY: the trampoline and its data page are ready for this AP (above and in
    // `prepare`), so INIT followed by start-up IPIs makes it run the trampoline, which ends
    // in `ap_entry` with `start`.
    unsafe { lapic.send_ipi(apic_id, 0x4500) }; // INIT, level assert
    delay(10_000, tsc_hz);
    for attempt in 0..2 {
        // SAFETY: as above; a start-up IPI with vector 0x08 starts the AP at 0x8000.
        unsafe { lapic.send_ipi(apic_id, 0x4600 | vector) };
        let wait = if attempt == 0 { 200 } else { 200_000 };
        let begin = rdtsc();
        while rdtsc().wrapping_sub(begin) < tsc_hz / 1_000_000 * wait {
            let answer = start.online.load(Ordering::Acquire);
            if answer != 0 {
                if answer != apic_id.wrapping_add(1) {
                    return Err("a different processor answered");
                }
                return Ok(rdtsc().wrapping_sub(t0) / (tsc_hz / 1_000_000).max(1));
            }
            core::hint::spin_loop();
        }
    }
    Err("no answer within 200 ms")
}

/// Where each AP enters Rust, on its own stack, from the trampoline.
extern "sysv64" fn ap_entry(start: *const ApStart) -> ! {
    // SAFETY: the boot processor leaked `start` for this AP (`start_one`), so it is valid
    // for ever and shared only through its atomic field and the tables pointer.
    let start = unsafe { &*start };
    // SAFETY: once per AP (each gets its own `start`), CPL 0, interrupts disabled (the
    // trampoline ran `cli`), after `cpu::init` on the boot processor; the tables are this
    // AP's alone, and so is `start.index` (the boot processor gives each index once).
    unsafe { cpu::init_ap(start.tables, start.index) };
    // SAFETY: CPL 0; the kernel has no user mappings. Every processor enforces SMEP and SMAP.
    let _ = unsafe { cpu::enable_smep_smap() };
    // SAFETY: CPL 0; the kernel's tables mark only the kernel half global.
    let _ = unsafe { crate::tlb::enable_global_pages() };
    cpu::check_pat();
    // SAFETY: CPL 0, this AP, once, before any SIMD runs on it.
    let _ = unsafe { cpu::enable_simd(&cpu::gather_cpuid()) };
    if !cpu::x2apic_supported() {
        start.online.store(u32::MAX, Ordering::Release);
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
        // timer, no scheduler; interrupts stay disabled).
        start.online.store(u32::MAX - 1, Ordering::Release);
        crate::halt();
    }
    start.online.store(id.wrapping_add(1), Ordering::Release);
    // M3: this processor's scheduler.
    crate::sched::run_ap(&lapic)
}
