//! Programs the kernel carries in its own image (ADR-0011 Amendment 2, L1; the note to ONBF
//! §8.3; task S4-1).
//!
//! Until the Loader Service exists (milestone M6), step 4's first-party programs reach ring 3
//! inside the KRN image, whose signature covers them. The build lays each out
//! (`onbf-pack carried`) as a [`Carried`]: its segments' bytes with every relocation's addend in
//! place, each segment's permissions, its entry and its relocation slots. For the kernel's own
//! target that description is compiled in here; nothing is parsed at run time.
//!
//! What is here is arithmetic, tested on the host: [`check`] holds a description to the rules
//! the launch path relies on, [`random_base`] chooses where a program goes, and [`fill_page`]
//! builds one page of it at a base, relocations applied. `launch.rs` maps those pages into a
//! new process, each with its final permissions, before its first thread exists.
//!
//! Where programs go is the kernel's own layout until the Loader Service decides it (M6), and is
//! not an interface: every base and every stack at a random page of `[PROGRAM_LOW,
//! PROGRAM_HIGH)`, below the start records' region (`launch.rs`).
#![forbid(unsafe_code)]

/// Bytes in a page.
pub const PAGE: u64 = 4096;
/// The lowest address a carried program or its stack may use: clear of the low pages, where a
/// null pointer plus a small offset lands.
pub const PROGRAM_LOW: u64 = 0x0000_0000_1000_0000;
/// One past the last address a carried program or its stack may use: the start of the start
/// records' region (`launch::RECORD_LOW`).
pub const PROGRAM_HIGH: u64 = 0x0000_1000_0000_0000;
/// The largest span a program may have (ONBF §6.1: at most 2 GiB, for the small code model).
pub const MAX_SPAN: u64 = 1 << 31;

/// How the launch path maps a segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegmentAccess {
    /// Read-only.
    Read,
    /// Read and execute: the program's code, the one range its system calls may come from
    /// (ADR-0005 A6).
    ReadExecute,
    /// Read and write, never executable.
    ReadWrite,
    /// Read-only to the program, its relocations written by the launch path first (RELRO).
    ReadAfterRelocation,
}

/// One segment of a carried program.
#[derive(Clone, Copy, Debug)]
pub struct Segment {
    /// Its offset from the program's base, page-aligned.
    pub rva: u64,
    /// The bytes it maps, a whole number of pages.
    pub size: u64,
    /// How it is mapped.
    pub access: SegmentAccess,
    /// Its first bytes, addends in place; the rest of `size` is zero.
    pub bytes: &'static [u8],
}

/// A program, as the build laid it out.
#[derive(Clone, Copy, Debug)]
pub struct Carried {
    /// Its name, for the boot's lines.
    pub name: &'static str,
    /// Its entry point's offset from the base.
    pub entry: u64,
    /// The bytes from the base to the end of its last segment.
    pub span: u64,
    /// Its segments, ascending and apart.
    pub segments: &'static [Segment],
    /// Each relocation slot's offset from the base, strictly ascending: the launch path adds the
    /// base to the 8 bytes there.
    pub relocations: &'static [u64],
}

/// Why a description cannot be launched. The build never writes one; [`check`] makes sure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutError {
    /// A segment is not page-aligned, overlaps the one before, holds more bytes than it maps,
    /// or ends past the span.
    Segment,
    /// Not exactly one read-execute segment, or the entry is not in it.
    Code,
    /// The span is zero, not page-aligned, or over [`MAX_SPAN`].
    Span,
    /// A relocation slot is out of order, misaligned, not in a writable or RELRO segment's
    /// bytes, or its value plus the base overflows.
    Relocation,
}

/// Whether `carried` keeps every rule the launch path relies on: page-aligned segments in
/// ascending order, each within the span and holding no more bytes than it maps; exactly one
/// read-execute segment, holding the entry; a span of whole pages up to [`MAX_SPAN`]; and
/// relocation slots strictly ascending, 8-byte aligned, each inside the bytes of a writable or
/// RELRO segment.
pub fn check(carried: &Carried) -> Result<(), LayoutError> {
    if carried.span == 0 || !carried.span.is_multiple_of(PAGE) || carried.span > MAX_SPAN {
        return Err(LayoutError::Span);
    }
    let mut end = 0;
    for s in carried.segments {
        let fits = s.rva.is_multiple_of(PAGE)
            && s.size.is_multiple_of(PAGE)
            && s.size != 0
            && s.rva >= end
            && s.bytes.len() as u64 <= s.size
            && s.rva.checked_add(s.size).is_some_and(|e| e <= carried.span);
        if !fits {
            return Err(LayoutError::Segment);
        }
        end = s.rva + s.size;
    }
    let mut code = carried
        .segments
        .iter()
        .filter(|s| s.access == SegmentAccess::ReadExecute);
    match (code.next(), code.next()) {
        (Some(s), None) if carried.entry >= s.rva && carried.entry - s.rva < s.size => {}
        _ => return Err(LayoutError::Code),
    }
    let mut previous = None;
    for &slot in carried.relocations {
        let in_bytes = carried.segments.iter().any(|s| {
            matches!(
                s.access,
                SegmentAccess::ReadWrite | SegmentAccess::ReadAfterRelocation
            ) && slot
                .checked_sub(s.rva)
                .and_then(|at| at.checked_add(8))
                .is_some_and(|end| end <= s.bytes.len() as u64)
        });
        if !slot.is_multiple_of(8) || previous.is_some_and(|p| slot <= p) || !in_bytes {
            return Err(LayoutError::Relocation);
        }
        previous = Some(slot);
    }
    Ok(())
}

