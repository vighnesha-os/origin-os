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
    /// Specific-purpose memory (UEFI 2.8 `EFI_MEMORY_SP`), such as high-bandwidth or
    /// CXL-attached memory set aside for one use: never general-purpose RAM.
    SpecificPurpose = 14,
    /// Memory that can be removed while the machine runs (UEFI 2.11
    /// `EFI_MEMORY_HOT_PLUGGABLE`): not used until the kernel can move its contents off it.
    HotPluggable = 15,
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
            14 => Self::SpecificPurpose,
            15 => Self::HotPluggable,
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

/// [`BootInfo::flags`]: the kernel image and direct-map bases were randomized (KASLR,
/// ADR-0004 H3). Clear when the processor offered no entropy source.
pub const FLAG_KASLR: u64 = 1;

/// The largest number of bytes a [`Framebuffer`] may span, in visible rows. A 16384 by 16384
/// display of 32-bit pixels is exactly this; nothing larger exists.
pub const MAX_FRAMEBUFFER_BYTES: u64 = 1 << 30;
/// Largest width or height of a [`Framebuffer`], in pixels.
pub const MAX_FRAMEBUFFER_DIMENSION: u32 = 16384;

/// The firmware's linear framebuffer: boot info 0.4 (ADR-0010 E1, accepted by the owner on
/// 2026-11-08).
///
/// A pixel is one little-endian 32-bit value at `base + y * stride + x * 4`. The channels
/// are given by masks, so the kernel never sees the firmware's format names: the loader turns
/// UEFI's RGB, BGR and bit-mask formats into masks, as it turns the UEFI memory map into
/// Origin's [`RegionKind`]s (ADR-0002 B4).
///
/// `base` is zero when there is no framebuffer to use. The kernel carries on without one
/// (ADR-0010: a missing console never stops the boot).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct Framebuffer {
    /// Physical address of the top-left pixel, or 0 if there is no usable framebuffer.
    pub base: u64,
    /// Bytes the firmware says belong to it (`FrameBufferSize`).
    pub size: u64,
    /// Virtual address of the top-left pixel in the first page tables, or 0 if the loader did
    /// not map it. The kernel's own tables map it at this same address.
    pub virtual_base: u64,
    /// Visible pixels per row.
    pub width: u32,
    /// Visible rows.
    pub height: u32,
    /// Bytes from the start of one row to the start of the next.
    pub stride: u32,
    /// The bits of a pixel that hold red.
    pub red_mask: u32,
    /// The bits of a pixel that hold green.
    pub green_mask: u32,
    /// The bits of a pixel that hold blue.
    pub blue_mask: u32,
}

/// Why a [`Framebuffer`] cannot be drawn on. None of these stops a boot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FramebufferError {
    /// `base` is zero: there is no framebuffer.
    Absent,
    /// A width or height of zero or above [`MAX_FRAMEBUFFER_DIMENSION`].
    Dimensions,
    /// `stride` is not a multiple of 4, or leaves no room for `width` pixels.
    Stride,
    /// `base` is not 4-byte aligned, or the framebuffer would end past 2^52.
    Address,
    /// `stride * height` is above `size` or above [`MAX_FRAMEBUFFER_BYTES`].
    Size,
    /// A channel mask is empty, not contiguous, wider than 16 bits, or overlaps another.
    Masks,
}

impl Framebuffer {
    /// No framebuffer.
    pub const NONE: Self = Self {
        base: 0,
        size: 0,
        virtual_base: 0,
        width: 0,
        height: 0,
        stride: 0,
        red_mask: 0,
        green_mask: 0,
        blue_mask: 0,
    };

    /// Whether the firmware offered a framebuffer at all.
    pub const fn is_present(&self) -> bool {
        self.base != 0
    }

    /// Bytes from the first pixel to the end of the last visible row.
    pub const fn visible_bytes(&self) -> u64 {
        self.stride as u64 * self.height as u64
    }

