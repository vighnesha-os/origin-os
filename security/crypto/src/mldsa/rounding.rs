//! Decompose and UseHint (FIPS 204 §7.4, Algorithms 36 and 40), and w1Encode of the hinted
//! high bits (Algorithm 28), for γ2 = (q − 1)/32, the value both parameter sets use.
//!
//! The divisions are by 2γ2, a non-zero constant of the parameter set, so they compile to
//! multiplications; the `checked_*` fallbacks are dead code that LLVM removes. Reducing
//! r1 ± 1 modulo m = (q − 1)/(2γ2) needs no division, because 0 ≤ r1 < m.

use super::encoding::W1_POLY_BYTES;
use super::field::{N, Poly, Q};
use super::params::ParameterSet;

/// (r1, r0) = Decompose(r) for a canonical r (Algorithm 36).
///
/// r0 = r mod± 2γ2, in (−γ2, γ2], and r1 = (r − r0)/(2γ2), except that when r − r0 = q − 1
/// the result is (0, r0 − 1). So r ≡ r1·2γ2 + r0 (mod q) and r1 < m = (q − 1)/(2γ2).
pub(crate) fn decompose<P: ParameterSet>(r: u32) -> (u32, i32) {
    let alpha = P::GAMMA2.wrapping_mul(2);
    let t = r.checked_rem(alpha).unwrap_or(0);
    // r0 is t or t − 2γ2, whichever lies in (−γ2, γ2]. In both cases `multiple` = r − r0 is
    // a multiple of 2γ2 in [0, q − 1]: q − 1 is itself a multiple of 2γ2 and r ≤ q − 1.
    // Values here are below 2^24, so the conversions to i32 cannot fail.
    let (r0, multiple) = if t > P::GAMMA2 {
        (
            signed(t).wrapping_sub(signed(alpha)),
            r.wrapping_add(alpha.wrapping_sub(t)),
        )
    } else {
        (signed(t), r.wrapping_sub(t))
    };
    if multiple == Q.wrapping_sub(1) {
        (0, r0.wrapping_sub(1))
    } else {
        (multiple.checked_div(alpha).unwrap_or(0), r0)
    }
}

/// `x` as an `i32`; every caller passes a value below 2^24.
#[inline(always)]
fn signed(x: u32) -> i32 {
    i32::try_from(x).unwrap_or(0)
}

/// UseHint(h, r) for a canonical r (Algorithm 40): r1, or r1 ± 1 mod m when the hint is
/// set, towards the side of r0.
pub(crate) fn use_hint<P: ParameterSet>(hint: bool, r: u32) -> u32 {
    let m = Q
        .wrapping_sub(1)
        .checked_div(P::GAMMA2.wrapping_mul(2))
        .unwrap_or(1);
    let (r1, r0) = decompose::<P>(r);
    if !hint {
        r1
    } else if r0 > 0 {
        // (r1 + 1) mod m
        if r1.wrapping_add(1) >= m {
            0
        } else {
            r1.wrapping_add(1)
        }
    } else {
        // (r1 − 1) mod m
        if r1 == 0 {
            m.wrapping_sub(1)
        } else {
            r1.wrapping_sub(1)
        }
    }
}

/// w1Encode of one row (Algorithm 28 with SimpleBitPack, Algorithm 16): the 4-bit values
/// UseHint(h_j, w_j), coefficient 2i in the low nibble of byte i and 2i + 1 in the high one.
/// `hints` holds h for this row, bit j of the 256-bit mask for coefficient j.
pub(crate) fn w1_encode_row<P: ParameterSet>(
    w: &Poly,
    hints: &[u64; N / 64],
    out: &mut [u8; W1_POLY_BYTES],
) {
    // 64 coefficients at a time: one word of the hint mask, 32 bytes of output.
    let groups = out
        .as_chunks_mut::<32>()
        .0
        .iter_mut()
        .zip(w.as_chunks::<64>().0)
        .zip(hints);
    for ((bytes, coeffs), &word) in groups {
        let mut bits = word;
        for (byte, &[a, b]) in bytes.iter_mut().zip(coeffs.as_chunks::<2>().0) {
            let lo = use_hint::<P>(bits & 1 == 1, a);
            let hi = use_hint::<P>(bits & 2 == 2, b);
            bits >>= 2;
            // Both values are below m = 16.
            let [packed, ..] = (lo | (hi << 4)).to_le_bytes();
            *byte = packed;
        }
    }
}
