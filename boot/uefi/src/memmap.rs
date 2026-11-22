//! Translating the UEFI memory map into boot-info regions (ADR-0002 B4, B5).
//!
//! This runs between `GetMemoryMap` and `ExitBootServices`, so it must not allocate: it
//! writes into a caller-provided array.

use origin_boot_info::{MemoryRegion, RegionKind};

/// Smallest `EFI_MEMORY_DESCRIPTOR` (§7.2); firmware may report a larger stride.
pub const DESCRIPTOR_MIN_SIZE: usize = 40;
const PAGE: u64 = 4096;

/// Why a memory map could not be translated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapError {
    /// The descriptor size is below 40 bytes or not a multiple of 8.
    DescriptorSize(usize),
    /// A descriptor's start address is not 4 KiB aligned.
    Misaligned(u64),
    /// A descriptor's end address overflows.
    Overflow(u64),
    /// Two descriptors overlap.
    Overlap(u64),
    /// More regions than the output array holds.
    Capacity,
    /// A range to carve out is not inside a single region.
    NotContained(u64),
    /// A range the loader names by address lies in a region that is not the firmware's loader
    /// data, at the range's base (ADR-0002 Amendment 7; TASKS item 133's ruling Q1).
    NotLoaderData(u64),
}

/// `EFI_MEMORY_SP` (UEFI 2.8): specific-purpose memory, not for general use.
pub const EFI_MEMORY_SP: u64 = 0x0000_0000_0004_0000;
/// `EFI_MEMORY_HOT_PLUGGABLE` (UEFI 2.11): memory that can be removed at run time.
pub const EFI_MEMORY_HOT_PLUGGABLE: u64 = 0x0000_0000_0010_0000;

/// The Origin region kind for a descriptor's type and attributes. Conventional memory
/// (type 7) that firmware marks specific-purpose or hot-pluggable is not handed to the kernel
/// as general RAM (UEFI 2.11 §7.2, the memory attribute definitions); specific-purpose wins
/// when both are set. Every other type keeps the kind its type gives, because the kernel uses
/// no other type as general RAM yet.
pub const fn kind_for(uefi_type: u32, attribute: u64) -> RegionKind {
    if uefi_type == 7 {
        if attribute & EFI_MEMORY_SP != 0 {
            return RegionKind::SpecificPurpose;
        }
        if attribute & EFI_MEMORY_HOT_PLUGGABLE != 0 {
            return RegionKind::HotPluggable;
        }
    }
    kind_of(uefi_type)
}

/// The Origin region kind for an `EFI_MEMORY_TYPE`.
pub const fn kind_of(uefi_type: u32) -> RegionKind {
    match uefi_type {
        // LoaderCode, LoaderData, BootServicesCode, BootServicesData.
        1..=4 => RegionKind::LoaderReclaimable,
        5 => RegionKind::RuntimeCode,
        6 => RegionKind::RuntimeData,
        7 => RegionKind::Usable,
        8 => RegionKind::Unusable,
        9 => RegionKind::AcpiReclaimable,
        10 => RegionKind::AcpiNvs,
        11 | 12 => RegionKind::Mmio,
        14 => RegionKind::Persistent,
        // `EfiReservedMemoryType` alone (boot info 0.7, decision item 293).
        0 => RegionKind::Reserved,
        // `EfiUnacceptedMemoryType`: RAM, once accepted.
        15 => RegionKind::Unaccepted,
        // PalCode (13), the OEM and the OS types, and every type this loader does not know:
        // firmware memory the kernel never uses, and never one an RMRR or IVMD may name. Since
        // ADR-0002 Amendment 7 no Origin loader allocates `0x8000_0002` or `0x8000_0003`, so a
        // descriptor of either is the firmware's, and is never boot info or a kernel stack: the
        // kernel takes its secret area only from boot info (TASKS item 133's ruling Q2).
        _ => RegionKind::FirmwareOther,
    }
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    let mut v = [0u8; 4];
    if let Some(src) = at.checked_add(4).and_then(|end| b.get(at..end)) {
        for (d, s) in v.iter_mut().zip(src) {
            *d = *s;
        }
    }
    u32::from_le_bytes(v)
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    let mut v = [0u8; 8];
    if let Some(src) = at.checked_add(8).and_then(|end| b.get(at..end)) {
        for (d, s) in v.iter_mut().zip(src) {
            *d = *s;
        }
    }
    u64::from_le_bytes(v)
}

