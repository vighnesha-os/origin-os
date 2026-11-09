//! Ed25519 signing (RFC 8032 §5.1.5, §5.1.6), for the build host only.
//!
//! The private key is a 32-byte seed. This module computes on the secret scalar and the nonce
//! in constant time: no branch or memory index depends on them (see the crate documentation).
//! The message and the public results are not secret.

use origin_crypto::Sha512;

use crate::{wipe, wipe_u64};

const LOW_51: u64 = (1 << 51) - 1;

/// An element of GF(2^255 − 19): five little-endian limbs of 51 bits. Limbs are below 2^52
/// after [`Fe::carry`].
#[derive(Clone, Copy)]
struct Fe([u64; 5]);

/// 4p, added before subtracting so that no limb can underflow.
const FOUR_P: [u64; 5] = [
    4 * ((1 << 51) - 19),
    4 * ((1 << 51) - 1),
    4 * ((1 << 51) - 1),
    4 * ((1 << 51) - 1),
    4 * ((1 << 51) - 1),
];

/// 2·d, where d = −121665/121666 (RFC 8032 §5.1).
const D2: Fe = Fe([
    0x69b9426b2f159,
    0x35050762add7a,
    0x3cf44c0038052,
    0x6738cc7407977,
    0x2406d9dc56dff,
]);

/// The base point B (RFC 8032 §5.1), in affine form with T = x·y.
const BASE_X: Fe = Fe([
    0x62d608f25d51a,
    0x412a4b4f6592a,
    0x75b7171a4b31d,
    0x1ff60527118fe,
    0x216936d3cd6e5,
]);
const BASE_Y: Fe = Fe([
    0x6666666666658,
    0x4cccccccccccc,
    0x1999999999999,
    0x3333333333333,
    0x6666666666666,
]);
const BASE_T: Fe = Fe([
    0x68ab3a5b7dda3,
    0x0eea2a5eadbb,
    0x2af8df483c27e,
    0x332b375274732,
    0x67875f0fd78b7,
]);

impl Fe {
    const ZERO: Self = Self([0; 5]);
    const ONE: Self = Self([1, 0, 0, 0, 0]);

    /// One pass of carry propagation: every limb below 2^52.
    fn carry(self) -> Self {
        let [mut a0, mut a1, mut a2, mut a3, mut a4] = self.0;
        a1 += a0 >> 51;
        a0 &= LOW_51;
        a2 += a1 >> 51;
        a1 &= LOW_51;
        a3 += a2 >> 51;
        a2 &= LOW_51;
        a4 += a3 >> 51;
        a3 &= LOW_51;
        a0 += 19 * (a4 >> 51);
        a4 &= LOW_51;
        Self([a0, a1, a2, a3, a4])
    }

    fn add(&self, o: &Self) -> Self {
        let mut r = self.0;
        for (a, b) in r.iter_mut().zip(o.0) {
            *a += b;
        }
        Self(r).carry()
    }

    fn sub(&self, o: &Self) -> Self {
        let mut r = self.0;
        for ((a, b), four_p) in r.iter_mut().zip(o.0).zip(FOUR_P) {
            *a = *a + four_p - b;
        }
        Self(r).carry()
    }

    fn mul(&self, o: &Self) -> Self {
        let [a0, a1, a2, a3, a4] = self.0.map(u128::from);
        let [b0, b1, b2, b3, b4] = o.0.map(u128::from);
        let (b1_19, b2_19, b3_19, b4_19) = (b1 * 19, b2 * 19, b3 * 19, b4 * 19);
        let c0 = a0 * b0 + a1 * b4_19 + a2 * b3_19 + a3 * b2_19 + a4 * b1_19;
        let mut c1 = a0 * b1 + a1 * b0 + a2 * b4_19 + a3 * b3_19 + a4 * b2_19;
        let mut c2 = a0 * b2 + a1 * b1 + a2 * b0 + a3 * b4_19 + a4 * b3_19;
        let mut c3 = a0 * b3 + a1 * b2 + a2 * b1 + a3 * b0 + a4 * b4_19;
        let mut c4 = a0 * b4 + a1 * b3 + a2 * b2 + a3 * b1 + a4 * b0;
        c1 += c0 >> 51;
        let r0 = (c0 as u64) & LOW_51;
        c2 += c1 >> 51;
        let r1 = (c1 as u64) & LOW_51;
        c3 += c2 >> 51;
        let r2 = (c2 as u64) & LOW_51;
        c4 += c3 >> 51;
        let r3 = (c3 as u64) & LOW_51;
        let r4 = (c4 as u64) & LOW_51;
        let r0 = u128::from(r0) + (c4 >> 51) * 19;
        let r1 = r1 + (r0 >> 51) as u64;
        Self([(r0 as u64) & LOW_51, r1, r2, r3, r4])
    }

