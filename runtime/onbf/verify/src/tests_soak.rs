//! Seeded multi-fault soak of signature verification (ONBF §7.1–§7.2, §13 item 3).
//!
//! Every fault here is applied to the reference-signed fixture, which no test can re-sign
//! (Origin OS has verification code only, §7.2). So "re-sealing" means handing the check
//! what it compares against: the statement's own digest and profile as the image's, or a
//! key's own ID, which lets a hostile statement, signature or key reach the signature code
//! behind the cheap checks. The order of those checks is judged exactly. The signature
//! code's own verdicts are counted, and one thing is asserted about them: nothing that
//! verifies differs from what was signed.
//!
//! A failure is reproduced by its seed and case number; `ORIGIN_MUTATION_SCALE` scales the
//! case counts.

use onbf_format::signature::SignatureBlock;
use origin_crypto::Sha256;

use super::tests::{Fixture, fixture};
use super::*;
use crate::testing::{Reached, Rng, cases, mutate, mutate_window};

const VERIFY_SEED: u64 = 0x4F4E_4246_5645_5231; // "ONBFVER1"

/// ML-DSA-87: c̃ is 64 bytes, then 7 × 640 of z, then 75 hint positions and 8 hint counts.
const MLDSA_Z: core::ops::Range<usize> = 64..64 + 7 * 640;
const MLDSA_HINTS: core::ops::Range<usize> = 4544..4627;

/// `key_id` from the S4 definition, with the crate's own head encoding not involved.
fn model_key_id(ed: &[u8; 32], ml: &[u8]) -> [u8; 32] {
    let mut doc = vec![0xA2, 0x00, 0x58, 0x20];
    doc.extend(ed);
    doc.push(0x01);
    match ml.len() {
        n @ 0..=23 => doc.push(0x40 | n as u8),
        n @ 24..=0xFF => doc.extend([0x58, n as u8]),
        n @ 0x100..=0xFFFF => {
            doc.push(0x59);
            doc.extend((n as u16).to_be_bytes());
        }
        // Documented on `bytes_head`: no head at all beyond 16 bits.
        _ => {}
    }
    doc.extend(ml);
    Sha256::digest(&doc)
}

fn offset_in(outer: &[u8], part: &[u8]) -> usize {
    part.as_ptr() as usize - outer.as_ptr() as usize
}

fn ed25519_name(e: Ed25519Error) -> &'static str {
    match e {
        Ed25519Error::PublicKeyNonCanonical => "ed: public key not canonical",
        Ed25519Error::PublicKeyNotOnCurve => "ed: public key not on the curve",
        Ed25519Error::PublicKeySmallOrder => "ed: public key of small order",
        Ed25519Error::RNonCanonical => "ed: R not canonical",
        Ed25519Error::RNotOnCurve => "ed: R not on the curve",
        Ed25519Error::RSmallOrder => "ed: R of small order",
        Ed25519Error::SNonCanonical => "ed: S not canonical",
        Ed25519Error::EquationFailed => "ed: equation failed",
    }
}

fn mldsa_name(e: MlDsaError) -> &'static str {
    match e {
        MlDsaError::ContextTooLong => "ml: context too long",
        MlDsaError::HintCountOutOfRange => "ml: hint count",
        MlDsaError::HintIndexNotIncreasing => "ml: hint order",
        MlDsaError::HintPaddingNonZero => "ml: hint padding",
        MlDsaError::ZOutOfRange => "ml: z out of range",
        MlDsaError::ChallengeMismatch => "ml: challenge mismatch",
    }
}

/// Everything a verifier can return, or be refused before, in this soak.
const VERDICTS: [&str; 19] = [
    "verified",
    "parse refused",
    "key id",
    "key length",
    "digest",
    "profile",
    "ed: public key not canonical",
    "ed: public key not on the curve",
    "ed: public key of small order",
    "ed: R not canonical",
    "ed: R not on the curve",
    "ed: R of small order",
    "ed: S not canonical",
    "ed: equation failed",
    "ml: hint count",
    "ml: hint order",
    "ml: hint padding",
    "ml: z out of range",
    "ml: challenge mismatch",
];

/// An Ed25519 point field: canonical-looking garbage, or the encodings the strict checks
/// are there for.
fn hostile_point(field: &mut [u8], rng: &mut Rng) {
    match rng.below(5) {
        // y = 2^255 - 1 and its sign bit: not below p.
        0 => field.fill(0xFF),
        // y = 1: the identity, of order 1.
        1 => {
            field.fill(0);
            field[0] = 1;
        }
        // y = p - 1: order 2.
        2 => {
            field.fill(0xFF);
            field[0] = 0xEC;
            field[31] = 0x7F;
        }
        // Random: about half of all y values have no curve point.
        _ => field.iter_mut().for_each(|b| *b = rng.byte()),
    }
}

