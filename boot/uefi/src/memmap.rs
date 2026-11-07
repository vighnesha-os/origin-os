//! Translating the UEFI memory map into boot-info regions (ADR-0002 B4, B5).
//!
//! This runs between `GetMemoryMap` and `ExitBootServices`, so it must not allocate: it
//! writes into a caller-provided array.

use origin_boot_info::{MemoryRegion, RegionKind};

use crate::protocols::memory_type;

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
        memory_type::OS_BOOT_INFO => RegionKind::BootInfo,
        memory_type::OS_KERNEL_STACK => RegionKind::KernelStack,
        // Reserved (0), PalCode (13), Unaccepted (15), unknown and other OEM/OS values.
        _ => RegionKind::Reserved,
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
            kind: kind_of(uefi_type) as u32,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn desc(stride: usize, ty: u32, base: u64, pages: u64) -> Vec<u8> {
        let mut d = vec![0u8; stride];
        d[0..4].copy_from_slice(&ty.to_le_bytes());
        d[8..16].copy_from_slice(&base.to_le_bytes());
        d[16..24].copy_from_slice(&0xDEAD_BEEFu64.to_le_bytes()); // VirtualStart, ignored
        d[24..32].copy_from_slice(&pages.to_le_bytes());
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
        // that merge, an OS-defined kernel range, and a zero-page descriptor that is
        // dropped.
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
                region(0x20_0000, 3, RegionKind::KernelStack),
                region(0xFEC0_0000, 1, RegionKind::Mmio),
            ]
        );
    }

    #[test]
    fn every_uefi_type_has_a_kind() {
        assert_eq!(kind_of(7), RegionKind::Usable);
        assert_eq!(kind_of(10), RegionKind::AcpiNvs);
        assert_eq!(kind_of(15), RegionKind::Reserved); // unaccepted memory
        assert_eq!(kind_of(0x7000_0000), RegionKind::Reserved);
        assert_eq!(kind_of(1), RegionKind::LoaderReclaimable); // LoaderCode, incl. the kernel image before carving
        assert_eq!(kind_of(memory_type::OS_BOOT_INFO), RegionKind::BootInfo);
        assert_eq!(
            kind_of(memory_type::OS_KERNEL_STACK),
            RegionKind::KernelStack
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
}
