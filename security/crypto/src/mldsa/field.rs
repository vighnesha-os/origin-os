//! Arithmetic in Z_q, q = 8380417 = 2^23 − 2^13 + 1 (FIPS 204 §2.3, §4), and the NTT
//! constants.
//!
//! Invariant: every coefficient that enters or leaves these functions is canonical, in
//! [0, q). With that bound:
//! - `add` and `sub` form a value in [0, 2q) < 2^24 and subtract q at most once;
//! - `mul` forms a product below q^2 < 2^46, and `reduce` accepts any `u64`.
//!
//! `reduce` is Barrett reduction with the reciprocal M = ⌊2^64 / q⌋: the quotient estimate
//! ⌊x·M / 2^64⌋ is ⌊x/q⌋ or ⌊x/q⌋ − 1 for every x < 2^64, so one conditional subtraction
//! finishes it. Proof: M = (2^64 − δ)/q with 0 < δ < q (q is odd, so it does not divide
//! 2^64). Then x·M/2^64 = x/q − x·δ/(q·2^64), and the subtracted term is in [0, 1) because
//! x < 2^64 and δ < q. Hence ⌊x/q⌋ − 1 ≤ ⌊x·M/2^64⌋ ≤ ⌊x/q⌋, and x − ⌊x·M/2^64⌋·q ∈ [0, 2q).
//!
//! The constants are computed at compile time from their definitions: ζ = 1753, a primitive
//! 512th root of unity mod q (§2.5), and 256^−1 mod q. No table is typed in; the tests
//! re-derive both another way.

/// The modulus q.
pub(crate) const Q: u32 = 8_380_417;
const Q64: u64 = Q as u64;
/// ζ, the primitive 512th root of unity that defines the NTT (FIPS 204 §2.5, Appendix B).
pub(crate) const ZETA: u32 = 1753;
/// Polynomial degree n.
pub(crate) const N: usize = 256;

/// A polynomial of R_q or its NTT representation: 256 canonical coefficients.
pub(crate) type Poly = [u32; N];

/// The Barrett reciprocal ⌊2^64 / q⌋, equal to ⌊(2^64 − 1) / q⌋ because q does not divide
/// 2^64.
const BARRETT: u64 = u64::MAX.wrapping_div(Q64);

/// The low 64 bits of `x`. The mask makes the conversion infallible, so the fallback is dead
/// code that LLVM removes; it replaces an `as` cast, which the crate denies.
#[inline(always)]
fn lo64(x: u128) -> u64 {
    u64::try_from(x & u128::from(u64::MAX)).unwrap_or(0)
}

/// The low 32 bits of `x`, by the same pattern.
#[inline(always)]
pub(crate) fn lo32(x: u64) -> u32 {
    u32::try_from(x & u64::from(u32::MAX)).unwrap_or(0)
}

/// `x mod q`, canonical, for any `x` (see the module documentation).
#[inline(always)]
pub(crate) fn reduce(x: u64) -> u32 {
    let quotient = lo64(u128::from(x).wrapping_mul(u128::from(BARRETT)) >> 64);
    let r = x.wrapping_sub(quotient.wrapping_mul(Q64));
    lo32(if r >= Q64 { r.wrapping_sub(Q64) } else { r })
}

/// `a·b mod q` for canonical `a` and `b`.
#[inline(always)]
pub(crate) fn mul(a: u32, b: u32) -> u32 {
    reduce(u64::from(a).wrapping_mul(u64::from(b)))
}

/// `a + b mod q` for canonical `a` and `b`.
#[inline(always)]
pub(crate) fn add(a: u32, b: u32) -> u32 {
    let s = a.wrapping_add(b);
    if s >= Q { s.wrapping_sub(Q) } else { s }
}

/// `a − b mod q` for canonical `a` and `b`, computed as a + (q − b) ∈ [1, 2q).
#[inline(always)]
pub(crate) fn sub(a: u32, b: u32) -> u32 {
    let s = a.wrapping_add(Q.wrapping_sub(b));
    if s >= Q { s.wrapping_sub(Q) } else { s }
}

/// `base^exp mod q` by square-and-multiply, for the compile-time constants.
const fn pow_mod(base: u32, exp: u32) -> u32 {
    let mut acc: u64 = 1;
    let mut square = base as u64;
    let mut e = exp;
    while e > 0 {
        if e & 1 == 1 {
            acc = acc.wrapping_mul(square).wrapping_rem(Q64);
        }
        square = square.wrapping_mul(square).wrapping_rem(Q64);
        e >>= 1;
    }
    // acc < q < 2^32.
    let [b0, b1, b2, b3, ..] = acc.to_le_bytes();
    u32::from_le_bytes([b0, b1, b2, b3])
}

/// BitRev8 (FIPS 204 §2.3): the 8-bit reversal of the low byte of `m`.
const fn bit_rev8(m: u32) -> u32 {
    let mut r = 0;
    let mut i = 0;
    while i < 8 {
        r = (r << 1) | (m.wrapping_shr(i) & 1);
        i = i.wrapping_add(1);
    }
    r
}

/// zetas[m] = ζ^BitRev8(m) mod q for m = 0..=255, the table of Algorithms 41 and 42
/// (Appendix B).
pub(crate) const ZETAS: [u32; N] = {
    let mut table = [0; N];
    // Slice patterns, not indexing: the crate's no-panic lints apply to const code too.
    let mut rest: &mut [u32] = &mut table;
    let mut m = 0;
    while let [slot, tail @ ..] = rest {
        *slot = pow_mod(ZETA, bit_rev8(m));
        rest = tail;
        m = m.wrapping_add(1);
    }
    table
};

/// 256^−1 mod q = 256^(q−2) mod q (Fermat), the final scaling of Algorithm 42 (its f).
pub(crate) const N_INV: u32 = pow_mod(256, Q.wrapping_sub(2));
