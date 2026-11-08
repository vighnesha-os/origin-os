//! `onbf-verify`: verification of one ONBF signature entry (ONBF §7.1–§7.2, encodings
//! S1–S4) against a public key the caller trusts.
//!
//! An entry verifies only if **all** of these hold, checked in this order:
//! 1. its key ID is SHA-256 of the key's canonical encoding (the OCBOR-1 `keys` map
//!    `{0: ed25519, 1: mldsa}`, S4), so the entry names this key;
//! 2. the key's ML-DSA length is the one the entry's suite requires;
//! 3. the statement's content digest equals the image's recomputed digest;
//! 4. the statement's profile equals the image header's profile;
//! 5. Ed25519 (RFC 8032, strict) verifies over the exact statement bytes;
//! 6. ML-DSA (FIPS 204, strict) verifies over the same bytes with the role's context string
//!    `origin/onbf/1/<role>`.
//!
//! Which keys are trusted, and certificate chains, are the trust store's business (§7.3, B7).
//!
//! Contract, as for the other freestanding libraries: `no_std`, no allocation, no `unsafe`,
//! no panics.
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::unreachable,
        clippy::cast_possible_truncation
    )
)]

use onbf_format::signature::{Entry, Suite};
use origin_crypto::{
    Ed25519Error, MlDsaError, Sha256, ed25519_verify, mldsa65_verify, mldsa87_verify,
};

/// A signer's public keys.
#[derive(Clone, Copy, Debug)]
pub struct PublicKey<'a> {
    /// The Ed25519 key.
    pub ed25519: &'a [u8; 32],
    /// The ML-DSA key: 1,952 bytes (ML-DSA-65) or 2,592 bytes (ML-DSA-87).
    pub mldsa: &'a [u8],
}

/// Why an entry did not verify.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerifyError {
    /// The entry's key ID is not this key's.
    KeyId,
    /// The key's ML-DSA length does not fit the entry's suite.
    KeyLength,
    /// The statement's content digest is not the image's.
    Digest,
    /// The statement's profile is not the image's.
    Profile,
    /// The Ed25519 signature failed.
    Ed25519(Ed25519Error),
    /// The ML-DSA signature failed.
    MlDsa(MlDsaError),
}

/// OCBOR-1 head for a byte string of `len` bytes (lengths here are 32, 1,952 or 2,592; a
/// length beyond 16 bits yields an empty head, which no stored key ID can match).
fn bytes_head(len: usize, out: &mut [u8; 3]) -> &[u8] {
    match (u8::try_from(len), u16::try_from(len)) {
        (Ok(n), _) if n < 24 => {
            *out = [0x40 | n, 0, 0];
            out.get(..1).unwrap_or(&[])
        }
        (Ok(n), _) => {
            *out = [0x58, n, 0];
            out.get(..2).unwrap_or(&[])
        }
        (_, Ok(n)) => {
            let [hi, lo] = n.to_be_bytes();
            *out = [0x59, hi, lo];
            out.get(..3).unwrap_or(&[])
        }
        _ => &[],
    }
}

/// SHA-256 of the key's canonical encoding: the OCBOR-1 map `{0: ed25519, 1: mldsa}`.
pub fn key_id(key: &PublicKey<'_>) -> [u8; 32] {
    let mut h = Sha256::new();
    let mut head = [0u8; 3];
    h.update(&[0xA2, 0x00]);
    h.update(bytes_head(32, &mut head));
    h.update(key.ed25519);
    h.update(&[0x01]);
    h.update(bytes_head(key.mldsa.len(), &mut head));
    h.update(key.mldsa);
    h.finalize()
}

/// Verifies `entry` under `key` for an image whose content digest is `content_digest` and
/// whose header profile is `profile`.
pub fn verify_entry(
    entry: &Entry<'_>,
    key: &PublicKey<'_>,
    content_digest: &[u8; 64],
    profile: u64,
) -> Result<(), VerifyError> {
    if entry.key_id != key_id(key) {
        return Err(VerifyError::KeyId);
    }
    if key.mldsa.len() != entry.suite.mldsa_public_key_len() {
        return Err(VerifyError::KeyLength);
    }
    if entry.statement.content_digest != content_digest {
        return Err(VerifyError::Digest);
    }
    if entry.statement.profile != profile {
        return Err(VerifyError::Profile);
    }
    let message = entry.statement.bytes;
    let ed_sig: &[u8; 64] = entry
        .sig_ed25519
        .try_into()
        .map_err(|_| VerifyError::Ed25519(Ed25519Error::EquationFailed))?;
    ed25519_verify(key.ed25519, message, ed_sig).map_err(VerifyError::Ed25519)?;
    let context = entry.role.context();
    match entry.suite {
        Suite::Oss1 => {
            let (Ok(pk), Ok(sig)) = (key.mldsa.try_into(), entry.sig_mldsa.try_into()) else {
                return Err(VerifyError::KeyLength);
            };
            mldsa65_verify(pk, message, context, sig).map_err(VerifyError::MlDsa)
        }
        Suite::Oss1R => {
            let (Ok(pk), Ok(sig)) = (key.mldsa.try_into(), entry.sig_mldsa.try_into()) else {
                return Err(VerifyError::KeyLength);
            };
            mldsa87_verify(pk, message, context, sig).map_err(VerifyError::MlDsa)
        }
    }
}

#[cfg(test)]
#[path = "../../format/src/testing.rs"]
mod testing;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_soak;
