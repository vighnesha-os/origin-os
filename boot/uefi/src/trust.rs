//! The keys this loader trusts (ONBF §8.1, ADR-0002 B2 step 3, B7).
//!
//! The trust store is compiled in. Today it holds at most one key: the **test** kernel key
//! of `boot/uefi/trust/`, and only in a build with the `test-trust-store` feature. That
//! key's private half is derived from public text (see that directory's README), so it
//! proves the code path and nothing else, and every boot of such a build says so. A build
//! without the feature trusts nothing and refuses every kernel, which is the safe failure
//! until Origin's own root and kernel keys exist (ADR-0002 B7, B8; errata V4).
//!
//! Certificate chains (ONBF §7.3–§7.4) are not implemented: these are the signers themselves,
//! not roots (errata V1).

#[cfg(feature = "test-trust-store")]
use onbf_format::signature::Role;
#[cfg(feature = "test-trust-store")]
use onbf_verify::PublicKey;
use onbf_verify::TrustedKey;

/// The Ed25519 half of the test kernel key.
#[cfg(feature = "test-trust-store")]
const TEST_ED25519: &[u8; 32] = include_bytes!("../trust/TEST-ONLY-origin-kernel.ed25519");
/// The ML-DSA-87 half of the test kernel key.
#[cfg(feature = "test-trust-store")]
const TEST_MLDSA87: &[u8; 2592] = include_bytes!("../trust/TEST-ONLY-origin-kernel.mldsa87");

/// Every key this build trusts.
pub static STORE: &[TrustedKey<'static>] = &[
    #[cfg(feature = "test-trust-store")]
    TrustedKey {
        role: Role::Kernel,
        key: PublicKey {
            ed25519: TEST_ED25519,
            mldsa: TEST_MLDSA87,
        },
    },
];

/// True if this build trusts a test key. The loader prints a warning on every boot.
pub const HOLDS_TEST_KEY: bool = cfg!(feature = "test-trust-store");

// "Must never ship" (CLAUDE.md §59), enforced: a released build has a non-zero build number
// (`origin-version.toml`), so a loader holding the test key cannot be built for one.
const _: () = assert!(
    !HOLDS_TEST_KEY || origin_version::BUILD == 0,
    "a loader holding the test key cannot be a released build"
);
