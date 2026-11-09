//! Placing a verified KRN image in memory: segment contents, zero-filled tails and RELR
//! relocations (ADR-0002 B3; ONBF §6.1).

use onbf_format::{Flags, Image, Kind};
use origin_boot_info::{KernelSegment, SEGMENT_EXECUTE, SEGMENT_READ, SEGMENT_WRITE};

/// Why an image could not be placed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaceError {
    /// The image has no SEGMENT sections.
    NoSegments,
    /// The destination is not exactly the image span.
    DestinationSize {
        /// The span in bytes.
        expected: u64,
        /// The destination length.
        found: u64,
    },
    /// A RELOCS section's length is not a multiple of 8.
    RelocsLength(u16),
    /// A RELR bitmap word appears before any address word.
    BitmapFirst(u16),
    /// A relocation slot is not 8-byte aligned.
    Misaligned(u64),
    /// A relocation slot is not inside the content of a writable, non-executable segment.
    NotWritable(u64),
    /// A relocation location runs past the top of the address space.
    Overflow(u64),
    /// The image has more segments than the boot info can describe.
    TooManySegments,
}

/// CR4.LA57 (Intel SDM Vol. 3A §2.5): the processor walks 5-level tables.
const CR4_LA57: u64 = 1 << 12;

/// Whether `cr4` has 5-level paging on. The loader's tables are 4-level (ADR-0004 H1): with
/// LA57 set the processor would walk their root as a PML5 and fault with no way to report it.
pub const fn five_level_paging(cr4: u64) -> bool {
    cr4 & CR4_LA57 != 0
}

/// The boot-info access bits for a segment with ONBF `flags`. RELRO drops write access:
/// the loader has applied every relocation before the kernel maps its image.
pub const fn segment_access(flags: Flags) -> u32 {
    let read = if flags.has(Flags::READ) {
        SEGMENT_READ
    } else {
        0
    };
    let write = if flags.has(Flags::WRITE) && !flags.has(Flags::RELRO) {
        SEGMENT_WRITE
    } else {
        0
    };
    let execute = if flags.has(Flags::EXECUTE) {
        SEGMENT_EXECUTE
    } else {
        0
    };
    read | write | execute
}

/// Writes the image's segments to `out` in section-table order, which `onbf-format` has
/// checked is sorted by RVA, page-sized and disjoint (errata G4), and returns how many
/// there are (ADR-0002 B4, boot info 0.2).
pub fn kernel_segments(image: &Image<'_>, out: &mut [KernelSegment]) -> Result<usize, PlaceError> {
    let mut count = 0;
    for s in image.sections().filter(|s| s.entry.kind() == Kind::Segment) {
        let slot = out.get_mut(count).ok_or(PlaceError::TooManySegments)?;
        *slot = KernelSegment {
            rva: s.entry.mem_offset,
            pages: s.entry.mem_size / 4096,
            access: segment_access(s.entry.flags),
            reserved: 0,
        };
        count += 1;
    }
    if count == 0 {
        return Err(PlaceError::NoSegments);
    }
    Ok(count)
}

/// The image span: the end of the highest segment, as an RVA.
pub fn span(image: &Image<'_>) -> Result<u64, PlaceError> {
    image
        .sections()
        .filter(|s| s.entry.kind() == Kind::Segment)
        .map(|s| s.entry.mem_offset.saturating_add(s.entry.mem_size))
        .max()
        .ok_or(PlaceError::NoSegments)
}

fn copy(dst: &mut [u8], src: &[u8]) {
    for (d, s) in dst.iter_mut().zip(src) {
        *d = *s;
    }
}

/// True if the 8 bytes at `rva` lie inside the stored content of a SEGMENT that is
/// writable and not executable.
fn writable_slot(image: &Image<'_>, rva: u64) -> bool {
    image.sections().any(|s| {
        let e = &s.entry;
        e.kind() == Kind::Segment
            && e.flags.has(Flags::WRITE)
            && !e.flags.has(Flags::EXECUTE)
            && rva >= e.mem_offset
            && rva.checked_add(8).is_some_and(|end| {
                e.mem_offset
                    .checked_add(e.content_size)
                    .is_some_and(|c| end <= c)
            })
    })
}

