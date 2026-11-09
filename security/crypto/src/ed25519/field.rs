//! Arithmetic in GF(p), p = 2^255 − 19 (RFC 8032 §5.1).
//!
//! An element is five 51-bit limbs, least significant first: Σ limb[i]·2^(51·i). Values
//! are kept only partly reduced between operations. Invariant: every `Fe` that leaves this
//! module has each limb below 2^51 + 2^18. With that bound:
//! - `add` and `sub` sums stay below 2^55 before their carry pass;
//! - every `mul`/`square` column is below 5·19·(2^51 + 2^18)^2 < 2^109, so it fits a `u128`
//!   and the carry out of the top limb, times 19, fits a `u64`.
//!
//! `to_bytes` is the only full (canonical) reduction; equality and parity use it.

use super::constants::{FOUR_P, SQRT_M1};

const MASK51: u64 = (1 << 51) - 1;

/// An element of GF(2^255 − 19). See the module invariant.
#[derive(Clone, Copy)]
pub(crate) struct Fe(pub(crate) [u64; 5]);

/// The low 64 bits of `x`. The mask makes the conversion infallible, so the fallback is dead
/// code that LLVM removes; it replaces an `as` cast, which the crate denies.
#[inline(always)]
fn lo64(x: u128) -> u64 {
    u64::try_from(x & u128::from(u64::MAX)).unwrap_or(0)
}

/// `a·b` as a `u128`; it cannot overflow.
#[inline(always)]
fn m(a: u64, b: u64) -> u128 {
    u128::from(a).wrapping_mul(u128::from(b))
}

/// One carry pass. For limbs below 2^63 the result satisfies the module invariant: limbs 1–4
/// are below 2^51 and limb 0 is below 2^51 + 19·2^12.
fn carry(l: [u64; 5]) -> [u64; 5] {
    let [l0, l1, l2, l3, l4] = l;
    let l1 = l1.wrapping_add(l0 >> 51);
    let l2 = l2.wrapping_add(l1 >> 51);
    let l3 = l3.wrapping_add(l2 >> 51);
    let l4 = l4.wrapping_add(l3 >> 51);
    // 2^255 ≡ 19 (mod p).
    let l0 = (l0 & MASK51).wrapping_add((l4 >> 51).wrapping_mul(19));
    [l0, l1 & MASK51, l2 & MASK51, l3 & MASK51, l4 & MASK51]
}

/// Reduces five product columns (each below 2^109) to the module invariant.
fn carry_wide(t: [u128; 5]) -> Fe {
    let [t0, t1, t2, t3, t4] = t;
    let t1 = t1.wrapping_add(t0 >> 51);
    let t2 = t2.wrapping_add(t1 >> 51);
    let t3 = t3.wrapping_add(t2 >> 51);
    let t4 = t4.wrapping_add(t3 >> 51);
    // The carry out of t4 is below 2^58, so 19 times it plus 2^51 stays below 2^63.
    let r0 = (lo64(t0) & MASK51).wrapping_add(lo64(t4 >> 51).wrapping_mul(19));
    let r1 = (lo64(t1) & MASK51).wrapping_add(r0 >> 51);
    Fe([
        r0 & MASK51,
        r1,
        lo64(t2) & MASK51,
        lo64(t3) & MASK51,
        lo64(t4) & MASK51,
    ])
}

impl Fe {
    pub(crate) const ZERO: Fe = Fe([0; 5]);
    pub(crate) const ONE: Fe = Fe([1, 0, 0, 0, 0]);

    /// The element encoded by the low 255 bits of `bytes`, little-endian. Bit 255 is ignored,
    /// and values from p to 2^255 − 1 are accepted (the caller checks canonicity).
    pub(crate) fn from_bytes(bytes: &[u8; 32]) -> Fe {
        let mut w = [0u64; 4];
        for (word, chunk) in w.iter_mut().zip(bytes.as_chunks::<8>().0) {
            *word = u64::from_le_bytes(*chunk);
        }
        let [w0, w1, w2, w3] = w;
        Fe([
            w0 & MASK51,
            ((w0 >> 51) | (w1 << 13)) & MASK51,
            ((w1 >> 38) | (w2 << 26)) & MASK51,
            ((w2 >> 25) | (w3 << 39)) & MASK51,
            (w3 >> 12) & MASK51,
        ])
    }

