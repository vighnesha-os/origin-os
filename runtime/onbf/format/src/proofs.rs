//! Kani's proofs of `onbf-format`'s parts (ONBF §13 item 5; CLAUDE.md §73; TASKS item 143,
//! step 3). Built only by `cargo kani` (`cfg(kani)`), which no build of the loader, the kernel
//! or any image sets. `check_padding` is proved in `image::proofs`, which can reach it.
//!
//! # What is proved, and within which bounds
//!
//! **P1** for each function below, on every input within its bound: it returns, and Kani
//! finds no panic, no index, slice or `get` out of bounds, no arithmetic overflow, no division
//! by zero, and no loop past its unwinding bound (the unwinding assertions are on). The crate
//! is safe Rust under `forbid(unsafe_code)`, so a read out of bounds would be a panic: P1
//! covers "never reads out of bounds" (ONBF §13 item 5). Release builds of the loader do not
//! check overflow today: `[profile.release]` sets no `overflow-checks`, and CLAUDE.md §14's
//! rule that they do lands with TASKS item 22's second half, not started. So an overflow in
//! these functions would wrap silently in today's loader, not stop it, and P1's absence of
//! overflow within the bounds is worth more for it: none would wrap either.
//!
//! | Function | Harness | Bound | Also proved |
//! |---|---|---|---|
//! | `Header::parse` | [`header_parse_any_bytes`] | every input of up to 128 bytes | P2: §4.2's rules, read from the raw bytes, and the fields it returns are those bytes |
//! | `SectionEntry::parse` | [`section_entry_parse_any_entry`] | every 128-byte entry, every profile, nested or not | P2: §4.3's flags and reserved fields, Appendix A's 1 TiB |
//! | `merkle_hash_blocks` | [`merkle_hash_blocks_any_size`] | every `u64` | |
//! | `align_up` | [`align_up_any_value`] | every value, every alignment | the next multiple, for every power of two |
//! | `slice` | [`slice_any_range`] | every input of up to 16 bytes, every range | `Some` exactly for a range inside |
//! | `ocbor::validate`, then `Cursor::next_item` and `skip_contents` of that item | [`ocbor_any_document`] | every input of up to 6 bytes | |
//! | the same | [`ocbor_validate_then_cursor_read`] | every input of up to 4 bytes | P3: an accepted document reads as one item that ends at its end |
//! | `SignatureBlock::parse` | [`signature_block_any_input`] | every input of up to 19 bytes: its 16-byte header and fewer than the first entry's 4 size bytes, so every input is refused | |
//!
//! **Core's UTF-8 check, stood in for.** `core::str::from_utf8` checks with a word-at-a-time
//! walk whose shape depends on the text's address, and every parser harness that reached it
//! ran past 300 s. Its private `run_utf8_validation` is stubbed; `from_utf8` itself still
//! turns `Ok` into the `&str`, so no harness needs `unsafe`.
//! - [`any_utf8_result`] (P1's harnesses) returns `Ok` or an error freely, for any text. It
//!   can only add behaviours, so P1 under it holds of core's check too. That rests on one
//!   fact of the code: the crate's only UTF-8 call, `ocbor.rs:164`,
//!   `core::str::from_utf8(body).map_err(|_| err(K::InvalidUtf8))`, reads nothing of the error.
//!   The harnesses cover a text that reads and a text that is refused, so both of its results
//!   are explored.
//! - [`validation_by_walk`] (P3's harness, which needs the same answer for the same bytes
//!   twice) is [`utf8_walk`], written from RFC 3629 §4's grammar, which
//!   [`utf8_walk_equals_core`] proves equal to core's `from_utf8` on every text of up to 8
//!   bytes, at each offset from 0 to 4 into its array (core's walk depends on the address):
//!   the same texts accepted, and an error's `valid_up_to` the walk's offset. The stand-in
//!   asserts that each text it is given is within that length; P3's texts lie at offsets 1 to
//!   3 of its 4-byte input.
//!
//! **Not proved by Kani** (the coordinating session's rulings (A) and (D), 2026-11-27; the
//! forms tried and their figures are in TASKS item 143's entry; the work is TASKS item 159):
//! - `Image::parse`, its accessors and `KrnManifest::check_against`: every image it can accept
//!   is at least 4 KiB (§4.1), and every harness over such an image ran past 300 s. P2 for
//!   the image (its layout, §4.1 to §4.4) is not proved either.
//! - `SignatureBlock::parse` past its header: an entry and its OCBOR-1 body. Neither any input
//!   past 19 bytes nor a block of fixed shape whose fields Kani chooses, of one entry or of
//!   nine, came under 300 s. So the 8-entry limit (Appendix A) is not proved.
//! - `KrnManifest::decode`: neither any input of 3 bytes nor a manifest of fixed shape came
//!   under 300 s.
//! - `SectionEntry::parse`'s other rules of §4.3 and §4.4 (the sizes against compression and
//!   memory, compression on a MAPPABLE entry, the integrity modes, `merkle_index`, the memory
//!   layout, a kind against its profile): P1 only.
//! - OCBOR-1 past 6 bytes (P1) and 4 bytes (P3), and the cursor beyond one `next_item` and its
//!   `skip_contents`; the machine code rustc 1.100.0 makes (Kani compiles with its own nightly,
//!   on the host's 64-bit `usize`, the width of x86-64's).
//!
//! **Covers:** each harness covers an accepted input within its bound, or, where its bound
//! reaches only refusals, the deepest refusal it reaches.
#![forbid(unsafe_code)]