/// Adds `base` to the u64 at `rva` in `dest`, after checking the slot. The addition wraps:
/// a negative addend is stored as u64 two's complement (errata P5), so a slot legitimately
/// holds a value that only comes back into range once the base is added.
fn relocate(
    writable: &impl Fn(u64) -> bool,
    dest: &mut [u8],
    base: u64,
    rva: u64,
) -> Result<(), PlaceError> {
    if !rva.is_multiple_of(8) {
        return Err(PlaceError::Misaligned(rva));
    }
    if !writable(rva) {
        return Err(PlaceError::NotWritable(rva));
    }
    let start = usize::try_from(rva).map_err(|_| PlaceError::NotWritable(rva))?;
    let slot = start
        .checked_add(8)
        .and_then(|end| dest.get_mut(start..end))
        .ok_or(PlaceError::NotWritable(rva))?;
    let mut word = [0u8; 8];
    copy(&mut word, slot);
    let value = u64::from_le_bytes(word).wrapping_add(base);
    copy(slot, &value.to_le_bytes());
    Ok(())
}

/// Applies one RELOCS section's RELR words (the SHT_RELR scheme, ONBF §6.1). `writable`
/// says whether the 8 bytes at an RVA may be relocated. Returns the number of slots
/// relocated.
pub fn apply_relr(
    writable: &impl Fn(u64) -> bool,
    index: u16,
    words: &[u8],
    dest: &mut [u8],
    base: u64,
) -> Result<u64, PlaceError> {
    let (chunks, rest) = words.as_chunks::<8>();
    if !rest.is_empty() {
        return Err(PlaceError::RelocsLength(index));
    }
    let mut next: Option<u64> = None; // the location after the last address word
    let mut count = 0u64;
    for chunk in chunks {
        let word = u64::from_le_bytes(*chunk);
        if word & 1 == 0 {
            relocate(writable, dest, base, word)?;
            count = count.saturating_add(1);
            next = Some(word.checked_add(8).ok_or(PlaceError::Overflow(word))?);
        } else {
            let at = next.ok_or(PlaceError::BitmapFirst(index))?;
            let mut bits = word >> 1;
            let mut slot = at;
            while bits != 0 {
                if bits & 1 == 1 {
                    relocate(writable, dest, base, slot)?;
                    count = count.saturating_add(1);
                }
                bits >>= 1;
                slot = slot.checked_add(8).ok_or(PlaceError::Overflow(slot))?;
            }
            next = Some(at.checked_add(63 * 8).ok_or(PlaceError::Overflow(at))?);
        }
    }
    Ok(count)
}

