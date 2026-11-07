//! Application-processor start-up (M3, first step; CLAUDE.md §24 SMP start-up, secondary
//! CPU initialization, per-CPU state).
//!
//! References: Intel SDM Vol. 3A, "Multiple-Processor Management" (the MP initialization
//! protocol: INIT, then two start-up IPIs; real-address mode at `vector << 12`) and
//! "Initializing IA-32e Mode" (PAE, CR3, EFER.LME, then CR0.PG); ACPI 6.6 §5.2.12 (MADT).
//!
//! 1. The processors come from the ACPI MADT (`framekernel::acpi`), so only enabled
//!    processors are started; nothing is broadcast.
//! 2. A trampoline is copied to [`TRAMPOLINE`] (frames reserved at boot), whose page is then
//!    remapped read/execute; its mutable data (a 32/64-bit GDT, CR3, the stack, the entry
//!    and its argument) lives in the separate read/write page [`TRAMPOLINE_DATA`]. The GDT
//!    descriptors are pre-marked accessed, so loading them never writes. So W^X holds even
//!    while an AP starts.
//! 3. For each AP, in turn: a heap stack and heap `CpuTables`, then INIT, 10 ms, start-up
//!    IPI, 200 µs, a second start-up IPI if it is not up yet, and up to 200 ms for its
//!    handshake. The AP runs the trampoline (real → protected → long mode with the
//!    kernel's own page tables), then [`ap_entry`]: its own GDT and TSS, the shared IDT,
//!    its local APIC in x2APIC mode, and the handshake with its APIC ID.
//! 4. The trampoline page goes back to read/write, never executable.
//!
//! The APs then halt with interrupts disabled: nothing schedules on them yet (M3 next).

use alloc::boxed::Box;
use alloc::vec;
use core::arch::global_asm;
use core::fmt::Write;
use core::ptr;
use core::sync::atomic::{AtomicU32, Ordering, fence};

use framekernel::acpi::{self, Processor};
use framekernel::paging::{Access, PAGE};
use framekernel::serial::Uart16550;
use origin_boot_info::{BootInfo, MemoryRegion, RegionKind};

use crate::Com1;
use crate::cpu::{self, CpuTables, rdtsc};
use crate::timer::{ApicRead, X2Apic};
use crate::vm;

/// Physical address of the trampoline code; the start-up IPI vector is `TRAMPOLINE >> 12`.
pub(crate) const TRAMPOLINE: u64 = 0x8000;
/// Physical address of the trampoline's data page.
pub(crate) const TRAMPOLINE_DATA: u64 = 0x9000;
/// Each AP's kernel stack.
const AP_STACK_BYTES: usize = 64 * 1024;
/// Processors this kernel tracks.
const MAX_CPUS: usize = 64;

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
    /// 0 until the AP is up, then its x2APIC ID + 1 (`u32::MAX` if it has no x2APIC).
    online: AtomicU32,
    /// The AP's own GDT, TSS and double-fault stack.
    tables: *mut CpuTables,
}

/// Brings up every enabled AP the MADT lists. Returns how many processors are online,
/// counting the boot processor.
pub(crate) fn start(
    com1: &mut Uart16550<Com1>,
    info: &BootInfo,
    regions: &[MemoryRegion],
    tsc_hz: u64,
) -> usize {
    let mut cpus = [Processor::default(); MAX_CPUS];
    let count = match discover(info.acpi_rsdp, &mut cpus) {
        Ok(n) => n,
        Err(e) => {
            let _ = writeln!(com1, "origin-kernel: ACPI processor discovery failed: {e}");
            return 1;
        }
    };
    let enabled = cpus.iter().take(count).filter(|p| p.enabled).count();
    let _ = writeln!(
        com1,
        "origin-kernel: ACPI MADT: {count} processors, {enabled} enabled"
    );
    // SAFETY: CPL 0, boot processor, interrupts disabled, x2APIC supported (the timer
    // bring-up checked it and already switched this APIC; the transition is then a no-op).
    let lapic = unsafe { X2Apic::enable() };
    let bsp = lapic.read(ApicRead::Id) as u32;
    if let Err(why) = prepare(regions) {
        let _ = writeln!(com1, "origin-kernel: AP start-up not possible: {why}");
        return 1;
    }
    let mut online = 1;
    for p in cpus
        .iter()
        .take(count)
        .filter(|p| p.enabled && p.apic_id != bsp)
    {
        match start_one(&lapic, p.apic_id, tsc_hz) {
            Ok(micros) => {
                online += 1;
                let _ = writeln!(
                    com1,
                    "origin-kernel: CPU {} (APIC ID {}) online after {micros} us: own GDT, TSS and IDT, x2APIC",
                    online - 1,
                    p.apic_id
                );
            }
            Err(why) => {
                let _ = writeln!(
                    com1,
                    "origin-kernel: APIC ID {}: {why}: AP start-up failed",
                    p.apic_id
                );
            }
        }
    }
    if let Err(e) = vm::set_access(TRAMPOLINE, Access::ReadWrite) {
        let _ = writeln!(
            com1,
            "origin-kernel: trampoline page not restored: {e:?}: AP start-up failed"
        );
    }
    let _ = writeln!(
        com1,
        "origin-kernel: SMP: {online} of {enabled} enabled processors online"
    );
    online
}

