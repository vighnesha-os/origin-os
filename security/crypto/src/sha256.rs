//! SHA-256 (FIPS 180-4 §6.2).

use core::fmt;

use crate::block::BlockBuffer;
use crate::constants::{H256, K256};

/// Streaming SHA-256.
#[derive(Clone)]
pub struct Sha256 {
    state: [u32; 8],
    buffer: BlockBuffer<64>,
    /// Message length in bytes, modulo 2^64. FIPS 180-4 limits messages to below 2^64 bits.
    bytes: u64,
}

/// Prints no internal state, so hashing secret data can never leak through logs.
impl fmt::Debug for Sha256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Sha256 { .. }")
    }
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    /// Digest length in bytes.
    pub const OUTPUT_LEN: usize = 32;
    /// Block length in bytes.
    pub const BLOCK_LEN: usize = 64;

    /// A hasher with the FIPS 180-4 §5.3.3 initial value.
    pub const fn new() -> Self {
        Self {
            state: H256,
            buffer: BlockBuffer::new(),
            bytes: 0,
        }
    }

    /// Absorbs `data`.
    pub fn update(&mut self, data: &[u8]) {
        self.bytes = self.bytes.wrapping_add(data.len() as u64);
        let state = &mut self.state;
        self.buffer.update(data, |block| compress(state, block));
    }

    /// Pads the message and returns the digest.
    pub fn finalize(self) -> [u8; 32] {
        let mut state = self.state;
        let bits = self.bytes.wrapping_mul(8);
        self.buffer
            .finish(bits.to_be_bytes(), |block| compress(&mut state, block));
        let mut out = [0u8; 32];
        for (dst, word) in out.as_chunks_mut::<4>().0.iter_mut().zip(state) {
            *dst = word.to_be_bytes();
        }
        out
    }

    /// The SHA-256 digest of `data`.
    pub fn digest(data: &[u8]) -> [u8; 32] {
        let mut h = Self::new();
        h.update(data);
        h.finalize()
    }
}

// FIPS 180-4 §4.1.2.
const fn ch(x: u32, y: u32, z: u32) -> u32 {
    (x & y) ^ (!x & z)
}
const fn maj(x: u32, y: u32, z: u32) -> u32 {
    (x & y) ^ (x & z) ^ (y & z)
}
const fn big_sigma0(x: u32) -> u32 {
    x.rotate_right(2) ^ x.rotate_right(13) ^ x.rotate_right(22)
}
const fn big_sigma1(x: u32) -> u32 {
    x.rotate_right(6) ^ x.rotate_right(11) ^ x.rotate_right(25)
}
const fn small_sigma0(x: u32) -> u32 {
    x.rotate_right(7) ^ x.rotate_right(18) ^ (x >> 3)
}
const fn small_sigma1(x: u32) -> u32 {
    x.rotate_right(17) ^ x.rotate_right(19) ^ (x >> 10)
}

/// Word `i mod 16` of the rolling message schedule. The mask keeps every index in bounds,
/// which the optimizer proves too, so no bounds check remains.
fn w_at(w: &[u32; 16], i: usize) -> u32 {
    w.get(i & 15).copied().unwrap_or(0)
}

/// The SHA-256 compression function (§6.2.2), with the message schedule kept in a
/// 16-word ring: `W[t]` lives at `t mod 16`.
fn compress(state: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 16];
    for (word, bytes) in w.iter_mut().zip(block.as_chunks::<4>().0) {
        *word = u32::from_be_bytes(*bytes);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for (t, &k) in K256.iter().enumerate() {
        let wt = if t < 16 {
            w_at(&w, t)
        } else {
            // W[t] = σ1(W[t-2]) + W[t-7] + σ0(W[t-15]) + W[t-16]
            let next = small_sigma1(w_at(&w, t.wrapping_add(14)))
                .wrapping_add(w_at(&w, t.wrapping_add(9)))
                .wrapping_add(small_sigma0(w_at(&w, t.wrapping_add(1))))
                .wrapping_add(w_at(&w, t));
            if let Some(slot) = w.get_mut(t & 15) {
                *slot = next;
            }
            next
        };
        let t1 = h
            .wrapping_add(big_sigma1(e))
            .wrapping_add(ch(e, f, g))
            .wrapping_add(k)
            .wrapping_add(wt);
        let t2 = big_sigma0(a).wrapping_add(maj(a, b, c));
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    for (s, v) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *s = s.wrapping_add(v);
    }
}
