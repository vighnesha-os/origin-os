//! UTF-8 decoding for the native text engine (CLAUDE.md §51 UTF-8, text editing and
//! selection). The core is a streaming, error-locating decoder: [`decode_first`] reads one
//! Unicode scalar from a byte slice and reports either how many bytes it used, that the
//! input is truncated, or that the bytes are ill-formed and how many to skip. On top of it,
//! [`validate`] mirrors the standard-library `Utf8Error` shape and [`chars_lossy`] iterates
//! scalars, substituting one U+FFFD per maximal ill-formed subpart (the Unicode-recommended
//! behaviour, matching `String::from_utf8_lossy`).
//!
//! Unlike `core::str::from_utf8`, this works on partial and corrupt input — the text engine
//! must decode a file, a paste or terminal bytes without discarding everything at the first
//! bad byte, and must know *where* the error is. Pure, no `unsafe`, no allocation.
//!
//! Well-formed UTF-8 (Unicode 18.0 §3.9, Table 3-7): overlong encodings, UTF-16 surrogates
//! (U+D800..U+DFFF) and scalars above U+10FFFF are all rejected by constraining the second
//! byte per lead byte.

/// The Unicode replacement character, substituted for ill-formed input.
pub const REPLACEMENT: char = '\u{FFFD}';

/// Why [`decode_first`] could not return a scalar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// The bytes are a valid prefix but the sequence is cut off; the input needs more bytes.
    /// Carries how many bytes are present (a valid partial sequence).
    Incomplete(usize),
    /// The bytes are ill-formed. Carries the length of the maximal valid subpart to skip for
    /// resynchronisation (at least 1), which is also the count of bytes one U+FFFD replaces.
    Invalid(usize),
}

/// A validation failure, shaped like the standard library's `Utf8Error`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Utf8Error {
    /// The number of bytes from the start that are valid UTF-8.
    pub valid_up_to: usize,
    /// The length of the ill-formed sequence at `valid_up_to`, or `None` if the input simply
    /// ended in the middle of a sequence.
    pub error_len: Option<usize>,
}

/// Decodes the first Unicode scalar in `bytes`, returning it and the number of bytes it
/// occupied, or why it could not (see [`DecodeError`]).
pub fn decode_first(bytes: &[u8]) -> Result<(char, usize), DecodeError> {
    let Some(&b0) = bytes.first() else {
        return Err(DecodeError::Incomplete(0));
    };
    if b0 < 0x80 {
        return Ok((b0 as char, 1)); // ASCII
    }
    // The sequence length and the allowed range of the *second* byte, which is what rejects
    // overlong encodings (0xE0, 0xF0), surrogates (0xED) and out-of-range scalars (0xF4).
    let (len, second_lo, second_hi): (usize, u8, u8) = match b0 {
        0xC2..=0xDF => (2, 0x80, 0xBF),
        0xE0 => (3, 0xA0, 0xBF),
        0xE1..=0xEC | 0xEE..=0xEF => (3, 0x80, 0xBF),
        0xED => (3, 0x80, 0x9F),
        0xF0 => (4, 0x90, 0xBF),
        0xF1..=0xF3 => (4, 0x80, 0xBF),
        0xF4 => (4, 0x80, 0x8F),
        // 0x80..=0xC1 (continuation or overlong lead) and 0xF5..=0xFF are never valid leads.
        _ => return Err(DecodeError::Invalid(1)),
    };

    let mut code = u32::from(b0 & (0x7F >> len)); // the lead byte's payload bits
    for i in 1..len {
        let (lo, hi) = if i == 1 {
            (second_lo, second_hi)
        } else {
            (0x80, 0xBF)
        };
        match bytes.get(i) {
            // A valid partial sequence that ran out of input: i bytes are present.
            None => return Err(DecodeError::Incomplete(i)),
            Some(&b) if (lo..=hi).contains(&b) => code = (code << 6) | u32::from(b & 0x3F),
            // The maximal valid subpart is the i bytes decoded so far.
            Some(_) => return Err(DecodeError::Invalid(i)),
        }
    }
    // The range constraints guarantee a valid scalar, but construct it checked anyway.
    match char::from_u32(code) {
        Some(c) => Ok((c, len)),
        None => Err(DecodeError::Invalid(1)),
    }
}