use crate::header::{Header, Profile};
use crate::ocbor::{self, Cursor};
use crate::read::{align_up, slice};
use crate::section::{SectionEntry, merkle_hash_blocks};
use crate::signature::{SignatureBlock, SignatureError};
use core::str::Utf8Error;

/// A little-endian field of `bytes` at `at`, read here from the specification's tables; the
/// harnesses use it so that what they check does not go through the parser's own reader.
fn le(bytes: &[u8], at: usize, width: usize) -> u64 {
    let mut value = 0u64;
    let mut i = 0;
    while i < width {
        value |= u64::from(bytes[at + i]) << (8 * i);
        i += 1;
    }
    value
}

/// True if `bytes[at..at + len]` is all zero, read byte by byte.
fn zero(bytes: &[u8], at: usize, len: usize) -> bool {
    let mut i = 0;
    while i < len {
        if bytes[at + i] != 0 {
            return false;
        }
        i += 1;
    }
    true
}

/// Any of the eight profiles (§1).
fn any_profile() -> Profile {
    let id: u16 = kani::any_where(|&id| (1..=8).contains(&id));
    match Profile::from_id(id) {
        Some(p) => p,
        None => {
            kani::assert(false, "every ID from 1 to 8 is a profile");
            Profile::Exe
        }
    }
}

// ---------------------------------------------------------------- the header (§4.2)

/// `Header::parse` on every input of up to 128 bytes: P1, and P2 from §4.2's table.
#[kani::proof]
#[kani::unwind(15)]
fn header_parse_any_bytes() {
    let buf: [u8; 128] = kani::any();
    let len: usize = kani::any_where(|&l| l <= 128);
    let bytes = &buf[..len];
    match Header::parse(bytes) {
        Ok(h) => {
            kani::assert(len == 128, "a header is 128 bytes");
            kani::assert(
                bytes[..8] == [0x89, b'O', b'N', b'B', b'F', 0x0D, 0x0A, 0x1A],
                "§4.2: the magic",
            );
            kani::assert(le(bytes, 8, 2) == 1, "§4.2: format_major 1");
            kani::assert(le(bytes, 14, 2) == 128, "§4.2: header_size 128");
            kani::assert(le(bytes, 16, 4) == 0, "§4.2: no header flags");
            let count = le(bytes, 20, 4);
            kani::assert((1..=256).contains(&count), "§4.2: 1 to 256 sections");
            kani::assert(
                u64::from(h.section_count) == count,
                "the count is the field's",
            );
            kani::assert(le(bytes, 24, 8) == 128, "§4.2: the table at 128");
            let profile = le(bytes, 12, 2);
            kani::assert(
                (1..=8).contains(&profile) && h.profile as u64 == profile,
                "§4.2: a profile of §1, the field's",
            );
            let (offset, size) = (le(bytes, 32, 8), le(bytes, 40, 8));
            kani::assert(
                h.signature_block_offset == offset && h.signature_block_size == size,
                "the signature block's fields are the bytes'",
            );
            kani::assert(size <= 1 << 20, "§4.2: a signature block of at most 1 MiB");
            kani::assert(offset != 0 || size == 0, "no signature block when unsigned");
            kani::assert(le(bytes, 112, 2) == 1, "§4.2: arch 1, x86-64");
            kani::assert(zero(bytes, 114, 14), "§4.2: the reserved field is zero");
            kani::cover(offset != 0, "a signed header is accepted");
            kani::cover(offset == 0, "an unsigned header is accepted");
        }
        Err(_) => kani::cover(len == 128, "a whole header is refused"),
    }
}

