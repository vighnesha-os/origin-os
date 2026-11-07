//! The boot-info structure: the one interface between the Origin boot loader and the
//! Framekernel (`docs/adr/ADR-0002-boot.md`, **Proposed**).
//!
//! The loader fills a [`BootInfo`] and passes its physical address to the kernel entry
//! point in `rdi` (System V). Every field has an explicit width, the layout is `repr(C)`
//! and pinned by compile-time offset checks, and no Rust-specific type crosses the
//! boundary (CLAUDE.md §42).
//!
//! The version is `[compatibility] boot-info` in `origin-version.toml`. Major 0 means the
//! layout is not yet stable and may change until ADR-0002 is accepted. The loader and the
//! kernel come from the same unified release (CLAUDE.md §62), and the kernel's KRN manifest
//! records the version it expects (key 24).
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

/// `"ORIGBOOT"` as a little-endian `u64`.
pub const MAGIC: u64 = u64::from_le_bytes(*b"ORIGBOOT");
/// Version the loader provides and the kernel expects.
pub const VERSION: (u16, u16) = (
    origin_version::BOOT_INFO_MAJOR,
    origin_version::BOOT_INFO_MINOR,
);

/// What a physical memory region is, after the loader has translated the UEFI memory map.
/// The values cross the ABI as `u32`; a kernel treats an unknown value as [`Self::Reserved`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum RegionKind {
    /// Free RAM.
    Usable = 1,
    /// Must not be used.
    Reserved = 2,
    /// ACPI tables; usable once the kernel has read them.
    AcpiReclaimable = 3,
    /// ACPI non-volatile storage; never usable.
    AcpiNvs = 4,
    /// Memory-mapped I/O.
    Mmio = 5,
    /// The loaded Framekernel image.
    KernelImage = 6,
    /// This structure and the region array.
    BootInfo = 7,
    /// The stack the kernel starts on.
    KernelStack = 8,
    /// Boot loader code and data and firmware boot-services memory; usable once the
    /// kernel no longer needs anything the loader passed.
    LoaderReclaimable = 9,
    /// UEFI runtime-services code.
    RuntimeCode = 10,
    /// UEFI runtime-services data.
    RuntimeData = 11,
    /// RAM with errors.
    Unusable = 12,
    /// Persistent memory.
    Persistent = 13,
}

impl RegionKind {
    /// The kind for a raw value, or `None` if the value is unknown.
    pub const fn from_raw(raw: u32) -> Option<Self> {
        Some(match raw {
            1 => Self::Usable,
            2 => Self::Reserved,
            3 => Self::AcpiReclaimable,
            4 => Self::AcpiNvs,
            5 => Self::Mmio,
            6 => Self::KernelImage,
            7 => Self::BootInfo,
            8 => Self::KernelStack,
            9 => Self::LoaderReclaimable,
            10 => Self::RuntimeCode,
            11 => Self::RuntimeData,
            12 => Self::Unusable,
            13 => Self::Persistent,
            _ => return None,
        })
    }
}

/// One physical memory region: `pages` 4 KiB pages starting at `base`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct MemoryRegion {
    /// Physical start address, 4 KiB aligned.
    pub base: u64,
    /// Length in 4 KiB pages.
    pub pages: u64,
    /// A [`RegionKind`] value.
    pub kind: u32,
    /// Zero.
    pub reserved: u32,
}

/// [`KernelSegment::access`]: readable.
pub const SEGMENT_READ: u32 = 1;
/// [`KernelSegment::access`]: writable. Never together with [`SEGMENT_EXECUTE`].
pub const SEGMENT_WRITE: u32 = 1 << 1;
/// [`KernelSegment::access`]: executable. Never together with [`SEGMENT_WRITE`].
pub const SEGMENT_EXECUTE: u32 = 1 << 2;

/// One segment of the loaded kernel image, taken from the verified ONBF segment table, so
/// the kernel maps its own image with exactly the permissions the signed image declares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct KernelSegment {
    /// Offset of the segment from [`BootInfo::kernel_base`], 4 KiB aligned.
    pub rva: u64,
    /// Length in 4 KiB pages, at least 1.
    pub pages: u64,
    /// [`SEGMENT_READ`], [`SEGMENT_WRITE`] and [`SEGMENT_EXECUTE`] bits. The loader has
    /// applied the relocations, so a RELRO segment arrives without [`SEGMENT_WRITE`].
    pub access: u32,
    /// Zero.
    pub reserved: u32,
}

/// Upper bound on `kernel_segment_count`.
pub const MAX_KERNEL_SEGMENTS: u64 = 16;

