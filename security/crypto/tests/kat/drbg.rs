//! NIST ACVP vectors for HMAC_DRBG over SHA-512 (SP 800-90A Rev. 1 §10.1.2), the mechanism of
//! the kernel's random-number generator (CLAUDE.md §59; ADR-0016).
//!
//! NIST's test instantiates a generator, reseeds it where the group has no prediction resistance,
//! asks it for output twice, and gives the second output as the answer. With prediction
//! resistance each request brings its own entropy input, and the generator reseeds from it before
//! it generates. Every vector here has a personalization string and additional input, so these
//! are NIST's answers for the paths RFC 6979's published values do not reach: reseeding,
//! additional input and prediction resistance.
//!
//! The answers were reproduced by `tools/hmac_drbg_reference.py`'s generator before the crate was
//! run against them.

use crate::support::{data_lines, same, unhex};
use origin_crypto::{DrbgError, HmacDrbgSha512};

const FILE: &str = include_str!("../vectors/acvp/hmac-drbg-sha2-512.txt");

/// Which of NIST's two procedures a vector follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// Prediction resistance: two requests, each with its own entropy input.
    PredictionResistance,
    /// A reseed, then two requests.
    Reseed,
}

/// One thing asked of the generator after it is instantiated.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Step {
    Reseed {
        entropy: Vec<u8>,
        additional: Vec<u8>,
    },
    Generate {
        additional: Vec<u8>,
    },
    /// A request with prediction resistance.
    GenerateFresh {
        entropy: Vec<u8>,
        additional: Vec<u8>,
    },
}

#[derive(Clone, Debug)]
struct Vector {
    tc: u32,
    kind: Kind,
    entropy: Vec<u8>,
    nonce: Vec<u8>,
    personalization: Vec<u8>,
    steps: Vec<Step>,
    answer: Vec<u8>,
}

fn vectors() -> Vec<Vector> {
    let mut vectors: Vec<Vector> = Vec::new();
    for line in data_lines(FILE) {
        let cols: Vec<&str> = line.split(' ').collect();
        match cols.as_slice() {
            ["drbg", tc, kind, entropy, nonce, personalization, answer] => vectors.push(Vector {
                tc: tc.parse().unwrap(),
                kind: match *kind {
                    "pr" => Kind::PredictionResistance,
                    "reseed" => Kind::Reseed,
                    other => panic!("unknown kind {other}"),
                },
                entropy: unhex(entropy),
                nonce: unhex(nonce),
                personalization: unhex(personalization),
                steps: Vec::new(),
                answer: unhex(answer),
            }),
            ["in", step, entropy, additional] => {
                let (entropy, additional) = (unhex(entropy), unhex(additional));
                let step = match (*step, entropy.is_empty()) {
                    ("reseed", false) => Step::Reseed {
                        entropy,
                        additional,
                    },
                    ("generate", true) => Step::Generate { additional },
                    ("generate", false) => Step::GenerateFresh {
                        entropy,
                        additional,
                    },
                    other => panic!("unknown step {other:?}"),
                };
                vectors
                    .last_mut()
                    .expect("a step before any vector")
                    .steps
                    .push(step);
            }
            _ => panic!("malformed line: {}", &line[..line.len().min(60)]),
        }
    }
    vectors
}

/// Runs the vector's procedure and returns the last request's output.
fn run(v: &Vector) -> Result<Vec<u8>, DrbgError> {
    let mut drbg = HmacDrbgSha512::new();
    drbg.instantiate(&v.entropy, &v.nonce, &v.personalization)?;
    let mut out = vec![0u8; v.answer.len()];
    for step in &v.steps {
        match step {
            Step::Reseed {
                entropy,
                additional,
            } => drbg.reseed(entropy, additional)?,
            Step::Generate { additional } => drbg.generate(&mut out, additional)?,
            Step::GenerateFresh {
                entropy,
                additional,
            } => drbg.reseed_and_generate(&mut out, entropy, additional)?,
        }
    }
    Ok(out)
}

fn check(v: &Vector) -> Result<(), String> {
    let got = run(v).map_err(|error| format!("tcId {}: {error:?}", v.tc))?;
    same(&got, &v.answer).map_err(|difference| format!("tcId {}: {difference}", v.tc))
}