// ---------------------------------------------------------------- a section entry (§4.3)

/// `SectionEntry::parse` on every 128 bytes, every profile, nested or not: P1, and P2 from
/// §4.3's table and Appendix A.
#[kani::proof]
#[kani::unwind(33)]
fn section_entry_parse_any_entry() {
    let raw: [u8; 128] = kani::any();
    let profile = any_profile();
    let nested: bool = kani::any();
    if let Ok(e) = SectionEntry::parse(&raw, profile, nested) {
        let flags = le(&raw, 4, 4);
        kani::assert(flags & !0x3F == 0, "§4.3: no flag past RELRO");
        kani::assert(
            flags & 0x18 != 0x18,
            "§4.3: never WRITE and EXECUTE together",
        );
        kani::assert(
            zero(&raw, 50, 2) && zero(&raw, 56, 8) && zero(&raw, 96, 32),
            "§4.3: the reserved fields are zero",
        );
        kani::assert(
            le(&raw, 16, 8) <= 1 << 40 && le(&raw, 24, 8) <= 1 << 40,
            "Appendix A: a section's content is at most 1 TiB",
        );
        kani::assert(
            e.file_offset == le(&raw, 8, 8),
            "file_offset is the field's",
        );
        kani::cover(le(&raw, 0, 4) == 0x10, "a SEGMENT entry is accepted");
        kani::cover(le(&raw, 0, 4) == 0x01, "a MANIFEST entry is accepted");
    }
}

/// `merkle_hash_blocks` for every content size: P1 (its loop ends within 8 rounds).
#[kani::proof]
#[kani::unwind(10)]
fn merkle_hash_blocks_any_size() {
    let size: u64 = kani::any();
    let blocks = merkle_hash_blocks(size);
    kani::cover(blocks > 1, "content with more than one hash block");
    kani::cover(blocks == 0 && size > 0, "content with no hash block");
}

// ---------------------------------------------------------------- alignment and slices (read.rs)

/// `align_up` for every value and every alignment: P1, and for every power-of-two alignment
/// the smallest multiple at or above the value, or `None` only when no such multiple fits in
/// 64 bits.
#[kani::proof]
fn align_up_any_value() {
    let x: u64 = kani::any();
    let shift: u32 = kani::any_where(|&s| s < 64);
    let align = 1u64 << shift;
    match align_up(x, align) {
        Some(y) => {
            kani::assert(
                y >= x && y % align == 0 && y - x < align,
                "the next multiple",
            );
            kani::cover(y > x, "a value is rounded up");
        }
        None => kani::assert(
            x > u64::MAX / align * align,
            "None only when no multiple of the alignment at or above the value fits",
        ),
    }
    let _ = align_up(x, kani::any()); // any alignment, 0 among them: P1 only
}

/// `slice` for every input of up to 16 bytes and every `start` and `end`: P1, and `Some`
/// exactly when `start <= end <= len`, with the range asked for.
#[kani::proof]
#[kani::unwind(18)]
fn slice_any_range() {
    let buf: [u8; 16] = kani::any();
    let len: usize = kani::any_where(|&l| l <= 16);
    let bytes = &buf[..len];
    let (start, end): (u64, u64) = (kani::any(), kani::any());
    let inside = start <= end && end <= len as u64;
    match slice(bytes, start, end) {
        Some(s) => {
            kani::assert(inside, "Some only for a range inside the input");
            kani::assert(
                s.len() as u64 == end - start
                    && core::ptr::eq(s.as_ptr(), bytes.as_ptr().wrapping_add(start as usize)),
                "the range asked for",
            );
            kani::cover(!s.is_empty(), "a non-empty range");
        }
        None => kani::assert(!inside, "None only for a range outside the input"),
    }
}