/// Everything the kernel learns from the loader.
///
/// All addresses are physical. In version 0.x the kernel starts with the firmware's
/// identity mapping still active, so physical addresses are directly usable until the
/// kernel installs its own page tables (ADR-0002).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct BootInfo {
    /// [`MAGIC`].
    pub magic: u64,
    /// [`VERSION`] major.
    pub version_major: u16,
    /// [`VERSION`] minor.
    pub version_minor: u16,
    /// `size_of::<BootInfo>()` as the loader built it.
    pub size: u32,
    /// Physical address of a `[MemoryRegion; region_count]`, sorted by `base`, with no
    /// overlaps.
    pub regions: u64,
    /// Number of entries at `regions`.
    pub region_count: u64,
    /// Physical address the KRN image's RVA 0 was loaded at.
    pub kernel_base: u64,
    /// Bytes from `kernel_base` to the end of the highest segment.
    pub kernel_span: u64,
    /// Physical address of the ACPI 2.0+ RSDP, or 0 if the firmware has none.
    pub acpi_rsdp: u64,
    /// Physical address of the UEFI system table, for runtime services, or 0.
    pub uefi_system_table: u64,
    /// Lowest address of the stack the kernel starts on.
    pub stack_base: u64,
    /// Size of that stack in bytes.
    pub stack_size: u64,
    /// Physical address of a `[KernelSegment; kernel_segment_count]` (version 0.2).
    pub kernel_segments: u64,
    /// Number of entries at `kernel_segments`, between 1 and [`MAX_KERNEL_SEGMENTS`].
    pub kernel_segment_count: u64,
}

// The ABI layout; a change here is a boot-info major version change.
const _: () = {
    use core::mem::{offset_of, size_of};
    assert!(size_of::<MemoryRegion>() == 24);
    assert!(offset_of!(BootInfo, version_major) == 8);
    assert!(offset_of!(BootInfo, size) == 12);
    assert!(offset_of!(BootInfo, regions) == 16);
    assert!(offset_of!(BootInfo, kernel_base) == 32);
    assert!(offset_of!(BootInfo, acpi_rsdp) == 48);
    assert!(offset_of!(BootInfo, stack_size) == 72);
    assert!(offset_of!(BootInfo, kernel_segments) == 80);
    assert!(offset_of!(BootInfo, kernel_segment_count) == 88);
    assert!(size_of::<BootInfo>() == 96);
    assert!(size_of::<KernelSegment>() == 24);
};

/// Why a kernel rejected the boot info it was given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootInfoError {
    /// `magic` is wrong: this is not a boot-info structure.
    BadMagic,
    /// A version this kernel was not built for.
    Version(u16, u16),
    /// `size` is smaller than this kernel's `BootInfo`.
    TooSmall(u32),
    /// The region list is empty or has an absurd length.
    Regions,
    /// The kernel segment list is empty or longer than [`MAX_KERNEL_SEGMENTS`].
    Segments,
    /// The kernel segment at this index is unaligned, empty, outside `kernel_span`, out of
    /// order or overlapping, or has invalid access bits (unknown bits, not readable, or
    /// both writable and executable).
    Segment(usize),
}

impl BootInfo {
    /// Checks the header fields. Validate before trusting any other field.
    pub fn check(&self) -> Result<(), BootInfoError> {
        if self.magic != MAGIC {
            return Err(BootInfoError::BadMagic);
        }
        // Major 0: every minor is incompatible. From 1.0 on, a higher minor may add fields.
        let compatible = if VERSION.0 == 0 {
            (self.version_major, self.version_minor) == VERSION
        } else {
            self.version_major == VERSION.0 && self.version_minor >= VERSION.1
        };
        if !compatible {
            return Err(BootInfoError::Version(
                self.version_major,
                self.version_minor,
            ));
        }
        if (self.size as usize) < core::mem::size_of::<Self>() {
            return Err(BootInfoError::TooSmall(self.size));
        }
        if self.region_count == 0 || self.region_count > MAX_REGIONS {
            return Err(BootInfoError::Regions);
        }
        if self.kernel_segment_count == 0 || self.kernel_segment_count > MAX_KERNEL_SEGMENTS {
            return Err(BootInfoError::Segments);
        }
        Ok(())
    }
}

/// Checks a kernel segment list against the contract: each segment 4 KiB aligned and at
/// least one page long, sorted by `rva` without overlaps, inside `kernel_span`, readable,
/// never both writable and executable, with no unknown access bits and `reserved` zero.
pub fn check_segments(segments: &[KernelSegment], kernel_span: u64) -> Result<(), BootInfoError> {
    const PAGE: u64 = 4096;
    let known = SEGMENT_READ | SEGMENT_WRITE | SEGMENT_EXECUTE;
    let mut previous_end = 0u64;
    for (index, s) in segments.iter().enumerate() {
        let end = s
            .pages
            .checked_mul(PAGE)
            .and_then(|bytes| s.rva.checked_add(bytes));
        let ok = s.rva.is_multiple_of(PAGE)
            && s.pages != 0
            && s.rva >= previous_end
            && end.is_some_and(|e| e <= kernel_span)
            && s.access & !known == 0
            && s.access & SEGMENT_READ != 0
            && s.access & (SEGMENT_WRITE | SEGMENT_EXECUTE) != SEGMENT_WRITE | SEGMENT_EXECUTE
            && s.reserved == 0;
        match (ok, end) {
            (true, Some(e)) => previous_end = e,
            _ => return Err(BootInfoError::Segment(index)),
        }
    }
    Ok(())
}

