//! The kernel's virtual address-space layout (ADR-0004, accepted 2026-11-07) and the
//! builder that both the loader (the first tables, H4) and the kernel (its own tables) use.
//!
//! - **User half** `[USER_START, USER_END)`: nothing of the kernel's (H1).
//! - **Direct map** at `direct_map_base + physical`, read/write and never executable: the
//!   RAM the kernel may touch, that is regions of kind `Usable`, `LoaderReclaimable`,
//!   `BootInfo`, `KernelStack` and `AcpiReclaimable`. Physical page 0 and the guard page
//!   below the boot stack (the first page of its `KernelStack` region) are left out.
//! - **Kernel image** at `kernel_virtual_base + rva`: each segment with exactly the access
//!   its verified ONBF segment table declares, never writable and executable. The
//!   `KernelImage` region is not in the direct map, so no writable alias of kernel code
//!   exists.
//! - **The firmware framebuffer** (ADR-0010), when there is one: uncached, at the
//!   start of the MMIO window ([`FRAMEBUFFER_WINDOW`]), so a console can draw from the
//!   kernel's first instruction and across the switch to its own tables.
//! - **Nothing else**: firmware runtime memory, ACPI NVS, other MMIO, reserved and unknown
//!   regions stay unmapped until a driver maps what it owns.

use origin_boot_info::{
    Framebuffer, KernelSegment, MAX_FRAMEBUFFER_BYTES, MemoryRegion, RegionKind, SEGMENT_EXECUTE,
    SEGMENT_WRITE,
};

use crate::{Access, AddressSpace, Attributes, LARGE_PAGE, MapError, PAGE, TableMemory};

/// First user address (page 0 stays unmapped).
pub const USER_START: u64 = 0x1000;
/// End of the user half (47 bits, H1); the top page below it stays unmapped too.
pub const USER_END: u64 = 0x0000_8000_0000_0000;
/// Start of the direct-map window (H2), 64 TiB.
pub const DIRECT_MAP_WINDOW: u64 = 0xFFFF_8000_0000_0000;
/// Size of the direct-map window.
pub const DIRECT_MAP_WINDOW_SIZE: u64 = 0x0000_4000_0000_0000;
/// Granularity of the direct-map slide (H3).
pub const DIRECT_MAP_ALIGN: u64 = 1 << 30;
/// Start of the kernel-image window (H2): the top 2 GiB.
pub const KERNEL_WINDOW: u64 = 0xFFFF_FFFF_8000_0000;
/// Size of the kernel-image window.
pub const KERNEL_WINDOW_SIZE: u64 = 0x8000_0000;
/// Granularity of the kernel-image slide (H3).
pub const KERNEL_ALIGN: u64 = LARGE_PAGE;
/// Start of the window the firmware framebuffer is mapped in: the first 2 GiB of the MMIO
/// window (H2).
pub const FRAMEBUFFER_WINDOW: u64 = 0xFFFF_E000_0000_0000;
/// Size of the framebuffer window. Device registers are mapped above it.
pub const FRAMEBUFFER_WINDOW_SIZE: u64 = 2 << 30;
/// Tables the kernel half's top level can take (ADR-0004 H2): one for each of PML4 entries
/// 256–511. [`AddressSpace::reserve_upper_half`] makes the ones [`build`] did not.
pub const KERNEL_HALF_SLOTS: u64 = (crate::ENTRIES / 2) as u64;
/// Bits of the address a page directory covers (1 GiB).
const GIB_SHIFT: u32 = 30;
/// Bits of the address a PDPT covers (512 GiB).
const PDPT_SHIFT: u32 = 39;
const _: () = assert!(
    FRAMEBUFFER_WINDOW_SIZE >= MAX_FRAMEBUFFER_BYTES + 2 * PAGE + LARGE_PAGE,
    "the window must hold the largest framebuffer at any offset within a large page"
);

/// Where a framebuffer is mapped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FramebufferMapping {
    /// Physical address of the first page mapped.
    pub phys: u64,
    /// Virtual address of the first page mapped.
    pub virt: u64,
    /// Bytes mapped: a whole number of pages.
    pub bytes: u64,
    /// Virtual address of the top-left pixel.
    pub first_pixel: u64,
}

