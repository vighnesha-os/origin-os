//! `origin-crypto`: established cryptographic algorithms, written from their specifications
//! (CLAUDE.md §4, §59). Origin OS needs verification only; signing lives in host tools and
//! release infrastructure (ADR-0001 D3).
//!
//! This version provides SHA-256 and SHA-512 from FIPS 180-4, the hashes ONBF uses for
//! content digests, section digests and Merkle trees (ONBF §4.5, §4.6). Both are portable
//! Rust. A SHA-NI path with runtime CPU detection is planned (ONBF §4.6; CLAUDE.md §24).
//! Ed25519 verification (RFC 8032, strict, ONBF §7.1) is variable time: it handles only
//! public data (ONBF §7.2).
//!
//! Contract, the same as the ONBF parser's (ONBF §12): `no_std`, no allocation, no
//! `unsafe`, no panics, and no data-dependent branches or table lookups. Round constants
//! are indexed by round number only. SHA-2 processes public data in Origin's verification
//! paths. The state is not zeroized on drop, which matters only if secrets are ever hashed
//! (see `SOURCE_PROVENANCE.md`).
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
mod sha256;
mod sha512;

pub use ed25519::{Ed25519Error, verify as ed25519_verify};
pub use sha256::Sha256;
pub use sha512::Sha512;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_ed25519;
