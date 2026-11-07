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
        memory_type::OS_KERNEL_IMAGE => RegionKind::KernelImage,
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
            desc(48, memory_type::OS_KERNEL_IMAGE, 0x20_0000, 3),
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
                region(0x20_0000, 3, RegionKind::KernelImage),
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
}