    /// The canonical 32-byte little-endian encoding of the value reduced into [0, p).
    pub(crate) fn to_bytes(self) -> [u8; 32] {
        // After one carry pass the value is below 2^255 + 19·2^12 < 2p.
        let [l0, l1, l2, l3, l4] = carry(self.0);
        // q = 1 exactly when value + 19 ≥ 2^255, that is when value ≥ p.
        let q = l0.wrapping_add(19) >> 51;
        let q = l1.wrapping_add(q) >> 51;
        let q = l2.wrapping_add(q) >> 51;
        let q = l3.wrapping_add(q) >> 51;
        let q = l4.wrapping_add(q) >> 51;
        // value − q·p = value + 19q − q·2^255: add 19q, carry, and drop bit 255.
        let l0 = l0.wrapping_add(q.wrapping_mul(19));
        let l1 = l1.wrapping_add(l0 >> 51);
        let l2 = l2.wrapping_add(l1 >> 51);
        let l3 = l3.wrapping_add(l2 >> 51);
        let l4 = l4.wrapping_add(l3 >> 51);
        let [l0, l1, l2, l3, l4] = [
            l0 & MASK51,
            l1 & MASK51,
            l2 & MASK51,
            l3 & MASK51,
            l4 & MASK51,
        ];
        let words = [
            l0 | (l1 << 51),
            (l1 >> 13) | (l2 << 38),
            (l2 >> 26) | (l3 << 25),
            (l3 >> 39) | (l4 << 12),
        ];
        let mut out = [0u8; 32];
        for (dst, word) in out.as_chunks_mut::<8>().0.iter_mut().zip(words) {
            *dst = word.to_le_bytes();
        }
        out
    }

    pub(crate) fn add(&self, rhs: &Fe) -> Fe {
        let mut l = self.0;
        for (a, b) in l.iter_mut().zip(rhs.0) {
            *a = a.wrapping_add(b);
        }
        Fe(carry(l))
    }

    /// `self − rhs`, computed as `self + 4p − rhs` so that no limb underflows: every limb
    /// of 4p is at least 2^53 − 76, above the invariant's bound.
    pub(crate) fn sub(&self, rhs: &Fe) -> Fe {
        let mut l = self.0;
        for ((a, b), four_p) in l.iter_mut().zip(rhs.0).zip(FOUR_P) {
            *a = a.wrapping_add(four_p).wrapping_sub(b);
        }
        Fe(carry(l))
    }

    pub(crate) fn neg(&self) -> Fe {
        Fe::ZERO.sub(self)
    }

    /// Schoolbook multiplication with the 2^255 ≡ 19 fold: limb products that land at
    /// position 5 or above are multiplied by 19 and added five positions lower.
    pub(crate) fn mul(&self, rhs: &Fe) -> Fe {
        let [a0, a1, a2, a3, a4] = self.0;
        let [b0, b1, b2, b3, b4] = rhs.0;
        let (b1_19, b2_19, b3_19, b4_19) = (
            b1.wrapping_mul(19),
            b2.wrapping_mul(19),
            b3.wrapping_mul(19),
            b4.wrapping_mul(19),
        );
        let sum = |terms: [u128; 5]| terms.iter().fold(0u128, |acc, t| acc.wrapping_add(*t));
        carry_wide([
            sum([
                m(a0, b0),
                m(a1, b4_19),
                m(a2, b3_19),
                m(a3, b2_19),
                m(a4, b1_19),
            ]),
            sum([
                m(a0, b1),
                m(a1, b0),
                m(a2, b4_19),
                m(a3, b3_19),
                m(a4, b2_19),
            ]),
            sum([m(a0, b2), m(a1, b1), m(a2, b0), m(a3, b4_19), m(a4, b3_19)]),
            sum([m(a0, b3), m(a1, b2), m(a2, b1), m(a3, b0), m(a4, b4_19)]),
            sum([m(a0, b4), m(a1, b3), m(a2, b2), m(a3, b1), m(a4, b0)]),
        ])
    }

