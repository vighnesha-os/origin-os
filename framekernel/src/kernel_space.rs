//! The kernel's first address space (M2): what it maps, with which permissions.
//!
//! Until the kernel moves to its own higher-half layout (a later ADR), it keeps the
//! loader's identity mapping (virtual = physical), but with its own tables and much less
//! in them than the firmware mapped:
//! - **Direct map**, read/write and never executable: the RAM the kernel may touch, that is
//!   regions of kind `Usable`, `LoaderReclaimable`, `BootInfo`, `KernelStack` and
//!   `AcpiReclaimable`. Page 0 is never mapped, so a null access faults.
//! - **Kernel image**: each segment with exactly the access its verified ONBF segment table
//!   declares (boot info 0.2): read-only, read/execute or read/write, never writable and
//!   executable. The `KernelImage` region is not in the direct map, so no writable alias of
//!   kernel code exists. A segment that overlaps the direct map is an error, not an alias.
//! - **Nothing else**: firmware runtime memory, ACPI NVS, MMIO, reserved and unknown regions
//!   stay unmapped until a driver maps what it owns.

use origin_boot_info::{KernelSegment, MemoryRegion, RegionKind, SEGMENT_EXECUTE, SEGMENT_WRITE};

use crate::paging::{Access, AddressSpace, Attributes, MapError, PAGE, TableMemory};

/// What [`build`] mapped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Bytes of RAM in the direct map.
    pub direct_bytes: u64,
    /// 4 KiB leaves, direct map and image together.
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

