//! Early boot (ADR-0011 N12; CLAUDE.md §19, §24, §28): what has to happen before the kernel's
//! typed interfaces exist, behind one capability.
//!
//! The kernel image's `_start` hands the boot loader's pointer to the kernel's entry function as
//! an [`Entry`], once. An `Entry` checks the relocations and reads the boot info, which gives the
//! [`Early`]: the boot processor's right to take the boot steps. Each step is a method: the
//! processor's own tables, SIMD, the frame allocator, the kernel's page tables, the heap, the
//! proofs of the page protections, and at the end taking back the loader's memory.
//!
//! These steps are safe to call because [`Early`] carries what their safety needs:
//! - there is one (only an `Entry` makes it, and only `_start` makes an `Entry`), and it stays on
//!   the boot processor: neither type is `Send`;
//! - each step runs at most once and does nothing before the steps it needs have run, so no
//!   order of calls can reach a step whose premises are false;
//! - nothing the caller passes is trusted: the boot data is read and validated here and never
//!   changes, and the allocator and the page tables stay inside this module until the
//!   framework's own memory and address-space owners take them over.
//!
//! The entry contract, which `_start` relies on: the Origin boot loader calls it after
//! `ExitBootServices`, with the physical address of a [`BootInfo`] in `rdi` (System V; null, or
//! readable, with region and segment arrays, if the header is valid, that are readable and are
//! never written afterwards: the loader leaves that memory to the kernel for good), interrupts
//! disabled, the processor at CPL 0, the loader's page tables active (a direct map of RAM at
//! `direct_map_base`, and the kernel image at `kernel_virtual_base`), `rsp` 16-byte aligned before
//! the `call`, on the stack `BootInfo` describes, the direction flag clear, and the image's
//! relocations applied.

use alloc::alloc::{Layout, alloc_zeroed, dealloc};
use core::fmt::Write;
use core::marker::PhantomData;
use core::ptr;
use core::slice;

use origin_boot_info::{
    BootInfo, KernelSegment, MemoryRegion, RegionKind, SEGMENT_EXECUTE, SEGMENT_WRITE,
};
use origin_paging::layout::{self, Placement};
use origin_paging::{Access, AddressSpace, PAGE, Table, TablePool};

use crate::boot::{self, InvalidBootInfo, MemorySummary};
use crate::frames::{self, FrameAllocator};
use crate::once::SetOnce;
use crate::smp::Online;
use crate::{cpu, memory, phys, tlb, vm};

/// The fewest frames reserved, next to the frame bitmap, for the kernel's page tables.
const TABLE_POOL_MIN_FRAMES: u64 = 256;
/// Frames kept beyond what the boot's own tables can need (`layout::table_frames`), for what
/// is mapped afterwards: device registers, firmware tables and guard pages that split a leaf.
/// The kernel stacks' region has tables of its own, made at boot (`layout::STACK_REGION_TABLES`).
const TABLE_POOL_RESERVE: u64 = 235;
/// Where the SMEP/SMAP probe maps its temporary user page: the top of the lower half.
const USER_PROBE_PAGE: u64 = 0x0000_7FFF_FFFF_E000;

/// The object [`RELOCATION_PROBE`] points at.
static RELOCATION_TARGET: [u8; 8] = *b"ORIGRELO";
/// A pointer stored in the image. The ELF has an `R_X86_64_RELATIVE` relocation for it, so
/// its run-time value equals the run-time address of [`RELOCATION_TARGET`] only if the
/// loader applied the image's relocations.
static RELOCATION_PROBE: &[u8; 8] = &RELOCATION_TARGET;

/// The boot loader's description of the machine, validated. It never changes: the memory it
/// refers to is the kernel's for good.
#[derive(Debug)]
pub struct BootData {
    info: BootInfo,
    regions: &'static [MemoryRegion],
    segments: &'static [KernelSegment],
    summary: MemorySummary,
}

impl BootData {
    /// The boot-info header.
    pub const fn info(&self) -> &BootInfo {
        &self.info
    }

