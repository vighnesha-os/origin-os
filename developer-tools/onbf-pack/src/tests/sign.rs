//! Signing: the encoder against the independent Python one, the block's placement, the
//! self-check, the host key file, and (when an OpenSSL with ML-DSA is installed) OpenSSL
//! verifying what this tool signed.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use onbf_format::signature::{Role, SignatureBlock};
use onbf_format::{Image, ParseOptions};
use onbf_verify::{KrnBlockError, PublicKey, TrustedKey, VerifyError, key_id, verify_krn_block};
use origin_crypto::Sha512;

use super::elf_writer::TestElf;
use super::options;
use crate::krn;
use crate::sign::testing::{attach_block, block_bytes, statement_bytes};
use crate::sign::{
    HostKey, KeyPaths, PublicKeys, SelfCheckError, SignError, SignOptions, Signatures, Signer,
    generate_keys, sign_krn,
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

// ---------------------------------------------------------------- with the real signer

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

/// The file paths for a key named `name` in `dir`.
struct OwnedPaths {
    key: PathBuf,
    public_ed25519: PathBuf,
    public_mldsa87: PathBuf,
}

impl OwnedPaths {
    fn new(dir: &Path, name: &str) -> Self {
        Self {
            key: dir.join(format!("{name}.key")),
            public_ed25519: dir.join(format!("{name}.ed25519")),
            public_mldsa87: dir.join(format!("{name}.mldsa87")),
        }
    }

    fn paths(&self) -> KeyPaths<'_> {
        KeyPaths {
            key: &self.key,
            public_ed25519: &self.public_ed25519,
            public_mldsa87: &self.public_mldsa87,
        }
    }
}

fn generate(dir: &Path, name: &str, seed: Option<&str>) -> (OwnedPaths, PublicKeys) {
    let paths = OwnedPaths::new(dir, name);
    let keys = generate_keys(&paths.paths(), seed).unwrap();
    (paths, keys)
}

#[test]
fn host_key_signatures_verify_end_to_end() {
    let dir = TempDir::new("end-to-end");
    let (paths, keys) = generate(&dir.0, "fresh", None);
    let signer = HostKey::load(&paths.key).unwrap();
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

    // Hedged signing: a second signature over the same statement differs, and verifies.
    let again = sign_krn(&image, &signer, &OPTIONS).unwrap();
    assert_ne!(signed, again);
    let parsed = Image::parse(&again, ParseOptions::default()).unwrap();
    assert_eq!(
        verify_krn_block(parsed.signature_block().unwrap(), &store, &digest),
        Ok(key_id(&store[0].key))
    );

    // Another key's signature does not verify under this one.
    let (other_paths, other) = generate(&dir.0, "other", None);
    let signer2 = HostKey::load(&other_paths.key).unwrap();
    let other_signed = sign_krn(&image, &signer2, &OPTIONS).unwrap();
    let parsed = Image::parse(&other_signed, ParseOptions::default()).unwrap();
    assert_eq!(
        verify_krn_block(parsed.signature_block().unwrap(), &store, &digest),
        Err(KrnBlockError::UntrustedKey)
    );
    assert_ne!(other.ed25519, keys.ed25519);
}