/// Where `framebuffer` belongs in the window, or `None` if it is unusable or too large for
/// it. The virtual address keeps the physical address's offset within 2 MiB, so an aligned
/// framebuffer gets 2 MiB leaves.
pub fn framebuffer_mapping(framebuffer: &Framebuffer) -> Option<FramebufferMapping> {
    framebuffer.check().ok()?;
    let phys = framebuffer.base & !(PAGE - 1);
    let bytes =
        (framebuffer.base + framebuffer.visible_bytes() - phys).checked_next_multiple_of(PAGE)?;
    let virt = FRAMEBUFFER_WINDOW + phys % LARGE_PAGE;
    if virt + bytes > FRAMEBUFFER_WINDOW + FRAMEBUFFER_WINDOW_SIZE {
        return None;
    }
    Some(FramebufferMapping {
        phys,
        virt,
        bytes,
        first_pixel: virt + (framebuffer.base - phys),
    })
}

/// Where the loader put things.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    /// Virtual address of physical 0.
    pub direct_map_base: u64,
    /// Virtual address of the kernel image's RVA 0.
    pub kernel_virtual_base: u64,
    /// Physical address of the kernel image's RVA 0.
    pub kernel_physical_base: u64,
    /// Physical address of the boot stack's guard page (0: none).
    pub stack_guard: u64,
}

/// The placements KASLR chooses from: `random` picks the direct-map slot and the kernel
/// slot (H3). `max_physical` is the end of the highest RAM to direct-map; `kernel_span` the
/// image span. Returns `None` if either window is too small.
pub fn place(random: u64, max_physical: u64, kernel_span: u64) -> Option<(u64, u64)> {
    let direct_slots = DIRECT_MAP_WINDOW_SIZE
        .checked_sub(max_physical.checked_next_multiple_of(DIRECT_MAP_ALIGN)?)?
        / DIRECT_MAP_ALIGN
        + 1;
    let kernel_slots = KERNEL_WINDOW_SIZE
        .checked_sub(kernel_span.checked_next_multiple_of(KERNEL_ALIGN)?)?
        / KERNEL_ALIGN;
    if kernel_slots == 0 {
        return None;
    }
    let direct = DIRECT_MAP_WINDOW + (random % direct_slots) * DIRECT_MAP_ALIGN;
    let kernel = KERNEL_WINDOW + ((random >> 32) % kernel_slots) * KERNEL_ALIGN;
    Some((direct, kernel))
}

/// An upper bound on the page tables that mapping these `regions` and a kernel image of
/// `kernel_span` bytes can take: [`build`], then [`map_framebuffer`], then the two identity
/// pages of the boot loader's entry stub.
///
/// The bound follows what [`AddressSpace::map_range`] does with 4 KiB and 2 MiB leaves:
/// - **the direct map:** a PDPT per 512 GiB up to the highest RAM, plus two for a window
///   slid by only 1 GiB; a page directory per GiB each region touches; and a page table for each
///   end of a region that is not on a 2 MiB boundary, so two per region, however large;
/// - **the image:** one PDPT, the directories of its span plus one (it may straddle a GiB),
///   and a page table per 2 MiB of it, plus one: its physical base is only page-aligned, so
///   every page of it may be a 4 KiB leaf;
/// - **the framebuffer:** one PDPT, two directories (at most 1 GiB, at any offset) and two
///   page tables for its ends, since its virtual address keeps the physical offset within 2 MiB;
/// - **the entry stub:** two adjacent pages, which in the worst case share nothing.
///
/// `regions` may come from an earlier map than the one the tables are built from: every later
/// allocation turns direct-mapped RAM into RAM of another kind, so the GiBs touched can only
/// shrink, but regions split, so their ends are counted from `max_regions`, the most regions
/// the later map can have. `None` if the bound does not fit in a `u64`.
pub fn table_frames(regions: &[MemoryRegion], max_regions: u64, kernel_span: u64) -> Option<u64> {
    let mut directories = 0u64;
    let mut max_physical = 0u64;
    for r in regions
        .iter()
        .filter(|r| direct_mapped(r.kind) && r.pages > 0)
    {
        let end = r.base.checked_add(r.pages.checked_mul(PAGE)?)?;
        directories = directories
            .checked_add(((end - 1) >> GIB_SHIFT) - (r.base >> GIB_SHIFT))?
            .checked_add(1)?;
        max_physical = max_physical.max(end);
    }
    let direct = ((max_physical >> PDPT_SHIFT) + 2)
        .checked_add(directories)?
        .checked_add(max_regions.checked_mul(2)?)?;
    let image =
        1 + (kernel_span.div_ceil(1 << GIB_SHIFT) + 1) + (kernel_span.div_ceil(LARGE_PAGE) + 1);
    let (root, framebuffer, entry_stub) = (1, 1 + 2 + 2, 2 + 2 + 2);
    direct
        .checked_add(image)?
        .checked_add(root + framebuffer + entry_stub)
}

