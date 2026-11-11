//! The seed area: where a processor's start-up draws are gathered and a first seed is put
//! together (ADR-0016 R5).
//!
//! The area is [`AREA_LEN`] bytes the caller lends for as long as a processor starts. It is one
//! run of bytes, so that each of the generator's three inputs is one slice of it:
//!
//! ```text
//! | head, right-aligned: label, numbers, extra | region 0: 1,024 draws | region 1: 1,024 draws |
//! 0                                          HEAD                 HEAD + REGION           AREA_LEN
//! ```
//!
//! - **Region 0** holds the 1,024 draws of the first instruction that is healthy on the
//!   processor: RDSEED where it is, RDRAND otherwise.
//! - **Region 1** holds the 1,024 draws of the second healthy instruction. On a root with one
//!   healthy instruction it holds four further draws of that one, for the nonce.
//! - **The head** ends where region 0 begins, so a processor's personalization string, which is
//!   the head and then its own draws, is one slice.
//!
//! | Input | A root (the boot processor, the loader) | A processor's generator |
//! |---|---|---|
//! | Entropy input | region 0 | 64 bytes of the root's output |
//! | Nonce | region 1: all of it, or the four further draws | 16 further bytes of the root's output |
//! | Personalization string | a label, the time-stamp counter, and the 64 extra bytes when there are any | a label, the processor's index, the time-stamp counter, then each full region |
//!
//! Numbers are little-endian. A draw is stored as its eight bytes, low byte first.

use crate::wipe::copy;

/// Room for the head.
pub const HEAD: usize = 128;
/// One region: 1,024 draws of eight bytes.
pub const REGION: usize = 8192;
/// The whole area.
pub const AREA_LEN: usize = HEAD + 2 * REGION;
/// The extra bytes a root may be given: the loader's seed for the kernel's root (R10), the
/// firmware's bytes for the loader's.
pub const EXTRA_LEN: usize = 64;
/// Further draws that make a root's nonce when one instruction is healthy (R5).
pub const NONCE_DRAWS: u16 = 4;

/// What tells a root's first seed from anything else these bytes could be.
const ROOT_LABEL: &[u8] = b"Origin OS random root, 1";
/// The same for a processor's generator.
const PROCESSOR_LABEL: &[u8] = b"Origin OS random processor, 1";

// The longest head fits: a root's label, eight bytes and the extra bytes; a processor's label
// and sixteen bytes.
const _: () = assert!(ROOT_LABEL.len() + 8 + EXTRA_LEN <= HEAD);
const _: () = assert!(PROCESSOR_LABEL.len() + 16 <= HEAD);
const _: () = assert!(REGION == crate::health::STARTUP_DRAWS as usize * 8);

/// Where the caller puts a root's extra bytes before the root is started: the last
/// [`EXTRA_LEN`] bytes of the head. `None` if `area` is not a whole seed area.
pub fn extra_slot(area: &mut [u8]) -> Option<&mut [u8]> {
    if area.len() < AREA_LEN {
        return None;
    }
    area.get_mut(HEAD.checked_sub(EXTRA_LEN)?..HEAD)
}

/// The eight bytes of draw `n` of `region`.
pub(crate) fn draw_slot(area: &mut [u8], region: u8, n: u16) -> Option<&mut [u8]> {
    if region > 1 {
        return None;
    }
    let start = HEAD
        .checked_add(usize::from(region).checked_mul(REGION)?)?
        .checked_add(usize::from(n).checked_mul(8)?)?;
    let end = start.checked_add(8)?;
    if end > HEAD.checked_add(usize::from(region).checked_add(1)?.checked_mul(REGION)?)? {
        return None;
    }
    area.get_mut(start..end)
}

/// The bytes of `region`.
pub(crate) fn region(area: &mut [u8], region: u8) -> Option<&mut [u8]> {
    if region > 1 {
        return None;
    }
    let start = HEAD.checked_add(usize::from(region).checked_mul(REGION)?)?;
    area.get_mut(start..start.checked_add(REGION)?)
}

/// Writes `parts` one after another so that the last ends at [`HEAD`], and returns how long
/// they are together. `None` if they do not fit.
fn write_head(area: &mut [u8], parts: &[&[u8]]) -> Option<usize> {
    let mut len = 0usize;
    for part in parts {
        len = len.checked_add(part.len())?;
    }
    let mut at = HEAD.checked_sub(len)?;
    for part in parts {
        let end = at.checked_add(part.len())?;
        copy(area.get_mut(at..end)?, part);
        at = end;
    }
    Some(len)
}

/// Writes a root's head and returns its length. With `extra`, the caller has already put the
/// extra bytes in [`extra_slot`], and they stay where they are.
pub(crate) fn write_root_head(area: &mut [u8], tsc: u64, extra: bool) -> Option<usize> {
    let tsc = tsc.to_le_bytes();
    if extra {
        // The label and the number go in front of the extra bytes.
        let front = ROOT_LABEL.len().checked_add(tsc.len())?;
        let mut at = HEAD.checked_sub(EXTRA_LEN)?.checked_sub(front)?;
        for part in [ROOT_LABEL, &tsc] {
            let end = at.checked_add(part.len())?;
            copy(area.get_mut(at..end)?, part);
            at = end;
        }
        front.checked_add(EXTRA_LEN)
    } else {
        write_head(area, &[ROOT_LABEL, &tsc])
    }
}

/// Writes a processor's head and returns its length.
pub(crate) fn write_processor_head(area: &mut [u8], index: u32, tsc: u64) -> Option<usize> {
    write_head(
        area,
        &[
            PROCESSOR_LABEL,
            &u64::from(index).to_le_bytes(),
            &tsc.to_le_bytes(),
        ],
    )
}