/// The first type UEFI leaves to OS loaders (UEFI 2.11 §7.2.1, `0x8000_0000` to `0xFFFF_FFFF`).
pub const OS_TYPES_FROM: u32 = 0x8000_0000;

/// A count of a raw memory map's descriptors of an OS-defined type (ADR-0002 Amendment 6, step
/// 2): the bisection mode's line that the map holds none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OsTypes {
    /// The descriptors read.
    pub descriptors: u32,
    /// Those of an OS-defined type, pages or none.
    pub count: u32,
    /// The first such type, or 0.
    pub first: u32,
}

/// Counts `map`'s descriptors (stride `descriptor_size`) whose type is [`OS_TYPES_FROM`] or above.
/// Reads only `map`; a stride the map cannot have counts nothing.
pub fn os_defined(map: &[u8], descriptor_size: usize) -> OsTypes {
    let mut t = OsTypes::default();
    if descriptor_size < DESCRIPTOR_MIN_SIZE || !descriptor_size.is_multiple_of(8) {
        return t;
    }
    for desc in map.chunks_exact(descriptor_size) {
        t.descriptors = t.descriptors.saturating_add(1);
        let uefi_type = u32_at(desc, 0);
        if uefi_type >= OS_TYPES_FROM {
            if t.count == 0 {
                t.first = uefi_type;
            }
            t.count = t.count.saturating_add(1);
        }
    }
    t
}

/// Translates the raw memory map `map` (stride `descriptor_size`) into `out`, sorted by
/// base, with adjacent regions of the same kind merged. Returns the number of regions.
pub fn translate(
    map: &[u8],
    descriptor_size: usize,
    out: &mut [MemoryRegion],
) -> Result<usize, MapError> {
    if descriptor_size < DESCRIPTOR_MIN_SIZE || !descriptor_size.is_multiple_of(8) {
        return Err(MapError::DescriptorSize(descriptor_size));
    }
    let mut n = 0usize;
    for desc in map.chunks_exact(descriptor_size) {
        let uefi_type = u32_at(desc, 0);
        let base = u64_at(desc, 8);
        let pages = u64_at(desc, 24);
        let attribute = u64_at(desc, 32);
        if pages == 0 {
            continue;
        }
        if !base.is_multiple_of(PAGE) {
            return Err(MapError::Misaligned(base));
        }
        pages
            .checked_mul(PAGE)
            .and_then(|len| base.checked_add(len))
            .ok_or(MapError::Overflow(base))?;
        let slot = out.get_mut(n).ok_or(MapError::Capacity)?;
        *slot = MemoryRegion {
            base,
            pages,
            kind: kind_for(uefi_type, attribute) as u32,
            reserved: 0,
        };
        n = n.saturating_add(1);
    }
    let regions = out.get_mut(..n).ok_or(MapError::Capacity)?;
    // Insertion sort: firmware maps are short, and this must not allocate.
    for i in 1..regions.len() {
        let mut j = i;
        while j > 0 {
            let (Some(prev), Some(cur)) = (regions.get(j.saturating_sub(1)), regions.get(j)) else {
                break;
            };
            if prev.base <= cur.base {
                break;
            }
            regions.swap(j.saturating_sub(1), j);
            j = j.saturating_sub(1);
        }
    }
    // Reject overlaps, then merge adjacent regions of the same kind in place.
    let mut kept = 0usize;
    for i in 0..regions.len() {
        let Some(cur) = regions.get(i).copied() else {
            break;
        };
        if let Some(last) = kept.checked_sub(1).and_then(|k| regions.get_mut(k)) {
            let last_end = last.base.saturating_add(last.pages.saturating_mul(PAGE));
            if cur.base < last_end {
                return Err(MapError::Overlap(cur.base));
            }
            if cur.base == last_end && cur.kind == last.kind {
                last.pages = last.pages.saturating_add(cur.pages);
                continue;
            }
        }
        if let Some(slot) = regions.get_mut(kept) {
            *slot = cur;
        }
        kept = kept.saturating_add(1);
    }
    Ok(kept)
}