    /// The firmware's memory map.
    pub const fn regions(&self) -> &'static [MemoryRegion] {
        self.regions
    }

    /// The kernel image's segments.
    pub const fn segments(&self) -> &'static [KernelSegment] {
        self.segments
    }

    /// What the memory map adds up to.
    pub const fn summary(&self) -> MemorySummary {
        self.summary
    }
}

/// Set once, by [`Entry::read_boot_info`].
static BOOT_DATA: SetOnce<BootData> = SetOnce::new();

/// The boot data, once it has been read.
pub fn boot_data() -> Option<&'static BootData> {
    BOOT_DATA.get()
}

/// What the boot loader handed the kernel at its entry point: its pointer to the boot info.
///
/// `_start` passes the loader's RDI straight to the kernel's entry function, which takes an
/// `Entry` by value under the `sysv64` ABI: hence `repr(transparent)`, and hence the one
/// `Entry` of a boot. Nothing else can make one, because the field is private.
#[derive(Debug)]
#[repr(transparent)]
pub struct Entry {
    boot_info: *const BootInfo,
}

impl Entry {
    /// Whether the loader applied the image's relocations.
    ///
    /// The compiler knows the link-time value of [`RELOCATION_PROBE`], so the load must be
    /// volatile to observe what is really in memory. The address of [`RELOCATION_TARGET`] is
    /// computed RIP-relative and is correct wherever the image was loaded.
    pub fn relocations_applied(&self) -> bool {
        let probe = (&raw const RELOCATION_PROBE).cast::<*const u8>();
        // SAFETY: `probe` points to a live, aligned static of pointer size. It is read as a
        // raw pointer, which is valid for any bit pattern, so an unrelocated value is never
        // treated as a reference.
        let stored = unsafe { ptr::read_volatile(probe) };
        ptr::eq(stored, RELOCATION_TARGET.as_ptr())
    }

    /// Copies and validates the boot info, checks its kernel segment list and summarizes its
    /// region array; then the boot steps can be taken.
    pub fn read_boot_info(self) -> Result<Early, InvalidBootInfo> {
        boot::check_address(self.boot_info.addr())?;
        // SAFETY: `boot_info` is non-null and aligned (checked above) and readable (the entry
        // contract in the module documentation: only `_start` makes an `Entry`). `BootInfo` has
        // only integer fields, so every bit pattern is a valid value. The header is copied, so
        // no reference into loader memory is kept.
        let info = unsafe { self.boot_info.read() };
        let array = boot::validate(&info)?;
        // Boot info 0.3: the arrays are given by physical address; read them through the direct
        // map the loader built.
        phys::init(&info);
        let first =
            ptr::with_exposed_provenance::<MemoryRegion>(phys::to_virt(array.addr as u64) as usize);
        // SAFETY: `validate` checked that the array is non-null, aligned, at most `isize::MAX`
        // bytes long and does not wrap. The entry contract makes it readable and unchanging
        // while the slice lives (the loader gives the kernel that memory, kind `BootInfo`, for
        // good). `MemoryRegion` has only integer fields, so every bit pattern is
        // a valid value.
        let regions = unsafe { slice::from_raw_parts(first, array.len) };
        let summary = boot::summarize(regions)?;
        let segment_array = boot::segment_array(&info)?;
        let first = ptr::with_exposed_provenance::<KernelSegment>(phys::to_virt(
            segment_array.addr as u64,
        ) as usize);
        // SAFETY: as for the regions: `segment_array` checked the layout requirements, the
        // entry contract makes the memory readable and unchanging, and `KernelSegment` has only
        // integer fields.
        let segments = unsafe { slice::from_raw_parts(first, segment_array.len) };
        boot::check_kernel_segments(segments, &info)?;
        // `Entry` is made once, so this is the first and only `set`.
        let _ = BOOT_DATA.set(BootData {
            info,
            regions,
            segments,
            summary,
        });
        let data = BOOT_DATA.get().ok_or(InvalidBootInfo::Null)?;
        Ok(Early {
            data,
            cpu_tables: false,
            xcr0: None,
            memory: None,
            memory_taken: false,
            frames_proven: false,
            tables_built: false,
            paged: false,
            heap: false,
            user_isolation: false,
            reclaimed: false,
            boot_processor: PhantomData,
        })
    }
}

