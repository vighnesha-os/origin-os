//! Decoding for verification (FIPS 204 §7.1–§7.2): the layouts of pkDecode (Algorithm 23)
//! and sigDecode (Algorithm 27), the unpacking of t1 (SimpleBitUnpack, Algorithm 18) and z
//! (BitUnpack, Algorithm 19), and HintBitUnpack (Algorithm 21) with every malformed-input
//! check of the standard.
//!
//! Bit strings are little-endian (IntegerToBits, BitsToBytes): coefficient i of a b-bit
//! packing occupies bits b·i .. b·i + b of the byte string, least significant bit first.
//! t1 (10 bits) packs 4 coefficients into each 5 bytes and z (20 bits) packs 2, so both
//! are read 40 bits at a time.

use super::MlDsaError;
use super::field::{N, Poly, Q, lo32};
use super::params::ParameterSet;

/// Bytes of one t1 polynomial: 32·(bitlen(q − 1) − d) = 32·10.
pub(crate) const T1_ROW_BYTES: usize = 320;
/// Bytes of one z polynomial for γ1 = 2^19: 32·(1 + bitlen(γ1 − 1)) = 32·20.
pub(crate) const Z_POLY_BYTES: usize = 640;
/// Bytes of one w1 polynomial for γ2 = (q − 1)/32: 32·bitlen((q − 1)/(2γ2) − 1) = 32·4.
pub(crate) const W1_POLY_BYTES: usize = 128;
/// d, the bits dropped from t (Power2Round).
const D: u32 = 13;

/// pk = ρ ‖ t1[0] ‖ … ‖ t1[k − 1] (Algorithm 22). pkDecode cannot fail: every 32-byte ρ and
/// every 10-bit coefficient is a valid encoding.
pub(crate) struct PublicKey<'a, const K: usize> {
    pub(crate) rho: &'a [u8; 32],
    pub(crate) t1: &'a [[u8; T1_ROW_BYTES]; K],
}

/// σ = c̃ ‖ z[0] ‖ … ‖ z[ℓ − 1] ‖ h (Algorithm 26), split but not yet decoded.
pub(crate) struct Signature<'a, const L: usize> {
    pub(crate) ctilde: &'a [u8],
    pub(crate) z: &'a [[u8; Z_POLY_BYTES]; L],
    /// The ω + k bytes of HintBitPack(h).
    pub(crate) hint: &'a [u8],
}

/// Splits `pk` into ρ and the k rows of t1, or `None` if its length is not 32 + 320·k.
pub(crate) fn split_public_key<const K: usize>(pk: &[u8]) -> Option<PublicKey<'_, K>> {
    let (rho, rest) = pk.split_first_chunk::<32>()?;
    let (rows, []) = rest.as_chunks::<T1_ROW_BYTES>() else {
        return None;
    };
    Some(PublicKey {
        rho,
        t1: rows.try_into().ok()?,
    })
}

/// Splits `sig` into c̃ (`ctilde_bytes`), the ℓ packed z polynomials and the hint bytes,
/// or `None` if it is too short. The hint length is checked by [`hint_bit_unpack`].
pub(crate) fn split_signature<const L: usize>(
    sig: &[u8],
    ctilde_bytes: usize,
) -> Option<Signature<'_, L>> {
    let (ctilde, rest) = sig.split_at_checked(ctilde_bytes)?;
    let (z, hint) = rest.split_at_checked(Z_POLY_BYTES.checked_mul(L)?)?;
    let (z, []) = z.as_chunks::<Z_POLY_BYTES>() else {
        return None;
    };
    Some(Signature {
        ctilde,
        z: z.try_into().ok()?,
        hint,
    })
}

/// Five bytes as a little-endian 40-bit integer.
#[inline(always)]
fn le40(bytes: &[u8; 5]) -> u64 {
    let [b0, b1, b2, b3, b4] = bytes.map(u64::from);
    b0 | (b1 << 8) | (b2 << 16) | (b3 << 24) | (b4 << 32)
}

/// t1[i]·2^d for one row (SimpleBitUnpack(·, 2^10 − 1)). Every value is canonical without
/// reduction: t1 ≤ 1023, and 1023·2^13 = q − 1.
pub(crate) fn t1_times_2d(row: &[u8; T1_ROW_BYTES], out: &mut Poly) {
    for (coeffs, bytes) in out
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(row.as_chunks::<5>().0)
    {
        let v = le40(bytes);
        let [c0, c1, c2, c3] = coeffs;
        *c0 = lo32(v & 0x3ff) << D;
        *c1 = lo32((v >> 10) & 0x3ff) << D;
        *c2 = lo32((v >> 20) & 0x3ff) << D;
        *c3 = lo32((v >> 30) & 0x3ff) << D;
    }
}

