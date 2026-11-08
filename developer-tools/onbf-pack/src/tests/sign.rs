//! Signing: the encoder against the independent Python one, the block's placement, the
//! self-check, and (when an OpenSSL with ML-DSA is installed) real signatures end to end.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use onbf_format::signature::{Role, SignatureBlock};
use onbf_format::{Image, ParseOptions};
use onbf_verify::{KrnBlockError, PublicKey, TrustedKey, VerifyError, key_id, verify_krn_block};
use origin_crypto::Sha512;

use super::elf_writer::TestElf;
use super::options;
use crate::krn;
use crate::sign::testing::{attach_block, block_bytes, statement_bytes, temp_dir_mode};
use crate::sign::{
    KeyPaths, OpensslSigner, PublicKeys, SelfCheckError, SignError, SignOptions, Signatures,
    Signer, generate_keys, sign_krn,
};

const OPTIONS: SignOptions = SignOptions {
    claimed_signing_time: 1_794_000_000,
};

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

/// The block `runtime/onbf/verify/tools/make_fixture.py` built with its own CBOR encoder.
fn reference_block() -> Vec<u8> {
    let text = include_str!("../../../../runtime/onbf/verify/tests/vectors/kernel-block.txt");
    unhex(text.lines().find_map(|l| l.strip_prefix("block ")).unwrap())
}

fn unsigned_image() -> Vec<u8> {
    krn::pack(&TestElf::kernel().bytes(), &options())
        .expect("the ELF converts")
        .bytes
}

/// A signer that returns fixed bytes.
struct Stub {
    keys: PublicKeys,
    signatures: Signatures,
}

impl Stub {
    fn new(ed25519: usize, mldsa87: usize) -> Self {
        Self {
            keys: PublicKeys {
                ed25519: [9; 32],
                mldsa87: vec![7; 2592],
            },
            signatures: Signatures {
                ed25519: vec![0x22; ed25519],
                mldsa87: vec![0x33; mldsa87],
            },
        }
    }
}

impl Signer for Stub {
    fn public_keys(&self) -> &PublicKeys {
        &self.keys
    }
    fn sign(&self, _statement: &[u8], _context: &[u8]) -> Result<Signatures, SignError> {
        Ok(self.signatures.clone())
    }
}

/// The statement and block encoders produce, byte for byte, what the independent Python
/// encoder wrote for the same fields.
#[test]
fn the_encoders_match_the_reference_encoder() {
    let reference = reference_block();
    let parsed = SignatureBlock::parse(&reference).unwrap();
    let entry = parsed.entries().next().unwrap();
    let digest: [u8; 64] = entry.statement.content_digest.try_into().unwrap();
    let time = entry.statement.claimed_signing_time;
    assert_eq!(
        statement_bytes(&digest, entry.statement.id, entry.statement.version, time),
        entry.statement.bytes,
    );
    let id: [u8; 32] = entry.key_id.try_into().unwrap();
    let signatures = Signatures {
        ed25519: entry.sig_ed25519.to_vec(),
        mldsa87: entry.sig_mldsa.to_vec(),
    };
    assert_eq!(
        block_bytes(
            &id,
            &digest,
            entry.statement.id,
            entry.statement.version,
            time,
            &signatures
        ),
        reference
    );
}

/// The block lands at its canonical offset (errata C3), the padding is zero, the header says
/// where it is, and the content digest is untouched: a loader outside developer mode parses
/// the result and finds the block.
#[test]
fn attaching_a_block_gives_a_canonical_signed_image() {
    let image = unsigned_image();
    let block = reference_block();
    let signed = attach_block(&image, &block);
    let parsed = Image::parse(&signed, ParseOptions::default()).expect("a signed image parses");
    assert_eq!(parsed.signature_block(), Some(block.as_slice()));
    assert_eq!(signed[48..112], image[48..112]);
    let offset = u64::from_le_bytes(signed[32..40].try_into().unwrap());
    assert_eq!(
        offset,
        u64::try_from(image.len().next_multiple_of(8)).unwrap()
    );
    assert!(signed[image.len()..offset as usize].iter().all(|&b| b == 0));
    assert_eq!(
        u64::from_le_bytes(signed[40..48].try_into().unwrap()),
        block.len() as u64
    );
    parsed
        .verify_content_digest(|parts: &[&[u8]; 3]| {
            let mut h = Sha512::new();
            for p in parts {
                h.update(p);
            }
            h.finalize()
        })
        .expect("the header fields it wrote are outside the digest");
}

/// The statement OpenSSL signs lives in a directory no other user can write to.
#[test]
fn the_signing_directory_is_private() {
    assert_eq!(temp_dir_mode(), 0o700);
}

