//! DEFLATE decompression (RFC 1951), the raw stream without the zlib or gzip wrapper
//! (CLAUDE.md §17 compression references, §46 package payloads). Supports all three block
//! types — stored, fixed-Huffman and dynamic-Huffman — with LZ77 back-references. Every
//! read is bounds-checked and the output is capped, so a truncated or hostile stream (a
//! decompression bomb) is a clean `None`, never a panic or an unbounded allocation
//! (untrusted input, §34). Pure, no `unsafe`; `alloc` for the output.
//!
//! The decoder follows the canonical reference method (zlib's `puff`): bits are read
//! least-significant-first from each byte, and a Huffman symbol is decoded by accumulating
//! bits most-significant-first against the per-length code counts. The code here is written
//! for this project (no source copied); the algorithm and the RFC 1951 constants are the
//! public specification.

extern crate alloc;
use alloc::vec::Vec;

/// The default output cap (64 MiB): a guard against a decompression bomb.
pub const DEFAULT_MAX_OUTPUT: usize = 64 << 20;

const MAX_BITS: usize = 15;

// RFC 1951 §3.2.5: length codes 257..=285 → base length and extra bits (index = sym - 257).
const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
// Distance codes 0..=29 → base distance and extra bits.
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
// The order code-length-code lengths appear in a dynamic block header.
const CLCL_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// A least-significant-bit-first reader over the compressed bytes.
struct BitReader<'a> {
    data: &'a [u8],
    byte: usize,
    bit: u32,
}

impl BitReader<'_> {
    fn bit(&mut self) -> Option<u32> {
        let b = *self.data.get(self.byte)?;
        let v = (u32::from(b) >> self.bit) & 1;
        self.bit += 1;
        if self.bit == 8 {
            self.bit = 0;
            self.byte += 1;
        }
        Some(v)
    }

    /// `n` bits, least-significant first (0 ≤ n ≤ 16).
    fn bits(&mut self, n: u32) -> Option<u32> {
        let mut v = 0;
        for i in 0..n {
            v |= self.bit()? << i;
        }
        Some(v)
    }

    /// Discards bits up to the next byte boundary.
    fn align(&mut self) {
        if self.bit != 0 {
            self.bit = 0;
            self.byte += 1;
        }
    }

    fn next_byte(&mut self) -> Option<u8> {
        let b = *self.data.get(self.byte)?;
        self.byte += 1;
        Some(b)
    }
}

/// A canonical Huffman decoder: how many codes of each length, and the symbols ordered by
/// (length, symbol).
struct Huffman {
    counts: [u16; MAX_BITS + 1],
    symbols: Vec<u16>,
}

impl Huffman {
    /// Builds the decoder from each symbol's code length (0 = absent).
    fn new(lengths: &[u8]) -> Huffman {
        let mut counts = [0u16; MAX_BITS + 1];
        for &l in lengths {
            if (l as usize) <= MAX_BITS {
                counts[l as usize] += 1;
            }
        }
        counts[0] = 0;
        // The first symbol index of each length.
        let mut offsets = [0u16; MAX_BITS + 1];
        for len in 1..=MAX_BITS {
            offsets[len] = offsets[len - 1] + counts[len - 1];
        }
        let mut symbols = alloc::vec![0u16; lengths.len()];
        for (sym, &l) in lengths.iter().enumerate() {
            if l != 0 && (l as usize) <= MAX_BITS {
                let slot = offsets[l as usize] as usize;
                if slot < symbols.len() {
                    symbols[slot] = sym as u16;
                }
                offsets[l as usize] += 1;
            }
        }
        Huffman { counts, symbols }
    }