/// What the frame allocator and the page-table pool start as, before the kernel's tables are
/// live and the heap takes the allocator.
struct BootMemory {
    frames: FrameAllocator<'static>,
    pool: &'static [Table],
    pool_base: u64,
}

/// The kernel's page tables, built and not loaded yet.
pub struct KernelTables {
    space: AddressSpace,
    tables: TablePool<'static>,
    stats: layout::Stats,
}

impl core::fmt::Debug for KernelTables {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("KernelTables").finish_non_exhaustive()
    }
}

/// The boot processor's right to take the boot steps: see the module documentation.
pub struct Early {
    data: &'static BootData,
    cpu_tables: bool,
    xcr0: Option<u64>,
    memory: Option<BootMemory>,
    /// [`Early::init_memory`] has run (`memory` is `None` again once the heap has it).
    memory_taken: bool,
    /// [`Early::start_heap`]'s proof of the frame allocator passed.
    frames_proven: bool,
    tables_built: bool,
    /// The kernel's own tables are live and proven.
    paged: bool,
    heap: bool,
    user_isolation: bool,
    reclaimed: bool,
    /// Not `Send`: this stays on the boot processor.
    boot_processor: PhantomData<*const ()>,
}

impl core::fmt::Debug for Early {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Early").finish_non_exhaustive()
    }
}

