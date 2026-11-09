//! Base64 encoding and decoding (RFC 4648), the standard `+/` alphabet. Needed wherever
//! binary is carried as text: PEM-wrapped keys and certificates (§59), tokens, and config
//! values. Encoding pads to a multiple of four; decoding is whitespace-tolerant (so a
//! line-wrapped PEM body decodes directly) and strict about everything else — a bad
//! character, a wrong length, or misplaced padding is rejected, never a panic. Pure, no
//! `unsafe`; `alloc` for the output.

extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encodes `data` as standard Base64 with padding.
pub fn encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        out.push(ALPHABET[(b0 >> 2) as usize] as char);
        out.push(ALPHABET[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(((b1 & 0x0F) << 2) | (b2 >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(b2 & 0x3F) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// The sextet value of a Base64 character, or `None` if it is not in the alphabet.
fn value(c: u8) -> Option<u8> {
    match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// Decodes standard Base64, ignoring ASCII whitespace (newlines, spaces, tabs). Returns
/// `None` on an invalid character, a length that is not a multiple of four, or padding that
/// is not one or two `=` at the very end.
pub fn decode(input: &str) -> Option<Vec<u8>> {
    let filtered: Vec<u8> = input.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if !filtered.len().is_multiple_of(4) {
        return None;
    }
    let quads = filtered.len() / 4;
    let mut out = Vec::with_capacity(quads * 3);
    for (q, quad) in filtered.chunks(4).enumerate() {
        let is_last = q + 1 == quads;
        let mut sextets = [0u8; 4];
        let mut pads = 0;
        for (i, &c) in quad.iter().enumerate() {
            if c == b'=' {
                pads += 1;
            } else {
                if pads > 0 {
                    return None; // a real character after padding
                }
                sextets[i] = value(c)?;
            }
        }
        // Padding is only ever one or two `=`, and only in the final quad.
        if pads != 0 && (!is_last || pads > 2) {
            return None;
        }
        out.push((sextets[0] << 2) | (sextets[1] >> 4));
        if pads < 2 {
            out.push((sextets[1] << 4) | (sextets[2] >> 2));
        }
        if pads < 1 {
            out.push((sextets[2] << 6) | sextets[3]);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use super::*;

    /// The RFC 4648 §10 test vectors.
    const VECTORS: [(&[u8], &str); 7] = [
        (b"", ""),
        (b"f", "Zg=="),
        (b"fo", "Zm8="),
        (b"foo", "Zm9v"),
        (b"foob", "Zm9vYg=="),
        (b"fooba", "Zm9vYmE="),
        (b"foobar", "Zm9vYmFy"),
    ];

    #[test]
    fn rfc4648_vectors_encode_and_decode() {
        for (bytes, text) in VECTORS {
            assert_eq!(encode(bytes), text, "encode {bytes:?}");
            assert_eq!(decode(text).as_deref(), Some(bytes), "decode {text:?}");
        }
    }

    #[test]
    fn decode_tolerates_whitespace() {
        // A PEM-style line-wrapped body (and surrounding whitespace) decodes.
        let wrapped = "  Zm9v\n Ymfy\r\n".replace("Ymfy", "YmFy");
        assert_eq!(decode(&wrapped).as_deref(), Some(&b"foobar"[..]));
        assert_eq!(decode("\n\tZm9v\n").as_deref(), Some(&b"foo"[..]));
    }

    #[test]
    fn decode_rejects_malformed_input() {
        assert_eq!(decode("Zg="), None, "length not a multiple of 4");
        assert_eq!(decode("Zg=A"), None, "character after padding");
        assert_eq!(decode("===="), None, "too much padding");
        assert_eq!(decode("Z!=="), None, "invalid character");
        assert_eq!(decode("Zm==Zm9v"), None, "padding not at the end");
        assert_eq!(decode("-_=="), None, "URL-safe alphabet is not accepted");
    }

    #[test]
    fn round_trips_arbitrary_bytes() {
        let mut x = 0x2545_F491_4F6C_DD1Du64;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for len in 0..200usize {
            let data: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            let encoded = encode(&data);
            assert_eq!(encoded.len() % 4, 0, "always padded to a multiple of 4");
            assert_eq!(decode(&encoded).as_deref(), Some(&data[..]), "len {len}");
        }
    }
}