    /// Decodes the next symbol, accumulating bits most-significant-first against the counts.
    fn decode(&self, r: &mut BitReader) -> Option<u16> {
        let mut code = 0i32;
        let mut first = 0i32;
        let mut index = 0i32;
        for len in 1..=MAX_BITS {
            code |= r.bit()? as i32;
            let count = i32::from(self.counts[len]);
            if code - first < count {
                return self.symbols.get((index + (code - first)) as usize).copied();
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        None
    }
}

/// Inflates a raw DEFLATE stream, capped at [`DEFAULT_MAX_OUTPUT`] bytes.
pub fn inflate(input: &[u8]) -> Option<Vec<u8>> {
    inflate_limit(input, DEFAULT_MAX_OUTPUT)
}

/// Inflates a raw DEFLATE stream, refusing (`None`) once the output would exceed `max`.
pub fn inflate_limit(input: &[u8], max: usize) -> Option<Vec<u8>> {
    inflate_inner(input, max).map(|(out, _)| out)
}

/// Inflates, also returning how many input bytes the stream occupied (rounded up to the
/// next byte boundary), so a wrapper can find a trailing checksum.
fn inflate_inner(input: &[u8], max: usize) -> Option<(Vec<u8>, usize)> {
    let mut r = BitReader {
        data: input,
        byte: 0,
        bit: 0,
    };
    let mut out = Vec::new();
    loop {
        let final_block = r.bit()? == 1;
        match r.bits(2)? {
            0 => stored_block(&mut r, &mut out, max)?,
            1 => huffman_block(&mut r, &mut out, &fixed_litlen(), &fixed_dist(), max)?,
            2 => {
                let (litlen, dist) = dynamic_tables(&mut r)?;
                huffman_block(&mut r, &mut out, &litlen, &dist, max)?;
            }
            _ => return None, // reserved block type 3
        }
        if final_block {
            let consumed = r.byte + usize::from(r.bit != 0);
            return Some((out, consumed));
        }
    }
}

/// The Adler-32 checksum (RFC 1950 §9), the zlib trailer's integrity check.
pub fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65521;
    let mut a = 1u32;
    let mut b = 0u32;
    for &x in data {
        a = (a + u32::from(x)) % MOD;
        b = (b + a) % MOD;
    }
    (b << 16) | a
}

/// Decompresses a zlib stream (RFC 1950): the 2-byte header, a raw DEFLATE body, and a
/// big-endian Adler-32 trailer, which must match. Capped at [`DEFAULT_MAX_OUTPUT`].
pub fn zlib_decompress(input: &[u8]) -> Option<Vec<u8>> {
    zlib_decompress_limit(input, DEFAULT_MAX_OUTPUT)
}

/// [`zlib_decompress`] with an explicit output cap.
pub fn zlib_decompress_limit(input: &[u8], max: usize) -> Option<Vec<u8>> {
    let cmf = *input.first()?;
    let flg = *input.get(1)?;
    if cmf & 0x0F != 8 {
        return None; // CM must be 8 (deflate)
    }
    if (u16::from(cmf) << 8 | u16::from(flg)) % 31 != 0 {
        return None; // the header checksum
    }
    if flg & 0x20 != 0 {
        return None; // FDICT: a preset dictionary is not supported
    }
    let (out, consumed) = inflate_inner(input.get(2..)?, max)?;
    let trailer = input.get(2 + consumed..2 + consumed + 4)?;
    let stored = u32::from_be_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]);
    (adler32(&out) == stored).then_some(out)
}

/// Decompresses a gzip stream (RFC 1952): the 10-byte header and its optional fields, a raw
/// DEFLATE body, and the CRC-32 + ISIZE trailer, both of which must match. Capped at
/// [`DEFAULT_MAX_OUTPUT`].
pub fn gzip_decompress(input: &[u8]) -> Option<Vec<u8>> {
    gzip_decompress_limit(input, DEFAULT_MAX_OUTPUT)
}

/// [`gzip_decompress`] with an explicit output cap.
pub fn gzip_decompress_limit(input: &[u8], max: usize) -> Option<Vec<u8>> {
    if input.get(0..3)? != [0x1F, 0x8B, 0x08] {
        return None; // magic + CM = deflate
    }
    let flg = *input.get(3)?;
    if flg & 0xE0 != 0 {
        return None; // reserved flag bits
    }
    let mut off = 10; // fixed header
    if flg & 0x04 != 0 {
        // FEXTRA: a 2-byte length then that many bytes.
        let xlen = usize::from(u16::from_le_bytes([*input.get(off)?, *input.get(off + 1)?]));
        off = off.checked_add(2)?.checked_add(xlen)?;
    }
    if flg & 0x08 != 0 {
        off = skip_cstring(input, off)?; // FNAME
    }
    if flg & 0x10 != 0 {
        off = skip_cstring(input, off)?; // FCOMMENT
    }
    if flg & 0x02 != 0 {
        off = off.checked_add(2)?; // FHCRC
    }
    let (out, consumed) = inflate_inner(input.get(off..)?, max)?;
    let trailer = input.get(off + consumed..off + consumed + 8)?;
    let crc = u32::from_le_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]);
    let isize = u32::from_le_bytes([trailer[4], trailer[5], trailer[6], trailer[7]]);
    if crate::gpt::crc32(&out) != crc || out.len() as u32 != isize {
        return None;
    }
    Some(out)
}