// ---------------------------------------------------------------- UTF-8 (RFC 3629 §4)

/// Checks `b` against RFC 3629 §4's grammar of UTF-8 (`UTF8-octets`), written here from the
/// grammar's table and not from core's validator: `Ok` if `b` is a sequence of well-formed
/// characters, or `Err(i)` where `i` is the offset of the first character that is not.
fn utf8_walk(b: &[u8]) -> Result<(), usize> {
    let mut i = 0;
    while i < b.len() {
        // The character's length, and the range its second byte must lie in (§4).
        let (n, lo, hi) = match b[i] {
            0x00..=0x7F => (1, 0x80, 0xBF),
            0xC2..=0xDF => (2, 0x80, 0xBF),
            0xE0 => (3, 0xA0, 0xBF),
            0xE1..=0xEC | 0xEE..=0xEF => (3, 0x80, 0xBF),
            0xED => (3, 0x80, 0x9F),
            0xF0 => (4, 0x90, 0xBF),
            0xF1..=0xF3 => (4, 0x80, 0xBF),
            0xF4 => (4, 0x80, 0x8F),
            _ => return Err(i),
        };
        let mut k = 1;
        while k < n {
            let Some(&t) = b.get(i + k) else {
                return Err(i);
            };
            let (l, h) = if k == 1 { (lo, hi) } else { (0x80, 0xBF) };
            if t < l || t > h {
                return Err(i);
            }
            k += 1;
        }
        i += n;
    }
    Ok(())
}

/// The longest text [`utf8_walk_equals_core`] covers, and so the longest [`validation_by_walk`]
/// accepts.
const UTF8_MAX: usize = 8;

/// Core's own `Utf8Error` for a text whose first `k` bytes are valid, computed by
/// `core::str::from_utf8` while compiling (it is a `const fn`), so that a stand-in can return
/// the error core would, without `unsafe` and without calling what it stands in for.
const fn error_at(k: usize) -> Utf8Error {
    let mut text = [b'a'; UTF8_MAX + 1];
    text[k] = 0xFF;
    match core::str::from_utf8(&text) {
        Err(e) => e,
        Ok(_) => panic!("0xFF never starts a UTF-8 character"),
    }
}

/// [`error_at`] for every offset up to [`UTF8_MAX`].
const UTF8_ERRORS: [Utf8Error; UTF8_MAX + 1] = {
    let mut table = [error_at(0); UTF8_MAX + 1];
    let mut k = 1;
    while k <= UTF8_MAX {
        table[k] = error_at(k);
        k += 1;
    }
    table
};

/// The stand-in for core's `run_utf8_validation` in P1's harnesses: `Ok` or an error, freely,
/// for any text (sound for P1; the module's documentation says why). Used only through
/// `#[kani::stub]`.
#[allow(dead_code)]
fn any_utf8_result(_text: &[u8]) -> Result<(), Utf8Error> {
    if kani::any() {
        Ok(())
    } else {
        Err(UTF8_ERRORS[0])
    }
}

/// The stand-in for core's `run_utf8_validation` in P3's harness: it asserts that the text is
/// one [`utf8_walk_equals_core`] covers, then returns `Ok` exactly when [`utf8_walk`] does, or
/// core's error for the same `valid_up_to`. Its `error_len` is always `Some(1)`; the crate
/// reads neither. Used only through `#[kani::stub]`.
#[allow(dead_code)]
fn validation_by_walk(text: &[u8]) -> Result<(), Utf8Error> {
    kani::assert(
        text.len() <= UTF8_MAX,
        "every text validated is one the walk is proved on",
    );
    match utf8_walk(text) {
        Ok(()) => Ok(()),
        Err(k) => Err(UTF8_ERRORS[k]),
    }
}

