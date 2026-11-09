//! Wycheproof's Ed25519 signature-verification vectors, under the strict policy of errata E1
//! (`docs/spec/ONBF-errata.md`).
//!
//! Wycheproof marks each vector valid, invalid or acceptable. Valid and invalid must hold as
//! written. Acceptable leaves the choice to the implementation, and E1 makes it: a small-order
//! public key is refused, and a mixed-order key is verified by the cofactorless equation like
//! any other.
//!
//! A signature that is not 64 bytes cannot reach `ed25519_verify`, which takes `&[u8; 64]`: the
//! caller has to refuse it (as `onbf-format` does). Those vectors are all invalid, and are
//! counted here as refused at that boundary.
//!
//! The outcomes each flag allows, and the tally of exact errors, were confirmed independently of
//! the crate by `tools/ed25519_reference.py`.

use crate::support::{data_lines, unhex};
use origin_crypto::ed25519_verify;
use std::collections::BTreeMap;

const FILE: &str = include_str!("../vectors/wycheproof/ed25519.txt");

const VERIFIED: &str = "Verified";
const WRONG_LENGTH: &str = "WrongLength";

#[derive(Clone, Debug)]
struct Vector {
    tc: u32,
    result: &'static str,
    flag: &'static str,
    public_key: Vec<u8>,
    message: Vec<u8>,
    signature: Vec<u8>,
}

fn vectors() -> Vec<Vector> {
    data_lines(FILE)
        .map(|line| {
            let cols: Vec<&'static str> = line.split(' ').collect();
            let ["ed", tc, result, flag, pk, msg, sig] = cols.as_slice() else {
                panic!("malformed line: {}", &line[..line.len().min(60)]);
            };
            Vector {
                tc: tc.parse().unwrap(),
                result,
                flag,
                public_key: unhex(pk),
                message: unhex(msg),
                signature: unhex(sig),
            }
        })
        .collect()
}

/// What the crate did with this vector.
fn outcome(v: &Vector) -> String {
    let Ok(signature) = <&[u8; 64]>::try_from(v.signature.as_slice()) else {
        return WRONG_LENGTH.to_string();
    };
    let public_key = <&[u8; 32]>::try_from(v.public_key.as_slice()).unwrap();
    match ed25519_verify(public_key, &v.message, signature) {
        Ok(()) => VERIFIED.to_string(),
        Err(error) => format!("{error:?}"),
    }
}

/// The outcomes this vector's verdict and flag allow.
fn allowed(v: &Vector) -> &'static [&'static str] {
    match (v.result, v.flag) {
        ("valid", "Valid" | "Ktv" | "TinkOverflow") => &[VERIFIED],
        ("acceptable", "SmallOrderPublicKey") => &["PublicKeySmallOrder"],
        ("acceptable", "MixedOrderPublicKey") => &[VERIFIED],
        ("invalid", "CompressedSignature" | "SignatureWithGarbage" | "TruncatedSignature") => {
            &[WRONG_LENGTH]
        }
        ("invalid", "SignatureMalleability" | "InvalidKtv") => &["SNonCanonical"],
        ("invalid", "MixedOrderPublicKey") => &["EquationFailed"],
        ("invalid", "InvalidSignature") => &["RNonCanonical", "RNotOnCurve", "RSmallOrder"],
        ("invalid", "InvalidEncoding") => &[
            "RNonCanonical",
            "RNotOnCurve",
            "RSmallOrder",
            "EquationFailed",
        ],
        (result, flag) => panic!("tcId {}: nothing expected for {result} {flag}", v.tc),
    }
}

fn check(v: &Vector) -> Result<String, String> {
    let got = outcome(v);
    if allowed(v).contains(&got.as_str()) {
        Ok(got)
    } else {
        Err(format!(
            "tcId {} ({} {}): {got}, allowed {:?}",
            v.tc,
            v.result,
            v.flag,
            allowed(v)
        ))
    }
}

#[test]
fn every_vector_has_an_allowed_outcome() {
    let vectors = vectors();
    assert_eq!(vectors.len(), 210);
    let mut tally: BTreeMap<String, usize> = BTreeMap::new();
    for v in &vectors {
        *tally.entry(check(v).unwrap()).or_default() += 1;
    }
    let want: BTreeMap<String, usize> = [
        (VERIFIED, 138),
        (WRONG_LENGTH, 12),
        ("EquationFailed", 13),
        ("PublicKeySmallOrder", 8),
        ("RNonCanonical", 7),
        ("RNotOnCurve", 12),
        ("RSmallOrder", 11),
        ("SNonCanonical", 9),
    ]
    .into_iter()
    .map(|(name, count)| (name.to_string(), count))
    .collect();
    assert_eq!(tally, want);
}

#[test]
fn negative_controls() {
    let vectors = vectors();
    let good = vectors
        .iter()
        .find(|v| v.result == "valid" && !v.message.is_empty())
        .unwrap();
    check(good).unwrap();

    // One corrupted bit in the signature, message or public key.
    let corruptions: [fn(&mut Vector); 3] = [
        |v| v.signature[0] ^= 0x01,
        |v| v.message[0] ^= 0x01,
        |v| v.public_key[0] ^= 0x01,
    ];
    for corrupt in corruptions {
        let mut bad = good.clone();
        corrupt(&mut bad);
        assert_ne!(outcome(&bad), VERIFIED, "corrupted vector accepted");
        assert!(check(&bad).is_err(), "the check missed it");
    }

    // A signature one byte long, which the type boundary refuses.
    let mut bad = good.clone();
    bad.signature.push(0);
    assert_eq!(outcome(&bad), WRONG_LENGTH);
    assert!(check(&bad).is_err(), "a wrong length passed as valid");

    // An invalid vector claimed as valid, and a valid one claimed as a malleated signature.
    let rejected = vectors
        .iter()
        .find(|v| v.result == "invalid" && v.flag == "SignatureMalleability")
        .unwrap();
    let claimed = Vector {
        result: "valid",
        flag: "Valid",
        ..rejected.clone()
    };
    assert!(check(&claimed).is_err(), "a rejection passed as valid");
    let claimed = Vector {
        result: "invalid",
        flag: "SignatureMalleability",
        ..good.clone()
    };
    assert!(check(&claimed).is_err(), "a pass claimed rejected");

    // E1's policy for an acceptable vector: both ways round, the check holds the crate to it.
    let small = vectors
        .iter()
        .find(|v| v.flag == "SmallOrderPublicKey")
        .unwrap();
    check(small).unwrap();
    let claimed = Vector {
        flag: "MixedOrderPublicKey",
        ..small.clone()
    };
    assert!(
        check(&claimed).is_err(),
        "E1's small-order rule is enforced"
    );
    let mixed = vectors
        .iter()
        .find(|v| v.result == "acceptable" && v.flag == "MixedOrderPublicKey")
        .unwrap();
    check(mixed).unwrap();
    let claimed = Vector {
        flag: "SmallOrderPublicKey",
        ..mixed.clone()
    };
    assert!(
        check(&claimed).is_err(),
        "E1's mixed-order rule is enforced"
    );
}