    fn square(&self) -> Self {
        self.mul(self)
    }

    /// `self^(p − 2)`: the inverse, by square-and-multiply over the public exponent.
    fn invert(&self) -> Self {
        // p − 2 = 2^255 − 21: bits 254 to 5 are all ones, then 0, 1, 0, 1, 1.
        let mut r = *self;
        for i in (0..254u32).rev() {
            r = r.square();
            if i != 4 && i != 2 {
                r = r.mul(self);
            }
        }
        r
    }

    /// The canonical 32-byte little-endian encoding of this element, modulo p.
    fn to_bytes(self) -> [u8; 32] {
        let mut t = self.carry().carry().0;
        // Is t ≥ p? Add 19 and see whether the sum reaches 2^255.
        let mut q = (t[0] + 19) >> 51;
        q = (t[1] + q) >> 51;
        q = (t[2] + q) >> 51;
        q = (t[3] + q) >> 51;
        q = (t[4] + q) >> 51;
        t[0] += 19 * q;
        t[1] += t[0] >> 51;
        t[0] &= LOW_51;
        t[2] += t[1] >> 51;
        t[1] &= LOW_51;
        t[3] += t[2] >> 51;
        t[2] &= LOW_51;
        t[4] += t[3] >> 51;
        t[3] &= LOW_51;
        t[4] &= LOW_51;
        let words = [
            t[0] | t[1] << 51,
            t[1] >> 13 | t[2] << 38,
            t[2] >> 26 | t[3] << 25,
            t[3] >> 39 | t[4] << 12,
        ];
        let mut out = [0u8; 32];
        for (chunk, word) in out.as_chunks_mut::<8>().0.iter_mut().zip(words) {
            *chunk = word.to_le_bytes();
        }
        out
    }

    /// `a` if `choose` is 0, `b` if it is 1, by mask (see [`mask_of`]).
    fn select(a: &Self, b: &Self, choose: u64) -> Self {
        let mask = mask_of(choose);
        let mut r = a.0;
        for (x, y) in r.iter_mut().zip(b.0) {
            *x ^= mask & (*x ^ y);
        }
        Self(r)
    }
}

/// A point on −x² + y² = 1 + d·x²·y² in extended coordinates (X : Y : Z : T), T = XY/Z.
#[derive(Clone, Copy)]
struct Point {
    x: Fe,
    y: Fe,
    z: Fe,
    t: Fe,
}

impl Point {
    const IDENTITY: Self = Self {
        x: Fe::ZERO,
        y: Fe::ONE,
        z: Fe::ONE,
        t: Fe::ZERO,
    };
    const BASE: Self = Self {
        x: BASE_X,
        y: BASE_Y,
        z: Fe::ONE,
        t: BASE_T,
    };

    /// Addition, a complete formula for a = −1 ("add-2008-hwcd-3").
    fn add(&self, o: &Self) -> Self {
        let a = self.y.sub(&self.x).mul(&o.y.sub(&o.x));
        let b = self.y.add(&self.x).mul(&o.y.add(&o.x));
        let c = self.t.mul(&D2).mul(&o.t);
        let d = self.z.mul(&o.z);
        let d = d.add(&d);
        let e = b.sub(&a);
        let f = d.sub(&c);
        let g = d.add(&c);
        let h = b.add(&a);
        Self {
            x: e.mul(&f),
            y: g.mul(&h),
            z: f.mul(&g),
            t: e.mul(&h),
        }
    }

    /// Doubling ("dbl-2008-hwcd", with a = −1).
    fn double(&self) -> Self {
        let a = self.x.square();
        let b = self.y.square();
        let c = self.z.square();
        let c = c.add(&c);
        let d = Fe::ZERO.sub(&a);
        let e = self.x.add(&self.y).square().sub(&a).sub(&b);
        let g = d.add(&b);
        let f = g.sub(&c);
        let h = d.sub(&b);
        Self {
            x: e.mul(&f),
            y: g.mul(&h),
            z: f.mul(&g),
            t: e.mul(&h),
        }
    }

