//! SHA-3 hash functions and SHAKE extendable-output functions (FIPS 202 §4–§6): the sponge
//! construction over Keccak-f[1600], with byte-oriented input and output.

use core::fmt;

use crate::block::{BlockBuffer, copy};
use crate::keccak::keccak_f1600;

/// The rate of Keccak[c] in bytes: r = 1600 − c bits (§5.2).
const fn rate(capacity_bits: usize) -> usize {
    1600usize.wrapping_sub(capacity_bits) / 8
}

// §6.1: SHA3-256(M) = KECCAK[512](M || 01, 256) and SHA3-512(M) = KECCAK[1024](M || 01, 512).
// §6.2: SHAKE128(M, d) = KECCAK[256](M || 1111, d) and SHAKE256(M, d) = KECCAK[512](M || 1111, d).
const SHA3_256_RATE: usize = rate(512);
const SHA3_512_RATE: usize = rate(1024);
const SHAKE128_RATE: usize = rate(256);
const SHAKE256_RATE: usize = rate(512);

/// The SHA-3 suffix 01 (§6.1) followed by the first bit of pad10*1, least significant bit
/// first (Appendix B.2).
const SHA3_SUFFIX: u8 = 0b110;
/// The SHAKE suffix 1111 (§6.2) followed by the first bit of pad10*1.
const SHAKE_SUFFIX: u8 = 0b1_1111;

/// XORs one rate-sized block into the state and applies the permutation (Algorithm 8,
/// step 6). Every rate here is a whole number of lanes.
fn absorb<const RATE: usize>(state: &mut [u64; 25], block: &[u8; RATE]) {
    for (lane, bytes) in state.iter_mut().zip(block.as_chunks::<8>().0) {
        *lane ^= u64::from_le_bytes(*bytes);
    }
    keccak_f1600(state);
}

/// The absorbing phase of the sponge with a rate of `RATE` bytes.
#[derive(Clone)]
struct Sponge<const RATE: usize> {
    state: [u64; 25],
    buffer: BlockBuffer<RATE>,
}

impl<const RATE: usize> Sponge<RATE> {
    /// S = 0^b (Algorithm 8 step 5).
    const fn new() -> Self {
        Self {
            state: [0; 25],
            buffer: BlockBuffer::new(),
        }
    }

    fn update(&mut self, data: &[u8]) {
        let state = &mut self.state;
        self.buffer.update(data, |block| absorb(state, block));
    }

    /// Appends `suffix` and pad10*1, absorbs the final block and returns the state, ready to
    /// be squeezed.
    fn finish(self, suffix: u8) -> [u64; 25] {
        let mut state = self.state;
        absorb(&mut state, &self.buffer.finish_sponge(suffix));
        state
    }
}

/// The first `N` bytes of the state, for N no greater than the rate (Algorithm 8 steps 8–9).
fn truncate<const N: usize>(state: &[u64; 25]) -> [u8; N] {
    let mut out = [0u8; N];
    for (dst, lane) in out.as_chunks_mut::<8>().0.iter_mut().zip(state) {
        *dst = lane.to_le_bytes();
    }
    out
}

macro_rules! sha3_hash {
    ($(#[$doc:meta])* $name:ident, $rate:expr, $out:literal) => {
        $(#[$doc])*
        #[derive(Clone)]
        pub struct $name {
            sponge: Sponge<{ $rate }>,
        }

        /// Prints no internal state, so hashing secret data can never leak through logs.
        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($name), " { .. }"))
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl $name {
            /// Digest length in bytes.
            pub const OUTPUT_LEN: usize = $out;
            /// Rate (block length) in bytes.
            pub const RATE: usize = $rate;

            /// A hasher over the empty message.
            pub const fn new() -> Self {
                Self {
                    sponge: Sponge::new(),
                }
            }

            /// Absorbs `data`.
            pub fn update(&mut self, data: &[u8]) {
                self.sponge.update(data);
            }

            /// Pads the message and returns the digest.
            pub fn finalize(self) -> [u8; $out] {
                truncate(&self.sponge.finish(SHA3_SUFFIX))
            }

            /// The digest of `data`.
            pub fn digest(data: &[u8]) -> [u8; $out] {
                let mut h = Self::new();
                h.update(data);
                h.finalize()
            }
        }
    };
}

