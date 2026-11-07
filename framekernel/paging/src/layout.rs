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
//! - **Nothing else**: firmware runtime memory, ACPI NVS, MMIO, reserved and unknown
//!   regions stay unmapped until a driver maps what it owns.

use origin_boot_info::{KernelSegment, MemoryRegion, RegionKind, SEGMENT_EXECUTE, SEGMENT_WRITE};

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

/// Builds the address space described in the module documentation.
pub fn build(
    mem: &mut impl TableMemory,
    at: &Placement,
    regions: &[MemoryRegion],
    segments: &[KernelSegment],
) -> Result<(AddressSpace, Stats), MapError> {
    let mut space = AddressSpace::new(mem)?;
    let mut stats = Stats::default();
    let rw = Attributes::kernel(Access::ReadWrite);
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
        let attributes = Attributes::kernel(segment_access(s));
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
