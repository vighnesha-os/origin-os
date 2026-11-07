//! Verification of the fixture from `tools/make_fixture.py` (signed by the independent
//! Python references), and every way it must fail.

use super::*;
use onbf_format::signature::SignatureBlock;

struct Fixture {
    digest: [u8; 64],
    profile: u64,
    ed: [u8; 32],
    ml: Vec<u8>,
    block: Vec<u8>,
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn fixture() -> Fixture {
    let text = include_str!("../tests/vectors/kernel-block.txt");
    let field = |name: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("{name} ")))
            .unwrap()
            .to_string()
    };
    Fixture {
        digest: unhex(&field("digest")).try_into().unwrap(),
        profile: field("profile").parse().unwrap(),
        ed: unhex(&field("ed25519_pk")).try_into().unwrap(),
        ml: unhex(&field("mldsa87_pk")),
        block: unhex(&field("block")),
    }
}

fn verify(f: &Fixture, block: &[u8]) -> Result<(), VerifyError> {
    let parsed = SignatureBlock::parse(block).expect("the block parses");
    let entry = parsed.entries().next().unwrap();
    let key = PublicKey {
        ed25519: &f.ed,
        mldsa: &f.ml,
    };
    verify_entry(entry, &key, &f.digest, f.profile)
}

#[test]
fn the_reference_signed_block_verifies() {
    let f = fixture();
    assert_eq!(verify(&f, &f.block), Ok(()));
}

#[test]
fn wrong_image_wrong_key_and_tampering_fail() {
    let f = fixture();
    let parsed = SignatureBlock::parse(&f.block).unwrap();
    let entry = parsed.entries().next().unwrap();
    let key = PublicKey {
        ed25519: &f.ed,
        mldsa: &f.ml,
    };
    let mut digest = f.digest;
    digest[0] ^= 1;
    assert_eq!(
        verify_entry(entry, &key, &digest, f.profile),
        Err(VerifyError::Digest)
    );
    assert_eq!(
        verify_entry(entry, &key, &f.digest, 5),
        Err(VerifyError::Profile)
    );
    let mut other_ed = f.ed;
    other_ed[3] ^= 1;
    let other = PublicKey {
        ed25519: &other_ed,
        mldsa: &f.ml,
    };
    assert_eq!(
        verify_entry(entry, &other, &f.digest, f.profile),
        Err(VerifyError::KeyId)
    );
    // Flip one byte in each signature (offsets from the end: ML-DSA is last).
    let n = f.block.len();
    let mut b = f.block.clone();
    b[n - 100] ^= 1;
    assert!(matches!(verify(&f, &b), Err(VerifyError::MlDsa(_))));
    let ed_at = n - 4627 - 3 - 1 - 10; // inside the Ed25519 signature
    let mut b = f.block.clone();
    b[ed_at] ^= 1;
    assert!(matches!(verify(&f, &b), Err(VerifyError::Ed25519(_))));
}

#[test]
fn any_flip_inside_the_statement_breaks_verification() {
    let f = fixture();
    let parsed = SignatureBlock::parse(&f.block).unwrap();
    let statement = parsed.entries().next().unwrap().statement.bytes;
    let start = f
        .block
        .windows(statement.len())
        .position(|w| w == statement)
        .unwrap();
    for i in start..start + statement.len() {
        let mut b = f.block.clone();
        b[i] ^= 0x01;
        if let Ok(p) = SignatureBlock::parse(&b) {
            let e = p.entries().next().unwrap();
            let key = PublicKey {
                ed25519: &f.ed,
                mldsa: &f.ml,
            };
            assert!(
                verify_entry(e, &key, &f.digest, f.profile).is_err(),
                "flip at {i} still verified"
            );
        }
    }
}