    /// Checks the description against the contract. Run by the loader before it maps the
    /// framebuffer and by the kernel again before it draws.
    pub fn check(&self) -> Result<(), FramebufferError> {
        use FramebufferError as E;
        if !self.is_present() {
            return Err(E::Absent);
        }
        if self.width == 0
            || self.height == 0
            || self.width > MAX_FRAMEBUFFER_DIMENSION
            || self.height > MAX_FRAMEBUFFER_DIMENSION
        {
            return Err(E::Dimensions);
        }
        if !self.stride.is_multiple_of(4) || u64::from(self.stride) < u64::from(self.width) * 4 {
            return Err(E::Stride);
        }
        let bytes = self.visible_bytes();
        if bytes > self.size || bytes > MAX_FRAMEBUFFER_BYTES {
            return Err(E::Size);
        }
        const PHYSICAL_LIMIT: u64 = 1 << 52;
        let end = self.base.checked_add(bytes);
        if !self.base.is_multiple_of(4) || end.is_none_or(|e| e > PHYSICAL_LIMIT) {
            return Err(E::Address);
        }
        let masks = [self.red_mask, self.green_mask, self.blue_mask];
        let contiguous = |m: u32| {
            m != 0
                && m.count_ones() <= 16
                && (m >> m.trailing_zeros())
                    .checked_add(1)
                    .is_some_and(u32::is_power_of_two)
        };
        let disjoint = self.red_mask & self.green_mask == 0
            && self.red_mask & self.blue_mask == 0
            && self.green_mask & self.blue_mask == 0;
        if !disjoint || !masks.iter().all(|&m| contiguous(m)) {
            return Err(E::Masks);
        }
        Ok(())
    }
}

/// Whether `[base, base + bytes)` touches a region that holds memory rather than device
/// space: everything except [`RegionKind::Mmio`], [`RegionKind::Reserved`] and unknown kinds,
/// which the contract treats as reserved. Firmware says nothing it hands the kernel about a
/// device, such as a framebuffer or a UART, may be drawn on or written to if this is true.
pub fn overlaps_ram(regions: &[MemoryRegion], base: u64, bytes: u64) -> bool {
    let end = base.saturating_add(bytes);
    regions.iter().any(|r| {
        let region_end = r.base.saturating_add(r.pages.saturating_mul(4096));
        let device = matches!(
            RegionKind::from_raw(r.kind),
            None | Some(RegionKind::Reserved | RegionKind::Mmio)
        );
        !device && r.pages != 0 && r.base < end && base < region_end
    })
}

/// Everything the kernel learns from the loader.
///
/// Since 0.3 (ADR-0004) the kernel starts at its higher-half virtual address on page
/// tables the loader built: `kernel_virtual_base` maps the image, `direct_map_base +
/// physical` maps the RAM the kernel may touch. The boot-info pointer in RDI and the
/// initial RSP are direct-map addresses; every address field below is **physical** unless
/// its documentation says otherwise.
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
    /// Lowest address of the stack the kernel starts on. The 4 KiB page below it belongs
    /// to the same `KernelStack` region and is a guard page: the kernel never maps it.
    pub stack_base: u64,
    /// Size of that stack in bytes.
    pub stack_size: u64,
    /// Physical address of a `[KernelSegment; kernel_segment_count]` (version 0.2).
    pub kernel_segments: u64,
    /// Number of entries at `kernel_segments`, between 1 and [`MAX_KERNEL_SEGMENTS`].
    pub kernel_segment_count: u64,
    /// Physical address of the PML4 the loader built and loaded into CR3 (0.3).
    pub page_table_root: u64,
    /// Virtual address of physical address 0 in the direct map (0.3).
    pub direct_map_base: u64,
    /// Virtual address of the kernel image's RVA 0 (0.3); `kernel_base` is its physical
    /// address.
    pub kernel_virtual_base: u64,
    /// [`FLAG_KASLR`] and reserved zero bits (0.3).
    pub flags: u64,
    /// The firmware's linear framebuffer (0.4); [`Framebuffer::NONE`] if there is none.
    pub framebuffer: Framebuffer,
    /// Physical address of the loader's secret area, [`BOOT_SECRET_LEN`] bytes from its
    /// random-number generator inside the region that holds this structure, or 0 if the loader
    /// has none to give (0.6; ADR-0016 R10 and its Amendment 1, decision item 166). The area
    /// is two things, one after the other:
    ///
    /// - [`BOOT_SEED_LEN`] bytes that the kernel mixes into its generator's first seed;
    /// - [`BOOT_STACK_VALUE_LEN`] bytes, a little-endian `u64`, from which the kernel places
    ///   its stacks' region when it builds its page tables, before its generator exists.
    ///
    /// The kernel takes each once and overwrites the whole area where it lies. Neither side
    /// prints any of it: this field is the area's address and nothing of its contents.
    pub boot_seed: u64,
}

