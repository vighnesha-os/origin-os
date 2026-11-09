//! Validation of the boot info the loader passes, and the memory summary the kernel
//! reports (boot-info v0.2, Proposed).
//!
//! The kernel copies the [`BootInfo`] header out of loader memory, then validates it here
//! before it reads anything else the header points to. Every check is total: no function in
//! this module can panic, whatever the loader wrote.

use core::mem::{align_of, size_of};

use origin_boot_info::{BootInfo, BootInfoError, KernelSegment, MemoryRegion, RegionKind};

/// Size of the pages [`MemoryRegion::pages`] counts.
pub const PAGE_SIZE: u64 = 4096;
/// 4 KiB pages per MiB.
const PAGES_PER_MIB: u64 = (1 << 20) / PAGE_SIZE;

/// Why the kernel rejected the boot info it was given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvalidBootInfo {
    /// The boot-info pointer is null.
    Null,
    /// The boot-info pointer is not aligned for [`BootInfo`].
    Misaligned,
    /// [`BootInfo::check`] rejected the header.
    Header(BootInfoError),
    /// `regions` is null or misaligned, or the array would wrap the address space.
    RegionArray,
    /// The region at this index has a base that is not 4 KiB aligned, or its end does not
    /// fit in 64 bits.
    RegionBounds(usize),
    /// The region at this index starts before the previous one ends: the array is not
    /// sorted by `base`, or two regions overlap.
    RegionOrder(usize),
    /// `kernel_segments` is null or misaligned, or the array would wrap the address space.
    SegmentArray,
    /// `kernel_base` is not 4 KiB aligned, or `kernel_base + kernel_span` overflows.
    KernelImage,
}

impl From<BootInfoError> for InvalidBootInfo {
    fn from(err: BootInfoError) -> Self {
        Self::Header(err)
    }
}

/// Checks the address the loader passed in `rdi` before the kernel reads through it.
pub fn check_address(addr: usize) -> Result<(), InvalidBootInfo> {
    if addr == 0 {
        Err(InvalidBootInfo::Null)
    } else if !addr.is_multiple_of(align_of::<BootInfo>()) {
        Err(InvalidBootInfo::Misaligned)
    } else {
        Ok(())
    }
}

/// Where the region array lives, once [`validate`] has accepted it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegionArray {
    /// Address of the first [`MemoryRegion`]: non-null and aligned.
    pub addr: usize,
    /// Number of entries, between 1 and [`origin_boot_info::MAX_REGIONS`].
    pub len: usize,
}

/// Validates a copied boot-info header and locates its region array.
///
/// On success, `len` entries starting at `addr` form an aligned, non-null array of at most
/// `isize::MAX` bytes that does not wrap the address space: the layout requirements of
/// `core::slice::from_raw_parts`. Whether that memory is readable is the loader's promise,
/// which no check can establish.
pub fn validate(info: &BootInfo) -> Result<RegionArray, InvalidBootInfo> {
    info.check()?;
    let image_end = info.kernel_base.checked_add(info.kernel_span);
    if !info.kernel_base.is_multiple_of(PAGE_SIZE) || image_end.is_none() {
        return Err(InvalidBootInfo::KernelImage);
    }
    // `check` bounds `region_count` by MAX_REGIONS, so the conversions and products in
    // `locate` cannot fail on a 64-bit target; they are checked anyway.
    let (addr, len) = locate::<MemoryRegion>(info.regions, info.region_count)
        .ok_or(InvalidBootInfo::RegionArray)?;
    Ok(RegionArray { addr, len })
}

/// Where the kernel segment array lives, once [`segment_array`] has accepted it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SegmentArray {
    /// Address of the first [`KernelSegment`]: non-null and aligned.
    pub addr: usize,
    /// Number of entries, between 1 and [`origin_boot_info::MAX_KERNEL_SEGMENTS`].
    pub len: usize,
}