/// The access a kernel segment gets. `check_kernel_segments` has already rejected a segment
/// that is both writable and executable; were one to arrive, it would lose write access.
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
    kernel_base: u64,
    regions: &[MemoryRegion],
    segments: &[KernelSegment],
) -> Result<(AddressSpace, Stats), MapError> {
    let mut space = AddressSpace::new(mem)?;
    let mut stats = Stats::default();
    let rw = Attributes::kernel(Access::ReadWrite);
    for r in regions.iter().filter(|r| direct_mapped(r.kind)) {
        let (base, pages) = if r.base == 0 {
            (PAGE, r.pages.saturating_sub(1))
        } else {
            (r.base, r.pages)
        };
        let bytes = pages.checked_mul(PAGE).ok_or(MapError::Overflow)?;
        if bytes == 0 {
            continue;
        }
        let (small, large) = space.map_range(mem, base, base, bytes, rw)?;
        stats.direct_bytes += bytes;
        stats.small_pages += small;
        stats.large_pages += large;
    }
    for s in segments {
        let virt = kernel_base.checked_add(s.rva).ok_or(MapError::Overflow)?;
        let bytes = s.pages.checked_mul(PAGE).ok_or(MapError::Overflow)?;
        let attributes = Attributes::kernel(segment_access(s));
        let (small, large) = space.map_range(mem, virt, virt, bytes, attributes)?;
        stats.small_pages += small;
        stats.large_pages += large;
        stats.image_pages += s.pages;
    }
    Ok((space, stats))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paging::{LARGE_PAGE, Table, TablePool};
    use core::sync::atomic::AtomicU64;
    use origin_boot_info::SEGMENT_READ;

    const POOL_BASE: u64 = 0x80_0000;
    const KERNEL: u64 = 0x1E0B_0000;

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
            region(0x80_0000, 0x100, RegionKind::Usable), // the table pool lives here
            region(0x90_0000, 0x10, RegionKind::AcpiNvs),
            region(0x100_0000, 0x1_D0B0, RegionKind::LoaderReclaimable),
            region(KERNEL, 0xE, RegionKind::KernelImage),
            region(KERNEL + 0xE000, 0x10, RegionKind::KernelStack),
            region(KERNEL + 0x1E000, 0x2, RegionKind::BootInfo),
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

    fn pool(n: usize) -> Vec<Table> {
        (0..n)
            .map(|_| core::array::from_fn(|_| AtomicU64::new(0)))
            .collect()
    }

    #[test]
    fn image_segments_get_their_exact_access_and_nothing_is_writable_and_executable() {
        let tables = pool(64);
        let mut mem = TablePool::new(&tables, POOL_BASE);
        let (space, stats) = build(&mut mem, KERNEL, &regions(), &segments()).unwrap();
        assert_eq!(stats.image_pages, 14);
        let at = |v| space.translate(&mem, v).unwrap();
        let rodata = at(KERNEL + 0x2FFF);
        assert!(!rodata.writable && !rodata.executable);
        let text = at(KERNEL + 0x3000);
        assert!(!text.writable && text.executable);
        let data = at(KERNEL + 0xB123);
        assert!(data.writable && !data.executable);
        assert_eq!(data.phys, KERNEL + 0xB123, "identity mapping");
        // Every mapped page in the low 512 MiB obeys W^X.
        for page in (0..0x2000_0000).step_by(PAGE as usize) {
            if let Some(t) = space.translate(&mem, page) {
                assert!(!(t.writable && t.executable), "{page:#x}");
                assert!(!t.user, "{page:#x}");
            }
        }
    }

    #[test]
    fn the_direct_map_covers_only_ram_the_kernel_may_touch() {
        let tables = pool(64);
        let mut mem = TablePool::new(&tables, POOL_BASE);
        let (space, stats) = build(&mut mem, KERNEL, &regions(), &segments()).unwrap();
        let mapped = |v| space.translate(&mem, v).is_some();
        assert!(!mapped(0), "the null page");
        assert!(mapped(0x1000) && mapped(0x9_F000), "low usable RAM");
        assert!(!mapped(0xA_0000), "reserved");
        assert!(!mapped(0x90_0000), "ACPI NVS");
        assert!(!mapped(0x1F00_0000), "runtime code");
        assert!(!mapped(0xFEC0_0000), "MMIO");
        assert!(mapped(POOL_BASE), "the table pool, so tables stay editable");
        let stack = space.translate(&mem, KERNEL + 0xE000).unwrap();
        assert!(stack.writable && !stack.executable);
        assert!(mapped(KERNEL + 0x1E000), "boot info");
        assert!(!mapped(KERNEL + 0x2_0000), "nothing after the boot info");
        let loader = space.translate(&mem, 0x100_0000).unwrap();
        assert_eq!(loader.size, LARGE_PAGE, "2 MiB pages where aligned");
        let expected = (0x9F + 0x700 + 0x100 + 0x1_D0B0 + 0x10 + 0x2) * PAGE;
        assert_eq!(stats.direct_bytes, expected);
        assert!(stats.large_pages > 200);
    }

    #[test]
    fn a_segment_overlapping_the_direct_map_is_an_error_not_an_alias() {
        let tables = pool(64);
        let mut mem = TablePool::new(&tables, POOL_BASE);
        // A kernel that claims to live inside direct-mapped RAM: in 4 KiB pages, and inside
        // a 2 MiB page.
        let err = build(&mut mem, 0x1000, &regions(), &segments()).unwrap_err();
        assert_eq!(err, MapError::AlreadyMapped(0x1000));
        let tables = pool(64);
        let mut mem = TablePool::new(&tables, POOL_BASE);
        let err = build(&mut mem, 0x20_0000, &regions(), &segments()).unwrap_err();
        assert_eq!(err, MapError::LargePage(0x20_0000));
        // A W+X segment (rejected earlier by the boot-info check) never becomes writable.
        let wx = segment(0, 1, SEGMENT_READ | SEGMENT_WRITE | SEGMENT_EXECUTE);
        assert_eq!(segment_access(&wx), Access::ReadExecute);
    }

    #[test]
    fn running_out_of_tables_is_reported() {
        let tables = pool(3);
        let mut mem = TablePool::new(&tables, POOL_BASE);
        assert_eq!(
            build(&mut mem, KERNEL, &regions(), &segments()).unwrap_err(),
            MapError::OutOfMemory
        );
    }
}