/// The seed for the kernel's generator: the first bytes of the area [`BootInfo::boot_seed`]
/// names.
pub const BOOT_SEED_LEN: u64 = 64;
/// The random value for the kernel stack region's base, directly after the seed (decision
/// item 166).
pub const BOOT_STACK_VALUE_LEN: u64 = 8;
/// The whole secret area [`BootInfo::boot_seed`] names: the seed, then the stack value.
pub const BOOT_SECRET_LEN: u64 = BOOT_SEED_LEN + BOOT_STACK_VALUE_LEN;

/// Where the loader's secret area is, if `info` names one the kernel may take:
/// [`BOOT_SECRET_LEN`] bytes that lie inside one region of kind [`RegionKind::BootInfo`] and
/// clear of the header at physical address `info_addr`, of the region array and of the kernel
/// segment array. Anything else is no area: the kernel reads and overwrites those bytes, so it
/// must know they are the loader's to give and nothing else's.
pub fn boot_seed(info: &BootInfo, info_addr: u64, regions: &[MemoryRegion]) -> Option<u64> {
    let seed = info.boot_seed;
    let end = seed.checked_add(BOOT_SECRET_LEN)?;
    if seed == 0 {
        return None;
    }
    let inside = regions.iter().any(|r| {
        let region_end = r.base.saturating_add(r.pages.saturating_mul(4096));
        RegionKind::from_raw(r.kind) == Some(RegionKind::BootInfo)
            && r.base <= seed
            && end <= region_end
    });
    let clear_of = |base: u64, bytes: u64| end <= base || base.saturating_add(bytes) <= seed;
    let header = core::mem::size_of::<BootInfo>() as u64;
    let region = core::mem::size_of::<MemoryRegion>() as u64;
    let segment = core::mem::size_of::<KernelSegment>() as u64;
    (inside
        && clear_of(info_addr, header)
        && clear_of(info.regions, info.region_count.saturating_mul(region))
        && clear_of(
            info.kernel_segments,
            info.kernel_segment_count.saturating_mul(segment),
        ))
    .then_some(seed)
}

