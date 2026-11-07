//! `_start`, the entry point of the Origin OS Framekernel (M1).
//!
//! The Origin boot loader calls `_start` after `ExitBootServices`, with:
//! - the physical address of a [`BootInfo`] in `rdi` (System V);
//! - interrupts disabled, and the processor at CPL 0;
//! - the firmware's identity-mapped page tables still active (physical == virtual);
//! - `rsp` 16-byte aligned before the `call`, on the stack `BootInfo` describes;
//! - the direction flag clear;
//! - the image's relocations applied.
//!
//! The kernel assumes nothing else about memory. It programs COM1, checks that its own
//! relocations were applied, validates the boot info and prints a summary. Then come the
//! first M2 steps: its own GDT, TSS and IDT (proven with a breakpoint), the frame
//! allocator, and its own page tables with a W^X kernel image (proven with deliberate
//! faults), after which it halts. Only the boot processor runs, and nothing here enables
//! interrupts.
#![no_std]
#![no_main]

extern crate alloc;

use core::arch::asm;
use core::fmt::Write;
use core::panic::PanicInfo;
use core::ptr;
use core::slice;
use core::sync::atomic::{AtomicBool, Ordering};

use framekernel::boot::{self, InvalidBootInfo, MemorySummary};

use crate::console::Console;
use framekernel::report;

mod console;
mod cpu;
mod firmware;
mod memory;
mod pcie;
mod phys;
mod process;
mod sched;
mod smp;
mod time;
mod timer;
mod tlb;
mod user;
mod vm;
mod wait;
use framekernel::frames::{self, FrameAllocator};
use framekernel::paging::layout::{self, Placement};
use framekernel::paging::{PAGE, Table, TablePool};
use framekernel::serial::{Register, Uart16550, UartRegisters};
use origin_boot_info::{
    BootInfo, KernelSegment, MemoryRegion, RegionKind, SEGMENT_EXECUTE, SEGMENT_WRITE,
};

/// Frames reserved, next to the frame bitmap, for the kernel's page tables: enough for a
/// direct map of well over 100 GiB with 2 MiB pages plus 4 KiB edges at every region.
const TABLE_POOL_FRAMES: u64 = 256;

/// I/O port base of COM1 on PC-compatible platforms.
const COM1_BASE: u16 = 0x3F8;

/// Set by the first panic. A panic while reporting a panic only halts.
static PANICKING: AtomicBool = AtomicBool::new(false);

/// The object [`RELOCATION_PROBE`] points at.
static RELOCATION_TARGET: [u8; 8] = *b"ORIGRELO";
/// A pointer stored in the image. The ELF has an `R_X86_64_RELATIVE` relocation for it, so
/// its run-time value equals the run-time address of [`RELOCATION_TARGET`] only if the
/// loader applied the image's relocations.
static RELOCATION_PROBE: &[u8; 8] = &RELOCATION_TARGET;

