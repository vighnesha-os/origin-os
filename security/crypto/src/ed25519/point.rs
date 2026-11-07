//! The Ed25519 group: the twisted Edwards curve −x² + y² = 1 + d·x²·y² over GF(p), in
//! extended homogeneous coordinates (RFC 8032 §5.1.4).
//!
//! A point (X:Y:Z:T) with Z ≠ 0 is the affine point x = X/Z, y = Y/Z, and T/Z = x·y. The
//! addition formulas are complete for this curve (a = −1 is a square and d is not, mod p),
//! so they need no special cases for the identity, doubling or small-order points.

use super::constants::{BASE_T, BASE_X, BASE_Y, D, D2};
use super::field::Fe;

/// A curve point in extended coordinates.
#[derive(Clone, Copy)]
pub(crate) struct Point {
    x: Fe,
    y: Fe,
    z: Fe,
    t: Fe,
}

/// Why a 32-byte encoding does not decode (RFC 8032 §5.1.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DecodeError {
    /// y ≥ p, or x = 0 with the sign bit set: not the one canonical encoding of a point.
    NonCanonical,
    /// No x satisfies the curve equation for this y.
    NotOnCurve,
}

impl Point {
    pub(crate) const IDENTITY: Point = Point {
        x: Fe::ZERO,
        y: Fe::ONE,
        z: Fe::ONE,
        t: Fe::ZERO,
    };

    /// The base point B (RFC 8032 §5.1).
    pub(crate) const BASE: Point = Point {
        x: BASE_X,
        y: BASE_Y,
        z: Fe::ONE,
        t: BASE_T,
    };

    /// Strict decoding (RFC 8032 §5.1.3): bits 0–254 are y, bit 255 is the parity of x.
    ///
    /// 1. y ≥ p is rejected (`NonCanonical`).
    /// 2. x² = (y² − 1)/(d·y² + 1); no square root means `NotOnCurve`. The denominator is
    ///    never zero because d is not a square.
    /// 3. x = 0 with the sign bit set is rejected (`NonCanonical`).
    /// 4. If the parity of x differs from the sign bit, x becomes p − x.
    pub(crate) fn decode(bytes: &[u8; 32]) -> Result<Point, DecodeError> {
        let y = Fe::from_bytes(bytes);
        let [.., top] = *bytes;
        let sign = top >> 7 == 1;
        // y is canonical exactly when re-encoding it gives back bits 0–254.
        let mut low = *bytes;
        if let Some(last) = low.last_mut() {
            *last &= 0x7f;
        }
        let canonical = y.to_bytes();
        if canonical
            .iter()
            .zip(&low)
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            != 0
        {
            return Err(DecodeError::NonCanonical);
        }
        let yy = y.square();
        let u = yy.sub(&Fe::ONE);
        let v = D.mul(&yy).add(&Fe::ONE);
        let mut x = Fe::sqrt_ratio(&u, &v).ok_or(DecodeError::NotOnCurve)?;
        if x.is_zero() && sign {
            return Err(DecodeError::NonCanonical);
        }
        if x.is_odd() != sign {
            x = x.neg();
        }
        Ok(Point {
            x,
            y,
            z: Fe::ONE,
            t: x.mul(&y),
        })
    }

    /// The canonical encoding (RFC 8032 §5.1.2). Verification compares points without
    /// encoding them, so this exists for tests only.
    #[cfg(test)]
    pub(crate) fn encode(&self) -> [u8; 32] {
        let zinv = self.z.invert();
        let x = self.x.mul(&zinv);
        let mut out = self.y.mul(&zinv).to_bytes();
        if let Some(last) = out.last_mut() {
            *last |= u8::from(x.is_odd()) << 7;
        }
        out
    }

    /// P + Q, RFC 8032 §5.1.4 (complete; also correct for P = Q).
    pub(crate) fn add(&self, q: &Point) -> Point {
        let a = self.y.sub(&self.x).mul(&q.y.sub(&q.x));
        let b = self.y.add(&self.x).mul(&q.y.add(&q.x));
        let c = self.t.mul(&D2).mul(&q.t);
        let zz = self.z.mul(&q.z);
        let d = zz.add(&zz);
        let (e, f, g, h) = (b.sub(&a), d.sub(&c), d.add(&c), b.add(&a));
        Point {
            x: e.mul(&f),
            y: g.mul(&h),
            z: f.mul(&g),
            t: e.mul(&h),
        }
    }

    /// 2P, RFC 8032 §5.1.4 (the doubling formulas do not use d or T).
    pub(crate) fn double(&self) -> Point {
        let a = self.x.square();
        let b = self.y.square();
        let zz = self.z.square();
        let c = zz.add(&zz);
        let h = a.add(&b);
        let e = h.sub(&self.x.add(&self.y).square());
        let g = a.sub(&b);
        let f = c.add(&g);
        Point {
            x: e.mul(&f),
            y: g.mul(&h),
            z: f.mul(&g),
            t: e.mul(&h),
        }
    }

    /// −P = (−x, y).
    pub(crate) fn neg(&self) -> Point {
        Point {
            x: self.x.neg(),
            y: self.y,
            z: self.z,
            t: self.t.neg(),
        }
    }

    /// Whether P and Q are the same point: X₁·Z₂ = X₂·Z₁ and Y₁·Z₂ = Y₂·Z₁ (Z is never
    /// zero under the complete formulas).
    pub(crate) fn equals(&self, q: &Point) -> bool {
        self.x.mul(&q.z).equals(&q.x.mul(&self.z)) && self.y.mul(&q.z).equals(&q.y.mul(&self.z))
    }

    /// Whether the order of P divides 8, that is [8]P is the identity (0 : 1 : 1 : 0).
    pub(crate) fn has_small_order(&self) -> bool {
        let p8 = self.double().double().double();
        p8.x.is_zero() && p8.y.equals(&p8.z)
    }

    /// [s]B + [k]Q for 256-bit little-endian scalars, by one shared double-and-add pass
    /// (Straus–Shamir) over the bits from the top. Variable time: it runs on public data
    /// only (ONBF §7.2).
    pub(crate) fn base_plus_mul(s: &[u64; 4], k: &[u64; 4], q: &Point) -> Point {
        let base_plus_q = Point::BASE.add(q);
        let mut acc = Point::IDENTITY;
        for (sw, kw) in s.iter().rev().zip(k.iter().rev()) {
            for bit in (0..64u32).rev() {
                acc = acc.double();
                let s_bit = sw.wrapping_shr(bit) & 1 == 1;
                let k_bit = kw.wrapping_shr(bit) & 1 == 1;
                match (s_bit, k_bit) {
                    (true, true) => acc = acc.add(&base_plus_q),
                    (true, false) => acc = acc.add(&Point::BASE),
                    (false, true) => acc = acc.add(q),
                    (false, false) => {}
                }
            }
        }
        acc
    }
}