/// Validates that `bytes` are entirely well-formed UTF-8, reporting the first error's
/// position and length ([`Utf8Error`]) otherwise. Agrees with `core::str::from_utf8`.
pub fn validate(bytes: &[u8]) -> Result<(), Utf8Error> {
    let mut i = 0;
    while i < bytes.len() {
        match decode_first(&bytes[i..]) {
            Ok((_, n)) => i += n,
            Err(DecodeError::Invalid(n)) => {
                return Err(Utf8Error {
                    valid_up_to: i,
                    error_len: Some(n),
                });
            }
            Err(DecodeError::Incomplete(_)) => {
                return Err(Utf8Error {
                    valid_up_to: i,
                    error_len: None,
                });
            }
        }
    }
    Ok(())
}

/// Whether `bytes` are entirely well-formed UTF-8.
pub fn is_valid(bytes: &[u8]) -> bool {
    validate(bytes).is_ok()
}

/// An iterator over the Unicode scalars of a byte slice, substituting one [`REPLACEMENT`]
/// scalar for each maximal ill-formed subpart. Equivalent to `String::from_utf8_lossy`'s
/// character sequence, but without allocating.
#[derive(Clone, Debug)]
pub struct CharsLossy<'a> {
    bytes: &'a [u8],
}

impl Iterator for CharsLossy<'_> {
    type Item = char;

    fn next(&mut self) -> Option<char> {
        if self.bytes.is_empty() {
            return None;
        }
        match decode_first(self.bytes) {
            Ok((c, n)) => {
                self.bytes = &self.bytes[n..];
                Some(c)
            }
            Err(DecodeError::Invalid(skip)) => {
                self.bytes = &self.bytes[skip..];
                Some(REPLACEMENT)
            }
            // A truncated trailing sequence is one replacement for the remaining bytes.
            Err(DecodeError::Incomplete(_)) => {
                self.bytes = &[];
                Some(REPLACEMENT)
            }
        }
    }
}