/// Kernel entry point; the ELF entry.
///
/// # Safety
///
/// Only the Origin boot loader may call this, once, under the entry contract in the module
/// documentation. `boot_info` must be null or the address of a readable `BootInfo` whose
/// region array, if the header is valid, is readable and is not written while the kernel
/// runs.
#[unsafe(no_mangle)]
pub unsafe extern "sysv64" fn _start(boot_info: *const BootInfo) -> ! {
    // The monotonic clock's zero (CLAUDE.md §68).
    time::mark_entry();
    // SAFETY: the entry contract: CPL 0, boot processor only, interrupts disabled. The
    // firmware and the loader stopped using COM1 at `ExitBootServices`, and no other kernel
    // code drives it.
    let mut uart = Uart16550::new(unsafe { Com1::new() });
    uart.init();

    if !relocations_applied() {
        // `core::fmt` would call through unrelocated vtables. `write_str` on the concrete
        // type is a direct call, and the string is addressed RIP-relative.
        let _ = uart.write_str(report::RELOCATIONS_NOT_APPLIED);
        halt();
    }
    // From here on every processor prints through the console, a line at a time.
    console::init(uart);
    let mut com1 = console::Console::new();

    // SAFETY: `boot_info` is the loader's pointer, passed on unchanged, and this function's
    // contract is exactly `read_boot_info`'s.
    let (info, regions, segments, memory) = match unsafe { read_boot_info(boot_info) } {
        Ok(v) => v,
        Err(err) => {
            let _ = report::write_invalid(&mut com1, err);
            halt();
        }
    };
    let _ = report::write_report(&mut com1, &info, &memory);
    for (i, s) in segments.iter().enumerate() {
        let access = match (
            s.access & SEGMENT_WRITE != 0,
            s.access & SEGMENT_EXECUTE != 0,
        ) {
            (false, false) => "R",
            (true, false) => "RW",
            (false, true) => "RX",
            (true, true) => "RWX",
        };
        let _ = writeln!(
            com1,
            "origin-kernel: kernel segment {i}: {access} at {:#x}, {} pages",
            info.kernel_virtual_base.wrapping_add(s.rva),
            s.pages
        );
    }

    // M2, first step: the kernel's own GDT, TSS and IDT, proven with a breakpoint whose
    // handler reports it and resumes here.
    // SAFETY: called once, at CPL 0, on the boot processor, with interrupts disabled (entry
    // contract), before anything depends on the firmware's tables.
    unsafe { cpu::init() };
    let _ = report::write_cpu_tables(&mut com1);
    cpu::breakpoint();

    // M2, second step: the physical frame allocator over the boot-info regions, with a
    // pool of frames for page tables beside its bitmap.
    let Some((mut alloc, pool, pool_base)) = init_memory(&mut com1, regions) else {
        halt();
    };

    // M2, third step: the kernel's own page tables, proven with deliberate faults; then
    // the frame allocator is proven again through the new direct map.
    let mut heap_ok = false;
    if init_paging(&mut com1, &info, regions, segments, pool, pool_base) {
        prove_frames(&mut com1, &mut alloc);
        // M2: the kernel heap, which takes over the frame allocator; from here on the
        // `alloc` crate works in the kernel.
        memory::init(alloc);
        heap_ok = prove_heap(&mut com1);
        if heap_ok {
            prove_user_isolation(&mut com1);
        }
    }

    // M2, fourth step: interrupts. The local APIC in x2APIC mode, its timer calibrated
    // against the PIT and proven by taking periodic interrupts.
    let tsc_hz = timer::init(&mut com1);
    if let Some(hz) = tsc_hz {
        prove_nmi_ist(&mut com1);
        // The clocks: the monotonic clock's TSC rate, proven against the RTC, and the wall
        // clock from the RTC (CLAUDE.md §68).
        time::init(&mut com1, hz, info.acpi_rsdp);
    }

    // M3, first step: start the application processors the ACPI MADT lists. It needs the
    // heap, the kernel's own tables and the calibrated TSC.
    if let Some(tsc_hz) = tsc_hz
        && heap_ok
    {
        let online = smp::start(&mut com1, &info, regions, tsc_hz);
        // M2 completion: once every enabled processor has left firmware code, the loader's
        // and the firmware's boot-time memory belongs to the kernel.
        if online.complete()
            && let Some((added, free)) = memory::reclaim_loader_memory()
        {
            let _ = writeln!(
                com1,
                "origin-kernel: reclaimed {} MiB of loader and boot-services memory; {} MiB free",
                (added * memory::FRAME) >> 20,
                (free * memory::FRAME) >> 20
            );
        }
        // M3, second step: preemptive kernel threads on every online processor.
        sched::prove(&mut com1, online.ids(), tsc_hz);
        // M5 groundwork: PCI Express enumeration through the ACPI MCFG.
        pcie::init(&mut com1, info.acpi_rsdp, regions, online.ids().len());
    }

    let _ = report::write_halt(&mut com1);
    halt()
}