/// A seed gives the same keys every time, and a different seed different ones. The boot
/// test's trusted key is the one whose public half is committed in `boot/uefi/trust`: it
/// was made by OpenSSL from the same seeds, so this also shows the two agree.
#[test]
fn test_keys_come_from_their_seed_only() {
    let dir = TempDir::new("seeded");
    let (_, first) = generate(&dir.0, "a", Some("a label"));
    let (_, again) = generate(&dir.0, "b", Some("a label"));
    let (_, other) = generate(&dir.0, "c", Some("another label"));
    assert_eq!(first, again);
    assert_ne!(first.ed25519, other.ed25519);
    assert_ne!(first.mldsa87, other.mldsa87);

    let (paths, trusted) = generate(
        &dir.0,
        "trusted",
        Some("TEST-ONLY origin-kernel key 1: the test trust store holds it"),
    );
    let committed = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../boot/uefi/trust");
    let read = |name: &str| fs::read(committed.join(name)).unwrap();
    assert_eq!(
        trusted.ed25519.as_slice(),
        read("TEST-ONLY-origin-kernel.ed25519")
    );
    assert_eq!(trusted.mldsa87, read("TEST-ONLY-origin-kernel.mldsa87"));
    assert_eq!(
        fs::read(&paths.public_ed25519).unwrap(),
        read("TEST-ONLY-origin-kernel.ed25519")
    );
    assert_eq!(
        fs::read(&paths.public_mldsa87).unwrap(),
        read("TEST-ONLY-origin-kernel.mldsa87")
    );
    assert_eq!(
        fs::metadata(&paths.key).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn keygen_refuses_to_overwrite_anything() {
    let dir = TempDir::new("refusals");
    let (existing, _) = generate(&dir.0, "k", None);
    // An existing file in each of the three positions is left alone.
    for position in 0..3 {
        let mut paths = OwnedPaths::new(&dir.0, "new");
        let target = match position {
            0 => &mut paths.key,
            1 => &mut paths.public_ed25519,
            _ => &mut paths.public_mldsa87,
        };
        *target = existing.key.clone();
        assert_eq!(
            generate_keys(&paths.paths(), None),
            Err(SignError::KeyExists(existing.key.clone())),
            "position {position}"
        );
        assert!(!OwnedPaths::new(&dir.0, "new").key.exists());
    }
    // The same new path twice: the second write is refused and the first is removed, so the
    // private key can never be replaced by a public key.
    let mut paths = OwnedPaths::new(&dir.0, "same");
    paths.public_ed25519 = paths.key.clone();
    assert_eq!(
        generate_keys(&paths.paths(), None),
        Err(SignError::KeyExists(paths.key.clone()))
    );
    assert!(!paths.key.exists());
}

#[test]
fn a_damaged_or_foreign_key_file_is_refused() {
    let dir = TempDir::new("key-files");
    let (paths, _) = generate(&dir.0, "k", Some("a label"));
    let good = fs::read(&paths.key).unwrap();
    assert_eq!(good.len(), 104);
    let load_with = |bytes: &[u8]| {
        fs::remove_file(&paths.key).unwrap();
        fs::write(&paths.key, bytes).unwrap();
        fs::set_permissions(&paths.key, fs::Permissions::from_mode(0o600)).unwrap();
        HostKey::load(&paths.key).map(|_| ())
    };
    let flipped = |at: usize| {
        let mut bytes = good.clone();
        bytes[at] ^= 1;
        bytes
    };
    // A real key's magic, for a second accepted form of the file.
    let real_key = [&b"ONBFKEY1"[..], &good[8..]].concat();
    assert_eq!(&good[..8], b"ONBFTKEY", "a key from a test seed says so");
    assert!(load_with(&real_key).is_ok());
    for (bytes, why) in [
        (good[..103].to_vec(), "not a key file: wrong length"),
        (
            [&b"NOTAKEY1"[..], &good[8..]].concat(),
            "not a key file: bad magic",
        ),
        (flipped(30), "damaged: its keys do not match its key ID"),
        (flipped(103), "damaged: its keys do not match its key ID"),
    ] {
        assert_eq!(
            load_with(&bytes),
            Err(SignError::KeyFile {
                path: paths.key.clone(),
                why,
            })
        );
    }
    assert!(load_with(&good).is_ok());
    assert!(matches!(
        HostKey::load(&dir.0.join("missing.key")),
        Err(SignError::Io(_))
    ));
    // A key file others can read is refused, like an SSH private key.
    fs::set_permissions(&paths.key, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        HostKey::load(&paths.key).map(|_| ()),
        Err(SignError::KeyFile {
            path: paths.key.clone(),
            why: "readable by others: a key file must be mode 0600",
        })
    );
}

/// The key file is opened once with `O_NOFOLLOW`, and only that open file is checked and read:
/// a symbolic link is never followed, even to a key file that would be accepted itself.
#[test]
fn a_symbolic_link_to_a_key_file_is_refused() {
    let dir = TempDir::new("symlink");
    let (paths, _) = generate(&dir.0, "symlink", Some("symlink test"));
    assert!(HostKey::load(&paths.key).is_ok(), "the file itself loads");

    let link = dir.0.join("link.key");
    std::os::unix::fs::symlink(&paths.key, &link).unwrap();
    let refused = HostKey::load(&link).map(|_| ());
    assert!(
        matches!(&refused, Err(SignError::Io(why)) if why.contains("cannot read")),
        "{refused:?}"
    );
    // A directory is refused too, by the check on the open handle.
    assert_eq!(
        HostKey::load(&dir.0).map(|_| ()),
        Err(SignError::KeyFile {
            path: dir.0.clone(),
            why: "not a regular file",
        })
    );
}

// ---------------------------------------------------------------- OpenSSL, as an outside check

/// An OpenSSL with ML-DSA (3.5 or later), if one is installed. Signing does not use it; this is
/// an independent implementation checking what `onbf-sign` produced.
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

/// Verifies with `openssl pkeyutl`, given the public key as DER.
fn openssl_accepts(
    openssl: &Path,
    dir: &Path,
    public_der: &[u8],
    message: &[u8],
    signature: &[u8],
    context: Option<&[u8]>,
) -> bool {
    let (der, msg, sig) = (
        dir.join("pub.der"),
        dir.join("msg.bin"),
        dir.join("sig.bin"),
    );
    fs::write(&der, public_der).unwrap();
    fs::write(&msg, message).unwrap();
    fs::write(&sig, signature).unwrap();
    let mut command = Command::new(openssl);
    command.args([
        "pkeyutl", "-verify", "-pubin", "-keyform", "DER", "-rawin", "-inkey",
    ]);
    command
        .arg(&der)
        .arg("-in")
        .arg(&msg)
        .arg("-sigfile")
        .arg(&sig);
    if let Some(context) = context {
        command
            .arg("-pkeyopt")
            .arg(format!("hexcontext-string:{}", crate::sign::hex(context)));
    }
    command.output().unwrap().status.success()
}

/// Everything an `onbf-pack sign` run produces, checked by OpenSSL: both signatures over the
/// exact statement, with the role's context string, under the raw public keys.
#[test]
fn openssl_accepts_these_signatures() {
    let Some(openssl) = openssl() else { return };
    let dir = TempDir::new("openssl-check");
    let (paths, keys) = generate(&dir.0, "k", Some("an OpenSSL cross-check"));
    let signer = HostKey::load(&paths.key).unwrap();
    let signed = sign_krn(&unsigned_image(), &signer, &OPTIONS).unwrap();
    let parsed = Image::parse(&signed, ParseOptions::default()).unwrap();
    let blocks = SignatureBlock::parse(parsed.signature_block().unwrap()).unwrap();
    let entry = blocks.entries().next().unwrap();

    // SPKI encodings of the raw keys: fixed prefixes for these two algorithms.
    let ed_der = [
        &[
            0x30, 0x2A, 0x30, 0x05, 0x06, 0x03, 0x2B, 0x65, 0x70, 0x03, 0x21, 0x00,
        ][..],
        &keys.ed25519[..],
    ]
    .concat();
    let ml_der = [
        &[
            0x30, 0x82, 0x0A, 0x32, 0x30, 0x0B, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03,
            0x04, 0x03, 0x13, 0x03, 0x82, 0x0A, 0x21, 0x00,
        ][..],
        &keys.mldsa87[..],
    ]
    .concat();
    let context = Role::Kernel.context();
    let statement = entry.statement.bytes;
    assert!(openssl_accepts(
        &openssl,
        &dir.0,
        &ed_der,
        statement,
        entry.sig_ed25519,
        None
    ));
    assert!(openssl_accepts(
        &openssl,
        &dir.0,
        &ml_der,
        statement,
        entry.sig_mldsa,
        Some(context)
    ));
    // A flipped byte is refused by OpenSSL too, so the check above is a real one.
    let mut bad = entry.sig_mldsa.to_vec();
    bad[100] ^= 1;
    assert!(!openssl_accepts(
        &openssl,
        &dir.0,
        &ml_der,
        statement,
        &bad,
        Some(context)
    ));
}