/// What [`build`] mapped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Bytes of RAM in the direct map.
    pub direct_bytes: u64,
    /// 4 KiB leaves.
    pub small_pages: u64,
    /// 2 MiB leaves.
    pub large_pages: u64,
    /// Kernel image pages mapped from the segment table.
    pub image_pages: u64,
}

/// Whether a region of this raw kind belongs in the direct map.
pub fn direct_mapped(kind: u32) -> bool {
    matches!(
        RegionKind::from_raw(kind),
        Some(
            RegionKind::Usable
                | RegionKind::LoaderReclaimable
                | RegionKind::BootInfo
                | RegionKind::KernelStack
                | RegionKind::AcpiReclaimable
        )
    )
}

/// The access a kernel segment gets. A segment that is both writable and executable (which
/// the boot-info check rejects earlier) would lose write access.
pub fn segment_access(segment: &KernelSegment) -> Access {
    if segment.access & SEGMENT_EXECUTE != 0 {
        Access::ReadExecute
    } else if segment.access & SEGMENT_WRITE != 0 {
        Access::ReadWrite
    } else {
        Access::Read
    }
}

/// Maps the firmware framebuffer, uncached, into `space`, after everything the boot needs has
/// its tables, so that running out of them here cannot cost anything else. Returns whether
/// the whole framebuffer is mapped; a framebuffer that cannot be mapped is not an error
/// (ADR-0010: a missing console never stops the boot).
pub fn map_framebuffer(
    space: &mut AddressSpace,
    mem: &mut impl TableMemory,
    framebuffer: &FramebufferMapping,
) -> bool {
    space
        .map_range(
            mem,
            framebuffer.virt,
            framebuffer.phys,
            framebuffer.bytes,
            Attributes::mmio(),
        )
        .is_ok()
}

