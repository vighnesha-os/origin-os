//! `origin-crypto`: established cryptographic algorithms, written from their specifications
//! (CLAUDE.md §4, §59). Origin OS verifies signatures and never makes them; signing lives in
//! host tools and release infrastructure (ADR-0001 D3).
//!
//! This version provides:
//!
//! - SHA-256 and SHA-512 from FIPS 180-4, the hashes ONBF uses for content digests, section
//!   digests and Merkle trees (ONBF §4.5, §4.6);
//! - SHA3-256, SHA3-512, SHAKE128 and SHAKE256 from FIPS 202. ML-DSA (FIPS 204), half of
//!   every ONBF signature suite, is built on SHAKE128 and SHAKE256 (ONBF §7.2, ADR-0001);
//! - strict Ed25519 verification from RFC 8032 §5.1 (ONBF §7.1), the other half.
//! - strict ML-DSA-65 and ML-DSA-87 verification from FIPS 204 (pure ML-DSA, with a context
//!   string; ONBF §7.1–§7.2), streaming Â so that it fits a 64 KiB kernel stack.
//! - HMAC-SHA-512 from FIPS 198-1 and RFC 2104;
//! - HMAC_DRBG over SHA-512 from NIST SP 800-90A Rev. 1 §10.1.2: the mechanism of the
//!   kernel's random-number generator (CLAUDE.md §59; ADR-0016). It makes no entropy. Its
//!   seeds, their sources and their health tests are its caller's.
//!
//! All are portable Rust. A SHA-NI path with runtime CPU detection is planned (ONBF §4.6;
//! CLAUDE.md §24).
//!
//! Contract, the same as the ONBF parser's (ONBF §12): `no_std`, no allocation, no
//! `unsafe` and no panics. The hashes have no data-dependent branches or table lookups:
//! SHA-2 round constants are indexed by round number only, and Keccak-f[1600] reads and
//! writes lanes at fixed positions only. The Keccak constants are computed at compile time
//! from their FIPS 202 definitions. Ed25519 verification is variable time by design, because
//! it handles only public data (ONBF §7.2). Origin's verification paths hash public data.
//!
//! Secrets: HMAC and the generator are the only parts that hold any. Neither has a branch or
//! a memory index that depends on one, and both overwrite what they held: [`HmacSha512`] when
//! it is finalized or dropped, [`HmacDrbgSha512`] at every step and when it is uninstantiated
//! or dropped. Without `unsafe` that overwrite is best effort: `wipe.rs` says what it does and
//! does not promise, and ADR-0016 gives the Framekernel the part that takes a volatile write.
//! The public hashers overwrite nothing. A secret hashed through [`Sha512`] or the others
//! stays in memory, which only the host-only signer does, where the owner accepted best-effort
//! zeroization (see `SOURCE_PROVENANCE.md`).
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
        clippy::todo,
        clippy::unimplemented,
        clippy::cast_possible_truncation
    )
)]

mod block;
mod constants;
mod ed25519;
mod hmac;
mod hmac_drbg;
mod keccak;
mod mldsa;
mod sha256;
mod sha3;
mod sha512;
mod wipe;

pub use ed25519::{Ed25519Error, verify as ed25519_verify};
pub use hmac::HmacSha512;
pub use hmac_drbg::{DrbgError, HmacDrbgSha512};
pub use mldsa::{
    MLDSA65_PUBLIC_KEY_BYTES, MLDSA65_SIGNATURE_BYTES, MLDSA87_PUBLIC_KEY_BYTES,
    MLDSA87_SIGNATURE_BYTES, MlDsaError, mldsa65_verify, mldsa87_verify,
};
pub use sha3::{Sha3_256, Sha3_512, Shake128, Shake256, ShakeReader};
pub use sha256::Sha256;
pub use sha512::Sha512;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_ed25519;
#[cfg(test)]
mod tests_hmac;
#[cfg(test)]
mod tests_hmac_drbg;
#[cfg(test)]
mod tests_mldsa;
#[cfg(test)]
mod tests_sha3;