// The ABI layout; a change here is a boot-info major version change.
const _: () = {
    use core::mem::{offset_of, size_of};
    assert!(size_of::<MemoryRegion>() == 24);
    assert!(offset_of!(Framebuffer, base) == 0);
    assert!(offset_of!(Framebuffer, size) == 8);
    assert!(offset_of!(Framebuffer, virtual_base) == 16);
    assert!(offset_of!(Framebuffer, width) == 24);
    assert!(offset_of!(Framebuffer, height) == 28);
    assert!(offset_of!(Framebuffer, stride) == 32);
    assert!(offset_of!(Framebuffer, red_mask) == 36);
    assert!(offset_of!(Framebuffer, green_mask) == 40);
    assert!(offset_of!(Framebuffer, blue_mask) == 44);
    assert!(size_of::<Framebuffer>() == 48);
    assert!(offset_of!(BootInfo, framebuffer) == 128);
    assert!(offset_of!(BootInfo, version_major) == 8);
    assert!(offset_of!(BootInfo, size) == 12);
    assert!(offset_of!(BootInfo, regions) == 16);
    assert!(offset_of!(BootInfo, kernel_base) == 32);
    assert!(offset_of!(BootInfo, acpi_rsdp) == 48);
    assert!(offset_of!(BootInfo, stack_size) == 72);
    assert!(offset_of!(BootInfo, kernel_segments) == 80);
    assert!(offset_of!(BootInfo, kernel_segment_count) == 88);
    assert!(offset_of!(BootInfo, page_table_root) == 96);
    assert!(offset_of!(BootInfo, direct_map_base) == 104);
    assert!(offset_of!(BootInfo, kernel_virtual_base) == 112);
    assert!(offset_of!(BootInfo, flags) == 120);
    assert!(offset_of!(BootInfo, boot_seed) == 176);
    assert!(size_of::<BootInfo>() == 184);
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
            page_table_root: 0x9_0000,
            direct_map_base: 0xFFFF_8000_0000_0000,
            kernel_virtual_base: 0xFFFF_FFFF_8000_0000,
            flags: FLAG_KASLR,
            framebuffer: Framebuffer::NONE,
            boot_seed: 0,
        }
    }

    /// A 1280 by 800 display in the blue-green-red byte order QEMU's VGA uses.
    fn display() -> Framebuffer {
        Framebuffer {
            base: 0x8000_0000,
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
        for raw in 1..=15 {
            assert_eq!(RegionKind::from_raw(raw).map(|k| k as u32), Some(raw));
        }
        assert_eq!(RegionKind::from_raw(14), Some(RegionKind::SpecificPurpose));
        assert_eq!(RegionKind::from_raw(15), Some(RegionKind::HotPluggable));
        assert_eq!(RegionKind::from_raw(0), None);
        assert_eq!(RegionKind::from_raw(16), None);
    }

    #[test]
    fn version_comes_from_origin_version_toml() {
        assert_eq!(VERSION, (0, 6));
    }

    #[test]
    fn a_boot_seed_is_taken_only_from_inside_the_boot_info_region() {
        // The header at 0x8000, 184 bytes; three regions at 0x8100; one segment at 0x8200; the
        // boot-info region is the two pages from 0x8000.
        let region = |base, pages, kind: RegionKind| MemoryRegion {
            base,
            pages,
            kind: kind as u32,
            reserved: 0,
        };
        let regions = [
            region(0x1000, 7, RegionKind::Usable),
            region(0x8000, 2, RegionKind::BootInfo),
            region(0xA000, 16, RegionKind::KernelStack),
        ];
        let mut info = sample();
        info.regions = 0x8100;
        info.region_count = 3;
        info.kernel_segments = 0x8200;
        info.kernel_segment_count = 1;
        let at = |seed: u64| {
            let mut info = info;
            info.boot_seed = seed;
            boot_seed(&info, 0x8000, &regions)
        };
        // The area is the 64-byte seed and the 8-byte stack value: 72 bytes.
        assert_eq!(
            (BOOT_SEED_LEN, BOOT_STACK_VALUE_LEN, BOOT_SECRET_LEN),
            (64, 8, 72)
        );
        assert_eq!(at(0), None, "the loader gave none");
        assert_eq!(at(0x9000), Some(0x9000));
        assert_eq!(at(0x9FB8), Some(0x9FB8), "the last 72 bytes of the region");
        assert_eq!(at(0x9FB9), None, "one byte past the region's end");
        assert_eq!(
            at(0x9FC0),
            None,
            "the seed would fit, and the stack value after it would not"
        );
        assert_eq!(at(0x7FFF), None, "begins before the region");
        assert_eq!(at(0x2000), None, "in usable memory");
        assert_eq!(at(0xA000), None, "in the kernel's stack");
        assert_eq!(at(u64::MAX - 10), None, "wraps");
        assert_eq!(
            at(u64::MAX - 71),
            None,
            "its end is the last address: no region holds it"
        );
        // Clear of the header, the region array and the segment array.
        assert_eq!(at(0x8000), None, "on the header");
        assert_eq!(at(0x80B7), None, "the header's last byte");
        assert_eq!(
            at(0x80B8),
            Some(0x80B8),
            "right after the header, up to the region array"
        );
        assert_eq!(
            at(0x80B9),
            None,
            "its last byte is the region array's first"
        );
        assert_eq!(at(0x8100 + 71), None, "the region array's last byte");
        assert_eq!(at(0x8100 + 72), Some(0x8148));
        assert_eq!(at(0x8200 - 72), Some(0x81B8), "up to the segment array");
        assert_eq!(
            at(0x8200 - 71),
            None,
            "its last byte is the segment array's first"
        );
        assert_eq!(
            at(0x8200 - 64),
            None,
            "the seed ends where the segment array begins, and the stack value is on it"
        );
        assert_eq!(at(0x8200 + 23), None, "the segment array's last byte");
        assert_eq!(at(0x8200 + 24), Some(0x8218));
        // A region of another kind at the same place gives nothing.
        let mut others = regions;
        others[1].kind = RegionKind::LoaderReclaimable as u32;
        info.boot_seed = 0x9000;
        assert_eq!(boot_seed(&info, 0x8000, &others), None);
    }

    #[test]
    fn a_framebuffer_description_is_checked_strictly() {
        use FramebufferError as E;
        type Mutate = fn(&mut Framebuffer);
        assert_eq!(display().check(), Ok(()));
        assert_eq!(Framebuffer::NONE.check(), Err(E::Absent));
        assert_eq!(display().visible_bytes(), 800 * 5120);

        // Red in the middle byte, 10-bit channels, a 16-bit channel: all contiguous and fine.
        let mut tall = display();
        tall.red_mask = 0x3FF0_0000;
        tall.green_mask = 0x000F_FC00;
        tall.blue_mask = 0x0000_03FF;
        assert_eq!(tall.check(), Ok(()));

        let cases: [(Mutate, E); 15] = [
            (|f| f.width = 0, E::Dimensions),
            (|f| f.height = 0, E::Dimensions),
            (|f| f.width = 16385, E::Dimensions),
            (|f| f.height = 16385, E::Dimensions),
            (|f| f.stride = 5121, E::Stride),
            (|f| f.stride = 5116, E::Stride), // one pixel short of width * 4
            (|f| f.size = 800 * 5120 - 1, E::Size),
            (
                |f| {
                    f.width = 16384;
                    f.height = 16384;
                    f.stride = 65540;
                    f.size = u64::MAX;
                },
                E::Size,
            ),
            (|f| f.base = 0x8000_0002, E::Address),
            (|f| f.base = u64::MAX - 3, E::Address),
            (|f| f.base = (1 << 52) - 0x1000, E::Address),
            (|f| f.red_mask = 0, E::Masks),
            (|f| f.green_mask = 0x00FF_0000, E::Masks), // overlaps red
            (|f| f.blue_mask = 0x0000_0505, E::Masks),  // not contiguous
            (|f| f.green_mask = 0xFFFF_FF00, E::Masks), // wider than 16 bits
        ];
        for (mutate, expected) in cases {
            let mut f = display();
            mutate(&mut f);
            assert_eq!(f.check(), Err(expected), "{f:?}");
        }
    }

    #[test]
    fn a_device_range_never_overlaps_ram() {
        let region = |base, pages, kind: RegionKind| MemoryRegion {
            base,
            pages,
            kind: kind as u32,
            reserved: 0,
        };
        let map = [
            region(0x0, 0xA0, RegionKind::Usable),
            region(0x10_0000, 0x100, RegionKind::LoaderReclaimable),
            region(0x30_0000, 0x10, RegionKind::Reserved),
            region(0xFEC0_0000, 1, RegionKind::Mmio),
            region(0x1_0000_0000, 0x100, RegionKind::Usable),
        ];
        // Inside RAM or crossing into it.
        assert!(overlaps_ram(&map, 0x1000, 8));
        assert!(overlaps_ram(&map, 0x9_FFFC, 8));
        assert!(overlaps_ram(&map, 0x0F_FFFC, 8));
        assert!(overlaps_ram(&map, 0x1_0000_0000 - 4, 8));
        // A reserved region, device space and holes in the map are not RAM.
        assert!(!overlaps_ram(&map, 0x30_0000, 0x1000));
        assert!(!overlaps_ram(&map, 0xFEC0_0000, 8));
        assert!(!overlaps_ram(&map, 0xA_0000, 8));
        assert!(!overlaps_ram(&map, 0xFEDC_9000, 0x20));
        // An unknown kind counts as reserved, and an empty region covers nothing.
        let other = [MemoryRegion {
            base: 0x10_0000,
            pages: 1,
            kind: 99,
            reserved: 0,
        }];
        assert!(!overlaps_ram(&other, 0x10_0000, 8));
        let empty = [region(0x10_0000, 0, RegionKind::Usable)];
        assert!(!overlaps_ram(&empty, 0x10_0000, 8));
        // A range running off the top of the address space is cut there, not wrapped round.
        assert!(!overlaps_ram(&map, u64::MAX - 7, 64));
    }
}