#[test]
fn mutated_signatures_keys_and_statements_never_verify() {
    let f: Fixture = fixture();
    let parsed = SignatureBlock::parse(&f.block).expect("the fixture parses");
    let reference = *parsed.entries().next().expect("one entry");
    let (key_id_at, ed_at, ml_at, st_at, st_len) = (
        offset_in(&f.block, reference.key_id),
        offset_in(&f.block, reference.sig_ed25519),
        offset_in(&f.block, reference.sig_mldsa),
        offset_in(&f.block, reference.statement.bytes),
        reference.statement.bytes.len(),
    );

    let mut reached = Reached::default();
    let total = cases(1500);
    for case in 0..total {
        let mut rng = Rng::for_case(VERIFY_SEED, case);
        let mut block = f.block.clone();
        let (mut ed, mut ml) = (f.ed, f.ml.clone());
        let (mut digest, mut profile) = (f.digest, f.profile);
        // Pass the statement's own digest and profile, so the checks behind them are
        // reached with whatever the statement now says.
        let mut seal_image = false;
        // Re-derive the key's ID into the block, so the checks behind it are reached.
        let mut seal_key_id = false;

        match rng.below(16) {
            // The pristine fixture, under each way of calling it.
            0 => {}
            1 => {
                seal_image = true;
                seal_key_id = true;
            }
            // Raw faults anywhere in the block.
            2 | 3 => mutate(&mut block, &mut rng),
            // The statement's bytes: everything both signatures cover.
            4 | 5 => {
                mutate_window(&mut block, st_at..st_at + st_len, &mut rng);
                seal_image = true;
            }
            // The Ed25519 signature: R and S.
            6 => {
                let r = ed_at..ed_at + 32;
                let s = ed_at + 32..ed_at + 64;
                match rng.below(4) {
                    0 => hostile_point(&mut block[r], &mut rng),
                    1 => block[s].fill(0xFF),
                    2 => block[s.start + 31] |= 0xF0,
                    _ => mutate_window(&mut block, ed_at..ed_at + 64, &mut rng),
                }
            }
            7 => {
                hostile_point(&mut block[ed_at..ed_at + 32], &mut rng);
                seal_image = true;
            }
            // The three parts of the ML-DSA signature, with Ed25519 left intact.
            8 => mutate_window(&mut block, ml_at..ml_at + 64, &mut rng),
            9 => {
                // z: a run of zeros or ones encodes a coefficient out of range.
                let at = ml_at + MLDSA_Z.start + rng.below(MLDSA_Z.len() - 8);
                let fill = rng.pick(&[0x00, 0xFF]);
                block[at..at + 8].fill(fill);
            }
            10 => {
                mutate_window(
                    &mut block,
                    ml_at + MLDSA_HINTS.start..ml_at + MLDSA_HINTS.end,
                    &mut rng,
                );
            }
            // A hostile Ed25519 key under its own ID.
            11 => {
                hostile_point(&mut ed, &mut rng);
                seal_key_id = true;
            }
            // A hostile ML-DSA key under its own ID: right length, then the wrong one.
            12 => {
                mutate_window(&mut ml, 0..f.ml.len(), &mut rng);
                seal_key_id = true;
            }
            13 => {
                let len = rng.pick(&[0, 1, 23, 24, 255, 256, 1952, 2591, 2593, 65_535, 65_536]);
                ml.resize(len, 0xA5);
                seal_key_id = true;
            }
            // What the loader compares against.
            14 => {
                if rng.chance(50) {
                    digest[rng.below(64)] ^= 1 << rng.below(8);
                } else {
                    profile = rng.edge_u64() | 0x100;
                }
            }
            // The key ID the entry claims.
            _ => block[key_id_at + rng.below(32)] ^= 1 << rng.below(8),
        }
        if seal_key_id {
            let id = key_id(&PublicKey {
                ed25519: &ed,
                mldsa: &ml,
            });
            block[key_id_at..key_id_at + 32].copy_from_slice(&id);
        }

        let Ok(parsed) = SignatureBlock::parse(&block) else {
            reached.hit("parse refused");
            continue;
        };
        let entry = parsed.entries().next().expect("one entry");
        if seal_image {
            digest.copy_from_slice(entry.statement.content_digest);
            profile = entry.statement.profile;
        }
        let key = PublicKey {
            ed25519: &ed,
            mldsa: &ml,
        };
        let result = verify_entry(entry, &key, &digest, profile);

        // The cheap checks, in their documented order, each judged independently.
        let want_early = if entry.key_id != model_key_id(&ed, &ml) {
            Some(VerifyError::KeyId)
        } else if ml.len() != entry.suite.mldsa_public_key_len() {
            Some(VerifyError::KeyLength)
        } else if entry.statement.content_digest != digest {
            Some(VerifyError::Digest)
        } else if entry.statement.profile != profile {
            Some(VerifyError::Profile)
        } else {
            None
        };
        match (want_early, result) {
            (Some(want), got) => assert_eq!(got, Err(want), "case {case}"),
            (None, Err(VerifyError::KeyId | VerifyError::KeyLength)) => {
                panic!("case {case}: a passed cheap check failed: {result:?}")
            }
            (None, Err(VerifyError::Digest | VerifyError::Profile)) => {
                panic!("case {case}: a passed cheap check failed: {result:?}")
            }
            (None, _) => {}
        }
        match result {
            Ok(()) => reached.hit("verified"),
            Err(VerifyError::KeyId) => reached.hit("key id"),
            Err(VerifyError::KeyLength) => reached.hit("key length"),
            Err(VerifyError::Digest) => reached.hit("digest"),
            Err(VerifyError::Profile) => reached.hit("profile"),
            Err(VerifyError::Ed25519(e)) => reached.hit(ed25519_name(e)),
            Err(VerifyError::MlDsa(e)) => reached.hit(mldsa_name(e)),
        }

        // The property that matters: nothing verifies except what was signed.
        if result.is_ok() {
            assert_eq!(
                entry.statement.bytes, reference.statement.bytes,
                "case {case}"
            );
            assert_eq!(entry.sig_ed25519, reference.sig_ed25519, "case {case}");
            assert_eq!(entry.sig_mldsa, reference.sig_mldsa, "case {case}");
            assert_eq!(entry.role, reference.role, "case {case}");
            assert_eq!(entry.suite, reference.suite, "case {case}");
            assert_eq!(ed, f.ed, "case {case}");
            assert_eq!(ml, f.ml, "case {case}");
        }
    }
    reached.require("signature verification soak", &VERDICTS);
    assert!(
        reached.count("verified") >= total as u64 / 32,
        "{}",
        reached.count("verified")
    );
}