impl Early {
    /// The validated boot data.
    pub const fn data(&self) -> &'static BootData {
        self.data
    }

    /// Loads the kernel's own GDT, TSS and IDT on the boot processor. Only the first call does
    /// anything.
    pub fn load_cpu_tables(&mut self) {
        if self.cpu_tables {
            return;
        }
        self.cpu_tables = true;
        // Off whatever came before, as `cpu::init` requires.
        cpu::disable_interrupts();
        // SAFETY: this is the only call (`Early` is one, and the flag above), at CPL 0, on the
        // boot processor (`Early` stays there), interrupts disabled (just now), before
        // anything depends on the firmware's tables: every step that does comes after this one.
        unsafe { cpu::init() };
    }

    /// Enables SSE and AVX state on the boot processor, from what its CPUID reports; returns
    /// XCR0. Only the first call does anything.
    pub fn enable_simd(&mut self) -> u64 {
        if let Some(xcr0) = self.xcr0 {
            return xcr0;
        }
        let cpuid = cpu::gather_cpuid();
        // SAFETY: CPL 0, the boot processor, once (`xcr0` above). No code on it uses SSE or
        // XSAVE yet: the kernel is soft-float, and no thread has been started. `cpuid` is what
        // this processor reports: it is read here and passed to nobody else.
        let xcr0 = unsafe { cpu::enable_simd(&cpuid) };
        self.xcr0 = Some(xcr0);
        xcr0
    }

    /// Builds the frame allocator, with its bitmap and then the page-table pool at the start
    /// of the first usable region that can hold both. Returns whether it could.
    pub fn init_memory(&mut self, com1: &mut dyn Write) -> bool {
        if self.memory_taken {
            return false;
        }
        self.memory_taken = true;
        let regions = self.data.regions;
        let kernel_span = self.data.info.kernel_span;
        let words = frames::words_for(frames::covered_end(regions));
        let bitmap_pages = (words * 8).div_ceil(frames::FRAME);
        // As large as the memory map can need, so a machine the loader could map is one the
        // kernel can map too.
        let Some(table_frames) = layout::table_frames(regions, regions.len() as u64, kernel_span)
            .and_then(|needed| needed.checked_add(TABLE_POOL_RESERVE))
            .and_then(|needed| needed.checked_add(layout::KERNEL_HALF_SLOTS))
            .and_then(|needed| needed.checked_add(layout::STACK_REGION_TABLES))
            .map(|frames| frames.max(TABLE_POOL_MIN_FRAMES))
        else {
            let _ = writeln!(
                com1,
                "origin-kernel: the memory map needs more page tables than can be counted"
            );
            return false;
        };
        let pages = bitmap_pages + table_frames;
        let usable = RegionKind::Usable as u32;
        let Some(home) = regions
            .iter()
            .find(|r| r.kind == usable && r.base != 0 && r.pages >= pages)
        else {
            let _ = writeln!(
                com1,
                "origin-kernel: no usable region can hold the {pages}-page frame bitmap and table pool"
            );
            return false;
        };
        let bitmap_ptr = ptr::with_exposed_provenance_mut::<u64>(phys::to_virt(home.base) as usize);
        // SAFETY: `home` is a `Usable` region from the validated boot info, so nothing else
        // owns it; the loader's direct map is in force, so its direct-map address is writable
        // (this step runs once, before the kernel's own tables exist); it holds at least
        // `pages` pages, which cover the bitmap's `words` words and the pool after it.
        unsafe { bitmap_ptr.write_bytes(0, (pages * frames::FRAME / 8) as usize) };
        // SAFETY: as above; the words were just zeroed, and the bitmap frames are reserved
        // below, so the allocator never hands them out. The slice lives for the rest of the
        // kernel's run, and nothing else refers to these frames.
        let bitmap = unsafe { slice::from_raw_parts_mut(bitmap_ptr, words as usize) };
        let pool_base = home.base + bitmap_pages * frames::FRAME;
        // SAFETY: the pool's `table_frames` frames follow the bitmap inside `home`, were
        // zeroed above and are reserved below, so only the pool uses them. `Table` is 4 KiB of
        // `AtomicU64`, valid for any bits and aligned (the frames are 4 KiB aligned); accesses
        // through the shared slice are atomic, so the processor's own accessed/dirty updates
        // do not race with them.
        let pool = unsafe {
            slice::from_raw_parts(
                ptr::with_exposed_provenance::<Table>(phys::to_virt(pool_base) as usize),
                table_frames as usize,
            )
        };
        // The AP trampoline's code and data frames (M3) are never handed out.
        let trampoline = (crate::smp::TRAMPOLINE, 2);
        let frames = match FrameAllocator::new(bitmap, regions, &[(home.base, pages), trampoline]) {
            Ok(a) => a,
            Err(e) => {
                let _ = writeln!(com1, "origin-kernel: frame allocator: {e:?}");
                return false;
            }
        };
        let _ = writeln!(
            com1,
            "origin-kernel: frame allocator: {} free frames ({} MiB), bitmap of {} pages at {:#x}",
            frames.free_frames(),
            (frames.free_frames() * frames::FRAME) >> 20,
            bitmap_pages,
            home.base
        );
        debug_assert_eq!(PAGE, frames::FRAME);
        self.memory = Some(BootMemory {
            frames,
            pool,
            pool_base,
        });
        true
    }

    /// Builds the kernel's address space from the boot data, without loading it. `None` if the
    /// frame allocator has not been built, this has run already, the processor has no NX bit
    /// (W^X could not be enforced), or the tables cannot be built.
    pub fn build_kernel_tables(&mut self, com1: &mut dyn Write) -> Option<KernelTables> {
        if self.tables_built {
            return None;
        }
        let (pool, pool_base) = self.memory.as_ref().map(|m| (m.pool, m.pool_base))?;
        self.tables_built = true;
        if !cpu::nx_supported() {
            let _ = writeln!(
                com1,
                "origin-kernel: the processor has no NX bit; W^X cannot be enforced"
            );
            return None;
        }
        let mut tables = TablePool::new(pool, pool_base);
        let info = &self.data.info;
        let at = Placement {
            direct_map_base: info.direct_map_base,
            kernel_virtual_base: info.kernel_virtual_base,
            kernel_physical_base: info.kernel_base,
            stack_guard: info.stack_base.wrapping_sub(PAGE),
        };
        let built = layout::build(&mut tables, &at, self.data.regions, self.data.segments)
            // ADR-0004 H2: every slot of the kernel half gets its table now, while nothing can
            // have copied the half yet; then the kernel stacks' region gets every table its
            // pages will need (Amendment 1, A2), so mapping a stack never takes one. The pool
            // was sized for both (`init_memory`).
            .and_then(|(mut space, stats)| {
                space.reserve_upper_half(&mut tables)?;
                space.reserve_tables(
                    &mut tables,
                    layout::STACK_REGION,
                    layout::STACK_REGION_SIZE,
                )?;
                Ok((space, stats))
            });
        match built {
            Ok((space, stats)) => Some(KernelTables {
                space,
                tables,
                stats,
            }),
            Err(e) => {
                let _ = writeln!(com1, "origin-kernel: page tables not built: {e:?}");
                None
            }
        }
    }

    /// Maps the firmware's framebuffer into `kernel` at the address the loader used, so the
    /// console keeps drawing through the switch (ADR-0010). Returns whether it could.
    pub fn map_framebuffer(&self, kernel: &mut KernelTables) -> bool {
        layout::framebuffer_mapping(&self.data.info.framebuffer)
            .is_some_and(|m| layout::map_framebuffer(&mut kernel.space, &mut kernel.tables, &m))
    }

    /// Loads `kernel` and proves its protections on the processor. Returns whether the kernel
    /// now runs on its own tables, proven.
    pub fn load_kernel_tables(&mut self, com1: &mut dyn Write, kernel: KernelTables) -> bool {
        let KernelTables {
            space,
            tables,
            stats,
        } = kernel;
        let info = &self.data.info;
        let segments = self.data.segments;
        cpu::disable_interrupts();
        // SAFETY: the boot processor, CPL 0, interrupts disabled (just now); NX is supported
        // (`build_kernel_tables`, the only way to a `KernelTables`, checked it). `layout::build`
        // mapped the kernel image segments, the stack this code runs on, the boot info and
        // region and segment arrays, the frame bitmap and this table pool (all in direct-mapped
        // regions), and the kernel's statics (the GDT, IDT, TSS and double-fault stack are in
        // the RW segment). COM1 is port I/O; a framebuffer mapping that failed only means the
        // console must not draw (`map_framebuffer`'s caller withdraws it).
        unsafe { cpu::load_page_tables(space.root()) };
        // SAFETY: CPL 0; the tables just loaded mark only the kernel half global (`layout`).
        // Toggling CR4.PGE also drops every translation cached under the loader's tables.
        let global = unsafe { tlb::enable_global_pages() };
        let used = tables.used();
        // SAFETY: once (`KernelTables` is made once and consumed here), with the tables
        // `load_page_tables` just loaded: they map everything running now, and from here on
        // every mapping goes through them.
        unsafe { vm::install(space, tables, self.data.regions) };
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
        self.paged = !identity && !alias && prove_protections(com1, info, segments);
        self.paged
    }

    /// Proves the frame allocator through the kernel's own direct map and gives it to the
    /// heap, which starts the `alloc` crate. Does nothing unless the kernel's own tables are
    /// live and proven. Returns whether the heap has started.
    pub fn start_heap(&mut self, com1: &mut dyn Write) -> bool {
        if self.heap || !self.paged {
            return false;
        }
        let Some(mut boot) = self.memory.take() else {
            return false;
        };
        self.frames_proven = prove_frames(com1, &mut boot.frames);
        memory::init(boot.frames);
        self.heap = true;
        true
    }

    /// Whether [`Early::start_heap`]'s proof of the frame allocator passed (ADR-0013).
    pub const fn frames_proven(&self) -> bool {
        self.frames_proven
    }

    /// Enables SMEP and SMAP on the boot processor and proves both, on a temporary user page
    /// holding `mov rax, -1; ret`, which a supervisor fetch (SMEP) and a supervisor read
    /// (SMAP) must both fault on. The page is removed afterwards. Runs once, once the heap has
    /// started; returns whether both protections hold.
    pub fn prove_user_isolation(&mut self, com1: &mut dyn Write) -> bool {
        if self.user_isolation || !self.heap {
            return false;
        }
        self.user_isolation = true;
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
        // SAFETY: the page is the proof's own fresh frame, about to be probed and unmapped:
        // nothing else ever uses it, and ring 3 runs no code in this address space yet.
        let mapped = unsafe { vm::map_user(USER_PROBE_PAGE, phys, Access::ReadExecute) };
        if mapped.is_err() {
            let _ = writeln!(com1, "origin-kernel: cannot map the user probe page");
            ok = false;
        } else {
            // SAFETY: the user page holds `PROBE_RETURN`.
            let fetch = unsafe { cpu::probe_execute(USER_PROBE_PAGE) };
            let read = cpu::probe_read(USER_PROBE_PAGE);
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
            // SAFETY: `USER_PROBE_PAGE` is the page this function mapped above, used by nothing
            // else, and nothing relies on it being mapped.
            if unsafe { vm::unmap(USER_PROBE_PAGE) }.is_err() {
                ok = false;
            }
        }
        // SAFETY: `frame` came from `alloc_zeroed(layout)` and is no longer mapped as a user
        // page.
        unsafe { dealloc(frame, layout) };
        ok
    }

    /// Returns the loader's and the firmware's boot-time memory to the frame allocator, if every
    /// processor the firmware enabled has left firmware code (`online`, which only
    /// `smp::start` makes). Returns the frames added and the free frames after, or `None` if
    /// it is not time, or it has been done.
    pub fn reclaim_loader_memory(&mut self, online: &Online) -> Option<(u64, u64)> {
        if self.reclaimed || !self.heap || !online.complete() {
            return None;
        }
        self.reclaimed = true;
        // SAFETY: once (the flag above). The kernel's own tables replaced the firmware's
        // (`heap`, which needs them live), and every enabled processor has left firmware code:
        // `online.complete` says so, and `Online` cannot be made outside `smp::start`.
        unsafe { memory::reclaim_loader_memory() }
    }
}