/// Locates the kernel segment array of a header that [`validate`] accepted, with the same
/// layout guarantees as the region array.
pub fn segment_array(info: &BootInfo) -> Result<SegmentArray, InvalidBootInfo> {
    let (addr, len) = locate::<KernelSegment>(info.kernel_segments, info.kernel_segment_count)
        .ok_or(InvalidBootInfo::SegmentArray)?;
    Ok(SegmentArray { addr, len })
}

/// Checks the kernel segment list the loader passed (see
/// [`origin_boot_info::check_segments`]).
pub fn check_kernel_segments(
    segments: &[KernelSegment],
    info: &BootInfo,
) -> Result<(), InvalidBootInfo> {
    origin_boot_info::check_segments(segments, info.kernel_span).map_err(InvalidBootInfo::Header)
}

/// `(addr, len)` of `count` entries of `T` at `addr` if they form a non-null, aligned array
/// of at most `isize::MAX` bytes that does not wrap the address space.
fn locate<T>(addr: u64, count: u64) -> Option<(usize, usize)> {
    let len = usize::try_from(count).ok()?;
    let addr = usize::try_from(addr).ok()?;
    let bytes = len.checked_mul(size_of::<T>())?;
    let fits = bytes <= isize::MAX.unsigned_abs() && addr.checked_add(bytes).is_some();
    (addr != 0 && addr.is_multiple_of(align_of::<T>()) && fits).then_some((addr, len))
}

/// What the kernel learned from the region array.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemorySummary {
    /// Total 4 KiB pages in regions of kind [`RegionKind::Usable`].
    pub usable_pages: u64,
}

impl MemorySummary {
    /// Usable memory in whole MiB, rounded down.
    pub const fn usable_mib(&self) -> u64 {
        self.usable_pages / PAGES_PER_MIB
    }
}

/// Checks the region array against the boot-info contract (4 KiB-aligned bases, sorted by
/// `base`, no overlaps, no region past the end of the 64-bit address space) and counts the
/// usable pages.
///
/// A region kind this kernel does not know counts as [`RegionKind::Reserved`], as the
/// contract requires.
pub fn summarize(regions: &[MemoryRegion]) -> Result<MemorySummary, InvalidBootInfo> {
    let mut usable_pages: u64 = 0;
    let mut previous_end: u64 = 0;
    for (index, region) in regions.iter().enumerate() {
        let end = region_end(region).ok_or(InvalidBootInfo::RegionBounds(index))?;
        if region.base < previous_end {
            return Err(InvalidBootInfo::RegionOrder(index));
        }
        previous_end = end;
        if RegionKind::from_raw(region.kind) == Some(RegionKind::Usable) {
            // Cannot overflow once the regions are disjoint and inside the address space.
            usable_pages = usable_pages
                .checked_add(region.pages)
                .ok_or(InvalidBootInfo::RegionBounds(index))?;
        }
    }
    Ok(MemorySummary { usable_pages })
}

