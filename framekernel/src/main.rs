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

use core::arch::asm;
use core::fmt::Write;
use core::panic::PanicInfo;
use core::ptr;
use core::slice;
use core::sync::atomic::{AtomicBool, Ordering};

use framekernel::boot::{self, InvalidBootInfo, MemorySummary};
use framekernel::report;

mod cpu;
use framekernel::frames::{self, FrameAllocator};
use framekernel::kernel_space;
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

/// Set once `_start` has programmed COM1, so the panic handler does not program it again,
/// which would clear the transmit FIFO.
static COM1_READY: AtomicBool = AtomicBool::new(false);
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
    // SAFETY: the entry contract: CPL 0, boot processor only, interrupts disabled. The
    // firmware and the loader stopped using COM1 at `ExitBootServices`, and no other kernel
    // code drives it.
    let mut com1 = Uart16550::new(unsafe { Com1::new() });
    com1.init();
    COM1_READY.store(true, Ordering::Release);

    if !relocations_applied() {
        // `core::fmt` would call through unrelocated vtables. `write_str` on the concrete
        // type is a direct call, and the string is addressed RIP-relative.
        let _ = com1.write_str(report::RELOCATIONS_NOT_APPLIED);
        halt();
    }

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
            info.kernel_base.wrapping_add(s.rva),
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
    if init_paging(&mut com1, &info, regions, segments, pool, pool_base) {
        prove_frames(&mut com1, &mut alloc);
    }

    let _ = report::write_halt(&mut com1);
    halt()
}

/// Builds the kernel's address space, loads it, and proves its protections on the
/// processor. Returns whether the kernel now runs on its own tables.
fn init_paging(
    com1: &mut Uart16550<Com1>,
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
    let (space, stats) = match kernel_space::build(&mut tables, info.kernel_base, regions, segments)
    {
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
    let _ = writeln!(
        com1,
        "origin-kernel: page tables loaded: {} tables, {} MiB direct map, {} image pages, {} 4 KiB and {} 2 MiB leaves",
        tables.used(),
        stats.direct_bytes >> 20,
        stats.image_pages,
        stats.small_pages,
        stats.large_pages
    );
    prove_protections(com1, info, segments)
}

/// Deliberate accesses that the new tables must refuse: a write to kernel code, a write to
/// read-only kernel data, an instruction fetch from writable data and a read of page 0.
/// Each must raise #PF with the expected error code (SDM Vol. 3A §4.7: bit 0 present,
/// bit 1 write, bit 4 instruction fetch). Returns whether all four did.
fn prove_protections(
    com1: &mut Uart16550<Com1>,
    info: &BootInfo,
    segments: &[KernelSegment],
) -> bool {
    let first = |want: u32, avoid: u32| {
        segments
            .iter()
            .find(|s| s.access & want == want && s.access & avoid == 0)
            .map(|s| info.kernel_base + s.rva)
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
    com1: &mut Uart16550<Com1>,
    regions: &[MemoryRegion],
) -> Option<(FrameAllocator<'static>, &'static [Table], u64)> {
    let words = frames::words_for(frames::usable_end(regions));
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
    let bitmap_ptr = ptr::with_exposed_provenance_mut::<u64>(home.base as usize);
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
            ptr::with_exposed_provenance::<Table>(pool_base as usize),
            TABLE_POOL_FRAMES as usize,
        )
    };
    let alloc = match FrameAllocator::new(bitmap, regions, &[(home.base, pages)]) {
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

/// Proves the allocator by allocating, writing, reading back and freeing three frames.
fn prove_frames(com1: &mut Uart16550<Com1>, alloc: &mut FrameAllocator<'_>) {
    let mut got = [0u64; 3];
    for (i, slot) in got.iter_mut().enumerate() {
        let Some(addr) = alloc.allocate() else {
            let _ = writeln!(com1, "origin-kernel: frame allocator exhausted");
            return;
        };
        let p = ptr::with_exposed_provenance_mut::<u64>(addr as usize);
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
    let first = ptr::with_exposed_provenance::<MemoryRegion>(array.addr);
    // SAFETY: `validate` checked that the array is non-null, aligned, at most `isize::MAX`
    // bytes long and does not wrap. The caller's contract makes it readable and unchanging
    // while the slice lives. `MemoryRegion` has only integer fields, so every bit pattern
    // is a valid value.
    let regions = unsafe { slice::from_raw_parts(first, array.len) };
    let memory = boot::summarize(regions)?;
    let segment_array = boot::segment_array(&info)?;
    let first = ptr::with_exposed_provenance::<KernelSegment>(segment_array.addr);
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
unsafe fn outb(port: u16, value: u8) {
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
unsafe fn inb(port: u16) -> u8 {
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
        // SAFETY: the kernel runs at CPL 0 on the boot processor only. A panic interrupts
        // `_start` on that same processor, so nothing drives COM1 concurrently; at worst a
        // half-written line is followed by the panic line.
        let mut com1 = Uart16550::new(unsafe { Com1::new() });
        if !COM1_READY.load(Ordering::Acquire) {
            com1.init();
        }
        let _ = report::write_panic(&mut com1, info);
    }
    halt()
}