/// Re-labels `pages` pages at `base` as `kind` inside the first `count` regions of `out`,
/// splitting the region that contains them into up to three. Returns the new count.
///
/// The kernel image is allocated as `EfiLoaderCode`, which firmware keeps executable, so
/// the memory map cannot mark it by type; the loader marks it by address (ADR-0002 B3).
pub fn carve(
    out: &mut [MemoryRegion],
    count: usize,
    base: u64,
    pages: u64,
    kind: RegionKind,
) -> Result<usize, MapError> {
    carve_within(out, count, base, pages, kind, None)
}

/// [`carve`], refused unless the region holding the range is of kind `within`, when given.
fn carve_within(
    out: &mut [MemoryRegion],
    count: usize,
    base: u64,
    pages: u64,
    kind: RegionKind,
    within: Option<RegionKind>,
) -> Result<usize, MapError> {
    let end = pages
        .checked_mul(PAGE)
        .and_then(|len| base.checked_add(len))
        .ok_or(MapError::Overflow(base))?;
    let regions = out.get(..count).ok_or(MapError::Capacity)?;
    let (i, r) = regions
        .iter()
        .copied()
        .enumerate()
        .find(|(_, r)| {
            let r_end = r.base.saturating_add(r.pages.saturating_mul(PAGE));
            r.base <= base && end <= r_end
        })
        .ok_or(MapError::NotContained(base))?;
    if within.is_some_and(|k| r.kind != k as u32) {
        return Err(MapError::NotLoaderData(base));
    }
    let r_end = r.base.saturating_add(r.pages.saturating_mul(PAGE));
    let left = (base > r.base).then(|| MemoryRegion {
        base: r.base,
        pages: (base - r.base) / PAGE,
        kind: r.kind,
        reserved: 0,
    });
    let middle = MemoryRegion {
        base,
        pages,
        kind: kind as u32,
        reserved: 0,
    };
    let right = (end < r_end).then(|| MemoryRegion {
        base: end,
        pages: (r_end - end) / PAGE,
        kind: r.kind,
        reserved: 0,
    });
    let parts: [Option<MemoryRegion>; 3] = [left, Some(middle), right];
    let n_parts = parts.iter().flatten().count();
    let extra = n_parts.saturating_sub(1);
    let new_count = count.checked_add(extra).ok_or(MapError::Capacity)?;
    // Open `extra` free slots at `i` by rotating the tail, then write the parts there.
    let tail = out.get_mut(i..new_count).ok_or(MapError::Capacity)?;
    tail.rotate_right(extra);
    for (slot, part) in tail.iter_mut().zip(parts.iter().flatten()) {
        *slot = *part;
    }
    Ok(new_count)
}

/// One of the loader's allocations the kernel must tell apart from the loader's other data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoaderRegion {
    /// The kernel image, `EfiLoaderCode`, which firmware keeps executable (ADR-0002 B3).
    KernelImage,
    /// The kernel stack and the guard page below it, `EfiLoaderData` (ADR-0002 Amendment 7).
    KernelStack,
    /// The boot info, `EfiLoaderData` (ADR-0002 Amendment 7).
    BootInfo,
}

impl LoaderRegion {
    /// The region kind the translated map names it by.
    const fn kind(self) -> RegionKind {
        match self {
            Self::KernelImage => RegionKind::KernelImage,
            Self::KernelStack => RegionKind::KernelStack,
            Self::BootInfo => RegionKind::BootInfo,
        }
    }
}

