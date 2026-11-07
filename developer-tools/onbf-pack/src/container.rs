//! The canonical ONBF container writer (ONBF §4.1–§4.5, errata C1–C4).
//!
//! Layout: the 128-byte header, the section table, zero padding to the next 4 KiB
//! boundary, then each section at its canonical offset, the end of the previous item
//! rounded up to 4 KiB (MAPPABLE) or 8 bytes (others), with zero padding (C1, C2). Images
//! are written unsigned: `signature_block_offset` and `_size` are 0 (C4), and the file ends
//! with the last section.

use onbf_format::{
    ARCH_X86_64, ENTRY_SIZE, FORMAT_MAJOR, Flags, HEADER_SIZE, MAGIC, MAX_SECTIONS, PAGE_SIZE,
    Profile, SMALL_ALIGN,
};
use origin_crypto::Sha512;

use crate::LayoutError;

/// One section to write. Content is stored uncompressed, so `stored_size` and
/// `content_size` are both `content.len()`.
#[derive(Clone, Debug)]
pub(crate) struct SectionSpec {
    pub(crate) kind: u32,
    pub(crate) flags: u32,
    pub(crate) content: Vec<u8>,
    pub(crate) mem_offset: u64,
    pub(crate) mem_size: u64,
    pub(crate) integrity: u8,
    pub(crate) merkle_index: u32,
    pub(crate) digest: [u8; 32],
}

/// Rounds `x` up to `align`, a power of two.
fn align_up(x: u64, align: u64) -> Option<u64> {
    let mask = align.checked_sub(1)?;
    Some(x.checked_add(mask)? & !mask)
}

fn usize_of(x: u64) -> Result<usize, LayoutError> {
    usize::try_from(x).map_err(|_| LayoutError::TooLarge)
}

/// Copies `src` to `out[at..]`.
fn put(out: &mut [u8], at: usize, src: &[u8]) -> Result<(), LayoutError> {
    let end = at.checked_add(src.len()).ok_or(LayoutError::TooLarge)?;
    out.get_mut(at..end)
        .ok_or(LayoutError::TooLarge)?
        .copy_from_slice(src);
    Ok(())
}

/// Writes a complete unsigned image with `format_minor` from `origin-version`.
pub(crate) fn write(profile: Profile, sections: &[SectionSpec]) -> Result<Vec<u8>, LayoutError> {
    let count = u32::try_from(sections.len()).map_err(|_| LayoutError::TooManySections)?;
    if count == 0 || count > MAX_SECTIONS {
        return Err(LayoutError::TooManySections);
    }
    let table_end = u64::from(count)
        .checked_mul(ENTRY_SIZE)
        .and_then(|t| t.checked_add(HEADER_SIZE))
        .ok_or(LayoutError::TooLarge)?;

    // Canonical offsets (C1).
    let mut pos = align_up(table_end, PAGE_SIZE).ok_or(LayoutError::TooLarge)?;
    let mut offsets = Vec::with_capacity(sections.len());
    for s in sections {
        let align = if s.flags & Flags::MAPPABLE != 0 {
            PAGE_SIZE
        } else {
            SMALL_ALIGN
        };
        let offset = align_up(pos, align).ok_or(LayoutError::TooLarge)?;
        let len = u64::try_from(s.content.len()).map_err(|_| LayoutError::TooLarge)?;
        pos = offset.checked_add(len).ok_or(LayoutError::TooLarge)?;
        offsets.push(offset);
    }
    let mut out = vec![0u8; usize_of(pos)?];

    // Header (§4.2). Unwritten bytes, including every reserved field, stay zero.
    let header_size = u16::try_from(HEADER_SIZE).map_err(|_| LayoutError::TooLarge)?;
    put(&mut out, 0, &MAGIC)?;
    put(&mut out, 8, &FORMAT_MAJOR.to_le_bytes())?;
    put(
        &mut out,
        10,
        &origin_version::ONBF_FORMAT_MINOR.to_le_bytes(),
    )?;
    put(&mut out, 12, &(profile as u16).to_le_bytes())?;
    put(&mut out, 14, &header_size.to_le_bytes())?;
    put(&mut out, 20, &count.to_le_bytes())?;
    put(&mut out, 24, &HEADER_SIZE.to_le_bytes())?;
    put(&mut out, 112, &ARCH_X86_64.to_le_bytes())?;

    // Section table (§4.3) and contents.
    let mut entry = usize_of(HEADER_SIZE)?;
    let entry_size = usize_of(ENTRY_SIZE)?;
    for (s, &offset) in sections.iter().zip(&offsets) {
        let len = u64::try_from(s.content.len()).map_err(|_| LayoutError::TooLarge)?;
        let at = |field: usize| entry.checked_add(field).ok_or(LayoutError::TooLarge);
        put(&mut out, at(0)?, &s.kind.to_le_bytes())?;
        put(&mut out, at(4)?, &s.flags.to_le_bytes())?;
        put(&mut out, at(8)?, &offset.to_le_bytes())?;
        put(&mut out, at(16)?, &len.to_le_bytes())?;
        put(&mut out, at(24)?, &len.to_le_bytes())?;
        put(&mut out, at(32)?, &s.mem_offset.to_le_bytes())?;
        put(&mut out, at(40)?, &s.mem_size.to_le_bytes())?;
        put(&mut out, at(48)?, &[0, s.integrity])?;
        put(&mut out, at(52)?, &s.merkle_index.to_le_bytes())?;
        put(&mut out, at(64)?, &s.digest)?;
        put(&mut out, usize_of(offset)?, &s.content)?;
        entry = entry.checked_add(entry_size).ok_or(LayoutError::TooLarge)?;
    }

    // Content digest (§4.5): SHA-512(header[0..32] ‖ header[112..128] ‖ section table).
    let table = out
        .get(usize_of(HEADER_SIZE)?..usize_of(table_end)?)
        .ok_or(LayoutError::TooLarge)?;
    let mut h = Sha512::new();
    h.update(out.get(0..32).ok_or(LayoutError::TooLarge)?);
    h.update(out.get(112..128).ok_or(LayoutError::TooLarge)?);
    h.update(table);
    let digest = h.finalize();
    put(&mut out, 48, &digest)?;
    Ok(out)
}