/// One z polynomial, BitUnpack(·, γ1 − 1, γ1): each 20-bit field f encodes z = γ1 − f, so
/// z ∈ [−γ1 + 1, γ1] and ‖z‖∞ = max |γ1 − f| (no reduction mod± q is involved, because
/// γ1 < q/2). Writes z mod q, canonical, to `out`, or fails with `ZOutOfRange` at the first
/// coefficient with |z| ≥ γ1 − β (Algorithm 8 step 13).
pub(crate) fn decode_z<P: ParameterSet>(
    bytes: &[u8; Z_POLY_BYTES],
    out: &mut Poly,
) -> Result<(), MlDsaError> {
    let bound = P::GAMMA1.wrapping_sub(P::BETA);
    for (pair, chunk) in out
        .as_chunks_mut::<2>()
        .0
        .iter_mut()
        .zip(bytes.as_chunks::<5>().0)
    {
        let v = le40(chunk);
        // v < 2^40, so both fields are below 2^20.
        for (dst, field) in pair.iter_mut().zip([lo32(v & 0xf_ffff), lo32(v >> 20)]) {
            let (magnitude, canonical) = if field <= P::GAMMA1 {
                let z = P::GAMMA1.wrapping_sub(field);
                (z, z)
            } else {
                // z = −(field − γ1), with field − γ1 in [1, 2^19 − 1].
                let neg = field.wrapping_sub(P::GAMMA1);
                (neg, Q.wrapping_sub(neg))
            };
            if magnitude >= bound {
                return Err(MlDsaError::ZOutOfRange);
            }
            *dst = canonical;
        }
    }
    Ok(())
}

/// HintBitUnpack(y) (Algorithm 21) for y = the last ω + k bytes of the signature: bytes
/// 0..ω are hint positions, and byte ω + i is the running count of positions up to and
/// including polynomial i. Writes h as one 256-bit mask per polynomial (bit j of
/// `masks[i]` is h[i]_j), or fails at the first malformed byte, in Algorithm 21's order:
///
/// for each polynomial i in turn,
/// 1. its count is below the previous count or above ω (step 4): `HintCountOutOfRange`;
/// 2. its positions are not strictly increasing (steps 8–9): `HintIndexNotIncreasing`;
///
/// and then, once all k polynomials are read,
/// 3. a position byte after the last used one is not zero (steps 16–17):
///    `HintPaddingNonZero`.
///
/// Positions restart with each polynomial, so the first position of polynomial i is not
/// compared with the last one of polynomial i − 1. With these checks the encoding of h is
/// unique, which strong unforgeability needs.
pub(crate) fn hint_bit_unpack<P: ParameterSet, const K: usize>(
    y: &[u8],
    masks: &mut [[u64; N / 64]; K],
) -> Result<(), MlDsaError> {
    let (positions, counts) = y
        .split_at_checked(P::OMEGA)
        .ok_or(MlDsaError::HintCountOutOfRange)?;
    let counts: &[u8; K] = counts
        .try_into()
        .map_err(|_| MlDsaError::HintCountOutOfRange)?;
    let mut index = 0usize;
    for (mask, &count) in masks.iter_mut().zip(counts) {
        let count = usize::from(count);
        if count < index || count > P::OMEGA {
            return Err(MlDsaError::HintCountOutOfRange);
        }
        // count ≤ ω = positions.len(), so this range always exists.
        let run = positions
            .get(index..count)
            .ok_or(MlDsaError::HintCountOutOfRange)?;
        let mut previous: Option<u8> = None;
        for &position in run {
            if previous.is_some_and(|p| p >= position) {
                return Err(MlDsaError::HintIndexNotIncreasing);
            }
            if let Some(word) = mask.get_mut(usize::from(position >> 6)) {
                *word |= 1u64.wrapping_shl(u32::from(position & 63));
            }
            previous = Some(position);
        }
        index = count;
    }
    if positions.iter().skip(index).any(|&b| b != 0) {
        return Err(MlDsaError::HintPaddingNonZero);
    }
    Ok(())
}
