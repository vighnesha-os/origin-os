//! The known-answer self-test: its constants are NIST's, and a wrong one fails it.

use crate::kat::VECTORS;
use crate::selftest::{Step, Vector, all_pass, passes};

const NIST: &str = include_str!("../../../crypto/tests/vectors/acvp/hmac-drbg-sha2-512.txt");

fn unhex(text: &str) -> Vec<u8> {
    if text == "-" {
        return Vec::new();
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
        .collect()
}

/// The lines of NIST's vector `tc` in the committed file: its own line and its steps.
fn nist(tc: u32) -> Vec<Vec<String>> {
    let mut lines = NIST
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.split(' ').map(str::to_string).collect::<Vec<_>>())
        .skip_while(|cols| !(cols[0] == "drbg" && cols[1] == tc.to_string()));
    let mut found = vec![lines.next().unwrap_or_else(|| panic!("no vector {tc}"))];
    found.extend(lines.take_while(|cols| cols[0] == "in"));
    found
}

#[test]
fn the_self_test_passes() {
    assert!(crate::self_test());
}

#[test]
fn the_constants_are_nists_vectors() {
    assert_eq!(VECTORS.len(), 2);
    let kinds: Vec<&str> = VECTORS
        .iter()
        .map(|vector| match vector.steps {
            [Step::Fresh { .. }, Step::Fresh { .. }] => "pr",
            [
                Step::Reseed { .. },
                Step::Generate { .. },
                Step::Generate { .. },
            ] => "reseed",
            _ => "another shape",
        })
        .collect();
    assert_eq!(
        kinds,
        ["pr", "reseed"],
        "one vector of each of NIST's two procedures"
    );
    for vector in &VECTORS {
        let lines = nist(vector.tc);
        let head = &lines[0];
        assert_eq!(
            head[2],
            kinds[VECTORS.iter().position(|v| v.tc == vector.tc).unwrap()]
        );
        assert_eq!(vector.entropy, unhex(&head[3]), "tcId {}", vector.tc);
        assert_eq!(vector.nonce, unhex(&head[4]));
        assert_eq!(vector.personalization, unhex(&head[5]));
        assert_eq!(vector.answer, unhex(&head[6]));
        assert_eq!(vector.answer.len(), 512);
        assert_eq!(vector.steps.len(), lines.len() - 1);
        for (step, line) in vector.steps.iter().zip(&lines[1..]) {
            let (entropy, additional) = (unhex(&line[2]), unhex(&line[3]));
            match step {
                Step::Reseed {
                    entropy: e,
                    additional: a,
                } => {
                    assert_eq!(line[1], "reseed");
                    assert_eq!((*e, *a), (&entropy[..], &additional[..]));
                }
                Step::Generate { additional: a } => {
                    assert_eq!(line[1], "generate");
                    assert!(entropy.is_empty());
                    assert_eq!(*a, &additional[..]);
                }
                Step::Fresh {
                    entropy: e,
                    additional: a,
                } => {
                    assert_eq!(line[1], "generate");
                    assert!(!entropy.is_empty());
                    assert_eq!((*e, *a), (&entropy[..], &additional[..]));
                }
            }
        }
    }
}

/// `vector` with one bit of one of its parts changed.
fn corrupted(vector: &Vector, part: usize) -> Vector {
    let flip = |bytes: &'static [u8]| -> &'static [u8] {
        let mut copy = bytes.to_vec();
        copy[0] ^= 1;
        Box::leak(copy.into_boxed_slice())
    };
    let mut bad = Vector {
        tc: vector.tc,
        entropy: vector.entropy,
        nonce: vector.nonce,
        personalization: vector.personalization,
        steps: vector.steps,
        answer: vector.answer,
    };
    match part {
        0 => bad.entropy = flip(vector.entropy),
        1 => bad.nonce = flip(vector.nonce),
        2 => bad.personalization = flip(vector.personalization),
        3 => bad.answer = flip(vector.answer),
        // A step left out.
        _ => bad.steps = &vector.steps[1..],
    }
    bad
}

#[test]
fn a_wrong_constant_fails_the_self_test() {
    for vector in &VECTORS {
        assert!(passes(vector), "tcId {}", vector.tc);
        for part in 0..5 {
            assert!(
                !passes(&corrupted(vector, part)),
                "tcId {}: corruption {part} passed",
                vector.tc
            );
        }
    }
    // An answer of another length is not compared at all.
    let short = Vector {
        answer: &VECTORS[0].answer[..511],
        tc: VECTORS[0].tc,
        entropy: VECTORS[0].entropy,
        nonce: VECTORS[0].nonce,
        personalization: VECTORS[0].personalization,
        steps: VECTORS[0].steps,
    };
    assert!(!passes(&short));
}

#[test]
fn every_vector_is_run() {
    let whole = |vector: &Vector| Vector {
        tc: vector.tc,
        entropy: vector.entropy,
        nonce: vector.nonce,
        personalization: vector.personalization,
        steps: vector.steps,
        answer: vector.answer,
    };
    assert!(all_pass(&[whole(&VECTORS[0]), whole(&VECTORS[1])]));
    // A wrong answer in either place fails the whole test.
    assert!(!all_pass(&[corrupted(&VECTORS[0], 3), whole(&VECTORS[1])]));
    assert!(!all_pass(&[whole(&VECTORS[0]), corrupted(&VECTORS[1], 3)]));
    assert!(all_pass(&[]));
}