/// The exclusive end address of `region`, or `None` if its base is not page aligned or the
/// end does not fit in a `u64`.
fn region_end(region: &MemoryRegion) -> Option<u64> {
    if !region.base.is_multiple_of(PAGE_SIZE) {
        return None;
    }
    region
        .base
        .checked_add(region.pages.checked_mul(PAGE_SIZE)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use origin_boot_info::{MAGIC, MAX_REGIONS, VERSION};

    fn header() -> BootInfo {
        BootInfo {
            magic: MAGIC,
            version_major: VERSION.0,
            version_minor: VERSION.1,
            size: size_of::<BootInfo>() as u32,
            regions: 0x8000,
            region_count: 3,
            kernel_base: 0x20_0000,
            kernel_span: 0x3000,
            acpi_rsdp: 0,
            uefi_system_table: 0,
            stack_base: 0x40_0000,
            stack_size: 0x1_0000,
            kernel_segments: 0x9000,
            kernel_segment_count: 2,
            page_table_root: 0x9_0000,
            direct_map_base: 0xFFFF_8000_0000_0000,
            kernel_virtual_base: 0xFFFF_FFFF_8000_0000,
            flags: 1,
            framebuffer: origin_boot_info::Framebuffer::NONE,
        }
    }

    fn region(base: u64, pages: u64, kind: RegionKind) -> MemoryRegion {
        MemoryRegion {
            base,
            pages,
            kind: kind as u32,
            reserved: 0,
        }
    }

    #[test]
    fn address_must_be_non_null_and_aligned() {
        assert_eq!(check_address(0), Err(InvalidBootInfo::Null));
        for addr in [1, 4, 0x1004, usize::MAX] {
            assert_eq!(
                check_address(addr),
                Err(InvalidBootInfo::Misaligned),
                "{addr:#x}"
            );
        }
        for addr in [8, 0x1000, 0x7fff_fff8] {
            assert_eq!(check_address(addr), Ok(()), "{addr:#x}");
        }
    }

    /// Whatever the loader hands over, validation is total and what it accepts really
    /// has the layout it promises (CLAUDE.md §72: the boot info is one word from the loader
    /// and its region list comes straight from the firmware's memory map).
    #[test]
    fn hostile_boot_info_is_total_and_keeps_its_promises() {
        use crate::testing::Rng;
        let mut rng = Rng::new(0x424F_4F54);
        let (mut accepted, mut summed) = (0u32, 0u32);
        for case in 0..crate::testing::cases(30_000) {
            let mut info = header();
            // A few header words edited each round, the rest left valid.
            for _ in 0..1 + rng.below(4) {
                match rng.below(9) {
                    0 => info.regions = rng.edge_u64(),
                    1 => info.region_count = rng.edge_u64(),
                    2 => info.kernel_base = rng.edge_u64(),
                    3 => info.kernel_span = rng.edge_u64(),
                    4 => info.kernel_segments = rng.edge_u64(),
                    5 => info.kernel_segment_count = rng.edge_u64(),
                    6 => info.size = rng.edge_u32(),
                    7 => info.magic = rng.edge_u64(),
                    _ => {}
                }
            }
            let _ = check_address(rng.edge_u64() as usize);
            if let Ok(a) = validate(&info) {
                accepted += 1;
                assert!(a.addr != 0, "case {case}: {a:?}");
                assert!(
                    a.addr.is_multiple_of(align_of::<MemoryRegion>()),
                    "case {case}"
                );
                let bytes = a.len * size_of::<MemoryRegion>();
                assert!(bytes <= isize::MAX.unsigned_abs(), "case {case}");
                assert!(a.addr.checked_add(bytes).is_some(), "case {case}");
                assert!(info.kernel_base.is_multiple_of(PAGE_SIZE), "case {case}");
                assert!(
                    info.kernel_base.checked_add(info.kernel_span).is_some(),
                    "case {case}"
                );
                if let Ok(s) = segment_array(&info) {
                    assert!(s.addr != 0, "case {case}: {s:?}");
                    assert!(
                        s.addr.is_multiple_of(align_of::<KernelSegment>()),
                        "case {case}"
                    );
                    assert!(s.len as u64 == info.kernel_segment_count, "case {case}");
                }
            }

            let regions: Vec<MemoryRegion> = (0..rng.below(8))
                .map(|_| MemoryRegion {
                    base: rng.edge_u64() & !0xFFF,
                    pages: rng.edge_u64() >> rng.below(64),
                    kind: rng.below(8) as u32,
                    reserved: 0,
                })
                .collect();
            if let Ok(summary) = summarize(&regions) {
                summed += 1;
                let usable: u128 = regions
                    .iter()
                    .filter(|r| r.kind == RegionKind::Usable as u32)
                    .map(|r| u128::from(r.pages))
                    .sum();
                assert_eq!(u128::from(summary.usable_pages), usable, "case {case}");
                let mut end = 0u128;
                for r in &regions {
                    assert!(u128::from(r.base) >= end, "case {case}: {r:?}");
                    end = u128::from(r.base) + u128::from(r.pages) * 4096;
                    assert!(end <= 1 << 64, "case {case}: {r:?}");
                }
            }

            let segments: Vec<KernelSegment> = (0..rng.below(5))
                .map(|_| KernelSegment {
                    rva: rng.edge_u64() & !0xFFF,
                    pages: rng.edge_u64() >> rng.below(64),
                    access: rng.next() as u32,
                    reserved: 0,
                })
                .collect();
            let _ = check_kernel_segments(&segments, &info);
        }
        assert!(
            accepted > 1000 && summed > 500,
            "{accepted} accepted, {summed} summed"
        );
    }

    #[test]
    fn validate_passes_header_errors_through() {
        let mut info = header();
        info.magic = 0;
        assert_eq!(
            validate(&info),
            Err(InvalidBootInfo::Header(BootInfoError::BadMagic))
        );
        let mut info = header();
        info.region_count = MAX_REGIONS + 1;
        assert_eq!(
            validate(&info),
            Err(InvalidBootInfo::Header(BootInfoError::Regions))
        );
    }

    #[test]
    fn validate_locates_the_region_array() {
        assert_eq!(
            validate(&header()),
            Ok(RegionArray {
                addr: 0x8000,
                len: 3
            })
        );
        let mut info = header();
        info.region_count = MAX_REGIONS;
        assert_eq!(validate(&info).map(|a| a.len), Ok(4096));
    }

    #[test]
    fn validate_rejects_a_bad_region_pointer() {
        // Null, misaligned, and an array that would wrap past the top of the address space.
        for regions in [0, 0x8004, 0x8001, u64::MAX - 0xf, !7] {
            let mut info = header();
            info.regions = regions;
            assert_eq!(
                validate(&info),
                Err(InvalidBootInfo::RegionArray),
                "{regions:#x}"
            );
        }
        // The last aligned address at which three entries (72 bytes) end below 2^64, and
        // the next one, whose end would be 2^64 itself.
        let last = (u64::MAX - 3 * 24) & !7;
        let mut info = header();
        info.regions = last;
        assert!(validate(&info).is_ok());
        info.regions = last + 8;
        assert_eq!(validate(&info), Err(InvalidBootInfo::RegionArray));
    }

    #[test]
    fn validate_checks_the_kernel_image_range() {
        for (base, span) in [(0x20_0800, 0x3000), (!0xFFF_u64, 0x2000)] {
            let mut info = header();
            info.kernel_base = base;
            info.kernel_span = span;
            assert_eq!(validate(&info), Err(InvalidBootInfo::KernelImage));
        }
    }

    #[test]
    fn segment_array_is_located_and_checked() {
        assert_eq!(
            segment_array(&header()),
            Ok(SegmentArray {
                addr: 0x9000,
                len: 2
            })
        );
        for addr in [0, 0x9004, u64::MAX - 0x10] {
            let mut info = header();
            info.kernel_segments = addr;
            assert_eq!(segment_array(&info), Err(InvalidBootInfo::SegmentArray));
        }
        let mut info = header();
        info.kernel_segment_count = 0;
        assert_eq!(
            validate(&info),
            Err(InvalidBootInfo::Header(BootInfoError::Segments))
        );
        let seg = |rva, pages, access| KernelSegment {
            rva,
            pages,
            access,
            reserved: 0,
        };
        let (r, rx) = (
            origin_boot_info::SEGMENT_READ,
            origin_boot_info::SEGMENT_READ | origin_boot_info::SEGMENT_EXECUTE,
        );
        let ok = [seg(0, 1, r), seg(0x1000, 2, rx)];
        assert_eq!(check_kernel_segments(&ok, &header()), Ok(()));
        // Past the 0x3000-byte span.
        let long = [seg(0, 1, r), seg(0x1000, 3, rx)];
        assert_eq!(
            check_kernel_segments(&long, &header()),
            Err(InvalidBootInfo::Header(BootInfoError::Segment(1)))
        );
    }

    #[test]
    fn summarize_counts_only_usable_pages() {
        let regions = [
            region(0, 1, RegionKind::Reserved),
            region(0x1000, 255, RegionKind::Usable),
            region(0x10_0000, 0x100, RegionKind::KernelImage),
            region(0x20_0000, 0x201, RegionKind::Usable),
            region(0x40_1000, 7, RegionKind::LoaderReclaimable),
            MemoryRegion {
                base: 0x40_8000,
                pages: 0x1000,
                kind: 99, // unknown: counts as reserved
                reserved: 0,
            },
        ];
        let summary = summarize(&regions).unwrap();
        assert_eq!(summary.usable_pages, 255 + 0x201);
        // 768 pages = 3 MiB exactly.
        assert_eq!(summary.usable_mib(), 3);
    }

    #[test]
    fn usable_mib_rounds_down() {
        for (pages, mib) in [(0, 0), (255, 0), (256, 1), (511, 1), (512, 2)] {
            let summary = MemorySummary {
                usable_pages: pages,
            };
            assert_eq!(summary.usable_mib(), mib, "{pages} pages");
        }
        let summary = MemorySummary {
            usable_pages: u64::MAX,
        };
        assert_eq!(summary.usable_mib(), u64::MAX / 256);
    }

    #[test]
    fn summarize_accepts_adjacent_and_empty_regions() {
        let regions = [
            region(0x1000, 1, RegionKind::Usable),
            region(0x2000, 0, RegionKind::Reserved),
            region(0x2000, 1, RegionKind::Usable),
            // Ends at 0xffff_ffff_ffff_f000, the last page boundary below 2^64.
            region(0xffff_ffff_f000_0000, 0xffff, RegionKind::Reserved),
        ];
        assert_eq!(summarize(&regions).map(|s| s.usable_pages), Ok(2));
    }

    #[test]
    fn summarize_rejects_misaligned_and_overflowing_regions() {
        let cases = [
            [
                region(0x1000, 1, RegionKind::Usable),
                region(0x2001, 1, RegionKind::Usable),
            ],
            // This case and the next end at exactly 2^64.
            [
                region(0x1000, 1, RegionKind::Usable),
                region(!0xfff, 1, RegionKind::Usable),
            ],
            [
                region(0x1000, 1, RegionKind::Usable),
                region(0xffff_ffff_f000_0000, 0x1_0000, RegionKind::Reserved),
            ],
            [
                region(0x1000, 1, RegionKind::Usable),
                region(0x2000, u64::MAX / PAGE_SIZE + 1, RegionKind::Reserved),
            ],
            [
                region(0x1000, 1, RegionKind::Usable),
                region(0x2000, u64::MAX, RegionKind::Usable),
            ],
        ];
        for regions in cases {
            assert_eq!(
                summarize(&regions),
                Err(InvalidBootInfo::RegionBounds(1)),
                "{regions:x?}"
            );
        }
    }

    #[test]
    fn summarize_rejects_unsorted_and_overlapping_regions() {
        let unsorted = [
            region(0x2000, 1, RegionKind::Usable),
            region(0x1000, 1, RegionKind::Usable),
        ];
        assert_eq!(summarize(&unsorted), Err(InvalidBootInfo::RegionOrder(1)));
        let overlapping = [
            region(0x1000, 1, RegionKind::Usable),
            region(0x2000, 2, RegionKind::Reserved),
            region(0x3000, 1, RegionKind::Usable),
        ];
        assert_eq!(
            summarize(&overlapping),
            Err(InvalidBootInfo::RegionOrder(2))
        );
    }
}