    fn select(a: &Self, b: &Self, choose: u64) -> Self {
        Self {
            x: Fe::select(&a.x, &b.x, choose),
            y: Fe::select(&a.y, &b.y, choose),
            z: Fe::select(&a.z, &b.z, choose),
            t: Fe::select(&a.t, &b.t, choose),
        }
    }

    /// The compressed encoding (RFC 8032 §5.1.2).
    fn compress(&self) -> [u8; 32] {
        let z_inverse = self.z.invert();
        let x = self.x.mul(&z_inverse).to_bytes();
        let mut out = self.y.mul(&z_inverse).to_bytes();
        out[31] |= (x[0] & 1) << 7;
        out
    }
}

/// `[scalar]B`, doubling and adding on every bit and choosing the sum by mask.
fn base_multiple(scalar: &[u8; 32]) -> Point {
    let mut r = Point::IDENTITY;
    for bit in (0..256).rev() {
        r = r.double();
        let sum = r.add(&Point::BASE);
        let choose = u64::from((scalar[bit / 8] >> (bit % 8)) & 1);
        r = Point::select(&r, &sum, choose);
    }
    r
}

/// The group order L = 2^252 + 27742317777372353535851937790883648493, little-endian.
pub(crate) const L: [u64; 4] = [
    0x5812631a5cf5d3ed,
    0x14def9dea2f79cd6,
    0,
    0x1000000000000000,
];

/// Reduces a 512-bit little-endian value modulo L, by shifting its bits in from the top and
/// conditionally subtracting L, by mask. The result is below L.
pub(crate) fn reduce_512(value: &[u8; 64]) -> [u64; 4] {
    let mut r = [0u64; 4];
    for bit in (0..512).rev() {
        let incoming = u64::from((value[bit / 8] >> (bit % 8)) & 1);
        // r < L < 2^253, so 2r + 1 fits in four words and is below 2L.
        r = [
            r[0] << 1 | incoming,
            r[1] << 1 | r[0] >> 63,
            r[2] << 1 | r[1] >> 63,
            r[3] << 1 | r[2] >> 63,
        ];
        r = subtract_l_if_at_least(r);
    }
    r
}

/// All ones for a `bit` of 1, zero for 0. `black_box` stops the compiler from turning the mask
/// into a branch on `bit`: without it, LLVM compiled the conditional subtraction below into
/// two data-dependent branches per bit on both aarch64 and x86-64.
fn mask_of(bit: u64) -> u64 {
    core::hint::black_box(bit).wrapping_neg()
}

/// `r − L` if `r ≥ L`, otherwise `r`, by mask.
fn subtract_l_if_at_least(r: [u64; 4]) -> [u64; 4] {
    let mut difference = [0u64; 4];
    let mut borrow = 0u64;
    for i in 0..4 {
        let (d1, b1) = r[i].overflowing_sub(L[i]);
        let (d2, b2) = d1.overflowing_sub(borrow);
        difference[i] = d2;
        borrow = u64::from(b1) | u64::from(b2);
    }
    let keep_r = mask_of(borrow);
    let mut out = [0u64; 4];
    for i in 0..4 {
        out[i] = (r[i] & keep_r) | (difference[i] & !keep_r);
    }
    out
}

pub(crate) fn words_from_bytes(bytes: &[u8; 32]) -> [u64; 4] {
    let mut w = [0u64; 4];
    for (word, chunk) in w.iter_mut().zip(bytes.as_chunks::<8>().0) {
        *word = u64::from_le_bytes(*chunk);
    }
    w
}

pub(crate) fn bytes_from_words(w: &[u64; 4]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (chunk, word) in out.as_chunks_mut::<8>().0.iter_mut().zip(w) {
        *chunk = word.to_le_bytes();
    }
    out
}

/// `(k·a + r) mod L`, with every step independent of the values.
pub(crate) fn scalar_mul_add(k: &[u64; 4], a: &[u8; 32], r: &[u64; 4]) -> [u8; 32] {
    let (mut a_words, mut wide) = ([0u64; 4], [0u64; 8]);
    scalar_mul_add_into(k, a, r, &mut a_words, &mut wide)
}

