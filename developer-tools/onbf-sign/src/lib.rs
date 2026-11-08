//! `onbf-sign`: Ed25519 and ML-DSA-87 **signing**, for the build host only (ONBF §7.2,
//! CLAUDE.md §59).
//!
//! Origin OS itself contains verification code only. This crate holds the signing half, written
//! from RFC 8032 and FIPS 204 for the owner's decision of 2026-11-08 (host signing in original
//! Rust rather than through OpenSSL). It must never be linked into the OS; `cargo xtask check`
//! enforces that.
//!
//! - [`ed25519`]: key generation and signing (RFC 8032 §5.1.5, §5.1.6).
//! - [`mldsa87`]: ML-DSA-87 key generation and signing (FIPS 204 Algorithms 2, 6, 7).
//!
//! Hashing reuses `origin-crypto`'s SHA-512, SHAKE128 and SHAKE256, whose state updates have no
//! data-dependent branches or table lookups (see that crate's documentation). Everything else
//! is written here and shares no arithmetic with the verifiers, so a signature made by this
//! crate that `origin-crypto` accepts is agreement between two independent implementations.
//!
//! ## Side-channel assessment (CLAUDE.md §59)
//!
//! Recorded after a separate read of the code and of its compiled form on aarch64 and x86-64.
//!
//! What handles secrets, and how:
//! - **Ed25519:** the scalar, the nonce and every intermediate point. Field arithmetic is on
//!   fixed-width limbs with no secret-dependent branch or memory index. The base-point
//!   multiplication doubles and adds on every bit and selects by mask, and scalars are reduced
//!   by shift-and-conditional-subtract, also by mask. Those two masks go through `black_box`:
//!   LLVM compiled the reduction's mask into data-dependent branches without it.
//! - **ML-DSA:** s1, s2, t0, K and each masking vector y. Coefficient arithmetic is on reduced
//!   32-bit values with Barrett reduction and mask-based conditional subtraction, and the NTT
//!   is a fixed sequence of operations indexed only by position.
//!
//! What can still depend on secret-related values, as FIPS 204 expects:
//! - ML-DSA's rejection sampling. `ExpandS` rejects nibbles. `Sign_internal` restarts on two
//!   combined decisions, so timing shows whether an attempt was rejected and at which of those
//!   two points, but no coefficient. Rejected candidates are discarded (FIPS 204 §3.6).
//! - `SampleInBall` branches on c̃. For the accepted attempt c̃ is public; for a rejected one
//!   it is not, but no attack on that is known.
//!
//! What this assessment does not establish:
//! - that a future compiler keeps these properties. `cargo xtask check` counts the conditional
//!   branches of `ed25519::sign` in its assembly on the build host, so a regression like the
//!   one found shows up, but the ML-DSA paths and the other architecture were checked once, by
//!   hand;
//! - that the processor does: Apple documents data-independent timing only under PSTATE.DIT,
//!   which needs `unsafe` to set, and the M-series data-memory-dependent prefetcher can leak
//!   even index-free code. Keep to trusted hosts;
//! - any timing measurement: none has been made.
//!
//! **Secret zeroization is best effort**, accepted by the owner for host development keys on
//! 2026-11-08. The crate forbids `unsafe` (ONBF §12), so it cannot
//! use volatile writes: [`wipe`] and [`wipe_words`] overwrite buffers and pass them to
//! `black_box`, which the compiler treats as an opaque use but does not promise to honour.
//! Secrets are written in place, to avoid copies from returns and moves, but coverage is
//! incomplete: `origin-crypto`'s hasher state is not wiped (see `SOURCE_PROVENANCE.md`, the
//! SHA-2 entry), and Rust moves leave old bytes behind. Release signing belongs in an HSM
//! (ONBF §7.2, CLAUDE.md §60), not this crate.
#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(clippy::panic, clippy::unwrap_used, clippy::expect_used)
)]

pub mod ed25519;
pub mod mldsa87;

#[cfg(test)]
mod tests;

/// Overwrites `bytes` with zeros, best effort (see the crate documentation).
pub fn wipe(bytes: &mut [u8]) {
    bytes.fill(0);
    core::hint::black_box(&*bytes);
}

/// Overwrites `words` with zeros, best effort (see the crate documentation).
pub fn wipe_words(words: &mut [u32]) {
    words.fill(0);
    core::hint::black_box(&*words);
}

/// Fills `out` from the operating system's random number generator.
pub fn os_random(out: &mut [u8]) -> std::io::Result<()> {
    use std::io::Read;
    std::fs::File::open("/dev/urandom")?.read_exact(out)
}