/// Deliberate accesses that the new tables must refuse: a write to kernel code, a write to
/// read-only kernel data, an instruction fetch from writable data and a read of page 0.
/// Each must raise #PF with the expected error code (SDM Vol. 3A §4.7: bit 0 present,
/// bit 1 write, bit 4 instruction fetch). Returns whether all four did.
fn prove_protections(com1: &mut dyn Write, info: &BootInfo, segments: &[KernelSegment]) -> bool {
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
        ("read of page 0", 0, 0x0, cpu::probe_read(0)),
        (
            "read of the boot stack's guard page",
            phys::to_virt(info.stack_base - PAGE),
            0x0,
            cpu::probe_read(phys::to_virt(info.stack_base - PAGE)),
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

/// Proves the allocator by allocating, writing, reading back and freeing three frames.
fn prove_frames(com1: &mut dyn Write, alloc: &mut FrameAllocator<'_>) -> bool {
    let mut got = [0u64; 3];
    for (i, slot) in got.iter_mut().enumerate() {
        let Some(addr) = alloc.allocate() else {
            let _ = writeln!(com1, "origin-kernel: frame allocator exhausted");
            return false;
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
            return false;
        }
        *slot = addr;
    }
    for &addr in &got {
        if alloc.free(addr).is_err() {
            let _ = writeln!(com1, "origin-kernel: frame {addr:#x} could not be freed");
            return false;
        }
    }
    let _ = writeln!(
        com1,
        "origin-kernel: allocated, wrote and freed 3 frames at {:#x}, {:#x}, {:#x}",
        got[0], got[1], got[2]
    );
    true
}
