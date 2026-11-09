//! UTF-16 transcoding (CLAUDE.md §51 text engine; UEFI/CHAR16 interop). Converts between
//! Unicode scalars and UTF-16 code units, the companion to [`crate::text`]'s UTF-8 decoder.
//! Scalars above U+FFFF use a surrogate pair; lone or mis-ordered surrogates are rejected.
//! UEFI strings are NUL-terminated UTF-16, so this is also the codec the loader's and
//! kernel's firmware-string handling builds on. Pure, no `unsafe`.

/// The Unicode replacement character, substituted for ill-formed units in the lossy path.
pub const REPLACEMENT: char = '\u{FFFD}';

/// Why [`decode_first`] could not return a scalar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Utf16Error {
    /// A high surrogate with no following unit (the input ended mid-pair).
    Incomplete,
    /// A lone low surrogate, or a high surrogate not followed by a low one.
    Invalid,
}

/// Encodes one scalar as one or two UTF-16 units. Returns the units and how many are used
/// (the second is 0 and unused for a BMP scalar).
pub fn encode_scalar(c: char) -> ([u16; 2], usize) {
    let u = u32::from(c);
    if u < 0x1_0000 {
        ([u as u16, 0], 1)
    } else {
        let v = u - 0x1_0000;
        ([0xD800 + (v >> 10) as u16, 0xDC00 + (v & 0x3FF) as u16], 2)
    }
}

/// Decodes the first scalar from `units`, returning it and how many units it used, or why it
/// could not (see [`Utf16Error`]). An empty slice is [`Utf16Error::Incomplete`].
pub fn decode_first(units: &[u16]) -> Result<(char, usize), Utf16Error> {
    let hi = *units.first().ok_or(Utf16Error::Incomplete)?;
    match hi {
        0xD800..=0xDBFF => match units.get(1).copied() {
            None => Err(Utf16Error::Incomplete),
            Some(lo @ 0xDC00..=0xDFFF) => {
                let c = 0x1_0000 + ((u32::from(hi - 0xD800) << 10) | u32::from(lo - 0xDC00));
                char::from_u32(c).map(|c| (c, 2)).ok_or(Utf16Error::Invalid)
            }
            Some(_) => Err(Utf16Error::Invalid), // high surrogate not followed by a low one
        },
        0xDC00..=0xDFFF => Err(Utf16Error::Invalid), // a lone low surrogate
        _ => char::from_u32(u32::from(hi))
            .map(|c| (c, 1))
            .ok_or(Utf16Error::Invalid),
    }
}

/// Encodes every scalar of `s` as UTF-16 into `out`; returns the number of units written, or
/// `None` if `out` is too small (nothing partial is relied upon).
pub fn encode_str_into(s: &str, out: &mut [u16]) -> Option<usize> {
    let mut n = 0;
    for c in s.chars() {
        let (units, used) = encode_scalar(c);
        out.get_mut(n..n + used)?.copy_from_slice(&units[..used]);
        n += used;
    }
    Some(n)
}

/// An iterator over the scalars of a UTF-16 unit slice, substituting [`REPLACEMENT`] for
/// each ill-formed unit (one per lone/incomplete surrogate), like `char::decode_utf16`
/// mapped through `unwrap_or(REPLACEMENT)`.
#[derive(Clone, Debug)]
pub struct DecodeLossy<'a> {
    units: &'a [u16],
}

impl Iterator for DecodeLossy<'_> {
    type Item = char;

    fn next(&mut self) -> Option<char> {
        if self.units.is_empty() {
            return None;
        }
        match decode_first(self.units) {
            Ok((c, n)) => {
                self.units = &self.units[n..];
                Some(c)
            }
            // A bad unit (lone/incomplete surrogate): one replacement, skip one unit.
            Err(_) => {
                self.units = &self.units[1..];
                Some(REPLACEMENT)
            }
        }
    }
}

/// Iterates the scalars of `units`, replacing ill-formed surrogates with [`REPLACEMENT`].
pub fn decode_lossy(units: &[u16]) -> DecodeLossy<'_> {
    DecodeLossy { units }
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use super::*;
    use alloc::string::String;
    use alloc::vec::Vec;

    #[test]
    fn encodes_and_decodes_bmp_and_supplementary() {
        // 'A' (BMP), '€' (BMP), '😀' U+1F600 (surrogate pair).
        assert_eq!(encode_scalar('A'), ([0x0041, 0], 1));
        assert_eq!(encode_scalar('€').1, 1);
        assert_eq!(encode_scalar('😀'), ([0xD83D, 0xDE00], 2));

        assert_eq!(decode_first(&[0x0041]), Ok(('A', 1)));
        assert_eq!(decode_first(&[0xD83D, 0xDE00]), Ok(('😀', 2)));
        assert_eq!(decode_first(&[0x20AC]), Ok(('€', 1)));
        // Boundary scalars.
        assert_eq!(
            decode_first(&encode_scalar('\u{FFFF}').0[..1]),
            Ok(('\u{FFFF}', 1))
        );
        let pair = encode_scalar('\u{10FFFF}').0;
        assert_eq!(decode_first(&pair), Ok(('\u{10FFFF}', 2)));
    }

    #[test]
    fn rejects_bad_surrogates() {
        assert_eq!(
            decode_first(&[0xDC00]),
            Err(Utf16Error::Invalid),
            "lone low"
        );
        assert_eq!(
            decode_first(&[0xD800]),
            Err(Utf16Error::Incomplete),
            "high at end"
        );
        assert_eq!(
            decode_first(&[0xD800, 0x0041]),
            Err(Utf16Error::Invalid),
            "high then non-low"
        );
        assert_eq!(decode_first(&[]), Err(Utf16Error::Incomplete));
    }

    #[test]
    fn encode_into_reports_a_small_buffer() {
        let mut out = [0u16; 3];
        assert_eq!(encode_str_into("A😀", &mut out), Some(3)); // 1 + 2 units
        assert_eq!(&out, &[0x0041, 0xD83D, 0xDE00]);
        let mut small = [0u16; 2];
        assert_eq!(encode_str_into("A😀", &mut small), None, "needs 3 units");
    }

    #[test]
    fn matches_std_round_trip_and_lossy() {
        // Encoding agrees with str::encode_utf16; decoding a valid stream round-trips.
        let samples = ["", "hello", "café ∀", "😀🏳️‍🌈 end", "Ελληνικά", "日本語"];
        for s in samples {
            let std_units: Vec<u16> = s.encode_utf16().collect();
            let mut mine = alloc::vec![0u16; std_units.len()];
            assert_eq!(encode_str_into(s, &mut mine), Some(std_units.len()));
            assert_eq!(mine, std_units, "{s:?}");
            let back: String = decode_lossy(&std_units).collect();
            assert_eq!(back, s, "{s:?}");
        }

        // Lossy decoding agrees with char::decode_utf16 mapped to U+FFFD, including on
        // deliberately broken unit sequences.
        let broken: [&[u16]; 4] = [
            &[0x0041, 0xD800, 0x0042], // high then non-low
            &[0xDC00, 0x0041],         // lone low
            &[0xD83D],                 // high at end
            &[0x0041, 0xD83D, 0xDE00, 0xDFFF],
        ];
        for units in broken {
            let std_lossy: String = char::decode_utf16(units.iter().copied())
                .map(|r| r.unwrap_or(REPLACEMENT))
                .collect();
            let mine: String = decode_lossy(units).collect();
            assert_eq!(mine, std_lossy, "{units:04x?}");
        }
    }
}
