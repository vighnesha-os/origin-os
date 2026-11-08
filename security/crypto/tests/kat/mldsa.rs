//! NIST ACVP signature-verification vectors for ML-DSA-65 and ML-DSA-87 (external interface,
//! pure: FIPS 204 Algorithm 3), the form Origin OS uses (CLAUDE.md §59).
//!
//! NIST's verdict is the vector's `reason` class: a valid signature, or one of the four
//! modifications. Beyond the verdict, the checks here demand the exact error the crate documents
//! for each modification. The expected errors were confirmed independently of the crate by
//! `tools/mldsa_reference.py`, which also agrees with NIST's verdict on all 30 vectors.

use crate::support::{data_lines, unhex};
use origin_crypto::{MlDsaError, mldsa65_verify, mldsa87_verify};

const FILE: &str = include_str!("../vectors/acvp/mldsa-sigver.txt");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Set {
    MlDsa65,
    MlDsa87,
}

/// How the vector was made, from its ACVP `reason`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Case {
    Valid,
    ModifiedMessage,
    ModifiedCommitment,
    ModifiedZ,
    ModifiedHint,
}

#[derive(Clone, Debug)]
struct Vector {
    tc: u32,
    set: Set,
    case: Case,
    public_key: Vec<u8>,
    message: Vec<u8>,
    context: Vec<u8>,
    signature: Vec<u8>,
}

fn vectors() -> Vec<Vector> {
    data_lines(FILE)
        .map(|line| {
            let cols: Vec<&str> = line.split(' ').collect();
            let ["mldsa", tc, set, case, pk, msg, ctx, sig] = cols.as_slice() else {
                panic!("malformed line: {}", &line[..line.len().min(60)]);
            };
            Vector {
                tc: tc.parse().unwrap(),
                set: match *set {
                    "65" => Set::MlDsa65,
                    "87" => Set::MlDsa87,
                    other => panic!("unknown parameter set {other}"),
                },
                case: match *case {
                    "valid" => Case::Valid,
                    "message" => Case::ModifiedMessage,
                    "commitment" => Case::ModifiedCommitment,
                    "z" => Case::ModifiedZ,
                    "hint" => Case::ModifiedHint,
                    other => panic!("unknown case {other}"),
                },
                public_key: unhex(pk),
                message: unhex(msg),
                context: unhex(ctx),
                signature: unhex(sig),
            }
        })
        .collect()
}

/// `public_key` and `signature` have their set's lengths; the vector file's own checks and the
/// array conversions make sure of it.
fn verify(v: &Vector) -> Result<(), MlDsaError> {
    let pk = v.public_key.as_slice();
    let sig = v.signature.as_slice();
    match v.set {
        Set::MlDsa65 => mldsa65_verify(
            pk.try_into().unwrap(),
            &v.message,
            &v.context,
            sig.try_into().unwrap(),
        ),
        Set::MlDsa87 => mldsa87_verify(
            pk.try_into().unwrap(),
            &v.message,
            &v.context,
            sig.try_into().unwrap(),
        ),
    }
}

/// The result the vector's class calls for. A modified hint is a different malformation in each
/// set of this file: positions that do not increase (ML-DSA-65) and non-zero padding after the
/// last position (ML-DSA-87). A modified z stays within bounds, so it reaches the comparison of
/// c̃ with c̃′.
fn expected(v: &Vector) -> Result<(), MlDsaError> {
    match (v.case, v.set) {
        (Case::Valid, _) => Ok(()),
        (Case::ModifiedMessage | Case::ModifiedCommitment | Case::ModifiedZ, _) => {
            Err(MlDsaError::ChallengeMismatch)
        }
        (Case::ModifiedHint, Set::MlDsa65) => Err(MlDsaError::HintIndexNotIncreasing),
        (Case::ModifiedHint, Set::MlDsa87) => Err(MlDsaError::HintPaddingNonZero),
    }
}

fn check(v: &Vector) -> Result<(), String> {
    let (got, want) = (verify(v), expected(v));
    if got == want {
        Ok(())
    } else {
        Err(format!(
            "tcId {} ({:?}, {:?}): {got:?}, want {want:?}",
            v.tc, v.set, v.case
        ))
    }
}

#[test]
fn every_vector_gets_the_verdict_and_error_of_its_class() {
    let vectors = vectors();
    // 15 per set, three of each class.
    for set in [Set::MlDsa65, Set::MlDsa87] {
        for case in [
            Case::Valid,
            Case::ModifiedMessage,
            Case::ModifiedCommitment,
            Case::ModifiedZ,
            Case::ModifiedHint,
        ] {
            let of_class = vectors.iter().filter(|v| v.set == set && v.case == case);
            assert_eq!(of_class.count(), 3, "{set:?} {case:?}");
        }
    }
    assert_eq!(vectors.len(), 30);
    for v in &vectors {
        check(v).unwrap();
    }
}

/// A valid vector of `set` with a non-empty context, so that every field can be corrupted.
fn valid_vector(vectors: &[Vector], set: Set) -> &Vector {
    vectors
        .iter()
        .find(|v| v.set == set && v.case == Case::Valid && !v.context.is_empty())
        .unwrap()
}

#[test]
fn negative_controls() {
    let vectors = vectors();
    for set in [Set::MlDsa65, Set::MlDsa87] {
        let good = valid_vector(&vectors, set);
        check(good).unwrap();
        // One corrupted bit in the signature, message, context or public key.
        let corruptions: [fn(&mut Vector); 4] = [
            |v| v.signature[0] ^= 0x01,
            |v| v.message[0] ^= 0x01,
            |v| v.context[0] ^= 0x01,
            |v| v.public_key[0] ^= 0x01,
        ];
        for corrupt in corruptions {
            let mut bad = good.clone();
            corrupt(&mut bad);
            assert!(verify(&bad).is_err(), "corrupted vector accepted: {set:?}");
            assert!(check(&bad).is_err(), "the check missed it: {set:?}");
        }

        // A rejected vector claimed as valid, and a valid one claimed as rejected: the check
        // follows the verdict the vector states, not whatever the verifier returns.
        let rejected = vectors
            .iter()
            .find(|v| v.set == set && v.case == Case::ModifiedHint)
            .unwrap();
        let claimed = Vector {
            case: Case::Valid,
            ..rejected.clone()
        };
        assert!(
            check(&claimed).is_err(),
            "a rejection passed as valid: {set:?}"
        );
        let claimed = Vector {
            case: Case::ModifiedMessage,
            ..good.clone()
        };
        assert!(check(&claimed).is_err(), "a pass claimed rejected: {set:?}");
    }
}