    /// `self²`, the `mul` columns with the symmetric products merged.
    pub(crate) fn square(&self) -> Fe {
        let [a0, a1, a2, a3, a4] = self.0;
        let (d0, d1, d2, d3) = (
            a0.wrapping_mul(2),
            a1.wrapping_mul(2),
            a2.wrapping_mul(2),
            a3.wrapping_mul(2),
        );
        let (a3_19, a4_19) = (a3.wrapping_mul(19), a4.wrapping_mul(19));
        let sum = |terms: [u128; 3]| terms.iter().fold(0u128, |acc, t| acc.wrapping_add(*t));
        carry_wide([
            sum([m(a0, a0), m(d1, a4_19), m(d2, a3_19)]),
            sum([m(d0, a1), m(d2, a4_19), m(a3, a3_19)]),
            sum([m(d0, a2), m(a1, a1), m(d3, a4_19)]),
            sum([m(d0, a3), m(d1, a2), m(a4, a4_19)]),
            sum([m(d0, a4), m(d1, a3), m(a2, a2)]),
        ])
    }

    /// `self^(2^k)`.
    fn pow2k(&self, k: u32) -> Fe {
        let mut x = *self;
        for _ in 0..k {
            x = x.square();
        }
        x
    }

    /// `self^((p − 5)/8) = self^(2^252 − 3)`, by an addition chain: 251 squarings and 11
    /// multiplications. Each step's comment is the exponent reached.
    pub(crate) fn pow_p58(&self) -> Fe {
        let z = *self;
        let z2 = z.square(); // 2
        let z9 = z2.pow2k(2).mul(&z); // 9
        let z11 = z9.mul(&z2); // 11
        let z_5 = z11.square().mul(&z9); // 31 = 2^5 − 1
        let z_10 = z_5.pow2k(5).mul(&z_5); // 2^10 − 1
        let z_20 = z_10.pow2k(10).mul(&z_10); // 2^20 − 1
        let z_40 = z_20.pow2k(20).mul(&z_20); // 2^40 − 1
        let z_50 = z_40.pow2k(10).mul(&z_10); // 2^50 − 1
        let z_100 = z_50.pow2k(50).mul(&z_50); // 2^100 − 1
        let z_200 = z_100.pow2k(100).mul(&z_100); // 2^200 − 1
        let z_250 = z_200.pow2k(50).mul(&z_50); // 2^250 − 1
        z_250.pow2k(2).mul(&z) // 2^252 − 4 + 1
    }

    /// `self^(p − 2) = self^(-1)` for non-zero `self`: (2^252 − 3)·8 + 3 = 2^255 − 21.
    #[cfg(test)]
    pub(crate) fn invert(&self) -> Fe {
        self.pow_p58().pow2k(3).mul(&self.square().mul(self))
    }

    /// A square root of `u/v`, by RFC 8032 §5.1.3 step 3, or `None` if `u/v` is not a
    /// square. `v` must be non-zero.
    ///
    /// The candidate is x = u·v³·(u·v⁷)^((p−5)/8). If v·x² = u, x is a root; if v·x² = −u,
    /// x·sqrt(−1) is; otherwise there is none.
    pub(crate) fn sqrt_ratio(u: &Fe, v: &Fe) -> Option<Fe> {
        let v3 = v.square().mul(v);
        let v7 = v3.square().mul(v);
        let x = u.mul(&v3).mul(&u.mul(&v7).pow_p58());
        let vx2 = v.mul(&x.square());
        if vx2.equals(u) {
            Some(x)
        } else if vx2.equals(&u.neg()) {
            Some(x.mul(&SQRT_M1))
        } else {
            None
        }
    }

    /// Whether `self ≡ rhs (mod p)`: the canonical encodings are equal.
    pub(crate) fn equals(&self, rhs: &Fe) -> bool {
        let (a, b) = (self.to_bytes(), rhs.to_bytes());
        a.iter().zip(&b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    }

    pub(crate) fn is_zero(&self) -> bool {
        self.equals(&Fe::ZERO)
    }

    /// Whether the canonical value is odd: the "negative" x of RFC 8032 §5.1.2.
    pub(crate) fn is_odd(&self) -> bool {
        let [b0, ..] = self.to_bytes();
        b0 & 1 == 1
    }
}