/// Builds the kernel's address space, loads it, and proves its protections on the
/// processor. Returns whether the kernel now runs on its own tables.
fn init_paging(
    com1: &mut Console,
    info: &BootInfo,
    regions: &[MemoryRegion],
    segments: &[KernelSegment],
    pool: &'static [Table],
    pool_base: u64,
) -> bool {
    if !cpu::nx_supported() {
        let _ = writeln!(
            com1,
            "origin-kernel: the processor has no NX bit; W^X cannot be enforced"
        );
        return false;
    }
    let mut tables = TablePool::new(pool, pool_base);
    let stack_guard = info.stack_base.wrapping_sub(PAGE);
    let at = Placement {
        direct_map_base: info.direct_map_base,
        kernel_virtual_base: info.kernel_virtual_base,
        kernel_physical_base: info.kernel_base,
        stack_guard,
    };
    let (space, stats) = match layout::build(&mut tables, &at, regions, segments) {
        Ok(v) => v,
        Err(e) => {
            let _ = writeln!(com1, "origin-kernel: page tables not built: {e:?}");
            return false;
        }
    };
    // SAFETY: boot processor, CPL 0, interrupts disabled (entry contract); NX is supported
    // (checked above). `kernel_space::build` mapped the kernel image segments, the stack
    // this code runs on, the boot info and region and segment arrays, the frame bitmap and
    // this table pool (all in direct-mapped regions), and the kernel's statics (the GDT,
    // IDT, TSS and double-fault stack are in the RW segment). COM1 is port I/O.
    unsafe { cpu::load_page_tables(space.root()) };
    // SAFETY: CPL 0; the tables just loaded mark only the kernel half global (`layout`).
    // Toggling CR4.PGE also drops every translation cached under the loader's tables.
    let global = unsafe { tlb::enable_global_pages() };
    let used = tables.used();
    vm::install(space, tables);
    let _ = writeln!(
        com1,
        "origin-kernel: page tables loaded: {} tables, {} MiB direct map, {} image pages, {} 4 KiB and {} 2 MiB leaves; global pages {global}",
        used,
        stats.direct_bytes >> 20,
        stats.image_pages,
        stats.small_pages,
        stats.large_pages
    );
    // ADR-0004: nothing of the kernel is left in the lower half, and its physical pages
    // have no second mapping in the direct map.
    let identity = vm::is_mapped(info.kernel_base);
    let alias = vm::is_mapped(phys::to_virt(info.kernel_base));
    let _ = writeln!(
        com1,
        "origin-kernel: higher half: kernel image identity address mapped {identity}, direct-map alias mapped {alias}{}",
        if identity || alias {
            ": higher half failed"
        } else {
            ""
        }
    );
    !identity && !alias && prove_protections(com1, info, segments)
}

/// Deliberate accesses that the new tables must refuse: a write to kernel code, a write to
/// read-only kernel data, an instruction fetch from writable data and a read of page 0.
/// Each must raise #PF with the expected error code (SDM Vol. 3A §4.7: bit 0 present,
/// bit 1 write, bit 4 instruction fetch). Returns whether all four did.
fn prove_protections(com1: &mut Console, info: &BootInfo, segments: &[KernelSegment]) -> bool {
    let first = |want: u32, avoid: u32| {
        segments
            .iter()
            .find(|s| s.access & want == want && s.access & avoid == 0)
            .map(|s| info.kernel_virtual_base + s.rva)
    };
    let (Some(text), Some(rodata)) = (
        first(SEGMENT_EXECUTE, SEGMENT_WRITE),
        first(0, SEGMENT_WRITE | SEGMENT_EXECUTE),
    ) else {
        let _ = writeln!(com1, "origin-kernel: no code or read-only segment to probe");
        return false;
    };
    let read = |addr: u64| {
        // SAFETY: `addr` is the first byte of a mapped, readable kernel segment.
        unsafe { ptr::with_exposed_provenance::<u8>(addr as usize).read_volatile() }
    };
    let (exec_addr, exec) = cpu::probe_execute_data();
    let cases = [
        ("write to kernel code", text, 0x3, {
            let byte = read(text);
            // SAFETY: `byte` is the value at `text`, kernel code that nothing modifies, and
            // `text` is RAM, not MMIO.
            unsafe { cpu::probe_write(text, byte) }
        }),
        ("write to read-only kernel data", rodata, 0x3, {
            let byte = read(rodata);
            // SAFETY: as above, for read-only data.
            unsafe { cpu::probe_write(rodata, byte) }
        }),
        (
            "instruction fetch from writable data",
            exec_addr,
            0x11,
            exec,
        ),
        // SAFETY: page 0 is RAM or unmapped, never MMIO.
        ("read of page 0", 0, 0x0, unsafe { cpu::probe_read(0) }),
        (
            "read of the boot stack's guard page",
            phys::to_virt(info.stack_base - PAGE),
            0x0,
            // SAFETY: the guard is a RAM page of the KernelStack region, not MMIO.
            unsafe { cpu::probe_read(phys::to_virt(info.stack_base - PAGE)) },
        ),
    ];
    let mut all = true;
    for (what, addr, expected, probe) in cases {
        match probe {
            cpu::Probe::Faulted { error, address } if error == expected && address == addr => {
                let _ = writeln!(
                    com1,
                    "origin-kernel: page protection: {what} at {addr:#x} faulted (#PF error {error:#x})"
                );
            }
            other => {
                all = false;
                let _ = writeln!(
                    com1,
                    "origin-kernel: page protection violated: {what} at {addr:#x}: {other:?}"
                );
            }
        }
    }
    all
}

