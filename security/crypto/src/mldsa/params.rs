//! The two ML-DSA parameter sets of the ONBF signature suites (ONBF §7.2): ML-DSA-65 for
//! OSS-1 and ML-DSA-87 for OSS-1R, with the values of FIPS 204 §4 Table 1 and the sizes of
//! Table 2. ML-DSA-44 is not used by Origin and is not provided.
//!
//! The values are typed from Table 1; `src/tests_mldsa.rs` checks the derived ones against
//! their definitions (β = τ·η, λ/4, the Table 2 sizes), and the differential vectors and
//! the OpenSSL cross-check exercise every one of them.

/// The parameters verification needs. η appears only through β = τ·η.
pub(crate) trait ParameterSet {
    /// Rows of Â: the length of t1, w and h.
    const K: usize;
    /// Columns of Â: the length of z.
    const L: usize;
    /// Hamming weight of the challenge c.
    const TAU: usize;
    /// The private-key coefficient range, |s| ≤ η. Not needed to verify except through β.
    const ETA: u32;
    /// Collision strength λ in bits; c̃ is λ/4 bytes.
    const LAMBDA: usize;
    /// The coefficient range of y (and of z): γ1.
    const GAMMA1: u32;
    /// The low-order rounding range γ2.
    const GAMMA2: u32;
    /// β = τ·η.
    const BETA: u32;
    /// Maximum number of 1s in the hint h.
    const OMEGA: usize;
    /// λ/4: the length of c̃ in bytes.
    const CTILDE_BYTES: usize;
    /// pk length (Table 2): 32 + 32·k·(bitlen(q − 1) − d) = 32 + 320·k.
    const PUBLIC_KEY_BYTES: usize;
    /// Signature length (Table 2): λ/4 + ℓ·32·(1 + bitlen(γ1 − 1)) + ω + k.
    const SIGNATURE_BYTES: usize;
}

/// ML-DSA-65 (security category 3).
#[derive(Debug)]
pub(crate) struct MlDsa65;

/// ML-DSA-87 (security category 5).
#[derive(Debug)]
pub(crate) struct MlDsa87;

impl ParameterSet for MlDsa65 {
    const K: usize = 6;
    const L: usize = 5;
    const TAU: usize = 49;
    const ETA: u32 = 4;
    const LAMBDA: usize = 192;
    const GAMMA1: u32 = 1 << 19;
    const GAMMA2: u32 = (super::field::Q - 1) / 32;
    const BETA: u32 = 196;
    const OMEGA: usize = 55;
    const CTILDE_BYTES: usize = 48;
    const PUBLIC_KEY_BYTES: usize = 1952;
    const SIGNATURE_BYTES: usize = 3309;
}

impl ParameterSet for MlDsa87 {
    const K: usize = 8;
    const L: usize = 7;
    const TAU: usize = 60;
    const ETA: u32 = 2;
    const LAMBDA: usize = 256;
    const GAMMA1: u32 = 1 << 19;
    const GAMMA2: u32 = (super::field::Q - 1) / 32;
    const BETA: u32 = 120;
    const OMEGA: usize = 75;
    const CTILDE_BYTES: usize = 64;
    const PUBLIC_KEY_BYTES: usize = 2592;
    const SIGNATURE_BYTES: usize = 4627;
}