/// Builds the address space described in the module documentation.
pub fn build(
    mem: &mut impl TableMemory,
    at: &Placement,
    regions: &[MemoryRegion],
    segments: &[KernelSegment],
) -> Result<(AddressSpace, Stats), MapError> {
    let mut space = AddressSpace::new(mem)?;
    let mut stats = Stats::default();
    // The whole kernel half is global (ADR-0004 H5): every address space shares it.
    let rw = Attributes::kernel_global(Access::ReadWrite);
    for r in regions.iter().filter(|r| direct_mapped(r.kind)) {
        let skip_first =
            r.base == 0 || (r.kind == RegionKind::KernelStack as u32 && r.base == at.stack_guard);
        let (base, pages) = if skip_first {
            (r.base + PAGE, r.pages.saturating_sub(1))
        } else {
            (r.base, r.pages)
        };
        let bytes = pages.checked_mul(PAGE).ok_or(MapError::Overflow)?;
        if bytes == 0 {
            continue;
        }
        let virt = at
            .direct_map_base
            .checked_add(base)
            .ok_or(MapError::Overflow)?;
        let (small, large) = space.map_range(mem, virt, base, bytes, rw)?;
        stats.direct_bytes += bytes;
        stats.small_pages += small;
        stats.large_pages += large;
    }
    for s in segments {
        let virt = at
            .kernel_virtual_base
            .checked_add(s.rva)
            .ok_or(MapError::Overflow)?;
        let phys = at
            .kernel_physical_base
            .checked_add(s.rva)
            .ok_or(MapError::Overflow)?;
        let bytes = s.pages.checked_mul(PAGE).ok_or(MapError::Overflow)?;
        let attributes = Attributes::kernel_global(segment_access(s));
        let (small, large) = space.map_range(mem, virt, phys, bytes, attributes)?;
        stats.small_pages += small;
        stats.large_pages += large;
        stats.image_pages += s.pages;
    }
    Ok((space, stats))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Table, TablePool};
    use core::sync::atomic::AtomicU64;
    use origin_boot_info::SEGMENT_READ;

    const POOL_BASE: u64 = 0x80_0000;
    const KERNEL: u64 = 0x1E0B_0000;
    const DIRECT: u64 = 0xFFFF_8040_0000_0000;
    const VIRT: u64 = 0xFFFF_FFFF_8460_0000;

    fn region(base: u64, pages: u64, kind: RegionKind) -> MemoryRegion {
        MemoryRegion {
            base,
            pages,
            kind: kind as u32,
            reserved: 0,
        }
    }

    fn segment(rva: u64, pages: u64, access: u32) -> KernelSegment {
        KernelSegment {
            rva,
            pages,
            access,
            reserved: 0,
        }
    }

    fn regions() -> Vec<MemoryRegion> {
        vec![
            region(0, 0xA0, RegionKind::Usable),
            region(0xA_0000, 0x60, RegionKind::Reserved),
            region(0x10_0000, 0x700, RegionKind::Usable),
            region(0x80_0000, 0x100, RegionKind::Usable),
            region(0x90_0000, 0x10, RegionKind::AcpiNvs),
            region(0x100_0000, 0x1_D0B0, RegionKind::LoaderReclaimable),
            region(KERNEL, 0xE, RegionKind::KernelImage),
            region(KERNEL + 0xE000, 0x11, RegionKind::KernelStack),
            region(KERNEL + 0x1F000, 0x2, RegionKind::BootInfo),
            region(0x1F00_0000, 0x100, RegionKind::RuntimeCode),
            region(0xFEC0_0000, 1, RegionKind::Mmio),
        ]
    }

    fn segments() -> [KernelSegment; 3] {
        [
            segment(0, 3, SEGMENT_READ),
            segment(0x3000, 8, SEGMENT_READ | SEGMENT_EXECUTE),
            segment(0xB000, 3, SEGMENT_READ | SEGMENT_WRITE),
        ]
    }

    fn at() -> Placement {
        Placement {
            direct_map_base: DIRECT,
            kernel_virtual_base: VIRT,
            kernel_physical_base: KERNEL,
            stack_guard: KERNEL + 0xE000,
        }
    }

    fn display(base: u64) -> Framebuffer {
        Framebuffer {
            base,
            size: 0x40_0000,
            virtual_base: 0,
            width: 1280,
            height: 800,
            stride: 5120,
            red_mask: 0x00FF_0000,
            green_mask: 0x0000_FF00,
            blue_mask: 0x0000_00FF,
        }
    }

    #[test]
    fn the_framebuffer_maps_uncached_in_its_window_and_keeps_its_offset() {
        // 2 MiB aligned: large leaves; the first pixel is at the start of the window.
        let mapping = framebuffer_mapping(&display(0x8000_0000)).unwrap();
        assert_eq!(mapping.phys, 0x8000_0000);
        assert_eq!(mapping.virt, FRAMEBUFFER_WINDOW);
        assert_eq!(mapping.first_pixel, FRAMEBUFFER_WINDOW);
        assert_eq!(mapping.bytes, (800 * 5120 + 0xFFF) & !0xFFF);

        let tables = pool(64);
        let mut mem = TablePool::new(&tables, POOL_BASE);
        let (mut space, _) = build(&mut mem, &at(), &regions(), &segments()).unwrap();
        assert!(map_framebuffer(&mut space, &mut mem, &mapping));
        let first = space.translate(&mem, mapping.first_pixel).unwrap();
        assert_eq!(first.phys, 0x8000_0000);
        assert!(first.writable && !first.executable && first.global && first.uncached);
        assert_eq!(first.size, LARGE_PAGE);
        let last = space
            .translate(&mem, mapping.first_pixel + 800 * 5120 - 4)
            .unwrap();
        assert_eq!(last.phys, 0x8000_0000 + 800 * 5120 - 4);
        assert_eq!(
            space.translate(&mem, mapping.virt + mapping.bytes),
            None,
            "nothing past it is mapped"
        );

        // Not page aligned: the offset within the page survives, and so does the alignment
        // within 2 MiB, so a large page can be used where the physical address allows one.
        let mapping = framebuffer_mapping(&display(0x8000_1800)).unwrap();
        assert_eq!(mapping.phys, 0x8000_1000);
        assert_eq!(mapping.virt, FRAMEBUFFER_WINDOW + 0x1000);
        assert_eq!(mapping.first_pixel, FRAMEBUFFER_WINDOW + 0x1800);
        assert_eq!(mapping.bytes % PAGE, 0);
        assert!(mapping.bytes >= 800 * 5120 + 0x800);
    }

    #[test]
    fn a_framebuffer_that_cannot_be_mapped_is_not_a_failed_boot() {
        // A table pool with nothing left once the kernel is mapped: the framebuffer does not
        // fit, and the rest of the address space is intact.
        let probe_tables = pool(64);
        let mut probe = TablePool::new(&probe_tables, POOL_BASE);
        let (_, _) = build(&mut probe, &at(), &regions(), &segments()).unwrap();
        let used = probe.used();

        let tables = pool(used);
        let mut mem = TablePool::new(&tables, POOL_BASE);
        let (mut space, _) = build(&mut mem, &at(), &regions(), &segments()).unwrap();
        let mapping = framebuffer_mapping(&display(0x8000_0000)).unwrap();
        assert!(!map_framebuffer(&mut space, &mut mem, &mapping));
        assert!(
            space.translate(&mem, DIRECT + 0x1000).is_some(),
            "the direct map is intact"
        );
    }

    #[test]
    fn an_unusable_framebuffer_has_no_mapping() {
        assert_eq!(framebuffer_mapping(&Framebuffer::NONE), None);
        let mut bad = display(0x8000_0000);
        bad.stride = 3;
        assert_eq!(framebuffer_mapping(&bad), None);
        // The largest accepted framebuffer still fits at any offset within a large page.
        let mut largest = display(0x8000_0000 + LARGE_PAGE - 4);
        largest.width = 16384;
        largest.height = 16384;
        largest.stride = 65536;
        largest.size = u64::MAX;
        let mapping = framebuffer_mapping(&largest).unwrap();
        assert_eq!(mapping.virt % LARGE_PAGE, mapping.phys % LARGE_PAGE);
        assert!(mapping.virt + mapping.bytes <= FRAMEBUFFER_WINDOW + FRAMEBUFFER_WINDOW_SIZE);
    }

    fn pool(n: usize) -> Vec<Table> {
        (0..n)
            .map(|_| core::array::from_fn(|_| AtomicU64::new(0)))
            .collect()
    }

    #[test]
    fn the_image_is_high_with_exact_access_and_nothing_is_writable_and_executable() {
        let tables = pool(64);
        let mut mem = TablePool::new(&tables, POOL_BASE);
        let (space, stats) = build(&mut mem, &at(), &regions(), &segments()).unwrap();
        assert_eq!(stats.image_pages, 14);
        let t = |v| space.translate(&mem, v).unwrap();
        let rodata = t(VIRT + 0x2FFF);
        assert!(!rodata.writable && !rodata.executable);
        assert_eq!(rodata.phys, KERNEL + 0x2FFF);
        let text = t(VIRT + 0x3000);
        assert!(!text.writable && text.executable);
        let data = t(VIRT + 0xB123);
        assert!(data.writable && !data.executable);
        // The kernel's physical pages have no alias in the direct map.
        assert_eq!(space.translate(&mem, DIRECT + KERNEL + 0x3000), None);
        // Nothing at all in the lower half.
        for page in (0..0x2000_0000).step_by(PAGE as usize) {
            assert_eq!(space.translate(&mem, page), None, "{page:#x}");
        }
        // Every leaf is in the kernel half and global (ADR-0004 H5), both large and small.
        assert!(text.global && data.global && t(DIRECT + 0x100_0000).global);
        let census = space.leaf_census(&mem);
        assert_eq!(
            census.upper_local + census.lower_global + census.lower_local,
            0
        );
        assert_eq!(census.upper_global, stats.small_pages + stats.large_pages);
    }

    #[test]
    fn the_direct_map_covers_only_ram_the_kernel_may_touch() {
        let tables = pool(64);
        let mut mem = TablePool::new(&tables, POOL_BASE);
        let (space, stats) = build(&mut mem, &at(), &regions(), &segments()).unwrap();
        let mapped = |phys: u64| space.translate(&mem, DIRECT + phys).is_some();
        assert!(!mapped(0), "physical page 0");
        assert!(mapped(0x1000) && mapped(0x9_F000));
        assert!(!mapped(0xA_0000) && !mapped(0x90_0000) && !mapped(0x1F00_0000));
        assert!(!mapped(0xFEC0_0000), "MMIO");
        assert!(!mapped(KERNEL + 0xE000), "the boot stack's guard page");
        let stack = space.translate(&mem, DIRECT + KERNEL + 0xF000).unwrap();
        assert!(stack.writable && !stack.executable);
        assert_eq!(stack.phys, KERNEL + 0xF000);
        assert!(mapped(KERNEL + 0x1F000), "boot info");
        let big = space.translate(&mem, DIRECT + 0x100_0000).unwrap();
        assert_eq!(big.size, LARGE_PAGE, "2 MiB pages where aligned");
        assert_eq!(
            stats.direct_bytes,
            (0x9F + 0x700 + 0x100 + 0x1_D0B0 + 0x10 + 0x2) * PAGE
        );
    }

    #[test]
    fn kaslr_placements_stay_inside_their_windows_and_aligned() {
        let ram = 64 << 30;
        let span = 0x10_0000;
        let mut seen = std::collections::HashSet::new();
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        for _ in 0..10_000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let (direct, kernel) = place(state, ram, span).unwrap();
            assert!(direct.is_multiple_of(DIRECT_MAP_ALIGN));
            assert!(direct >= DIRECT_MAP_WINDOW);
            assert!(direct + ram <= DIRECT_MAP_WINDOW + DIRECT_MAP_WINDOW_SIZE);
            assert!(kernel.is_multiple_of(KERNEL_ALIGN));
            assert!(kernel >= KERNEL_WINDOW);
            assert!(kernel.checked_add(span).is_some());
            seen.insert((direct, kernel));
        }
        assert!(seen.len() > 9_900, "the slides vary: {}", seen.len());
        assert_eq!(place(0, DIRECT_MAP_WINDOW_SIZE + 1, span), None);
        assert_eq!(place(0, ram, KERNEL_WINDOW_SIZE), None);
        assert_eq!(
            place(0, ram, span),
            Some((DIRECT_MAP_WINDOW, KERNEL_WINDOW))
        );
    }

    /// Everything [`table_frames`] bounds, built into a pool of exactly that many tables. Returns
    /// the tables used, and whether the framebuffer stayed.
    fn build_within_the_bound(
        tables: &[Table],
        regions: &[MemoryRegion],
        direct_base: u64,
        kernel_physical_base: u64,
        span: u64,
    ) -> (usize, bool) {
        let bound = table_frames(regions, regions.len() as u64, span).unwrap() as usize;
        assert!(bound <= tables.len(), "the test's pool is too small");
        let mut mem = TablePool::new(&tables[..bound], POOL_BASE);
        let at = Placement {
            direct_map_base: direct_base,
            kernel_virtual_base: KERNEL_WINDOW + LARGE_PAGE,
            kernel_physical_base,
            stack_guard: 0,
        };
        let pages = span / PAGE;
        let segments = [
            segment(0, pages / 2, SEGMENT_READ),
            segment(
                (pages / 2) * PAGE,
                pages - pages / 2,
                SEGMENT_READ | SEGMENT_WRITE,
            ),
        ];
        let (mut space, _) = build(&mut mem, &at, regions, &segments)
            .unwrap_or_else(|e| panic!("{e:?}: {regions:x?}"));
        let framebuffer = framebuffer_mapping(&display(0x8000_0000)).unwrap();
        let kept = map_framebuffer(&mut space, &mut mem, &framebuffer);
        // The entry stub, in the worst place: its two pages straddle a 512 GiB boundary.
        let stub = (1 << 39) - PAGE;
        let rx = Attributes::kernel(Access::ReadExecute);
        space.map(&mut mem, stub, stub, rx).unwrap();
        space.map(&mut mem, stub + PAGE, stub + PAGE, rx).unwrap();
        (mem.used(), kept)
    }

    #[test]
    fn the_table_bound_holds_for_large_and_for_ragged_memory_maps() {
        let tables = pool(6000);
        let span = 512 * PAGE;
        let direct = DIRECT_MAP_WINDOW + (17 << 30);

        // One region of 1 TiB, as a server's firmware reports it above 4 GiB: 1,024 page
        // directories, far beyond the fixed 256 frames this replaced.
        let big = [region(4 << 30, (1 << 40) / PAGE, RegionKind::Usable)];
        assert!(table_frames(&big, 1, span).unwrap() > 1024);
        let (used, kept) = build_within_the_bound(&tables, &big, direct, 0x1E0B_0000, span);
        assert!(kept, "the framebuffer survives");
        assert!(used > 1024);

        // 300 GiB cut into many regions, each with both ends off a 2 MiB boundary, some of them
        // not direct-mapped at all.
        let mut ragged = Vec::new();
        let mut base = 0x10_0000u64;
        for i in 0..300 {
            let kind = if i % 7 == 3 {
                RegionKind::Reserved
            } else {
                RegionKind::LoaderReclaimable
            };
            let pages = (1 << 18) + 5 * (i % 11);
            ragged.push(region(base, pages, kind));
            base += (pages + 3) * PAGE;
        }
        let (used, kept) = build_within_the_bound(&tables, &ragged, direct, 0x1234_5000, span);
        assert!(kept);
        assert!(used > 256, "{used}: this map did not fit in the fixed pool");

        // Random maps: unaligned bases and sizes, large and tiny regions, holes of any size.
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for case in 0..400 {
            let mut regions = Vec::new();
            let mut base = (next() % (1 << 21)) & !(PAGE - 1);
            for _ in 0..1 + next() % 64 {
                let pages = 1 + next() % (1 << (next() % 22));
                let kind = match next() % 4 {
                    0 => RegionKind::Usable,
                    1 => RegionKind::LoaderReclaimable,
                    2 => RegionKind::AcpiReclaimable,
                    _ => RegionKind::Reserved,
                };
                regions.push(region(base, pages, kind));
                base += (pages + next() % (1 << (next() % 20))) * PAGE;
            }
            let slide = (next() % (1 << 11)) << 30;
            let physical_base = (next() % (1 << 30)) & !(PAGE - 1);
            let span = (1 + next() % (1 << 15)) * PAGE;
            let (used, kept) = build_within_the_bound(
                &tables,
                &regions,
                DIRECT_MAP_WINDOW + slide,
                physical_base,
                span,
            );
            assert!(kept, "case {case}");
            let bound = table_frames(&regions, regions.len() as u64, span).unwrap() as usize;
            assert!(used <= bound, "case {case}: {used} of {bound}");
        }
    }

    #[test]
    fn the_table_bound_counts_later_splits_and_ignores_what_is_not_direct_mapped() {
        let usable = [region(0x20_0000, 512, RegionKind::Usable)];
        // One table per extra region end, for regions that exist only in the later map.
        assert_eq!(
            table_frames(&usable, 10, PAGE).unwrap() - table_frames(&usable, 1, PAGE).unwrap(),
            2 * 9
        );
        // Memory that is never direct-mapped costs nothing, however much of it there is.
        let mmio = [usable[0], region(1 << 42, 1 << 30, RegionKind::Mmio)];
        assert_eq!(
            table_frames(&mmio, 2, PAGE).unwrap(),
            table_frames(&usable, 2, PAGE).unwrap()
        );
        // A bound that does not fit is reported, not wrapped.
        let absurd = [region(!(PAGE - 1), 2, RegionKind::Usable)];
        assert_eq!(table_frames(&absurd, 1, PAGE), None);
    }

    #[test]
    fn a_segment_over_the_direct_map_is_an_error_not_an_alias() {
        let tables = pool(64);
        let mut mem = TablePool::new(&tables, POOL_BASE);
        // A kernel placed where the direct map already is.
        let bad = Placement {
            kernel_virtual_base: DIRECT + 0x1000,
            ..at()
        };
        assert_eq!(
            build(&mut mem, &bad, &regions(), &segments()).unwrap_err(),
            MapError::AlreadyMapped(DIRECT + 0x1000)
        );
        let wx = segment(0, 1, SEGMENT_READ | SEGMENT_WRITE | SEGMENT_EXECUTE);
        assert_eq!(segment_access(&wx), Access::ReadExecute);
    }
}
