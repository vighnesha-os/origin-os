//! SHA-512 (FIPS 180-4 §6.4).

use core::fmt;

use crate::block::BlockBuffer;
use crate::constants::{H512, K512};
use crate::wipe::wipe_words;

/// Streaming SHA-512.
#[derive(Clone)]
pub struct Sha512 {
    state: [u64; 8],
    buffer: BlockBuffer<128>,
    /// Message length in bytes, modulo 2^128. FIPS 180-4 limits messages to below 2^128 bits.
    bytes: u128,
}

/// Prints no internal state, so hashing secret data can never leak through logs.
impl fmt::Debug for Sha512 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Sha512 { .. }")
    }
}

impl Default for Sha512 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha512 {
    /// Digest length in bytes.
    pub const OUTPUT_LEN: usize = 64;
    /// Block length in bytes.
    pub const BLOCK_LEN: usize = 128;

    /// A hasher with the FIPS 180-4 §5.3.5 initial value.
    pub const fn new() -> Self {
        Self {
            state: H512,
            buffer: BlockBuffer::new(),
            bytes: 0,
        }
    }

    /// Absorbs `data`.
    pub fn update(&mut self, data: &[u8]) {
        self.bytes = self.bytes.wrapping_add(data.len() as u128);
        let state = &mut self.state;
        self.buffer.update(data, |block| compress(state, block));
    }

    /// Pads the message and returns the digest.
    pub fn finalize(self) -> [u8; 64] {
        let mut state = self.state;
        let bits = self.bytes.wrapping_mul(8);
        self.buffer
            .finish(bits.to_be_bytes(), |block| compress(&mut state, block));
        let mut out = [0u8; 64];
        for (dst, word) in out.as_chunks_mut::<8>().0.iter_mut().zip(state) {
            *dst = word.to_be_bytes();
        }
        out
    }

    /// The SHA-512 digest of `data`.
    pub fn digest(data: &[u8]) -> [u8; 64] {
        let mut h = Self::new();
        h.update(data);
        h.finalize()
    }

    /// [`Self::update`] for a hasher whose input or chaining value is secret, as HMAC's is from
    /// its first block on: each block's message schedule is overwritten once the block is
    /// compressed (see [`compress_secret`]).
    pub(crate) fn update_secret(&mut self, data: &[u8]) {
        self.bytes = self.bytes.wrapping_add(data.len() as u128);
        let state = &mut self.state;
        self.buffer
            .update(data, |block| compress_secret(state, block));
    }

    /// [`Self::finalize`] for a hasher that [`Self::update_secret`] fed: pads the message,
    /// writes the digest to `out`, and overwrites the hasher. It works in place, where
    /// `finalize` moves the state and the buffer and would leave those copies behind.
    pub(crate) fn finalize_secret(&mut self, out: &mut [u8; 64]) {
        let bits = self.bytes.wrapping_mul(8);
        let state = &mut self.state;
        self.buffer
            .finish_in_place(bits.to_be_bytes(), |block| compress_secret(state, block));
        for (dst, word) in out.as_chunks_mut::<8>().0.iter_mut().zip(self.state.iter()) {
            *dst = word.to_be_bytes();
        }
        self.wipe();
    }

    /// Overwrites the chaining value and the buffered input, and leaves a hasher that has
    /// absorbed nothing (see [`crate::wipe`] for what the overwrite guarantees).
    pub(crate) fn wipe(&mut self) {
        wipe_words(&mut self.state);
        self.buffer.wipe();
        self.state = H512;
        self.bytes = 0;
    }

    /// Whether the hasher is as [`Self::new`] and [`Self::wipe`] leave it.
    #[cfg(test)]
    pub(crate) fn is_fresh(&self) -> bool {
        self.state == H512 && self.bytes == 0 && self.buffer.is_blank()
    }
}

// FIPS 180-4 §4.1.3.
const fn ch(x: u64, y: u64, z: u64) -> u64 {
    (x & y) ^ (!x & z)
}
const fn maj(x: u64, y: u64, z: u64) -> u64 {
    (x & y) ^ (x & z) ^ (y & z)
}
const fn big_sigma0(x: u64) -> u64 {
    x.rotate_right(28) ^ x.rotate_right(34) ^ x.rotate_right(39)
}
const fn big_sigma1(x: u64) -> u64 {
    x.rotate_right(14) ^ x.rotate_right(18) ^ x.rotate_right(41)
}
const fn small_sigma0(x: u64) -> u64 {
    x.rotate_right(1) ^ x.rotate_right(8) ^ (x >> 7)
}
const fn small_sigma1(x: u64) -> u64 {
    x.rotate_right(19) ^ x.rotate_right(61) ^ (x >> 6)
}

/// Word `i mod 16` of the rolling message schedule; see `sha256::w_at`.
fn w_at(w: &[u64; 16], i: usize) -> u64 {
    w.get(i & 15).copied().unwrap_or(0)
}

/// The SHA-512 compression function (§6.4.2), with the same 16-word schedule ring as
/// SHA-256 over 80 rounds.
fn compress(state: &mut [u64; 8], block: &[u8; 128]) {
    let mut w = [0u64; 16];
    rounds(state, block, &mut w);
}

/// [`compress`] for a block or a chaining value that is secret. The schedule ring is
/// overwritten afterwards: the schedule's recurrence runs backwards as well as forwards, so
/// the ring's last sixteen words give back the whole block, which for HMAC's first block is
/// the key.
///
/// The rounds run out of line here, so that the ring this function overwrites is the memory
/// they used and not a copy of words the optimizer kept somewhere else. The eight working
/// variables stay the compiler's to place: `crate::wipe` says what that leaves to the caller.
fn compress_secret(state: &mut [u64; 8], block: &[u8; 128]) {
    let mut w = [0u64; 16];
    rounds_out_of_line(state, block, &mut w);
    wipe_words(&mut w);
}

/// [`rounds`] as a function of its own, for [`compress_secret`].
#[inline(never)]
fn rounds_out_of_line(state: &mut [u64; 8], block: &[u8; 128], w: &mut [u64; 16]) {
    rounds(state, block, w);
}

/// The 80 rounds of §6.4.2 on the caller's schedule ring `w`, which is left holding the last
/// sixteen schedule words.
#[inline(always)]
fn rounds(state: &mut [u64; 8], block: &[u8; 128], w: &mut [u64; 16]) {
    for (word, bytes) in w.iter_mut().zip(block.as_chunks::<8>().0) {
        *word = u64::from_be_bytes(*bytes);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for (t, &k) in K512.iter().enumerate() {
        let wt = if t < 16 {
            w_at(w, t)
        } else {
            // W[t] = σ1(W[t-2]) + W[t-7] + σ0(W[t-15]) + W[t-16]
            let next = small_sigma1(w_at(w, t.wrapping_add(14)))
                .wrapping_add(w_at(w, t.wrapping_add(9)))
                .wrapping_add(small_sigma0(w_at(w, t.wrapping_add(1))))
                .wrapping_add(w_at(w, t));
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