/// [`scalar_mul_add`], with the two working buffers that hold the secret scalar and its product
/// supplied by the caller, and wiped here before the result is returned (see the crate
/// documentation). Separate so that a test can see them. Both must hold zeros.
pub(crate) fn scalar_mul_add_into(
    k: &[u64; 4],
    a: &[u8; 32],
    r: &[u64; 4],
    a_words: &mut [u64; 4],
    wide: &mut [u64; 8],
) -> [u8; 32] {
    *a_words = words_from_bytes(a);
    // The 512-bit product k·a plus r: k < 2^256 and a < 2^255, so it cannot overflow.
    for (i, &ki) in k.iter().enumerate() {
        let mut carry = 0u128;
        for (j, &aj) in a_words.iter().enumerate() {
            let sum = u128::from(wide[i + j]) + u128::from(ki) * u128::from(aj) + carry;
            wide[i + j] = sum as u64;
            carry = sum >> 64;
        }
        wide[i + 4] = carry as u64;
    }
    let mut carry = 0u128;
    for (i, slot) in wide.iter_mut().enumerate() {
        let sum = u128::from(*slot) + u128::from(*r.get(i).unwrap_or(&0)) + carry;
        *slot = sum as u64;
        carry = sum >> 64;
    }
    let mut bytes = [0u8; 64];
    for (chunk, word) in bytes.as_chunks_mut::<8>().0.iter_mut().zip(wide.iter()) {
        *chunk = word.to_le_bytes();
    }
    let result = reduce_512(&bytes);
    wipe(&mut bytes);
    wipe_u64(a_words);
    wipe_u64(wide);
    bytes_from_words(&result)
}

/// Writes the secret scalar and the nonce prefix from a seed (RFC 8032 §5.1.5) into the
/// caller's buffers, so that no copy is left behind by a return.
fn expand_seed(seed: &[u8; 32], scalar: &mut [u8; 32], prefix: &mut [u8; 32]) {
    let mut h = Sha512::digest(seed);
    scalar.copy_from_slice(&h[..32]);
    prefix.copy_from_slice(&h[32..]);
    wipe(&mut h);
    scalar[0] &= 248;
    scalar[31] &= 127;
    scalar[31] |= 64;
}

/// The public key for `seed` (RFC 8032 §5.1.5).
pub fn public_key(seed: &[u8; 32]) -> [u8; 32] {
    let (mut scalar, mut prefix) = ([0u8; 32], [0u8; 32]);
    expand_seed(seed, &mut scalar, &mut prefix);
    let public = base_multiple(&scalar).compress();
    wipe(&mut scalar);
    wipe(&mut prefix);
    public
}

/// Signs `message` with the key from `seed` (RFC 8032 §5.1.6). Deterministic. The result is
/// always accepted by a strict verifier: R is a canonical encoding and S < L.
pub fn sign(seed: &[u8; 32], message: &[u8]) -> [u8; 64] {
    let (mut scalar, mut prefix) = ([0u8; 32], [0u8; 32]);
    expand_seed(seed, &mut scalar, &mut prefix);
    let public = base_multiple(&scalar).compress();

    let mut h = Sha512::new();
    h.update(&prefix);
    h.update(message);
    let mut nonce_hash = h.finalize();
    let mut r = reduce_512(&nonce_hash);
    let mut r_bytes = bytes_from_words(&r);
    let big_r = base_multiple(&r_bytes).compress();

    let mut h = Sha512::new();
    h.update(&big_r);
    h.update(&public);
    h.update(message);
    let k = reduce_512(&h.finalize());
    let s = scalar_mul_add(&k, &scalar, &r);

    wipe(&mut scalar);
    wipe(&mut prefix);
    wipe(&mut nonce_hash);
    wipe(&mut r_bytes);
    wipe_u64(&mut r);

    let mut signature = [0u8; 64];
    signature[..32].copy_from_slice(&big_r);
    signature[32..].copy_from_slice(&s);
    signature
}

#[cfg(test)]
pub(crate) mod testing {
    //! Internals for `src/tests.rs`.
    pub(crate) use super::{
        L, bytes_from_words, reduce_512, scalar_mul_add, scalar_mul_add_into, words_from_bytes,
    };
}