/// Reads `len` bytes of firmware memory at physical `addr`, mapping it read-only first if
/// the kernel does not map it.
fn firmware_bytes(addr: u64, len: usize) -> Result<&'static [u8], &'static str> {
    if addr == 0 || addr.checked_add(len as u64).is_none() {
        return Err("bad table address");
    }
    vm::map_readonly(addr, len as u64).map_err(|_| "cannot map an ACPI table")?;
    // SAFETY: `[addr, addr + len)` is mapped readable (just ensured), does not wrap, and
    // holds ACPI tables, which the firmware does not change after boot; `u8` has no
    // alignment or validity requirement.
    Ok(unsafe {
        core::slice::from_raw_parts(ptr::with_exposed_provenance::<u8>(addr as usize), len)
    })
}

/// RSDP → XSDT → MADT → processors.
fn discover(rsdp_addr: u64, out: &mut [Processor]) -> Result<usize, &'static str> {
    let rsdp = acpi::parse_rsdp(firmware_bytes(rsdp_addr, acpi::RSDP_LENGTH)?)
        .map_err(|_| "invalid RSDP")?;
    let table = |addr: u64, sig: &[u8; 4]| -> Result<&'static [u8], &'static str> {
        let header = firmware_bytes(addr, acpi::HEADER_LENGTH)?;
        let len = acpi::table_length(header).map_err(|_| "invalid table length")?;
        let bytes = firmware_bytes(addr, len as usize)?;
        acpi::check_table(bytes, sig).map_err(|_| "invalid table")?;
        Ok(bytes)
    };
    let xsdt = table(rsdp.xsdt, b"XSDT")?;
    for addr in acpi::xsdt_entries(xsdt) {
        let header = firmware_bytes(addr, acpi::HEADER_LENGTH)?;
        if acpi::signature(header) == Some(*b"APIC") {
            let madt = table(addr, b"APIC")?;
            return acpi::madt_processors(madt, out).map_err(|_| "invalid MADT");
        }
    }
    Err("no MADT")
}

/// Writes `value` at `offset` in the trampoline data page.
fn poke(offset: u64, value: u64) {
    let p = ptr::with_exposed_provenance_mut::<u64>((TRAMPOLINE_DATA + offset) as usize);
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
    // SAFETY: the source is the trampoline's bytes in the kernel's read-only-or-executable
    // image (readable); the destination is the reserved trampoline frame, mapped read/write
    // until `set_access` below; they do not overlap.
    unsafe {
        ptr::copy_nonoverlapping(
            ptr::with_exposed_provenance::<u8>(start),
            ptr::with_exposed_provenance_mut::<u8>(TRAMPOLINE as usize),
            len,
        );
    }
    vm::set_access(TRAMPOLINE, Access::ReadExecute).map_err(|_| "cannot remap the trampoline")?;
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

/// Starts the AP with x2APIC ID `apic_id`; returns microseconds until its handshake.
fn start_one(lapic: &X2Apic, apic_id: u32, tsc_hz: u64) -> Result<u64, &'static str> {
    // SAFETY: an all-zero `CpuTables` is valid (integers only); `init_ap` writes it before
    // the processor uses it.
    let tables = Box::into_raw(unsafe { Box::<CpuTables>::new_zeroed().assume_init() });
    let stack = Box::leak(vec![0u8; AP_STACK_BYTES].into_boxed_slice());
    let stack_top = (stack.as_mut_ptr().addr() + AP_STACK_BYTES) as u64 & !0xF;
    let start: &'static ApStart = Box::leak(Box::new(ApStart {
        online: AtomicU32::new(0),
        tables,
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
    // AP's alone.
    unsafe { cpu::init_ap(start.tables) };
    let answer = if cpu::x2apic_supported() {
        // SAFETY: CPL 0, this AP only, interrupts disabled, x2APIC supported, and the
        // spurious vector has a gate in the shared IDT.
        let lapic = unsafe { X2Apic::enable() };
        (lapic.read(ApicRead::Id) as u32).wrapping_add(1)
    } else {
        u32::MAX
    };
    start.online.store(answer, Ordering::Release);
    crate::halt()
}
