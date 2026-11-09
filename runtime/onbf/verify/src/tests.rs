//! Verification of the fixture from `tools/make_fixture.py` (signed by the independent
//! Python references), and every way it must fail.

use super::*;
use onbf_format::signature::{Role, SignatureBlock, SignatureError};

pub(crate) struct Fixture {
    pub(crate) digest: [u8; 64],
    pub(crate) profile: u64,
    pub(crate) ed: [u8; 32],
    pub(crate) ml: Vec<u8>,
    pub(crate) block: Vec<u8>,
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

pub(crate) fn fixture() -> Fixture {
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

// ---------------------------------------------------------------- the KRN trust-store policy

/// A store that trusts the fixture's key for `role`.
pub(crate) fn fixture_store(f: &Fixture, role: Role) -> [TrustedKey<'_>; 1] {
    [TrustedKey {
        role,
        key: PublicKey {
            ed25519: &f.ed,
            mldsa: &f.ml,
        },
    }]
}

fn head(major: u8, arg: usize) -> Vec<u8> {
    let m = major << 5;
    match arg {
        0..=23 => vec![m | arg as u8],
        24..=0xFF => vec![m | 24, arg as u8],
        _ => [&[m | 25][..], &(arg as u16).to_be_bytes()].concat(),
    }
}
fn uint(v: usize) -> Vec<u8> {
    head(0, v)
}
fn cbor_bytes(b: &[u8]) -> Vec<u8> {
    [head(2, b.len()), b.to_vec()].concat()
}
fn text(t: &str) -> Vec<u8> {
    [head(3, t.len()), t.as_bytes().to_vec()].concat()
}
fn map(entries: &[(usize, Vec<u8>)]) -> Vec<u8> {
    let mut out = head(5, entries.len());
    for (k, v) in entries {
        out.extend(uint(*k));
        out.extend_from_slice(v);
    }
    out
}

/// A well-formed entry with garbage signatures: `certs` certificates in its chain, and the
/// suite its role requires.
pub(crate) fn hand_entry(role: usize, key_id: &[u8; 32], certs: usize, digest: &[u8]) -> Vec<u8> {
    let (suite, sig_len) = if matches!(role, 4 | 5) {
        (2, 4627)
    } else {
        (1, 3309)
    };
    let statement = map(&[
        (0, uint(1)),
        (1, uint(6)),
        (2, cbor_bytes(digest)),
        (3, text("origin.framekernel")),
        (4, text("0.1.0-dev")),
        (5, uint(role)),
        (6, uint(1_794_000_000)),
    ]);
    let mut chain = head(4, certs);
    for _ in 0..certs {
        chain.extend(map(&[(0, uint(1))]));
    }
    map(&[
        (1, uint(role)),
        (2, uint(suite)),
        (3, cbor_bytes(key_id)),
        (4, chain),
        (5, statement),
        (6, cbor_bytes(&[0x22; 64])),
        (7, cbor_bytes(&vec![0x33; sig_len])),
    ])
}

pub(crate) fn hand_block(entries: &[&[u8]]) -> Vec<u8> {
    let mut out = b"ONBFSIG1".to_vec();
    out.extend(1u32.to_le_bytes());
    out.extend((entries.len() as u32).to_le_bytes());
    for e in entries {
        out.extend((e.len() as u32).to_le_bytes());
        out.extend_from_slice(e);
    }
    out
}

#[test]
fn the_reference_block_passes_the_krn_policy() {
    let f = fixture();
    let store = fixture_store(&f, Role::Kernel);
    let key = key_id(&store[0].key);
    assert_eq!(verify_krn_block(&f.block, &store, &f.digest), Ok(key));
}

#[test]
fn the_trust_store_decides_who_signs() {
    let f = fixture();
    let refused = Err(KrnBlockError::UntrustedKey);
    // Nothing is trusted: the build of a loader with no keys compiled in.
    assert_eq!(verify_krn_block(&f.block, &[], &f.digest), refused);
    // The right key, trusted only for another role.
    let store = fixture_store(&f, Role::Release);
    assert_eq!(verify_krn_block(&f.block, &store, &f.digest), refused);
    // Another key, trusted for the right role: its ID is not the entry's.
    let mut other_ed = f.ed;
    other_ed[3] ^= 1;
    let store = [TrustedKey {
        role: Role::Kernel,
        key: PublicKey {
            ed25519: &other_ed,
            mldsa: &f.ml,
        },
    }];
    assert_eq!(verify_krn_block(&f.block, &store, &f.digest), refused);
    // An entry naming a key the store does not hold: refused before any signature is read.
    let id = [0x5A; 32];
    let entry = hand_entry(4, &id, 0, &f.digest);
    let store = fixture_store(&f, Role::Kernel);
    assert_eq!(
        verify_krn_block(&hand_block(&[&entry]), &store, &f.digest),
        refused
    );
}

#[test]
fn a_krn_block_is_one_entry_in_the_kernel_role_with_no_chain() {
    let f = fixture();
    let store = fixture_store(&f, Role::Kernel);
    let trusted_id = key_id(&store[0].key);
    let first = &f.block[20..];
    let other = hand_entry(4, &[0x5A; 32], 0, &f.digest);
    // Two entries, the first one valid: still refused (errata V2).
    assert_eq!(
        verify_krn_block(&hand_block(&[first, &other]), &store, &f.digest),
        Err(KrnBlockError::EntryCount(2))
    );
    // A valid-looking entry in another role, by a key the store holds (role, then key).
    let publisher = hand_entry(1, &trusted_id, 0, &f.digest);
    assert_eq!(
        verify_krn_block(&hand_block(&[&publisher]), &store, &f.digest),
        Err(KrnBlockError::Role(Role::Publisher))
    );
    // A certificate chain cannot be honoured yet, so the entry is refused (errata V1).
    let chained = hand_entry(4, &trusted_id, 1, &f.digest);
    assert_eq!(
        verify_krn_block(&hand_block(&[&chained]), &store, &f.digest),
        Err(KrnBlockError::ChainNotSupported)
    );
    // A block that does not parse.
    let mut bad = f.block.clone();
    bad[0] ^= 1;
    assert_eq!(
        verify_krn_block(&bad, &store, &f.digest),
        Err(KrnBlockError::Malformed(SignatureError::Magic))
    );
}

#[test]
fn a_tampered_statement_or_signature_is_refused_for_what_it_is() {
    let f = fixture();
    let store = fixture_store(&f, Role::Kernel);
    let parsed = SignatureBlock::parse(&f.block).unwrap();
    let entry = parsed.entries().next().unwrap();
    let at = |part: &[u8]| part.as_ptr() as usize - f.block.as_ptr() as usize;
    let flip = |offset: usize| {
        let mut b = f.block.clone();
        b[offset] ^= 1;
        verify_krn_block(&b, &store, &f.digest)
    };
    // The statement's digest, then its profile: the statement names another image.
    assert_eq!(
        flip(at(entry.statement.content_digest)),
        Err(KrnBlockError::Entry(VerifyError::Digest))
    );
    assert_eq!(
        flip(at(entry.statement.bytes) + 4),
        Err(KrnBlockError::Entry(VerifyError::Profile))
    );
    // Each signature on its own; the other one is untouched, so each is really checked.
    assert!(matches!(
        flip(at(entry.sig_ed25519)),
        Err(KrnBlockError::Entry(VerifyError::Ed25519(_)))
    ));
    assert!(matches!(
        flip(at(entry.sig_mldsa) + 100),
        Err(KrnBlockError::Entry(VerifyError::MlDsa(_)))
    ));
    // A statement field nothing compares (the version): the Ed25519 signature no longer
    // covers these bytes. `is_semver` keeps a flip in the last digit valid.
    let version = entry.statement.version;
    assert!(matches!(
        flip(at(version.as_bytes()) + version.len() - 5),
        Err(KrnBlockError::Entry(VerifyError::Ed25519(_)))
    ));
}

/// An image that isn't this entry's: a wrong digest is refused, and the digest check
/// runs before any signature is checked (a bad signature here would otherwise report as
/// `Ed25519`).
#[test]
fn another_image_digest_is_refused_before_the_signatures() {
    let f = fixture();
    let store = fixture_store(&f, Role::Kernel);
    let mut digest = f.digest;
    digest[10] ^= 0x80;
    assert_eq!(
        verify_krn_block(&f.block, &store, &digest),
        Err(KrnBlockError::Entry(VerifyError::Digest))
    );
    // The same with a bad signature left in the entry.
    let mut b = f.block.clone();
    b[f.block.len() - 1] ^= 1;
    assert_eq!(
        verify_krn_block(&b, &store, &digest),
        Err(KrnBlockError::Entry(VerifyError::Digest))
    );
}