/// Skips a NUL-terminated string starting at `off`; returns the offset just past the NUL.
fn skip_cstring(input: &[u8], mut off: usize) -> Option<usize> {
    loop {
        let b = *input.get(off)?;
        off += 1;
        if b == 0 {
            return Some(off);
        }
    }
}

fn stored_block(r: &mut BitReader, out: &mut Vec<u8>, max: usize) -> Option<()> {
    r.align();
    let len = u16::from(r.next_byte()?) | (u16::from(r.next_byte()?) << 8);
    let nlen = u16::from(r.next_byte()?) | (u16::from(r.next_byte()?) << 8);
    if len != !nlen {
        return None; // the one's-complement check
    }
    let len = usize::from(len);
    if out.len().checked_add(len)? > max {
        return None;
    }
    for _ in 0..len {
        out.push(r.next_byte()?);
    }
    Some(())
}

fn huffman_block(
    r: &mut BitReader,
    out: &mut Vec<u8>,
    litlen: &Huffman,
    dist: &Huffman,
    max: usize,
) -> Option<()> {
    loop {
        let sym = litlen.decode(r)?;
        match sym {
            0..=255 => {
                if out.len() >= max {
                    return None;
                }
                out.push(sym as u8);
            }
            256 => return Some(()), // end of block
            257..=285 => {
                let li = (sym - 257) as usize;
                let length =
                    usize::from(LENGTH_BASE[li]) + r.bits(u32::from(LENGTH_EXTRA[li]))? as usize;
                let dsym = dist.decode(r)? as usize;
                if dsym >= DIST_BASE.len() {
                    return None;
                }
                let distance =
                    usize::from(DIST_BASE[dsym]) + r.bits(u32::from(DIST_EXTRA[dsym]))? as usize;
                if distance == 0 || distance > out.len() {
                    return None;
                }
                if out.len().checked_add(length)? > max {
                    return None;
                }
                for _ in 0..length {
                    out.push(out[out.len() - distance]);
                }
            }
            _ => return None, // 286, 287: invalid
        }
    }
}