/// Iterates the Unicode scalars of `bytes`, replacing ill-formed sequences with [`REPLACEMENT`].
pub fn chars_lossy(bytes: &[u8]) -> CharsLossy<'_> {
    CharsLossy { bytes }
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use super::*;
    use alloc::string::String;
    use alloc::vec::Vec;

    #[test]
    fn decodes_every_sequence_length() {
        // 1-byte ASCII, 2-byte é, 3-byte ₹, 4-byte 😀.
        assert_eq!(decode_first(b"A"), Ok(('A', 1)));
        assert_eq!(decode_first("é".as_bytes()), Ok(('é', 2)));
        assert_eq!(decode_first("₹".as_bytes()), Ok(('₹', 3)));
        assert_eq!(decode_first("😀".as_bytes()), Ok(('😀', 4)));
        // The length is the scalar's own; trailing bytes are not consumed.
        assert_eq!(decode_first("é-tail".as_bytes()), Ok(('é', 2)));
        // The full boundary scalars.
        assert_eq!(decode_first("\u{0080}".as_bytes()), Ok(('\u{0080}', 2)));
        assert_eq!(decode_first("\u{07FF}".as_bytes()), Ok(('\u{07FF}', 2)));
        assert_eq!(decode_first("\u{0800}".as_bytes()), Ok(('\u{0800}', 3)));
        assert_eq!(decode_first("\u{FFFF}".as_bytes()), Ok(('\u{FFFF}', 3)));
        assert_eq!(decode_first("\u{10000}".as_bytes()), Ok(('\u{10000}', 4)));
        assert_eq!(decode_first("\u{10FFFF}".as_bytes()), Ok(('\u{10FFFF}', 4)));
    }

    #[test]
    fn rejects_ill_formed_sequences() {
        use DecodeError::*;
        assert_eq!(decode_first(&[0x80]), Err(Invalid(1)), "stray continuation");
        assert_eq!(
            decode_first(&[0xC0, 0x80]),
            Err(Invalid(1)),
            "overlong 2-byte"
        );
        assert_eq!(
            decode_first(&[0xC1, 0xBF]),
            Err(Invalid(1)),
            "overlong 2-byte"
        );
        assert_eq!(
            decode_first(&[0xE0, 0x80, 0x80]),
            Err(Invalid(1)),
            "overlong 3-byte"
        );
        assert_eq!(
            decode_first(&[0xED, 0xA0, 0x80]),
            Err(Invalid(1)),
            "surrogate U+D800"
        );
        assert_eq!(
            decode_first(&[0xF0, 0x80, 0x80, 0x80]),
            Err(Invalid(1)),
            "overlong 4-byte"
        );
        assert_eq!(
            decode_first(&[0xF4, 0x90, 0x80, 0x80]),
            Err(Invalid(1)),
            "above U+10FFFF"
        );
        assert_eq!(decode_first(&[0xF5]), Err(Invalid(1)), "invalid lead");
        // A bad continuation after a good one: the maximal subpart is 2 bytes.
        assert_eq!(decode_first(&[0xE2, 0x82, 0x28]), Err(Invalid(2)));
        assert_eq!(decode_first(&[0xE2, 0x28, 0x82]), Err(Invalid(1)));
        // Truncation reports how many valid bytes are present, not an error length.
        assert_eq!(decode_first(&[0xE2, 0x82]), Err(Incomplete(2)));
        assert_eq!(decode_first(&[0xF0, 0x9F]), Err(Incomplete(2)));
        assert_eq!(decode_first(&[]), Err(Incomplete(0)));
    }

    #[test]
    fn validate_reports_the_first_error() {
        assert_eq!(validate("okay ∀x".as_bytes()), Ok(()));
        // "ab" + overlong + "cd": valid up to 2, a 1-byte error.
        let mut v = Vec::new();
        v.extend_from_slice(b"ab");
        v.extend_from_slice(&[0xC0, 0x80]);
        v.extend_from_slice(b"cd");
        assert_eq!(
            validate(&v),
            Err(Utf8Error {
                valid_up_to: 2,
                error_len: Some(1)
            })
        );
        // A truncated tail has no error length.
        assert_eq!(
            validate(&[b'x', 0xE2, 0x82]),
            Err(Utf8Error {
                valid_up_to: 1,
                error_len: None
            })
        );
    }

    #[test]
    fn lossy_substitutes_one_replacement_per_subpart() {
        // 0xE2 0x82 is a truncated euro sign before 'x': one U+FFFD, then 'x'.
        let got: String = chars_lossy(&[0xE2, 0x82, b'x']).collect();
        assert_eq!(got, "\u{FFFD}x");
        // Two separate bad bytes become two replacements.
        let got: String = chars_lossy(&[0xFF, 0xFE]).collect();
        assert_eq!(got, "\u{FFFD}\u{FFFD}");
        // Valid text passes through unchanged.
        let got: String = chars_lossy("héllo ∀😀".as_bytes()).collect();
        assert_eq!(got, "héllo ∀😀");
    }

    /// A deterministic xorshift byte source, biased toward lead/continuation bytes so the
    /// multi-byte paths and their boundaries are exercised.
    fn fuzz_bytes(seed: u64, out: &mut Vec<u8>) {
        let mut x = seed | 1;
        let len = (seed % 24) as usize;
        out.clear();
        for _ in 0..len {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            // Half the time a byte in 0x80..=0xF7 (leads and continuations), half anywhere.
            let b = if x & 1 == 0 {
                0x80 + (x >> 8) as u8 % 0x78
            } else {
                (x >> 16) as u8
            };
            out.push(b);
        }
    }

    #[test]
    fn matches_std_across_fuzzed_input() {
        let mut bytes = Vec::new();
        for seed in 0..20_000u64 {
            fuzz_bytes(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15), &mut bytes);

            // validate() must agree with core::str exactly: ok/err, position and length.
            match core::str::from_utf8(&bytes) {
                Ok(_) => assert_eq!(validate(&bytes), Ok(()), "{bytes:02x?}"),
                Err(e) => {
                    let got = validate(&bytes).unwrap_err();
                    assert_eq!(got.valid_up_to, e.valid_up_to(), "{bytes:02x?}");
                    assert_eq!(got.error_len, e.error_len(), "{bytes:02x?}");
                }
            }

            // chars_lossy() must match String::from_utf8_lossy exactly.
            let lossy: String = chars_lossy(&bytes).collect();
            assert_eq!(lossy, String::from_utf8_lossy(&bytes), "{bytes:02x?}");

            // A valid slice decodes to the same scalars core::str yields.
            if let Ok(s) = core::str::from_utf8(&bytes) {
                assert!(chars_lossy(&bytes).eq(s.chars()), "{bytes:02x?}");
            }
        }
    }
}