/// Core's `from_utf8` and [`utf8_walk`] agree on every text of up to [`UTF8_MAX`] bytes, at
/// each offset from 0 to 4 into its array: the same texts are accepted, `Ok`'s `&str` is the
/// text itself, and an error's `valid_up_to` is the walk's offset.
#[kani::proof]
#[kani::unwind(10)]
fn utf8_walk_equals_core() {
    let buf: [u8; UTF8_MAX + 4] = kani::any();
    let offset: usize = kani::any_where(|&o| o <= 4);
    let len: usize = kani::any_where(|&l| l <= UTF8_MAX);
    let text = &buf[offset..offset + len];
    match core::str::from_utf8(text) {
        Ok(s) => {
            kani::assert(
                utf8_walk(text).is_ok(),
                "the walk accepts what core accepts",
            );
            kani::assert(
                s.len() == len && core::ptr::eq(s.as_ptr(), text.as_ptr()),
                "core's Ok is the text itself",
            );
            kani::cover(
                len == 4 && text[0] >= 0xF0,
                "a 4-byte character is accepted",
            );
        }
        Err(e) => kani::assert(
            utf8_walk(text) == Err(e.valid_up_to()),
            "the walk refuses where core does, at the same offset",
        ),
    }
}

// ---------------------------------------------------------------- OCBOR-1 (§5)

/// `ocbor::validate`, then a [`Cursor`]'s `next_item` and `skip_contents`, on every input of
/// up to 6 bytes, validated or not (a caller may hand the cursor anything): P1. At 7 bytes it
/// used 7.9 GB on the development Mac, past the margin of 7.5 (the coordinating session's
/// ruling of 2026-11-27), so the bound is 6.
#[kani::proof]
#[kani::unwind(8)]
#[kani::stub(core::str::validations::run_utf8_validation, any_utf8_result)]
fn ocbor_any_document() {
    let buf: [u8; 6] = kani::any();
    let len: usize = kani::any_where(|&l| l <= 6);
    let doc = &buf[..len];
    let validated = ocbor::validate(doc);
    kani::cover(
        validated.is_ok() && len == 6,
        "a document of 6 bytes is accepted",
    );
    kani::cover(
        matches!(validated, Err(e) if e.kind == ocbor::OcborErrorKind::InvalidUtf8),
        "a text is refused as UTF-8",
    );
    let mut c = Cursor::new(doc);
    if let Ok(item) = c.next_item() {
        kani::cover(
            matches!(item, ocbor::Item::Text(t) if !t.is_empty()),
            "a text is read",
        );
        let _ = c.skip_contents(item);
    }
}

/// P3 on every input of up to 4 bytes: when `ocbor::validate` accepts it, a [`Cursor`] reads
/// one item and its contents without error and ends exactly at the document's end (§5: a
/// document is exactly one item).
#[kani::proof]
#[kani::unwind(6)]
#[kani::stub(core::str::validations::run_utf8_validation, validation_by_walk)]
fn ocbor_validate_then_cursor_read() {
    let buf: [u8; 4] = kani::any();
    let len: usize = kani::any_where(|&l| l <= 4);
    let doc = &buf[..len];
    if ocbor::validate(doc).is_ok() {
        let mut c = Cursor::new(doc);
        match c.next_item() {
            Ok(item) => {
                kani::cover(
                    matches!(item, ocbor::Item::Text(t) if !t.is_empty()),
                    "an accepted text",
                );
                kani::assert(c.skip_contents(item).is_ok(), "P3: its contents read");
                kani::assert(
                    c.position() == len,
                    "P3: the cursor ends at the document's end",
                );
            }
            Err(_) => kani::assert(false, "P3: an accepted document's item reads"),
        }
        kani::cover(len == 4, "a document of 4 bytes is accepted");
    }
}

// ---------------------------------------------------------------- the signature block (§7.1)

/// `SignatureBlock::parse` on every input of up to 19 bytes: P1. Its header is 16 bytes and
/// an entry's size 4 more, so every input here is refused; the harness covers the refusal
/// past the header.
#[kani::proof]
#[kani::unwind(21)]
fn signature_block_any_input() {
    let buf: [u8; 19] = kani::any();
    let len: usize = kani::any_where(|&l| l <= 19);
    let result = SignatureBlock::parse(&buf[..len]);
    kani::cover(
        matches!(result, Err(SignatureError::Truncated)) && len >= 16,
        "a whole header, then too short for an entry's size",
    );
}