/// The read-execute segment at `base`: `[start, end)`, the range ADR-0005 A6 accepts its system
/// calls from. `None` if [`check`] would refuse `carried`.
#[must_use]
pub fn code_range(carried: &Carried, base: u64) -> Option<(u64, u64)> {
    let s = carried
        .segments
        .iter()
        .find(|s| s.access == SegmentAccess::ReadExecute)?;
    let start = base.checked_add(s.rva)?;
    Some((start, start.checked_add(s.size)?))
}

/// The page-aligned place in `[PROGRAM_LOW, PROGRAM_HIGH)` at which `pages` pages start, from a
/// random value `random`: every page that fits is a possible answer. `None` if they do not fit.
#[must_use]
pub fn random_base(pages: u64, random: u64) -> Option<u64> {
    let bytes = pages.checked_mul(PAGE)?;
    let slots = ((PROGRAM_HIGH - PROGRAM_LOW).checked_sub(bytes)? / PAGE).checked_add(1)?;
    Some(PROGRAM_LOW + (random % slots) * PAGE)
}

/// Builds page `page` of segment `segment` of `carried`, placed at `base`, into `out` (one page):
/// the segment's bytes there, zero after them, and `base` added to every relocation slot on the
/// page. `carried` must have passed [`check`].
pub fn fill_page(
    carried: &Carried,
    segment: usize,
    page: u64,
    base: u64,
    out: &mut [u8],
) -> Result<(), LayoutError> {
    let s = carried.segments.get(segment).ok_or(LayoutError::Segment)?;
    if out.len() as u64 != PAGE || page >= s.size / PAGE {
        return Err(LayoutError::Segment);
    }
    out.fill(0);
    let start = usize::try_from(page * PAGE).map_err(|_| LayoutError::Segment)?;
    if let Some(bytes) = s.bytes.get(start..) {
        let bytes = bytes.get(..out.len()).unwrap_or(bytes);
        out.get_mut(..bytes.len())
            .ok_or(LayoutError::Segment)?
            .copy_from_slice(bytes);
    }
    let low = s.rva + page * PAGE;
    let first = carried.relocations.partition_point(|&slot| slot < low);
    for &slot in carried
        .relocations
        .get(first..)
        .unwrap_or(&[])
        .iter()
        .take_while(|&&slot| slot < low + PAGE)
    {
        let at = usize::try_from(slot - low).map_err(|_| LayoutError::Relocation)?;
        let word = out.get_mut(at..at + 8).ok_or(LayoutError::Relocation)?;
        let value = u64::from_le_bytes(word.try_into().map_err(|_| LayoutError::Relocation)?)
            .checked_add(base)
            .ok_or(LayoutError::Relocation)?;
        word.copy_from_slice(&value.to_le_bytes());
    }
    Ok(())
}

/// The value the slot at offset `slot` should hold once `carried` is placed at `base`: its
/// addend, from the description's bytes, plus the base. `None` if no segment holds the slot.
#[must_use]
pub fn relocated(carried: &Carried, slot: u64, base: u64) -> Option<u64> {
    let s = carried
        .segments
        .iter()
        .find(|s| slot.checked_sub(s.rva).is_some_and(|at| at < s.size))?;
    let at = usize::try_from(slot - s.rva).ok()?;
    let word = s.bytes.get(at..at.checked_add(8)?)?;
    u64::from_le_bytes(word.try_into().ok()?).checked_add(base)
}

// The descriptions `onbf-pack carried` wrote for this build (`cargo xtask` names their directory
// in ORIGIN_CARRIED_PROGRAMS), and what each program is given, written beside its image
// (ADR-0005 G6).
#[cfg(target_os = "none")]
include!(concat!(
    env!(
        "ORIGIN_CARRIED_PROGRAMS",
        "build the kernel with `cargo xtask`, which builds the programs it carries and names their descriptions' directory in ORIGIN_CARRIED_PROGRAMS"
    ),
    "/first_program.rs"
));

/// What the first program is given (ADR-0005 G6), each kind deliberately: its three start
/// handles, room for the one message it writes, and the developer calls, since it reports to its
/// boot proof; no channels of its own, memory objects, mappings, events or ports; and eight
/// pages of stack.
#[cfg(target_os = "none")]
pub(crate) const FIRST_PROGRAM_RUN: crate::launch::ProgramLimits = crate::launch::ProgramLimits {
    limits: crate::process::Limits {
        handles: 3,
        channels: 0,
        message_bytes: 64,
        debug_calls: true,
        memory: crate::process::MemoryLimits::NONE,
        objects: crate::quota::ObjectLimits::NONE,
    },
    stack_pages: 8,
};

#[cfg(test)]
mod tests;
