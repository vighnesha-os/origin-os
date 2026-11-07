//! Pseudorandom sampling for verification (FIPS 204 §7.3): SampleInBall (Algorithm 29) and
//! RejNTTPoly (Algorithm 30), which generates each entry of Â = ExpandA(ρ) (Algorithm 32).
//!
//! Both are rejection samplers over a SHAKE output stream; neither bounds its loop, as in
//! the standard. Each candidate is accepted with probability at least q/2^23 > 0.999
//! (RejNTTPoly) or 197/256 (SampleInBall, since τ ≤ 60), so an input that makes either run
//! long would have to defeat SHAKE.

use super::field::{N, Poly, Q, add, mul};
use super::params::ParameterSet;
use crate::{Shake128, Shake256};

/// c = SampleInBall(ρ) (Algorithm 29), ρ being all of c̃ (λ/4 bytes): τ coefficients are
/// ±1, written canonically as 1 or q − 1, and the rest are 0.
///
/// The first 8 bytes of H(ρ) are 64 sign bits, least significant first; then for
/// i = 256 − τ .. 255, a byte j ≤ i is drawn by rejection, c_i ← c_j, and c_j ← (−1)^(sign
/// bit i + τ − 256).
pub(crate) fn sample_in_ball<P: ParameterSet>(seed: &[u8], c: &mut Poly) {
    const { assert!(P::TAU <= 64) };
    let mut h = Shake256::new();
    h.update(seed);
    let mut xof = h.finalize_xof();
    let mut sign_bytes = [0u8; 8];
    xof.squeeze(&mut sign_bytes);
    let mut signs = u64::from_le_bytes(sign_bytes);
    c.fill(0);
    for i in N.wrapping_sub(P::TAU)..N {
        let j = loop {
            let mut byte = [0u8; 1];
            xof.squeeze(&mut byte);
            let [j] = byte;
            if usize::from(j) <= i {
                break usize::from(j);
            }
        };
        let cj = c.get(j).copied().unwrap_or(0);
        if let Some(ci) = c.get_mut(i) {
            *ci = cj;
        }
        if let Some(cj) = c.get_mut(j) {
            *cj = if signs & 1 == 1 { Q.wrapping_sub(1) } else { 1 };
        }
        signs >>= 1;
    }
}

/// CoeffFromThreeBytes (Algorithm 14): the low 23 bits of b0 + 2^8·b1 + 2^16·b2 (the top
/// bit of b2 cleared), if below q.
#[inline(always)]
fn coeff_from_three_bytes(b0: u8, b1: u8, b2: u8) -> Option<u32> {
    let z = u32::from(b0) | (u32::from(b1) << 8) | (u32::from(b2 & 0x7f) << 16);
    (z < Q).then_some(z)
}

/// `acc ← acc + Â[row][column] ∘ ẑ` with Â[row][column] = RejNTTPoly(ρ ‖ column ‖ row)
/// (Algorithms 30 and 32: the column byte comes first).
///
/// Each coefficient of the matrix entry is multiplied into `acc` as soon as it is sampled,
/// so the entry itself is never stored: verification holds no part of Â.
pub(crate) fn add_matrix_entry_product(
    acc: &mut Poly,
    rho: &[u8; 32],
    row: u8,
    column: u8,
    z_hat: &Poly,
) {
    let mut g = Shake128::new();
    g.update(rho);
    g.update(&[column, row]);
    let mut xof = g.finalize_xof();
    let mut slots = acc.iter_mut().zip(z_hat);
    // One SHAKE128 block (168 bytes = 56 candidates) at a time. Algorithm 30 squeezes 3 bytes
    // per candidate; reading the stream in larger pieces yields the same bytes.
    let mut block = [0u8; Shake128::RATE];
    loop {
        xof.squeeze(&mut block);
        for &[b0, b1, b2] in block.as_chunks::<3>().0 {
            let Some(a) = coeff_from_three_bytes(b0, b1, b2) else {
                continue;
            };
            let Some((slot, z)) = slots.next() else {
                return;
            };
            *slot = add(*slot, mul(a, *z));
            if slots.len() == 0 {
                return;
            }
        }
    }
}