/// Builds the frame allocator, with its bitmap and then the page-table pool at the start of
/// the first usable region that can hold both. Returns the allocator, the pool and the
/// pool's physical address.
fn init_memory(
    com1: &mut Console,
    regions: &'static [MemoryRegion],
) -> Option<(FrameAllocator<'static>, &'static [Table], u64)> {
    let words = frames::words_for(frames::covered_end(regions));
    let bitmap_pages = (words * 8).div_ceil(frames::FRAME);
    let pages = bitmap_pages + TABLE_POOL_FRAMES;
    let usable = RegionKind::Usable as u32;
    let Some(home) = regions
        .iter()
        .find(|r| r.kind == usable && r.base != 0 && r.pages >= pages)
    else {
        let _ = writeln!(
            com1,
            "origin-kernel: no usable region can hold the {pages}-page frame bitmap and table pool"
        );
        return None;
    };
    let bitmap_ptr = ptr::with_exposed_provenance_mut::<u64>(phys::to_virt(home.base) as usize);
    // SAFETY: `home` is a `Usable` region from the validated boot info, so nothing else
    // owns it; the firmware's identity mapping is active, so its physical address is
    // directly writable; it holds at least `pages` pages, which cover the bitmap's `words`
    // words and the pool after it.
    unsafe { bitmap_ptr.write_bytes(0, (pages * frames::FRAME / 8) as usize) };
    // SAFETY: as above; the words were just zeroed, and the bitmap frames are reserved
    // below, so the allocator never hands them out. The slice lives for the rest of the
    // kernel's run, and nothing else refers to these frames.
    let bitmap = unsafe { slice::from_raw_parts_mut(bitmap_ptr, words as usize) };
    let pool_base = home.base + bitmap_pages * frames::FRAME;
    // SAFETY: the pool's `TABLE_POOL_FRAMES` frames follow the bitmap inside `home`, were
    // zeroed above and are reserved below, so only the pool uses them. `Table` is 4 KiB of
    // `AtomicU64`, valid for any bits and aligned (the frames are 4 KiB aligned); accesses
    // through the shared slice are atomic, so the processor's own accessed/dirty updates
    // do not race with them.
    let pool = unsafe {
        slice::from_raw_parts(
            ptr::with_exposed_provenance::<Table>(phys::to_virt(pool_base) as usize),
            TABLE_POOL_FRAMES as usize,
        )
    };
    // The AP trampoline's code and data frames (M3) are never handed out.
    let trampoline = (smp::TRAMPOLINE, 2);
    let alloc = match FrameAllocator::new(bitmap, regions, &[(home.base, pages), trampoline]) {
        Ok(a) => a,
        Err(e) => {
            let _ = writeln!(com1, "origin-kernel: frame allocator: {e:?}");
            return None;
        }
    };
    let _ = writeln!(
        com1,
        "origin-kernel: frame allocator: {} free frames ({} MiB), bitmap of {} pages at {:#x}",
        alloc.free_frames(),
        (alloc.free_frames() * frames::FRAME) >> 20,
        bitmap_pages,
        home.base
    );
    debug_assert_eq!(PAGE, frames::FRAME);
    Some((alloc, pool, pool_base))
}