/// `key_id` is the S4 hash for every key length, and differs for different keys.
#[test]
fn key_ids_follow_the_canonical_encoding() {
    let mut rng = Rng::new(0x4B45_5949_4453_3031);
    for case in 0..cases(1500) {
        let ed = [rng.byte(); 32];
        let len = match rng.below(4) {
            0 => rng.below(32),
            1 => rng.pick(&[23, 24, 255, 256, 65_535, 65_536]),
            2 => rng.pick(&[1952, 2592]),
            _ => rng.below(70_000),
        };
        // The length is what the encoding depends on; the content only needs to vary.
        let ml = vec![rng.byte(); len];
        let key = PublicKey {
            ed25519: &ed,
            mldsa: &ml,
        };
        assert_eq!(
            key_id(&key),
            model_key_id(&ed, &ml),
            "case {case}: length {len}"
        );
    }
}

/// Mis-sized fields can only come from a hand-built `Entry` (the parser fixes both lengths),
/// but the verifier takes a public struct, so each is refused rather than trusted.
#[test]
fn hand_built_entries_with_wrong_signature_lengths_are_refused() {
    let f = fixture();
    let parsed = SignatureBlock::parse(&f.block).unwrap();
    let entry = *parsed.entries().next().unwrap();
    let key = PublicKey {
        ed25519: &f.ed,
        mldsa: &f.ml,
    };
    assert_eq!(verify_entry(&entry, &key, &f.digest, f.profile), Ok(()));
    let mut short_ed = entry;
    short_ed.sig_ed25519 = &entry.sig_ed25519[..63];
    assert_eq!(
        verify_entry(&short_ed, &key, &f.digest, f.profile),
        Err(VerifyError::Ed25519(Ed25519Error::EquationFailed))
    );
    let mut short_ml = entry;
    short_ml.sig_mldsa = &entry.sig_mldsa[..4626];
    assert_eq!(
        verify_entry(&short_ml, &key, &f.digest, f.profile),
        Err(VerifyError::KeyLength)
    );
}