/// A signer whose output the loader would refuse: nothing is returned.
#[test]
fn the_self_check_refuses_a_signature_that_does_not_verify() {
    let result = sign_krn(&unsigned_image(), &Stub::new(64, 4627), &OPTIONS);
    match result {
        Err(SignError::SelfCheck(SelfCheckError::Block(KrnBlockError::Entry(
            VerifyError::Ed25519(_),
        )))) => {}
        other => panic!("{other:?}"),
    }
}

#[test]
fn signatures_of_the_wrong_length_are_refused() {
    for (ed, ml, algorithm) in [(63, 4627, "Ed25519"), (64, 4626, "ML-DSA-87")] {
        let result = sign_krn(&unsigned_image(), &Stub::new(ed, ml), &OPTIONS);
        match result {
            Err(SignError::SignatureLength { algorithm: a, .. }) if a == algorithm => {}
            other => panic!("{algorithm}: {other:?}"),
        }
    }
}

#[test]
fn only_a_valid_unsigned_krn_image_can_be_signed() {
    let image = unsigned_image();
    let signed = attach_block(&image, &reference_block());
    assert_eq!(
        sign_krn(&signed, &Stub::new(64, 4627), &OPTIONS),
        Err(SignError::AlreadySigned)
    );
    assert!(matches!(
        sign_krn(b"not an image", &Stub::new(64, 4627), &OPTIONS),
        Err(SignError::Input(_))
    ));
    let mut tampered = image;
    tampered[60] ^= 1;
    assert!(matches!(
        sign_krn(&tampered, &Stub::new(64, 4627), &OPTIONS),
        Err(SignError::Input(_))
    ));
}

// ---------------------------------------------------------------- with a real OpenSSL

/// An OpenSSL with ML-DSA (3.5 or later), if one is installed. The rest of these tests run
/// without one; this is a live check of the path the boot test depends on.
fn openssl() -> Option<PathBuf> {
    let path = PathBuf::from("openssl");
    let out = Command::new(&path).arg("version").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut words = text.split_whitespace();
    let (name, version) = (words.next()?, words.next()?);
    let mut numbers = version.split('.');
    let major: u32 = numbers.next()?.parse().ok()?;
    let minor: u32 = numbers.next()?.parse().ok()?;
    if name == "OpenSSL" && (major, minor) >= (3, 5) {
        Some(path)
    } else {
        eprintln!("skipped: `openssl` is {text}: ML-DSA needs OpenSSL 3.5 or later");
        None
    }
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("onbf-pack-sign-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn store(keys: &PublicKeys) -> [TrustedKey<'_>; 1] {
    [TrustedKey {
        role: Role::Kernel,
        key: PublicKey {
            ed25519: &keys.ed25519,
            mldsa: &keys.mldsa87,
        },
    }]
}

fn generate(
    openssl: &Path,
    dir: &Path,
    name: &str,
    seed: Option<&str>,
) -> (PathBuf, PathBuf, PublicKeys) {
    let ed = dir.join(format!("{name}.ed25519.pem"));
    let ml = dir.join(format!("{name}.mldsa87.pem"));
    let paths = key_paths(&ed, &ml, dir, name);
    let keys = generate_keys(openssl, &paths.borrow(), seed).unwrap();
    (ed, ml, keys)
}

/// The two public-key files beside a generated key's two private ones.
struct OwnedPaths {
    ed25519_key: PathBuf,
    mldsa87_key: PathBuf,
    public_ed25519: PathBuf,
    public_mldsa87: PathBuf,
}

impl OwnedPaths {
    fn borrow(&self) -> KeyPaths<'_> {
        KeyPaths {
            ed25519_key: &self.ed25519_key,
            mldsa87_key: &self.mldsa87_key,
            public_ed25519: &self.public_ed25519,
            public_mldsa87: &self.public_mldsa87,
        }
    }
}

fn key_paths(ed: &Path, ml: &Path, dir: &Path, name: &str) -> OwnedPaths {
    OwnedPaths {
        ed25519_key: ed.to_path_buf(),
        mldsa87_key: ml.to_path_buf(),
        public_ed25519: dir.join(format!("{name}.ed25519.pub")),
        public_mldsa87: dir.join(format!("{name}.mldsa87.pub")),
    }
}

