//! The generator's known-answer self-test (NIST SP 800-90A Rev. 1 §11.3; ADR-0016 R5).
//!
//! It runs before a generator is first instantiated, in the kernel and in the loader. It
//! instantiates, reseeds and generates from fixed inputs and compares the output with fixed
//! answers: NIST's own, from its ACVP vectors for HMAC_DRBG over SHA-512 (`kat.rs`). It then
//! checks that an uninstantiated generator answers nothing and that the refusals the standard
//! asks for are made. If any of it fails there is no generator (R8).
//!
//! Everything here is public test data: nothing is overwritten afterwards.

use origin_crypto::{DrbgError, HmacDrbgSha512};

use crate::kat::VECTORS;

/// One thing asked of the generator after it is instantiated.
pub(crate) enum Step {
    /// The reseed function.
    Reseed {
        entropy: &'static [u8],
        additional: &'static [u8],
    },
    /// A request.
    Generate { additional: &'static [u8] },
    /// A request with prediction resistance.
    Fresh {
        entropy: &'static [u8],
        additional: &'static [u8],
    },
}

/// One of NIST's vectors: instantiate, do each step, and the last request's output is the
/// answer.
pub(crate) struct Vector {
    /// NIST's number for it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) tc: u32,
    pub(crate) entropy: &'static [u8],
    pub(crate) nonce: &'static [u8],
    pub(crate) personalization: &'static [u8],
    pub(crate) steps: &'static [Step],
    pub(crate) answer: &'static [u8],
}

/// The length of every answer in NIST's set for this generator.
const ANSWER_LEN: usize = 512;

/// Runs the self-test. `true` only if every vector gives NIST's answer and every refusal is
/// made.
pub fn run() -> bool {
    VECTORS.iter().all(passes) && refuses()
}

/// Whether the generator gives `vector`'s answer.
pub(crate) fn passes(vector: &Vector) -> bool {
    if vector.answer.len() != ANSWER_LEN {
        return false;
    }
    let mut out = [0u8; ANSWER_LEN];
    let mut drbg = HmacDrbgSha512::new();
    if drbg
        .instantiate(vector.entropy, vector.nonce, vector.personalization)
        .is_err()
    {
        return false;
    }
    for step in vector.steps {
        let done = match step {
            Step::Reseed {
                entropy,
                additional,
            } => drbg.reseed(entropy, additional),
            Step::Generate { additional } => drbg.generate(&mut out, additional),
            Step::Fresh {
                entropy,
                additional,
            } => drbg.reseed_and_generate(&mut out, entropy, additional),
        };
        if done.is_err() {
            return false;
        }
    }
    let mut difference = 0u8;
    for (got, want) in out.iter().zip(vector.answer) {
        difference |= got ^ want;
    }
    drbg.uninstantiate();
    difference == 0 && !drbg.is_instantiated()
}

/// The error handling §11.3 asks to be tested: no output without a state, before
/// instantiation and after uninstantiation; no instantiation from too little entropy or too
/// short a nonce; no second instantiation over a state.
fn refuses() -> bool {
    let mut out = [0u8; 16];
    let mut drbg = HmacDrbgSha512::new();
    let before = drbg.generate(&mut out, &[]) == Err(DrbgError::NotInstantiated);
    let short = drbg.instantiate(&[0x5a; 31], &[0xa5; 16], &[]) == Err(DrbgError::EntropyTooShort)
        && drbg.instantiate(&[0x5a; 32], &[0xa5; 15], &[]) == Err(DrbgError::NonceTooShort)
        && !drbg.is_instantiated();
    let first = drbg.instantiate(&[0x5a; 32], &[0xa5; 16], &[]).is_ok();
    let second =
        drbg.instantiate(&[0x5a; 32], &[0xa5; 16], &[]) == Err(DrbgError::AlreadyInstantiated);
    let answers = drbg.generate(&mut out, &[]).is_ok();
    drbg.uninstantiate();
    let after = drbg.generate(&mut out, &[]) == Err(DrbgError::NotInstantiated)
        && drbg.reseed(&[0x5a; 32], &[]) == Err(DrbgError::NotInstantiated);
    before && short && first && second && answers && after
}