/// Proves the heap with real `alloc` collections: a large vector (a frame run), thousands
/// of boxes (slabs), a B-tree map and a string, with their contents checked; then that
/// dropping them all returns the heap and the frame allocator exactly to where they were.
fn prove_heap(com1: &mut Console) -> bool {
    use alloc::boxed::Box;
    use alloc::collections::BTreeMap;
    use alloc::string::String;
    use alloc::vec::Vec;

    let Some(before) = memory::stats() else {
        return false;
    };
    let peak = {
        let numbers: Vec<u64> = (0..100_000u64).collect();
        let sum: u64 = numbers.iter().sum();
        let boxes: Vec<Box<[u64; 4]>> = (0..2000u64).map(|i| Box::new([i; 4])).collect();
        let box_sum: u64 = boxes.iter().map(|b| b[3]).sum();
        let mut map = BTreeMap::new();
        for i in 0..1000u64 {
            map.insert(i * 7 % 1000, i);
        }
        let mut text = String::new();
        let _ = write!(text, "{sum}");
        let right = sum == 4_999_950_000
            && box_sum == 1_999_000
            && map.len() == 1000
            && map.keys().copied().eq(0..1000)
            && text == "4999950000";
        if !right {
            let _ = writeln!(com1, "origin-kernel: heap contents are wrong");
            return false;
        }
        memory::stats()
    };
    let after = memory::stats();
    if let Some((p, _)) = peak {
        let _ = writeln!(
            com1,
            "origin-kernel: heap: Vec of 100000 u64, 2000 boxes, B-tree of 1000 keys, a string: {} KiB in use, {} slabs, {} pages in runs",
            p.bytes_in_use >> 10,
            p.slabs,
            p.large_pages
        );
    }
    if after != Some(before) {
        let _ = writeln!(
            com1,
            "origin-kernel: heap leaked: {after:?} after, {before:?} before"
        );
        return false;
    }
    let _ = writeln!(
        com1,
        "origin-kernel: heap: everything freed; {} free frames ({} MiB), as before",
        before.1,
        (before.1 * memory::FRAME) >> 20
    );
    true
}

/// Sends this processor an NMI (x2APIC ICR, NMI delivery, "self" shorthand) and checks
/// that it was taken on the dedicated NMI stack (IST2), not on the interrupted stack.
fn prove_nmi_ist(com1: &mut Console) -> bool {
    let (before, _) = cpu::nmis();
    // SAFETY: CPL 0, boot processor; x2APIC mode is on (timer bring-up), so this is a no-op
    // transition yielding the handle.
    let lapic = unsafe { timer::X2Apic::enable() };
    // SAFETY: an NMI to this processor runs the NMI handler, which only records it.
    unsafe { lapic.send_ipi(0, (1 << 18) | 0x400) };
    let start = cpu::rdtsc();
    while cpu::nmis().0 == before && cpu::rdtsc().wrapping_sub(start) < 100_000_000 {
        core::hint::spin_loop();
    }
    let (count, frame) = cpu::nmis();
    let (low, high) = cpu::bsp_nmi_stack();
    let on_ist = count > before && (low..high).contains(&frame);
    let _ = writeln!(
        com1,
        "origin-kernel: NMI taken on its own stack (IST2): frame at {frame:#x}, stack {low:#x}..{high:#x}{}",
        if on_ist { "" } else { ": NMI stack failed" }
    );
    on_ist
}

/// Where the SMEP/SMAP probe maps its temporary user page: the top of the lower half.
const USER_PROBE_PAGE: u64 = 0x0000_7FFF_FFFF_E000;