#[test]
fn openssl_signatures_verify_end_to_end() {
    let Some(openssl) = openssl() else { return };
    let dir = TempDir::new("end-to-end");
    let (ed, ml, keys) = generate(&openssl, &dir.0, "fresh", None);
    let signer = OpensslSigner::new(&openssl, &ed, &ml).unwrap();
    assert_eq!(signer.public_keys(), &keys);

    let image = unsigned_image();
    let signed = sign_krn(&image, &signer, &OPTIONS).unwrap();

    // Verify independently of `sign_krn`'s own self-check.
    let parsed = Image::parse(&signed, ParseOptions::default()).unwrap();
    let store = store(&keys);
    let block = parsed.signature_block().unwrap();
    let digest = parsed.header().content_digest;
    assert_eq!(digest, image[48..112]);
    assert_eq!(
        verify_krn_block(block, &store, &digest),
        Ok(key_id(&store[0].key))
    );
    // The statement says what `sign_krn` promises.
    let blocks = SignatureBlock::parse(block).unwrap();
    let entry = blocks.entries().next().unwrap();
    assert_eq!(entry.statement.id, crate::sign::KRN_ID);
    assert_eq!(entry.statement.version, origin_version::VERSION);
    assert_eq!(
        entry.statement.claimed_signing_time,
        OPTIONS.claimed_signing_time
    );

    // Another key's signature, from the same tool, does not verify under this one.
    let (ed2, ml2, other) = generate(&openssl, &dir.0, "other", None);
    let signer2 = OpensslSigner::new(&openssl, &ed2, &ml2).unwrap();
    let other_signed = sign_krn(&image, &signer2, &OPTIONS).unwrap();
    let parsed = Image::parse(&other_signed, ParseOptions::default()).unwrap();
    assert_eq!(
        verify_krn_block(parsed.signature_block().unwrap(), &store, &digest),
        Err(KrnBlockError::UntrustedKey)
    );
    assert_ne!(other.ed25519, keys.ed25519);
}

/// A seed gives the same keys every time: the boot test's trusted key is reproducible. No
/// second run with another seed gives the same key.
#[test]
fn test_keys_come_from_their_seed_only() {
    let Some(openssl) = openssl() else { return };
    let dir = TempDir::new("seeded");
    let (_, _, first) = generate(&openssl, &dir.0, "a", Some("a label"));
    let (_, _, again) = generate(&openssl, &dir.0, "b", Some("a label"));
    let (_, _, other) = generate(&openssl, &dir.0, "c", Some("another label"));
    assert_eq!(first, again);
    assert_ne!(first.ed25519, other.ed25519);
    assert_ne!(first.mldsa87, other.mldsa87);
}

#[test]
fn keygen_refuses_to_overwrite_and_the_signer_refuses_other_keys() {
    let Some(openssl) = openssl() else { return };
    let dir = TempDir::new("refusals");
    let (ed, ml, _) = generate(&openssl, &dir.0, "k", None);
    let fresh = |name: &str| dir.0.join(name);
    let fresh_paths = OwnedPaths {
        ed25519_key: fresh("new.ed25519.pem"),
        mldsa87_key: fresh("new.mldsa87.pem"),
        public_ed25519: fresh("new.ed25519.pub"),
        public_mldsa87: fresh("new.mldsa87.pub"),
    };
    // A private key that exists, in each of the four positions, is left alone.
    for mutate in [
        |p: &mut OwnedPaths, ed: &Path| p.ed25519_key = ed.to_path_buf(),
        |p: &mut OwnedPaths, ed: &Path| p.mldsa87_key = ed.to_path_buf(),
        |p: &mut OwnedPaths, ed: &Path| p.public_ed25519 = ed.to_path_buf(),
        |p: &mut OwnedPaths, ed: &Path| p.public_mldsa87 = ed.to_path_buf(),
    ] {
        let mut paths = OwnedPaths {
            ed25519_key: fresh_paths.ed25519_key.clone(),
            mldsa87_key: fresh_paths.mldsa87_key.clone(),
            public_ed25519: fresh_paths.public_ed25519.clone(),
            public_mldsa87: fresh_paths.public_mldsa87.clone(),
        };
        mutate(&mut paths, &ed);
        assert_eq!(
            generate_keys(&openssl, &paths.borrow(), None),
            Err(SignError::KeyExists(ed.clone()))
        );
    }
    assert!(!fresh_paths.ed25519_key.exists());
    // The keys swapped: each file fails the prefix check for the algorithm expected.
    assert!(matches!(
        OpensslSigner::new(&openssl, &ml, &ed),
        Err(SignError::KeyFormat {
            expected: "an Ed25519 private key",
            ..
        })
    ));
    assert!(matches!(
        OpensslSigner::new(&openssl, &ed, &dir.0.join("missing.pem")),
        Err(SignError::KeyFormat { .. })
    ));
    // Not an OpenSSL at all.
    assert!(matches!(
        OpensslSigner::new(Path::new("/nonexistent/openssl"), &ed, &ml),
        Err(SignError::Openssl(_))
    ));
}