sha3_hash!(
    /// Streaming SHA3-256 (FIPS 202 §6.1).
    Sha3_256,
    SHA3_256_RATE,
    32
);
sha3_hash!(
    /// Streaming SHA3-512 (FIPS 202 §6.1).
    Sha3_512,
    SHA3_512_RATE,
    64
);

macro_rules! shake {
    ($(#[$doc:meta])* $name:ident, $rate:expr) => {
        $(#[$doc])*
        #[derive(Clone)]
        pub struct $name {
            sponge: Sponge<{ $rate }>,
        }

        /// Prints no internal state, so hashing secret data can never leak through logs.
        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($name), " { .. }"))
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl $name {
            /// Rate (block length) in bytes.
            pub const RATE: usize = $rate;

            /// An XOF over the empty message.
            pub const fn new() -> Self {
                Self {
                    sponge: Sponge::new(),
                }
            }

            /// Absorbs `data`.
            pub fn update(&mut self, data: &[u8]) {
                self.sponge.update(data);
            }

            /// Pads the message and returns a reader for the output stream.
            pub fn finalize_xof(self) -> ShakeReader {
                ShakeReader::new(self.sponge.finish(SHAKE_SUFFIX), Self::RATE)
            }
        }
    };
}

shake!(
    /// Streaming SHAKE128 (FIPS 202 §6.2).
    Shake128,
    SHAKE128_RATE
);
shake!(
    /// Streaming SHAKE256 (FIPS 202 §6.2).
    Shake256,
    SHAKE256_RATE
);

/// The output stream of [`Shake128`] or [`Shake256`]: the squeezing phase of the sponge
/// (Algorithm 8 steps 7–10). Any sequence of [`squeeze`](Self::squeeze) calls yields the same
/// bytes as one call of their total length. Cloning forks the stream.
#[derive(Clone)]
pub struct ShakeReader {
    state: [u64; 25],
    /// The first `rate` bytes of `state`, serialized after each permutation.
    block: [u8; SHAKE128_RATE],
    /// 168 for SHAKE128 or 136 for SHAKE256; never more than `block.len()`.
    rate: usize,
    /// Bytes of `block` already returned, at most `rate`.
    pos: usize,
}

/// Prints no internal state: the output stream may be secret.
impl fmt::Debug for ShakeReader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ShakeReader { .. }")
    }
}

impl ShakeReader {
    fn new(state: [u64; 25], rate: usize) -> Self {
        let mut reader = Self {
            state,
            block: [0; SHAKE128_RATE],
            rate,
            pos: 0,
        };
        reader.serialize();
        reader
    }

    /// Z = Trunc_r(S) (Algorithm 8 step 8), one lane at a time.
    fn serialize(&mut self) {
        let block = self.block.get_mut(..self.rate).unwrap_or_default();
        for (dst, lane) in block.as_chunks_mut::<8>().0.iter_mut().zip(&self.state) {
            *dst = lane.to_le_bytes();
        }
    }

    /// Writes the next `out.len()` bytes of the output stream to `out`. The next block is
    /// computed (S = f(S), Algorithm 8 step 10) only when output beyond the current one is
    /// requested.
    pub fn squeeze(&mut self, out: &mut [u8]) {
        let mut out = out;
        while !out.is_empty() {
            if self.pos >= self.rate {
                keccak_f1600(&mut self.state);
                self.serialize();
                self.pos = 0;
            }
            let available = self.block.get(self.pos..self.rate).unwrap_or_default();
            let n = available.len().min(out.len());
            // `n` is positive because `pos < rate <= block.len()`. Returning on 0 keeps the
            // loop finite even if that invariant were ever broken.
            if n == 0 {
                return;
            }
            let Some((head, rest)) = core::mem::take(&mut out).split_at_mut_checked(n) else {
                return;
            };
            copy(head, available);
            self.pos = self.pos.saturating_add(n);
            out = rest;
        }
    }
}