/// Enables SMEP and SMAP and proves both: a temporary user page holding `mov rax, -1;
/// ret`, which a supervisor fetch (SMEP) and a supervisor read (SMAP) must both fault on.
/// The page is removed afterwards.
fn prove_user_isolation(com1: &mut Console) -> bool {
    use alloc::alloc::{Layout, alloc_zeroed, dealloc};
    // SAFETY: CPL 0, and the kernel has no user mappings yet.
    let (smep, smap) = unsafe { cpu::enable_smep_smap() };
    if !(smep && smap) {
        let _ = writeln!(
            com1,
            "origin-kernel: SMEP {smep}, SMAP {smap}: supervisor isolation from user pages incomplete"
        );
        return false;
    }
    let Ok(layout) = Layout::from_size_align(4096, 4096) else {
        return false;
    };
    // SAFETY: `layout` has a non-zero size.
    let frame = unsafe { alloc_zeroed(layout) };
    if frame.is_null() {
        return false;
    }
    // SAFETY: `frame` is a fresh, writable 4 KiB allocation.
    unsafe {
        ptr::copy_nonoverlapping(cpu::PROBE_RETURN.as_ptr(), frame, cpu::PROBE_RETURN.len());
    }
    let phys = phys::to_phys(frame.expose_provenance() as u64);
    let mut ok = true;
    if vm::map_user(
        USER_PROBE_PAGE,
        phys,
        framekernel::paging::Access::ReadExecute,
    )
    .is_err()
    {
        let _ = writeln!(com1, "origin-kernel: cannot map the user probe page");
        ok = false;
    } else {
        // SAFETY: the user page holds `PROBE_RETURN`.
        let fetch = unsafe { cpu::probe_execute(USER_PROBE_PAGE) };
        // SAFETY: the user page is RAM, not MMIO.
        let read = unsafe { cpu::probe_read(USER_PROBE_PAGE) };
        for (what, probe, expected) in [
            ("SMEP: supervisor fetch from a user page", fetch, 0x11),
            ("SMAP: supervisor read of a user page", read, 0x1),
        ] {
            match probe {
                cpu::Probe::Faulted { error, address }
                    if error == expected && address == USER_PROBE_PAGE =>
                {
                    let _ = writeln!(
                        com1,
                        "origin-kernel: page protection: {what} at {USER_PROBE_PAGE:#x} faulted (#PF error {error:#x})"
                    );
                }
                other => {
                    ok = false;
                    let _ = writeln!(
                        com1,
                        "origin-kernel: page protection violated: {what}: {other:?}"
                    );
                }
            }
        }
        if vm::unmap(USER_PROBE_PAGE).is_err() {
            ok = false;
        }
    }
    // SAFETY: `frame` came from `alloc_zeroed(layout)` and is no longer mapped as a user
    // page.
    unsafe { dealloc(frame, layout) };
    ok
}

/// Proves the allocator by allocating, writing, reading back and freeing three frames.
fn prove_frames(com1: &mut Console, alloc: &mut FrameAllocator<'_>) {
    let mut got = [0u64; 3];
    for (i, slot) in got.iter_mut().enumerate() {
        let Some(addr) = alloc.allocate() else {
            let _ = writeln!(com1, "origin-kernel: frame allocator exhausted");
            return;
        };
        let p = ptr::with_exposed_provenance_mut::<u64>(phys::to_virt(addr) as usize);
        let pattern = 0x4F52_4947_494E_0000 | i as u64;
        // SAFETY: the allocator just handed out this usable frame, which the kernel's
        // direct map covers read/write, and nothing else uses it; a volatile write and read
        // prove the RAM really holds it.
        unsafe { p.write_volatile(pattern) };
        // SAFETY: as above.
        let back = unsafe { p.read_volatile() };
        if back != pattern {
            let _ = writeln!(
                com1,
                "origin-kernel: frame {addr:#x} did not keep its contents"
            );
            return;
        }
        *slot = addr;
    }
    for &addr in &got {
        if alloc.free(addr).is_err() {
            let _ = writeln!(com1, "origin-kernel: frame {addr:#x} could not be freed");
            return;
        }
    }
    let _ = writeln!(
        com1,
        "origin-kernel: allocated, wrote and freed 3 frames at {:#x}, {:#x}, {:#x}",
        got[0], got[1], got[2]
    );
}

/// What [`read_boot_info`] returns: the copied header, the region and kernel segment
/// arrays, and the memory summary.
type BootData = (
    BootInfo,
    &'static [MemoryRegion],
    &'static [KernelSegment],
    MemorySummary,
);

