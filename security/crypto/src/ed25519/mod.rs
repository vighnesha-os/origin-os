//! Ed25519 signature verification (RFC 8032 §5.1, the pure variant; not Ed25519ph or
//! Ed25519ctx), with the strict encoding checks ONBF §7.1 requires so that the host signer
//! and the OS verifier always agree.
//!
//! Strict semantics, checked in this order; the first failure is the result:
//! 1. A, the public key, decodes strictly (RFC 8032 §5.1.3): y < p, a square root exists,
//!    and not x = 0 with the sign bit set. A must not have small order ([8]A ≠ identity).
//! 2. R, the first half of the signature: the same checks.
//! 3. S, the second half, is canonical: S < L.
//! 4. With k = SHA-512(R ‖ A ‖ M) mod L, the cofactorless equation [S]B = R + [k]A holds.
//!    It is evaluated as [S]B + [k](−A) = R, comparing projective points.
//!
//! Points of mixed order (2L, 4L or 8L: a small-order component, but not small order) are
//! accepted as A and as R. The cofactorless equation still gives them one deterministic
//! verdict, and an honest signer never produces them.
//!
//! Variable time: verification handles public data only (ONBF §7.2).

pub(crate) mod constants;
pub(crate) mod field;
pub(crate) mod point;
pub(crate) mod scalar;

use crate::Sha512;
use point::{DecodeError, Point};

/// Why a signature was rejected. Each variant names the first failing check, in the order
/// of the module documentation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ed25519Error {
    /// The public key's y is not below p, or x = 0 with the sign bit set.
    PublicKeyNonCanonical,
    /// No curve point has the public key's y.
    PublicKeyNotOnCurve,
    /// The public key has order dividing 8.
    PublicKeySmallOrder,
    /// R's y is not below p, or x = 0 with the sign bit set.
    RNonCanonical,
    /// No curve point has R's y.
    RNotOnCurve,
    /// R has order dividing 8.
    RSmallOrder,
    /// S is not below the group order L.
    SNonCanonical,
    /// Every encoding is valid, but [S]B ≠ R + [k]A: the signature is not for this key and
    /// message.
    EquationFailed,
}

/// Verifies an Ed25519 `signature` (R ‖ S) on `message` under `public_key` (RFC 8032
/// §5.1.7), with the strict checks of the module documentation.
pub fn verify(
    public_key: &[u8; 32],
    message: &[u8],
    signature: &[u8; 64],
) -> Result<(), Ed25519Error> {
    let a = Point::decode(public_key).map_err(|e| match e {
        DecodeError::NonCanonical => Ed25519Error::PublicKeyNonCanonical,
        DecodeError::NotOnCurve => Ed25519Error::PublicKeyNotOnCurve,
    })?;
    if a.has_small_order() {
        return Err(Ed25519Error::PublicKeySmallOrder);
    }
    let mut r_bytes = [0u8; 32];
    let mut s_bytes = [0u8; 32];
    for (dst, src) in r_bytes.iter_mut().chain(s_bytes.iter_mut()).zip(signature) {
        *dst = *src;
    }
    let r = Point::decode(&r_bytes).map_err(|e| match e {
        DecodeError::NonCanonical => Ed25519Error::RNonCanonical,
        DecodeError::NotOnCurve => Ed25519Error::RNotOnCurve,
    })?;
    if r.has_small_order() {
        return Err(Ed25519Error::RSmallOrder);
    }
    let s = scalar::canonical(&s_bytes).ok_or(Ed25519Error::SNonCanonical)?;
    let mut h = Sha512::new();
    h.update(&r_bytes);
    h.update(public_key);
    h.update(message);
    let k = scalar::reduce_wide(&h.finalize());
    if Point::base_plus_mul(&s, &k, &a.neg()).equals(&r) {
        Ok(())
    } else {
        Err(Ed25519Error::EquationFailed)
    }
}