/// The fixed literal/length code lengths (RFC 1951 §3.2.6).
fn fixed_litlen() -> Huffman {
    let mut lengths = [0u8; 288];
    for (i, l) in lengths.iter_mut().enumerate() {
        *l = match i {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    Huffman::new(&lengths)
}

/// The fixed distance code lengths: all 5 bits.
fn fixed_dist() -> Huffman {
    Huffman::new(&[5u8; 30])
}

/// Reads a dynamic block's header and builds its literal/length and distance decoders.
fn dynamic_tables(r: &mut BitReader) -> Option<(Huffman, Huffman)> {
    let hlit = r.bits(5)? as usize + 257;
    let hdist = r.bits(5)? as usize + 1;
    let hclen = r.bits(4)? as usize + 4;
    if hlit > 286 || hdist > 30 || hclen > 19 {
        return None;
    }

    // The code-length-code lengths, in their shuffled order.
    let mut cl_lengths = [0u8; 19];
    for &pos in CLCL_ORDER.iter().take(hclen) {
        cl_lengths[pos] = r.bits(3)? as u8;
    }
    let cl = Huffman::new(&cl_lengths);

    // Decode HLIT + HDIST code lengths, run-length encoded (16/17/18).
    let total = hlit + hdist;
    let mut lengths = alloc::vec![0u8; total];
    let mut i = 0;
    while i < total {
        match cl.decode(r)? {
            sym @ 0..=15 => {
                lengths[i] = sym as u8;
                i += 1;
            }
            16 => {
                // Copy the previous length 3..=6 times.
                let prev = *i.checked_sub(1).and_then(|p| lengths.get(p))?;
                let repeat = 3 + r.bits(2)? as usize;
                for _ in 0..repeat {
                    *lengths.get_mut(i)? = prev;
                    i += 1;
                }
            }
            17 => {
                let repeat = 3 + r.bits(3)? as usize;
                i = i.checked_add(repeat).filter(|&n| n <= total)?;
            }
            18 => {
                let repeat = 11 + r.bits(7)? as usize;
                i = i.checked_add(repeat).filter(|&n| n <= total)?;
            }
            _ => return None,
        }
    }

    let litlen = Huffman::new(lengths.get(..hlit)?);
    let dist = Huffman::new(lengths.get(hlit..)?);
    Some((litlen, dist))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference vectors: raw DEFLATE produced by Python's zlib (`wbits=-15`), each the
    /// compression of a plaintext this test reconstructs. The compressed bytes are a
    /// known-answer vector (CLAUDE.md §72); the decoder above is original.
    fn vectors() -> alloc::vec::Vec<(Vec<u8>, &'static [u8])> {
        alloc::vec![
            // empty
            (Vec::new(), &[0x03, 0x00][..]),
            // "hello, deflate world" (fixed-Huffman block)
            (
                b"hello, deflate world".to_vec(),
                &[
                    0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0x48, 0x49, 0x4d, 0xcb, 0x49, 0x2c,
                    0x49, 0x55, 0x28, 0xcf, 0x2f, 0xca, 0x49, 0x01, 0x00,
                ][..],
            ),
            // "abc" * 40 (back-references, fixed-Huffman block)
            (
                b"abc".iter().cycle().take(120).copied().collect(),
                &[0x4b, 0x4c, 0x4a, 0x4e, 0x1c, 0x08, 0x04, 0x00][..],
            ),
            // 300 zero bytes, level 0 (a stored block)
            (alloc::vec![0u8; 300], &STORED_ZEROS[..]),
            // 300 zero bytes, default level (fixed block, long back-references)
            (
                alloc::vec![0u8; 300],
                &[0x63, 0x60, 0x18, 0x05, 0xc4, 0x02, 0x00][..]
            ),
        ]
    }

    // The 300-zero stored block is long; build its bytes rather than spell them out.
    const STORED_ZEROS: [u8; 305] = stored_zeros();
    const fn stored_zeros() -> [u8; 305] {
        let mut b = [0u8; 305];
        b[0] = 0x01; // final stored block
        b[1] = 0x2c; // LEN = 300 (low)
        b[2] = 0x01; // LEN = 300 (high)
        b[3] = 0xd3; // NLEN = !300 (low)
        b[4] = 0xfe; // NLEN = !300 (high)
        b // bytes 5.. are the 300 zero payload, already zero
    }

    #[test]
    fn inflates_reference_vectors() {
        for (plain, compressed) in vectors() {
            assert_eq!(
                inflate(compressed).as_deref(),
                Some(&plain[..]),
                "vector of {} plaintext bytes",
                plain.len()
            );
        }
    }

    #[test]
    fn a_dynamic_block_round_trips() {
        // A longer, skewed text compresses to a dynamic-Huffman block (BTYPE 2), which
        // exercises the code-length-code table and the RLE of the literal/distance lengths.
        let lorem: &[u8] = b"Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do \
eiusmod tempor incididunt ut labore et dolore magna aliqua. ";
        let text: Vec<u8> = lorem
            .iter()
            .cycle()
            .take(lorem.len() * 8)
            .copied()
            .collect();
        // zlib default-level raw DEFLATE of `lorem * 8` — a verified dynamic block.
        let compressed: &[u8] = &[
            0xed, 0xcd, 0xc1, 0x0d, 0x03, 0x31, 0x08, 0x44, 0xd1, 0x56, 0xa6, 0x80, 0x28, 0x95,
            0x6c, 0x13, 0xc4, 0xa0, 0x15, 0x92, 0xc1, 0x5e, 0x03, 0xfd, 0xc7, 0x52, 0x6a, 0xc8,
            0xcd, 0xe7, 0xd1, 0x9f, 0x77, 0x8d, 0x25, 0x06, 0x9d, 0x51, 0x06, 0x1e, 0x7d, 0x2c,
            0x84, 0x26, 0xc8, 0x24, 0x5f, 0x68, 0xc3, 0x43, 0x5a, 0x4a, 0xd6, 0x02, 0xb1, 0x4e,
            0x8d, 0xa6, 0x7e, 0x43, 0xba, 0xee, 0x31, 0x84, 0x77, 0x00, 0xd1, 0x0a, 0x1b, 0x8c,
            0x14, 0x9b, 0x3b, 0x56, 0x6f, 0xca, 0xca, 0xe5, 0x89, 0x4a, 0x74, 0xfa, 0xec, 0x7b,
            0x48, 0xfe, 0xae, 0x05, 0x46, 0xb7, 0x13, 0xa8, 0xeb, 0x53, 0xf4, 0xc6, 0x75, 0xec,
            0x63, 0x1f, 0xfb, 0xaf, 0xf6, 0x17,
        ];
        assert_eq!(inflate(compressed).as_deref(), Some(&text[..]));
    }

    #[test]
    fn rejects_malformed_and_bombs() {
        assert_eq!(inflate(&[]), None, "no bytes");
        assert_eq!(
            inflate(&[0x02, 0x00]),
            None,
            "reserved block type 3 path / truncated"
        );
        // A stored block whose NLEN does not complement LEN.
        assert_eq!(
            inflate(&[0x01, 0x05, 0x00, 0x00, 0x00, 1, 2, 3, 4, 5]),
            None
        );
        // The low output cap turns a valid but expanding stream into a clean None.
        let zeros_rle: &[u8] = &[0x63, 0x60, 0x18, 0x05, 0xc4, 0x02, 0x00]; // 300 zeros
        assert_eq!(
            inflate_limit(zeros_rle, 100),
            None,
            "300 zeros exceeds a 100-byte cap"
        );
        assert_eq!(
            inflate_limit(zeros_rle, 300).map(|v| v.len()),
            Some(300),
            "the same stream fits a 300-byte cap"
        );
    }

    #[test]
    fn adler32_known_values() {
        assert_eq!(adler32(b""), 1);
        assert_eq!(adler32(b"Wikipedia"), 0x11e6_0398);
    }

    #[test]
    fn zlib_and_gzip_wrappers() {
        let zp: Vec<u8> = b"hello, zlib wrapper! "
            .iter()
            .cycle()
            .take(63)
            .copied()
            .collect();
        let zlib_stream: &[u8] = &[
            0x78, 0x9c, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0xa8, 0xca, 0xc9, 0x4c, 0x52,
            0x28, 0x2f, 0x4a, 0x2c, 0x28, 0x48, 0x2d, 0x52, 0x54, 0xc8, 0x20, 0x5a, 0x10, 0x00,
            0xd4, 0xe3, 0x16, 0x5a,
        ];
        assert_eq!(zlib_decompress(zlib_stream).as_deref(), Some(&zp[..]));

        let gp: Vec<u8> = b"hello, gzip wrapper! "
            .iter()
            .cycle()
            .take(63)
            .copied()
            .collect();
        let gzip_stream: &[u8] = &[
            0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xcb, 0x48, 0xcd, 0xc9,
            0xc9, 0xd7, 0x51, 0x48, 0xaf, 0xca, 0x2c, 0x50, 0x28, 0x2f, 0x4a, 0x2c, 0x28, 0x48,
            0x2d, 0x52, 0x54, 0xc8, 0x20, 0x5a, 0x10, 0x00, 0x32, 0x7a, 0x9a, 0x68, 0x3f, 0x00,
            0x00, 0x00,
        ];
        assert_eq!(gzip_decompress(gzip_stream).as_deref(), Some(&gp[..]));

        // A corrupted Adler-32 trailer is rejected.
        let mut bad = zlib_stream.to_vec();
        let n = bad.len();
        bad[n - 1] ^= 0xFF;
        assert_eq!(zlib_decompress(&bad), None, "bad adler");
        // A corrupted gzip CRC/ISIZE trailer is rejected.
        let mut bad = gzip_stream.to_vec();
        let n = bad.len();
        bad[n - 5] ^= 0xFF; // flips a CRC byte
        assert_eq!(gzip_decompress(&bad), None, "bad crc");
        // A zlib header that fails the %31 check, and a non-gzip magic.
        assert_eq!(zlib_decompress(&[0x78, 0x9d, 0x00, 0x00, 0x00, 0x00]), None);
        assert_eq!(gzip_decompress(&[0x1f, 0x8b, 0x00]), None, "CM must be 8");
        assert_eq!(gzip_decompress(&[]), None);
    }

    #[test]
    fn mutations_never_panic() {
        let base: Vec<u8> = (0..64u16).map(|i| i as u8).collect();
        let seeds = vectors();
        let mut rng = crate::testing::Rng::new(0x3DEF_1A7E);
        for _ in 0..crate::testing::cases(20_000) {
            // Mutate each reference compressed stream and a bit of arbitrary data.
            for (_, compressed) in &seeds {
                let mut data = compressed.to_vec();
                crate::testing::mutate(&mut data, &mut rng);
                let _ = inflate_limit(&data, 1 << 16);
            }
            let mut data = base.clone();
            crate::testing::mutate(&mut data, &mut rng);
            let _ = inflate_limit(&data, 1 << 16);
            // The zlib and gzip wrappers must also never panic on mutated input.
            let _ = zlib_decompress_limit(&data, 1 << 16);
            let _ = gzip_decompress_limit(&data, 1 << 16);
        }
    }
}