/// Copies and validates the boot info, then checks its kernel segment list and summarizes
/// its region array.
///
/// # Safety
///
/// `boot_info` must be null or the address of a readable `BootInfo` whose region and
/// segment arrays, if the header is valid, are readable and are never written afterwards:
/// the returned slices are `'static`, which holds because the loader leaves that memory to
/// the kernel for good.
unsafe fn read_boot_info(boot_info: *const BootInfo) -> Result<BootData, InvalidBootInfo> {
    boot::check_address(boot_info.addr())?;
    // SAFETY: `boot_info` is non-null and aligned (checked above) and readable (the
    // caller's contract). `BootInfo` has only integer fields, so every bit pattern is a
    // valid value. The header is copied, so no reference into loader memory is kept.
    let info = unsafe { boot_info.read() };
    let array = boot::validate(&info)?;
    // Boot info 0.3: the arrays are given by physical address; read them through the direct
    // map the loader built.
    phys::init(&info);
    let first =
        ptr::with_exposed_provenance::<MemoryRegion>(phys::to_virt(array.addr as u64) as usize);
    // SAFETY: `validate` checked that the array is non-null, aligned, at most `isize::MAX`
    // bytes long and does not wrap. The caller's contract makes it readable and unchanging
    // while the slice lives. `MemoryRegion` has only integer fields, so every bit pattern
    // is a valid value.
    let regions = unsafe { slice::from_raw_parts(first, array.len) };
    let memory = boot::summarize(regions)?;
    let segment_array = boot::segment_array(&info)?;
    let first = ptr::with_exposed_provenance::<KernelSegment>(phys::to_virt(
        segment_array.addr as u64,
    ) as usize);
    // SAFETY: as for the regions: `segment_array` checked the layout requirements, the
    // caller's contract makes the memory readable and unchanging, and `KernelSegment` has
    // only integer fields.
    let segments = unsafe { slice::from_raw_parts(first, segment_array.len) };
    boot::check_kernel_segments(segments, &info)?;
    Ok((info, regions, segments, memory))
}

/// Whether the loader applied the image's relocations.
///
/// The compiler knows the link-time value of [`RELOCATION_PROBE`], so the load must be
/// volatile to observe what is really in memory. The address of [`RELOCATION_TARGET`] is
/// computed RIP-relative and is correct wherever the image was loaded.
fn relocations_applied() -> bool {
    let probe = (&raw const RELOCATION_PROBE).cast::<*const u8>();
    // SAFETY: `probe` points to a live, aligned static of pointer size. It is read as a
    // raw pointer, which is valid for any bit pattern, so an unrelocated value is never
    // treated as a reference.
    let stored = unsafe { ptr::read_volatile(probe) };
    ptr::eq(stored, RELOCATION_TARGET.as_ptr())
}

/// Disables interrupts and halts this processor forever.
pub(crate) fn halt() -> ! {
    loop {
        // SAFETY: the kernel runs at CPL 0 (entry contract). `cli; hlt` touches no memory
        // and needs no stack. An NMI or SMI can still resume the processor, so it halts
        // again.
        unsafe { asm!("cli", "hlt", options(nomem, nostack)) };
    }
}

/// COM1's registers, reached through x86 port I/O.
pub(crate) struct Com1(());

impl Com1 {
    /// # Safety
    ///
    /// The caller must run at CPL 0, and no other processor or driver may program COM1
    /// while the value is used. Accesses through it reach only COM1's registers
    /// 0x3F8–0x3FD, which cannot affect memory.
    pub(crate) unsafe fn new() -> Self {
        Self(())
    }
}

impl UartRegisters for Com1 {
    fn read(&mut self, reg: Register) -> u8 {
        // SAFETY: `Com1::new`'s contract (CPL 0, COM1 owned by the kernel), and the port is
        // one of COM1's registers, so the read cannot affect memory.
        unsafe { inb(COM1_BASE + reg.offset()) }
    }

    fn write(&mut self, reg: Register, value: u8) {
        // SAFETY: as for `read`.
        unsafe { outb(COM1_BASE + reg.offset(), value) }
    }
}

/// Writes `value` to I/O port `port`.
///
/// # Safety
///
/// The processor must be at CPL 0 (or hold I/O permission for `port`), and `port` must be a
/// device register that this code owns and whose write cannot affect memory safety, such as
/// a UART register.
pub(crate) unsafe fn outb(port: u16, value: u8) {
    // SAFETY: the caller's contract. `out` reads no memory, needs no stack and leaves the
    // flags unchanged.
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags));
    }
}

/// Reads I/O port `port`.
///
/// # Safety
///
/// As for [`outb`]: CPL 0 (or I/O permission), and a register this code owns whose read
/// cannot affect memory safety.
pub(crate) unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    // SAFETY: the caller's contract. `in` writes only `al`, reads no memory, needs no stack
    // and leaves the flags unchanged.
    unsafe {
        asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack, preserves_flags));
    }
    value
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    if !PANICKING.swap(true, Ordering::AcqRel) {
        // The urgent console never waits long for its lock, so a panic inside the console
        // still reports; before `console::init` it programs COM1 itself.
        let mut out = console::Console::urgent();
        let _ = report::write_panic(&mut out, info);
    }
    halt()
}