/// Where the loader's three allocations are, each as its base and its pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoaderRegions {
    /// The kernel image.
    pub kernel: (u64, u64),
    /// The kernel stack's allocation: the guard page's base, and the stack's pages with the
    /// guard's. The page tables leave the first page of a kernel-stack region unmapped only when
    /// the region starts at the guard (`origin_paging::layout`), so it is named from there.
    pub stack: (u64, u64),
    /// The boot info: the header, the region and segment arrays, and the secret area.
    pub info: (u64, u64),
}

/// Which allocation could not be named, and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CarveFailure {
    /// The allocation.
    pub region: LoaderRegion,
    /// Why.
    pub error: MapError,
}

/// Names the loader's three allocations by address in the first `count` regions of `out`, each
/// inside a region the firmware gave as loader data (`LoaderReclaimable` once translated), as
/// ADR-0002 B3 names the kernel image and Amendment 7 the boot info and the kernel stack. Used on
/// every map the loader translates: the one the page tables are built from and the one boot info
/// hands the kernel. Pure computation: it may run between `GetMemoryMap` and `ExitBootServices`.
/// Returns the new count.
pub fn carve_loader_regions(
    out: &mut [MemoryRegion],
    count: usize,
    at: &LoaderRegions,
) -> Result<usize, CarveFailure> {
    let mut n = count;
    for (region, (base, pages)) in [
        (LoaderRegion::KernelImage, at.kernel),
        (LoaderRegion::KernelStack, at.stack),
        (LoaderRegion::BootInfo, at.info),
    ] {
        n = carve_within(
            out,
            n,
            base,
            pages,
            region.kind(),
            Some(RegionKind::LoaderReclaimable),
        )
        .map_err(|error| CarveFailure { region, error })?;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocols::memory_type;

    fn desc(stride: usize, ty: u32, base: u64, pages: u64) -> Vec<u8> {
        desc_attr(stride, ty, base, pages, 0xF) // UC, WC, WT and WB: cacheable RAM
    }

    fn desc_attr(stride: usize, ty: u32, base: u64, pages: u64, attribute: u64) -> Vec<u8> {
        let mut d = vec![0u8; stride];
        d[0..4].copy_from_slice(&ty.to_le_bytes());
        d[8..16].copy_from_slice(&base.to_le_bytes());
        d[16..24].copy_from_slice(&0xDEAD_BEEFu64.to_le_bytes()); // VirtualStart, ignored
        d[24..32].copy_from_slice(&pages.to_le_bytes());
        d[32..40].copy_from_slice(&attribute.to_le_bytes());
        d
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
    fn translates_sorts_and_merges() {
        // Out of order, with OVMF's 48-byte stride, adjacent Loader/BootServices ranges
        // that merge, a range of the type B3 gave the kernel stack before Amendment 7 (now the
        // firmware's), and a zero-page descriptor that is dropped.
        let map = [
            desc(48, 7, 0x10_0000, 0x100), // conventional
            desc(48, 2, 0x1000, 1),        // loader data
            desc(48, 4, 0x2000, 2),        // boot services data, adjacent: merges
            desc(48, memory_type::OS_KERNEL_STACK, 0x20_0000, 3),
            desc(48, 9, 0x4000, 0),       // empty: ignored
            desc(48, 11, 0xFEC0_0000, 1), // MMIO
        ]
        .concat();
        let mut out = [region(0, 0, RegionKind::Reserved); 8];
        let n = translate(&map, 48, &mut out).unwrap();
        assert_eq!(
            &out[..n],
            &[
                region(0x1000, 3, RegionKind::LoaderReclaimable),
                region(0x10_0000, 0x100, RegionKind::Usable),
                region(0x20_0000, 3, RegionKind::FirmwareOther),
                region(0xFEC0_0000, 1, RegionKind::Mmio),
            ]
        );
    }

    #[test]
    fn attributes_keep_special_conventional_memory_out_of_general_use() {
        // Conventional memory: plain, specific-purpose (UEFI 2.8), hot-pluggable (2.11), both
        // (specific-purpose wins), and the caching attributes alone (still usable).
        assert_eq!(kind_for(7, 0xF), RegionKind::Usable);
        assert_eq!(
            kind_for(7, 0xF | EFI_MEMORY_SP),
            RegionKind::SpecificPurpose
        );
        assert_eq!(
            kind_for(7, 0xF | EFI_MEMORY_HOT_PLUGGABLE),
            RegionKind::HotPluggable
        );
        assert_eq!(
            kind_for(7, EFI_MEMORY_SP | EFI_MEMORY_HOT_PLUGGABLE),
            RegionKind::SpecificPurpose
        );
        // Other types keep their kind: the kernel uses none of them as general RAM yet.
        assert_eq!(
            kind_for(1, EFI_MEMORY_HOT_PLUGGABLE),
            RegionKind::LoaderReclaimable
        );
        assert_eq!(kind_for(14, EFI_MEMORY_SP), RegionKind::Persistent);
        // Through the whole translation: specific-purpose and hot-pluggable ranges stay
        // separate from the RAM around them, and like ranges still merge.
        let map = [
            desc(48, 7, 0x10_0000, 0x100),
            desc_attr(48, 7, 0x20_0000, 0x10, 0xF | EFI_MEMORY_SP),
            desc_attr(48, 7, 0x21_0000, 0x10, 0xF | EFI_MEMORY_SP),
            desc_attr(48, 7, 0x22_0000, 0x20, 0xF | EFI_MEMORY_HOT_PLUGGABLE),
            desc(48, 7, 0x24_0000, 0x40),
        ]
        .concat();
        let mut out = [region(0, 0, RegionKind::Reserved); 8];
        let n = translate(&map, 48, &mut out).unwrap();
        assert_eq!(
            &out[..n],
            &[
                region(0x10_0000, 0x100, RegionKind::Usable),
                region(0x20_0000, 0x20, RegionKind::SpecificPurpose),
                region(0x22_0000, 0x20, RegionKind::HotPluggable),
                region(0x24_0000, 0x40, RegionKind::Usable),
            ]
        );
    }

    #[test]
    fn os_defined_types_are_counted_from_0x80000000() {
        let map = [
            desc(48, memory_type::LOADER_CODE, 0x10_0000, 4),
            desc(48, memory_type::LOADER_DATA, 0x20_0000, 2),
            desc(48, memory_type::OS_KERNEL_STACK, 0x30_0000, 17),
            desc(48, memory_type::OS_BOOT_INFO, 0x40_0000, 1),
            desc(48, 0x7FFF_FFFF, 0x50_0000, 1),
        ]
        .concat();
        assert_eq!(
            os_defined(&map, 48),
            OsTypes {
                descriptors: 5,
                count: 2,
                first: memory_type::OS_KERNEL_STACK
            }
        );
        let none = [
            desc(48, memory_type::LOADER_DATA, 0x10_0000, 4),
            desc(48, memory_type::LOADER_DATA, 0x30_0000, 17),
        ]
        .concat();
        assert_eq!(
            os_defined(&none, 48),
            OsTypes {
                descriptors: 2,
                count: 0,
                first: 0
            }
        );
        // A stride the map cannot have counts nothing, rather than reading across descriptors.
        assert_eq!(os_defined(&map, 44), OsTypes::default());
    }

    #[test]
    fn every_uefi_type_has_a_kind() {
        assert_eq!(kind_of(7), RegionKind::Usable);
        assert_eq!(kind_of(10), RegionKind::AcpiNvs);
        // Boot info 0.7 (decision item 293): type 0 alone is `Reserved`.
        assert_eq!(kind_of(0), RegionKind::Reserved);
        assert_eq!(kind_of(5), RegionKind::RuntimeCode);
        assert_eq!(kind_of(6), RegionKind::RuntimeData);
        assert_eq!(kind_of(15), RegionKind::Unaccepted);
        for other in [
            13,          // PalCode
            16,          // the first type UEFI 2.11 does not define
            0x6FFF_FFFF, // the last below the OEM types
            0x7000_0000, // the OEM types
            0x7FFF_FFFF,
            0x8000_0000, // the OS types, Origin's own two apart
            0x8000_0001,
            0x8000_0004,
            u32::MAX,
        ] {
            assert_eq!(kind_of(other), RegionKind::FirmwareOther, "{other:#x}");
        }
        // Every type UEFI 2.11 §7.2 defines, in order.
        use RegionKind::*;
        let defined = [
            Reserved,
            LoaderReclaimable,
            LoaderReclaimable,
            LoaderReclaimable,
            LoaderReclaimable,
            RuntimeCode,
            RuntimeData,
            Usable,
            Unusable,
            AcpiReclaimable,
            AcpiNvs,
            Mmio,
            Mmio,
            FirmwareOther,
            Persistent,
            Unaccepted,
        ];
        for (t, kind) in defined.into_iter().enumerate() {
            assert_eq!(kind_of(t as u32), kind, "type {t}");
        }
        assert_eq!(kind_of(1), RegionKind::LoaderReclaimable); // LoaderCode, incl. the kernel image before carving
        // ADR-0002 Amendment 7: no Origin loader allocates these any more, so they are the
        // firmware's (TASKS item 133's ruling Q2).
        assert_eq!(
            kind_of(memory_type::OS_BOOT_INFO),
            RegionKind::FirmwareOther
        );
        assert_eq!(
            kind_of(memory_type::OS_KERNEL_STACK),
            RegionKind::FirmwareOther
        );
    }

    #[test]
    fn rejects_bad_maps() {
        let mut out = [region(0, 0, RegionKind::Reserved); 4];
        assert_eq!(
            translate(&[], 32, &mut out),
            Err(MapError::DescriptorSize(32))
        );
        assert_eq!(
            translate(&[], 44, &mut out),
            Err(MapError::DescriptorSize(44))
        );
        assert_eq!(
            translate(&desc(40, 7, 0x1001, 1), 40, &mut out),
            Err(MapError::Misaligned(0x1001))
        );
        assert_eq!(
            translate(&desc(40, 7, !0xFFFu64, 2), 40, &mut out),
            Err(MapError::Overflow(!0xFFFu64))
        );
        let overlap = [desc(40, 7, 0x1000, 2), desc(40, 2, 0x2000, 1)].concat();
        assert_eq!(
            translate(&overlap, 40, &mut out),
            Err(MapError::Overlap(0x2000))
        );
        let many: Vec<u8> = (0..5)
            .flat_map(|i| desc(40, 7, 0x10_0000 * (i + 1), 1))
            .collect();
        assert_eq!(translate(&many, 40, &mut out), Err(MapError::Capacity));
        // A trailing partial descriptor is ignored, never read past.
        let mut partial = desc(40, 7, 0x1000, 1);
        partial.extend_from_slice(&[0xFF; 20]);
        assert_eq!(translate(&partial, 40, &mut out), Ok(1));
    }
    #[test]
    fn carve_splits_the_containing_region() {
        let k = RegionKind::KernelImage;
        let lr = RegionKind::LoaderReclaimable;
        let mut out = [region(0, 0, RegionKind::Reserved); 8];
        out[0] = region(0x1000, 1, RegionKind::Usable);
        out[1] = region(0x10_0000, 0x10, lr);
        out[2] = region(0x20_0000, 1, RegionKind::Mmio);
        // Middle of a region: three parts.
        let n = carve(&mut out, 3, 0x10_4000, 2, k).unwrap();
        assert_eq!(
            &out[..n],
            &[
                region(0x1000, 1, RegionKind::Usable),
                region(0x10_0000, 4, lr),
                region(0x10_4000, 2, k),
                region(0x10_6000, 10, lr),
                region(0x20_0000, 1, RegionKind::Mmio),
            ]
        );
        // The whole of a region: relabelled in place.
        let n2 = carve(&mut out, n, 0x1000, 1, RegionKind::BootInfo).unwrap();
        assert_eq!(n2, n);
        assert_eq!(out[0], region(0x1000, 1, RegionKind::BootInfo));
        // At the start and at the end of a region: two parts each.
        let mut out = [region(0, 0, RegionKind::Reserved); 4];
        out[0] = region(0x10_0000, 4, lr);
        assert_eq!(carve(&mut out, 1, 0x10_0000, 1, k), Ok(2));
        assert_eq!(
            &out[..2],
            &[region(0x10_0000, 1, k), region(0x10_1000, 3, lr)]
        );
        let mut out = [region(0, 0, RegionKind::Reserved); 4];
        out[0] = region(0x10_0000, 4, lr);
        assert_eq!(carve(&mut out, 1, 0x10_3000, 1, k), Ok(2));
        assert_eq!(
            &out[..2],
            &[region(0x10_0000, 3, lr), region(0x10_3000, 1, k)]
        );
    }

    #[test]
    fn carve_rejects_ranges_outside_one_region_and_full_arrays() {
        let lr = RegionKind::LoaderReclaimable;
        let mut out = [
            region(0x10_0000, 2, lr),
            region(0x10_2000, 2, RegionKind::Usable),
        ];
        // Spans two regions.
        assert_eq!(
            carve(&mut out, 2, 0x10_1000, 2, RegionKind::KernelImage),
            Err(MapError::NotContained(0x10_1000))
        );
        // Not in the map at all.
        assert_eq!(
            carve(&mut out, 2, 0x50_0000, 1, RegionKind::KernelImage),
            Err(MapError::NotContained(0x50_0000))
        );
        // A split needs room the array does not have.
        let mut full = [region(0x10_0000, 4, lr)];
        assert_eq!(
            carve(&mut full, 1, 0x10_1000, 1, RegionKind::KernelImage),
            Err(MapError::Capacity)
        );
    }

    /// A firmware map as the loader sees it at the call under ADR-0002 Amendment 7: the kernel
    /// image as `EfiLoaderCode`, and the kernel stack (with its guard page) and the boot info as
    /// `EfiLoaderData`, inside one block with more loader data after them.
    fn amendment_7_map() -> Vec<u8> {
        [
            desc(48, 7, 0x10_0000, 0x100),                       // conventional
            desc(48, memory_type::LOADER_CODE, 0x20_0000, 4),    // the kernel image
            desc(48, memory_type::LOADER_DATA, 0x30_0000, 0x20), // stack, boot info, more
        ]
        .concat()
    }

    const AT: LoaderRegions = LoaderRegions {
        kernel: (0x20_0000, 4),
        stack: (0x30_0000, 17),
        info: (0x31_1000, 2),
    };

    #[test]
    fn the_loader_regions_are_named_by_address_inside_loader_data() {
        let mut out = [region(0, 0, RegionKind::Reserved); 12];
        let n = translate(&amendment_7_map(), 48, &mut out).unwrap();
        let n = carve_loader_regions(&mut out, n, &AT).unwrap();
        assert_eq!(
            &out[..n],
            &[
                region(0x10_0000, 0x100, RegionKind::Usable),
                region(0x20_0000, 4, RegionKind::KernelImage),
                region(0x30_0000, 17, RegionKind::KernelStack),
                region(0x31_1000, 2, RegionKind::BootInfo),
                region(0x31_3000, 13, RegionKind::LoaderReclaimable),
            ]
        );
        // The kernel-stack region starts at the guard page, which is what makes the page tables
        // leave that page unmapped (`origin_paging::layout`, its `stack_guard`), the loader's
        // tables and the kernel's alike (TASKS item 133's ruling Q3).
        let stack = out[..n]
            .iter()
            .find(|r| r.kind == RegionKind::KernelStack as u32)
            .unwrap();
        assert_eq!(stack.base, AT.stack.0);
        // Naming them again, as the retry after a failed call does on a fresh map, gives the same.
        let mut again = [region(0, 0, RegionKind::Reserved); 12];
        let m = translate(&amendment_7_map(), 48, &mut again).unwrap();
        let m = carve_loader_regions(&mut again, m, &AT).unwrap();
        assert_eq!(&again[..m], &out[..n]);
    }

    #[test]
    fn a_loader_region_outside_the_firmware_s_loader_data_is_refused() {
        // TASKS item 133's ruling Q1: each region is named only inside loader data.
        let fails = |map: Vec<u8>, at: LoaderRegions| {
            let mut out = [region(0, 0, RegionKind::Reserved); 12];
            let n = translate(&map, 48, &mut out).unwrap();
            carve_loader_regions(&mut out, n, &at).unwrap_err()
        };
        // The stack in conventional memory.
        let at = LoaderRegions {
            stack: (0x10_0000, 17),
            ..AT
        };
        assert_eq!(
            fails(amendment_7_map(), at),
            CarveFailure {
                region: LoaderRegion::KernelStack,
                error: MapError::NotLoaderData(0x10_0000)
            }
        );
        // The boot info in a region of the type B3 gave it before Amendment 7: the firmware's.
        let map = [
            amendment_7_map(),
            desc(48, memory_type::OS_BOOT_INFO, 0x40_0000, 2),
        ]
        .concat();
        let at = LoaderRegions {
            info: (0x40_0000, 2),
            ..AT
        };
        assert_eq!(
            fails(map, at),
            CarveFailure {
                region: LoaderRegion::BootInfo,
                error: MapError::NotLoaderData(0x40_0000)
            }
        );
        // The kernel image outside every region, as before.
        let at = LoaderRegions {
            kernel: (0x90_0000, 4),
            ..AT
        };
        assert_eq!(
            fails(amendment_7_map(), at),
            CarveFailure {
                region: LoaderRegion::KernelImage,
                error: MapError::NotContained(0x90_0000)
            }
        );
    }

    #[test]
    fn a_firmware_region_of_the_old_boot_info_type_gives_no_secret_area() {
        // TASKS item 133's ruling Q2: a firmware descriptor of `0x8000_0002` is never boot info,
        // so the kernel never takes a secret area from inside it; a region the loader named as
        // boot info is the positive control.
        let map = [
            amendment_7_map(),
            desc(48, memory_type::OS_BOOT_INFO, 0x40_0000, 1),
            desc(48, memory_type::OS_KERNEL_STACK, 0x41_0000, 1),
        ]
        .concat();
        let mut out = [region(0, 0, RegionKind::Reserved); 12];
        let n = translate(&map, 48, &mut out).unwrap();
        let kinds: Vec<u32> = out[..n]
            .iter()
            .filter(|r| r.base >= 0x40_0000)
            .map(|r| r.kind)
            .collect();
        assert_eq!(
            kinds,
            [
                RegionKind::FirmwareOther as u32,
                RegionKind::FirmwareOther as u32
            ]
        );
        let info = |seed| origin_boot_info::BootInfo {
            magic: origin_boot_info::MAGIC,
            version_major: origin_boot_info::VERSION.0,
            version_minor: origin_boot_info::VERSION.1,
            size: core::mem::size_of::<origin_boot_info::BootInfo>() as u32,
            regions: 0x1000,
            region_count: n as u64,
            kernel_base: 0x20_0000,
            kernel_span: 0x4000,
            acpi_rsdp: 0,
            uefi_system_table: 0,
            stack_base: 0x30_1000,
            stack_size: 0x1_0000,
            kernel_segments: 0x1800,
            kernel_segment_count: 3,
            page_table_root: 0x9_0000,
            direct_map_base: 0xFFFF_8000_0000_0000,
            kernel_virtual_base: 0xFFFF_FFFF_8000_0000,
            flags: 0,
            framebuffer: origin_boot_info::Framebuffer::NONE,
            boot_seed: seed,
            early_uart: origin_boot_info::EarlyUart::NONE,
        };
        assert_eq!(
            origin_boot_info::boot_seed(&info(0x40_0100), 0x31_1000, &out[..n]),
            None
        );
        let n = carve_loader_regions(&mut out, n, &AT).unwrap();
        assert_eq!(
            origin_boot_info::boot_seed(&info(0x31_1800), 0x31_1000, &out[..n]),
            Some(0x31_1800)
        );
    }
}
