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
