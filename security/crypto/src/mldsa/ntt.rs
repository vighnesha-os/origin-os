//! The number-theoretic transform of FIPS 204 §7.5: Algorithm 41 (NTT) and Algorithm 42
//! (NTT^−1), transcribed level by level.
//!
//! NTT(w)[i] = w(ζ^(2·BitRev8(i) + 1)), so multiplication in R_q = Z_q[X]/(X^256 + 1) is
//! coefficient-wise multiplication of NTT representations (Algorithm 45). ExpandA samples
//! the matrix directly in this representation, so the output order here is part of the
//! standard, not a free choice.
//!
//! Level `len` (128, 64, …, 1) splits each block of 2·len coefficients with the zetas
//! ZETAS[128/len .. 256/len], one per block in increasing order (Algorithm 41's m counts
//! up); Algorithm 42 walks the levels in reverse with m counting down, so each level takes
//! the same zetas in decreasing order, negated. Every coefficient stays canonical
//! (`field` module invariant).

use super::field::{N_INV, Poly, ZETAS, add, mul, sub};

/// ZETAS[first .. 2·first]: the zetas of the level whose blocks number `first`.
fn level_zetas(first: usize) -> &'static [u32] {
    ZETAS.get(first..first.wrapping_mul(2)).unwrap_or_default()
}

/// One level of Algorithm 41: for each block of `BLOCK = 2·LEN` coefficients and its zeta z,
/// (a, b) ← (a + z·b, a − z·b) across the two halves.
#[inline(always)]
fn forward_level<const LEN: usize, const BLOCK: usize>(w: &mut Poly, zetas: &[u32]) {
    const { assert!(BLOCK == 2 * LEN) };
    for (block, &zeta) in w.as_chunks_mut::<BLOCK>().0.iter_mut().zip(zetas) {
        if let Some((lo, hi)) = block.split_at_mut_checked(LEN) {
            for (a, b) in lo.iter_mut().zip(hi) {
                let t = mul(zeta, *b);
                *b = sub(*a, t);
                *a = add(*a, t);
            }
        }
    }
}

/// One level of Algorithm 42: for each block and z = −zetas[m], with m decreasing,
/// (a, b) ← (a + b, z·(a − b)) = (a + b, zetas[m]·(b − a)).
#[inline(always)]
fn inverse_level<const LEN: usize, const BLOCK: usize>(w: &mut Poly, zetas: &[u32]) {
    const { assert!(BLOCK == 2 * LEN) };
    for (block, &zeta) in w
        .as_chunks_mut::<BLOCK>()
        .0
        .iter_mut()
        .zip(zetas.iter().rev())
    {
        if let Some((lo, hi)) = block.split_at_mut_checked(LEN) {
            for (a, b) in lo.iter_mut().zip(hi) {
                let t = *a;
                *a = add(t, *b);
                *b = mul(zeta, sub(*b, t));
            }
        }
    }
}

/// NTT(w) in place (Algorithm 41).
pub(crate) fn ntt(w: &mut Poly) {
    forward_level::<128, 256>(w, level_zetas(1));
    forward_level::<64, 128>(w, level_zetas(2));
    forward_level::<32, 64>(w, level_zetas(4));
    forward_level::<16, 32>(w, level_zetas(8));
    forward_level::<8, 16>(w, level_zetas(16));
    forward_level::<4, 8>(w, level_zetas(32));
    forward_level::<2, 4>(w, level_zetas(64));
    forward_level::<1, 2>(w, level_zetas(128));
}

/// NTT^−1(ŵ) in place (Algorithm 42), including the final scaling by f = 256^−1 mod q.
pub(crate) fn inverse_ntt(w: &mut Poly) {
    inverse_level::<1, 2>(w, level_zetas(128));
    inverse_level::<2, 4>(w, level_zetas(64));
    inverse_level::<4, 8>(w, level_zetas(32));
    inverse_level::<8, 16>(w, level_zetas(16));
    inverse_level::<16, 32>(w, level_zetas(8));
    inverse_level::<32, 64>(w, level_zetas(4));
    inverse_level::<64, 128>(w, level_zetas(2));
    inverse_level::<128, 256>(w, level_zetas(1));
    for c in w.iter_mut() {
        *c = mul(*c, N_INV);
    }
}