#[test]
fn every_vector_gives_nists_answer() {
    let vectors = vectors();
    // 15 of each procedure, each with the steps NIST's specification gives it.
    for (kind, steps) in [(Kind::PredictionResistance, 2), (Kind::Reseed, 3)] {
        let of_kind: Vec<&Vector> = vectors.iter().filter(|v| v.kind == kind).collect();
        assert_eq!(of_kind.len(), 15, "{kind:?}");
        for v in of_kind {
            assert_eq!(v.steps.len(), steps, "tcId {}", v.tc);
            let fresh = v
                .steps
                .iter()
                .filter(|step| matches!(step, Step::GenerateFresh { .. }))
                .count();
            let reseeds = v
                .steps
                .iter()
                .filter(|step| matches!(step, Step::Reseed { .. }))
                .count();
            match kind {
                Kind::PredictionResistance => assert_eq!((fresh, reseeds), (2, 0)),
                Kind::Reseed => {
                    assert_eq!((fresh, reseeds), (0, 1));
                    assert!(matches!(v.steps[0], Step::Reseed { .. }), "tcId {}", v.tc);
                }
            }
            // What these groups exercise: a personalization string, additional input in every
            // step, and 512 bytes of output, which is eight output blocks.
            assert!(!v.personalization.is_empty(), "tcId {}", v.tc);
            assert_eq!(v.answer.len(), 512, "tcId {}", v.tc);
            assert!(v.steps.iter().all(|step| match step {
                Step::Reseed { additional, .. }
                | Step::Generate { additional }
                | Step::GenerateFresh { additional, .. } => !additional.is_empty(),
            }));
        }
    }
    assert_eq!(vectors.len(), 30);
    for v in &vectors {
        check(v).unwrap();
    }
}

#[test]
fn negative_controls() {
    let vectors = vectors();
    for kind in [Kind::PredictionResistance, Kind::Reseed] {
        let good = vectors.iter().find(|v| v.kind == kind).unwrap();
        check(good).unwrap();

        // One corrupted bit in the answer, or in anything the generator is given.
        let corruptions: [fn(&mut Vector); 7] = [
            |v| v.answer[0] ^= 0x01,
            |v| *v.answer.last_mut().unwrap() ^= 0x80,
            |v| v.entropy[0] ^= 0x01,
            |v| v.nonce[0] ^= 0x01,
            |v| v.personalization[0] ^= 0x01,
            |v| match &mut v.steps[0] {
                Step::Reseed { entropy, .. } | Step::GenerateFresh { entropy, .. } => {
                    entropy[0] ^= 0x01
                }
                Step::Generate { .. } => panic!("the first step brings entropy"),
            },
            |v| match v.steps.last_mut().unwrap() {
                Step::Reseed { additional, .. }
                | Step::Generate { additional }
                | Step::GenerateFresh { additional, .. } => additional[0] ^= 0x01,
            },
        ];
        for (number, corrupt) in corruptions.into_iter().enumerate() {
            let mut bad = good.clone();
            corrupt(&mut bad);
            assert!(check(&bad).is_err(), "{kind:?}: corruption {number} passed");
        }

        // The procedure itself: a step left out, the two requests exchanged, and a request with
        // prediction resistance made as a reseed followed by a request that still carries the
        // additional input, which is what SP 800-90A §9.3.1 step 7.4 rules out.
        let mut bad = good.clone();
        bad.steps.remove(0);
        assert!(check(&bad).is_err(), "{kind:?}: a missing step passed");
        let mut bad = good.clone();
        let last = bad.steps.len() - 1;
        bad.steps.swap(last - 1, last);
        assert!(check(&bad).is_err(), "{kind:?}: exchanged requests passed");
        if kind == Kind::PredictionResistance {
            let mut bad = good.clone();
            bad.steps = bad
                .steps
                .into_iter()
                .flat_map(|step| match step {
                    Step::GenerateFresh {
                        entropy,
                        additional,
                    } => vec![
                        Step::Reseed {
                            entropy,
                            additional: additional.clone(),
                        },
                        Step::Generate { additional },
                    ],
                    other => vec![other],
                })
                .collect();
            assert!(
                check(&bad).is_err(),
                "additional input used twice in a request with prediction resistance passed"
            );
        }
    }

    // A vector the generator must refuse is reported as a refusal, not as a wrong answer: with
    // 31 bytes of entropy input there is no instantiation.
    let mut bad = vectors[0].clone();
    bad.entropy.truncate(31);
    assert_eq!(run(&bad), Err(DrbgError::EntropyTooShort));
    assert!(check(&bad).is_err());
}