/// Writes the image into `dest`, which is its span and will live at physical/virtual
/// address `base`: zero-fills it, copies every segment's content to its RVA, and applies
/// every RELOCS section. Returns the number of relocations applied.
pub fn place(image: &Image<'_>, dest: &mut [u8], base: u64) -> Result<u64, PlaceError> {
    let expected = span(image)?;
    let found = u64::try_from(dest.len()).unwrap_or(u64::MAX);
    if found != expected {
        return Err(PlaceError::DestinationSize { expected, found });
    }
    dest.fill(0);
    for s in image.sections().filter(|s| s.entry.kind() == Kind::Segment) {
        let start = usize::try_from(s.entry.mem_offset).map_err(|_| PlaceError::NoSegments)?;
        if let Some(dst) = start
            .checked_add(s.stored.len())
            .and_then(|end| dest.get_mut(start..end))
        {
            copy(dst, s.stored);
        }
    }
    let writable = |rva| writable_slot(image, rva);
    let mut total = 0u64;
    for s in image.sections().filter(|s| s.entry.kind() == Kind::Relocs) {
        total = total.saturating_add(apply_relr(&writable, s.index, s.stored, dest, base)?);
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::AtomicU64;
    use onbf_format::ParseOptions;
    use origin_boot_info::{MemoryRegion, RegionKind, SEGMENT_READ};
    use origin_paging::layout::{self, Placement};
    use origin_paging::{Table, TablePool};

    const BASE: u64 = 0x20_0000;

    fn words(ws: &[u64]) -> Vec<u8> {
        ws.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    /// A 4 KiB image whose first page is writable and holds `i` in slot `i`.
    fn dest() -> Vec<u8> {
        (0..512u64).flat_map(|i| i.to_le_bytes()).collect()
    }

    fn slot(d: &[u8], i: usize) -> u64 {
        u64::from_le_bytes(d[i * 8..i * 8 + 8].try_into().unwrap())
    }

    const ALL: fn(u64) -> bool = |rva| rva + 8 <= 4096;

    /// A developer-mode KRN container: an 8-byte manifest, an RX page, a RELRO page, an RW page
    /// and a RELOCS section with one slot in each writable page. Digests are not looked at here.
    fn relro_image() -> Vec<u8> {
        let code = Flags::REQUIRED | Flags::MAPPABLE | Flags::READ | Flags::EXECUTE;
        let relro = Flags::REQUIRED | Flags::MAPPABLE | Flags::READ | Flags::WRITE | Flags::RELRO;
        let data = Flags::REQUIRED | Flags::MAPPABLE | Flags::READ | Flags::WRITE;
        let (manifest, relocs) = (4096usize, 16448usize);
        let mut out = vec![0u8; relocs + 16];
        out[..8].copy_from_slice(&onbf_format::MAGIC);
        out[8..10].copy_from_slice(&1u16.to_le_bytes());
        out[12..14].copy_from_slice(&(onbf_format::Profile::Krn as u16).to_le_bytes());
        out[14..16].copy_from_slice(&128u16.to_le_bytes());
        out[20..24].copy_from_slice(&5u32.to_le_bytes());
        out[24..32].copy_from_slice(&128u64.to_le_bytes());
        out[112..114].copy_from_slice(&1u16.to_le_bytes());
        let mut entry = |i: usize, kind: u32, flags: u32, at: usize, len: u64, rva: u64| {
            let e = 128 + 128 * i;
            out[e..e + 4].copy_from_slice(&kind.to_le_bytes());
            out[e + 4..e + 8].copy_from_slice(&flags.to_le_bytes());
            out[e + 8..e + 16].copy_from_slice(&(at as u64).to_le_bytes());
            out[e + 16..e + 24].copy_from_slice(&len.to_le_bytes());
            out[e + 24..e + 32].copy_from_slice(&len.to_le_bytes());
            out[e + 32..e + 40].copy_from_slice(&rva.to_le_bytes());
            let mem_size = if kind == 0x10 { 4096 } else { len };
            out[e + 40..e + 48].copy_from_slice(&mem_size.to_le_bytes());
            out[e + 49] = u8::from(kind == 0x10); // integrity: Merkle
            out[e + 52..e + 56].copy_from_slice(&onbf_format::NO_MERKLE.to_le_bytes());
        };
        entry(0, 0x01, Flags::REQUIRED, manifest, 8, 0);
        entry(1, 0x10, code, 8192, 64, 0);
        entry(2, 0x10, relro, 12288, 64, 0x1000);
        entry(3, 0x10, data, 16384, 64, 0x2000);
        entry(4, 0x11, Flags::REQUIRED, relocs, 16, 0);
        // The two addends, and the RELR words naming their slots.
        out[12288 + 16..12288 + 24].copy_from_slice(&0x10u64.to_le_bytes());
        out[16384 + 32..16384 + 40].copy_from_slice(&0x20u64.to_le_bytes());
        out[relocs..relocs + 8].copy_from_slice(&0x1010u64.to_le_bytes());
        out[relocs + 8..relocs + 16].copy_from_slice(&0x2020u64.to_le_bytes());
        out
    }

    /// ADR-0001 default 9, errata L1: the loader relocates a RELRO segment first, then hands it to
    /// the kernel without write access, and the tables built from that list, the loader's and
    /// (with the same `layout::build`) the kernel's own, map it read-only.
    #[test]
    fn a_relro_segment_is_relocated_and_then_mapped_read_only() {
        let bytes = relro_image();
        let options = ParseOptions {
            developer_mode: true,
            ..ParseOptions::default()
        };
        let image = Image::parse(&bytes, options).unwrap();
        let virtual_base = 0xFFFF_FFFF_8460_0000u64;
        let mut memory = vec![0u8; span(&image).unwrap() as usize];
        assert_eq!(place(&image, &mut memory, virtual_base), Ok(2));
        assert_eq!(
            slot(&memory, 0x1010 / 8),
            virtual_base + 0x10,
            "the RELRO slot"
        );
        assert_eq!(
            slot(&memory, 0x2020 / 8),
            virtual_base + 0x20,
            "the RW slot"
        );

        let mut list = [KernelSegment {
            rva: 0,
            pages: 0,
            access: 0,
            reserved: 0,
        }; 16];
        let count = kernel_segments(&image, &mut list).unwrap();
        let list = &list[..count];
        assert_eq!(
            list.iter().map(|s| s.access).collect::<Vec<_>>(),
            [
                SEGMENT_READ | SEGMENT_EXECUTE,
                SEGMENT_READ,
                SEGMENT_READ | SEGMENT_WRITE
            ]
        );

        let pool: Vec<Table> = (0..64)
            .map(|_| core::array::from_fn(|_| AtomicU64::new(0)))
            .collect();
        let mut tables = TablePool::new(&pool, 0x80_0000);
        let at = Placement {
            direct_map_base: 0xFFFF_8040_0000_0000,
            kernel_virtual_base: virtual_base,
            kernel_physical_base: 0x20_0000,
            stack_guard: 0,
        };
        let regions = [MemoryRegion {
            base: 0x10_0000,
            pages: 16,
            kind: RegionKind::Usable as u32,
            reserved: 0,
        }];
        let (space, _) = layout::build(&mut tables, &at, &regions, list).unwrap();
        let relro = space.translate(&tables, virtual_base + 0x1010).unwrap();
        assert!(!relro.writable && !relro.executable, "{relro:?}");
        let data = space.translate(&tables, virtual_base + 0x2020).unwrap();
        assert!(data.writable && !data.executable, "{data:?}");
    }

    #[test]
    fn five_level_paging_is_bit_12_of_cr4() {
        assert!(five_level_paging(1 << 12));
        assert!(five_level_paging(u64::MAX));
        assert!(!five_level_paging(0));
        assert!(!five_level_paging(!(1 << 12)));
    }

    #[test]
    fn segment_access_follows_the_flags_and_relro_drops_write() {
        let (r, w, x, relro) = (Flags::READ, Flags::WRITE, Flags::EXECUTE, Flags::RELRO);
        let cases = [
            (r, SEGMENT_READ),
            (r | x, SEGMENT_READ | SEGMENT_EXECUTE),
            (r | w, SEGMENT_READ | SEGMENT_WRITE),
            (r | w | relro, SEGMENT_READ),
            (x, SEGMENT_EXECUTE),
            (0, 0),
        ];
        for (flags, access) in cases {
            let required = Flags::REQUIRED | Flags::MAPPABLE;
            assert_eq!(
                segment_access(Flags(flags | required)),
                access,
                "{flags:#x}"
            );
        }
    }

    #[test]
    fn address_words_relocate_single_slots() {
        let mut d = dest();
        assert_eq!(
            apply_relr(&ALL, 0, &words(&[0, 16 * 8]), &mut d, BASE),
            Ok(2)
        );
        assert_eq!(slot(&d, 0), BASE);
        assert_eq!(slot(&d, 16), 16 + BASE);
        assert_eq!(slot(&d, 1), 1, "untouched");
    }

    #[test]
    fn bitmaps_cover_the_63_slots_after_the_last_address() {
        // Address 0 relocates slot 0; the next location is slot 1. Bitmap bits 1..63 cover
        // slots 1..63; set every bit, then a second bitmap continues at slot 64.
        let full = u64::MAX; // bit 0 marks a bitmap; bits 1..63 all set
        let second = (1u64 << 1) | (1u64 << 3) | 1; // slots 64 and 66
        let mut d = dest();
        assert_eq!(
            apply_relr(&ALL, 0, &words(&[0, full, second]), &mut d, BASE),
            Ok(1 + 63 + 2)
        );
        for i in 0..64 {
            assert_eq!(slot(&d, i), i as u64 + BASE, "slot {i}");
        }
        assert_eq!(slot(&d, 64), 64 + BASE);
        assert_eq!(slot(&d, 65), 65, "bit clear");
        assert_eq!(slot(&d, 66), 66 + BASE);
    }

    #[test]
    fn malformed_relr_is_rejected() {
        let mut d = dest();
        assert_eq!(
            apply_relr(&ALL, 3, &[0u8; 12], &mut d, BASE),
            Err(PlaceError::RelocsLength(3))
        );
        assert_eq!(
            apply_relr(&ALL, 3, &words(&[0b11]), &mut d, BASE),
            Err(PlaceError::BitmapFirst(3))
        );
        assert_eq!(
            apply_relr(&ALL, 0, &words(&[4]), &mut d, BASE),
            Err(PlaceError::Misaligned(4))
        );
        // Slot outside the writable range: an address, and a bitmap that runs past it.
        assert_eq!(
            apply_relr(&ALL, 0, &words(&[4096]), &mut d, BASE),
            Err(PlaceError::NotWritable(4096))
        );
        let mut small = vec![0u8; 64];
        let some = |rva: u64| rva < 32;
        assert_eq!(
            apply_relr(&some, 0, &words(&[0, u64::MAX]), &mut small, BASE),
            Err(PlaceError::NotWritable(32))
        );
    }

    /// Errata P5: addends are u64 two's complement, so the loader adds with wrapping.
    #[test]
    fn a_negative_addend_wraps_to_its_address() {
        let kernel_base = 0xFFFF_FFFF_8460_0000_u64;
        let mut d = dest();
        d[..8].copy_from_slice(&(-0x1000_i64 as u64).to_le_bytes());
        d[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(
            apply_relr(&ALL, 0, &words(&[0, 0b11]), &mut d, kernel_base),
            Ok(2)
        );
        assert_eq!(slot(&d, 0), kernel_base - 0x1000);
        assert_eq!(slot(&d, 1), kernel_base - 1);
    }
}