/// Upper bound on `region_count`, so a corrupt count cannot make a kernel walk off into
/// unmapped memory.
pub const MAX_REGIONS: u64 = 4096;

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> BootInfo {
        BootInfo {
            magic: MAGIC,
            version_major: VERSION.0,
            version_minor: VERSION.1,
            size: core::mem::size_of::<BootInfo>() as u32,
            regions: 0x1000,
            region_count: 3,
            kernel_base: 0x20_0000,
            kernel_span: 0x3000,
            acpi_rsdp: 0,
            uefi_system_table: 0,
            stack_base: 0x40_0000,
            stack_size: 0x1_0000,
            kernel_segments: 0x1060,
            kernel_segment_count: 3,
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

    #[test]
    fn magic_is_origboot() {
        assert_eq!(&MAGIC.to_le_bytes(), b"ORIGBOOT");
    }

    #[test]
    fn check_accepts_a_valid_header_and_rejects_bad_ones() {
        assert_eq!(sample().check(), Ok(()));
        let mut b = sample();
        b.magic ^= 1;
        assert_eq!(b.check(), Err(BootInfoError::BadMagic));
        let mut b = sample();
        b.version_minor = b.version_minor.wrapping_add(1);
        assert!(matches!(b.check(), Err(BootInfoError::Version(..))));
        let mut b = sample();
        b.size = 72;
        assert_eq!(b.check(), Err(BootInfoError::TooSmall(72)));
        for count in [0, MAX_REGIONS + 1] {
            let mut b = sample();
            b.region_count = count;
            assert_eq!(b.check(), Err(BootInfoError::Regions));
        }
        for count in [0, MAX_KERNEL_SEGMENTS + 1] {
            let mut b = sample();
            b.kernel_segment_count = count;
            assert_eq!(b.check(), Err(BootInfoError::Segments));
        }
    }

    #[test]
    fn segment_lists_are_checked_strictly() {
        const R: u32 = SEGMENT_READ;
        const RW: u32 = SEGMENT_READ | SEGMENT_WRITE;
        const RX: u32 = SEGMENT_READ | SEGMENT_EXECUTE;
        let good = [
            segment(0, 1, R),
            segment(0x1000, 2, RX),
            segment(0x4000, 1, RW),
        ];
        assert_eq!(check_segments(&good, 0x5000), Ok(()));
        // The span may extend past the last segment (bss-free gaps are allowed too).
        assert_eq!(check_segments(&good, 0x9000), Ok(()));
        let cases: [(usize, KernelSegment, u64); 9] = [
            (1, segment(0x1800, 1, RX), 0x5000),                 // unaligned
            (1, segment(0x1000, 0, RX), 0x5000),                 // empty
            (1, segment(0, 1, RX), 0x5000),                      // overlaps segment 0
            (2, segment(0x4000, 2, RW), 0x5000),                 // past the span
            (1, segment(0x1000, 2, RX | 8), 0x5000),             // unknown bit
            (1, segment(0x1000, 2, SEGMENT_EXECUTE), 0x5000),    // not readable
            (1, segment(0x1000, 2, RX | SEGMENT_WRITE), 0x5000), // W and X
            (2, segment(!0xFFF_u64, 1, RW), u64::MAX),           // end overflows
            (2, segment(0x4000, u64::MAX, RW), u64::MAX),        // size overflows
        ];
        for (at, bad, span) in cases {
            let mut list = good;
            list[at] = bad;
            assert_eq!(
                check_segments(&list, span),
                Err(BootInfoError::Segment(at)),
                "{bad:?}"
            );
        }
        let mut list = good;
        list[0].reserved = 1;
        assert_eq!(
            check_segments(&list, 0x5000),
            Err(BootInfoError::Segment(0))
        );
    }

    #[test]
    fn region_kinds_round_trip() {
        for raw in 1..=13 {
            assert_eq!(RegionKind::from_raw(raw).map(|k| k as u32), Some(raw));
        }
        assert_eq!(RegionKind::from_raw(0), None);
        assert_eq!(RegionKind::from_raw(14), None);
    }

    #[test]
    fn version_comes_from_origin_version_toml() {
        assert_eq!(VERSION, (0, 2));
    }
}
