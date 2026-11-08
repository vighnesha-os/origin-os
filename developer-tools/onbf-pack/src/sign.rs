//! Signing a KRN image on the build host (ONBF §7.1–§7.2, encodings S1–S4; CLAUDE.md §59).
//!
//! `onbf-pack` builds the statement, the entry and the block itself, and then verifies the
//! result with the code the boot loader runs. The two signatures come from a [`Signer`]:
//! today [`HostKey`], which signs with the original Rust code of `onbf-sign` (the owner's
//! decision of 2026-11-08). The private key is a 104-byte file that only this tool reads.
//! Why, and what was rejected: `docs/research/2026-11-08-krn-signature-verification.md`.
//!
//! Signing the statement is the one step of a build that is not reproducible: ML-DSA signing
//! is hedged with fresh randomness (FIPS 204 §3.4), so its signature differs from run to run.
//! The container, the content digest and the statement stay deterministic; the claimed signing
//! time is an input, never the host's clock (ONBF-R9; errata V6).

use core::fmt;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use onbf_format::signature::{self, Role, Suite};
use onbf_format::{Image, ParseOptions, Profile, SMALL_ALIGN, ocbor};
use onbf_sign::{ed25519, mldsa87, os_random, wipe};
use onbf_verify::{KrnBlockError, PublicKey, TrustedKey, key_id, verify_krn_block};
use origin_crypto::{MLDSA87_SIGNATURE_BYTES, Sha256, Sha512};

use crate::container::{align_up, put};
use crate::krn::{self, VerifyError};
use crate::ocbor::Value;

/// The KRN statement's `id` (errata V3): a KRN manifest has no `id` of its own (K4), and the
/// loader does not compare this one. ASCII, so errata O1 does not arise.
pub const KRN_ID: &str = "origin.framekernel";

/// A signer's raw public keys.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicKeys {
    /// The Ed25519 key.
    pub ed25519: [u8; 32],
    /// The ML-DSA-87 key.
    pub mldsa87: Vec<u8>,
}

impl PublicKeys {
    /// The key as `onbf-verify` takes it.
    fn as_public_key(&self) -> PublicKey<'_> {
        PublicKey {
            ed25519: &self.ed25519,
            mldsa: &self.mldsa87,
        }
    }
}

/// Both signatures over one statement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signatures {
    /// 64 bytes.
    pub ed25519: Vec<u8>,
    /// 4,627 bytes.
    pub mldsa87: Vec<u8>,
}

/// What signs a statement. The private key stays behind this interface.
pub trait Signer {
    /// The public keys of the signing key.
    fn public_keys(&self) -> &PublicKeys;
    /// Both signatures over the exact bytes of `statement`, ML-DSA with context string
    /// `context` (ONBF §7.1).
    fn sign(&self, statement: &[u8], context: &[u8]) -> Result<Signatures, SignError>;
}

/// Inputs to the statement that are not in the image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignOptions {
    /// Statement key 6, seconds since 1970-01-01 UTC. Informational only (§7.6). The caller
    /// supplies it, so a build never reads the clock (ONBF-R9).
    pub claimed_signing_time: u64,
}

/// Why an image could not be signed, or a key could not be made or read. Nothing is written
/// on any error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignError {
    /// A key file or public-key file already exists; `keygen` never overwrites anything.
    KeyExists(PathBuf),
    /// A key file is not what [`generate_keys`] wrote, or it is damaged.
    KeyFile {
        /// The file.
        path: PathBuf,
        /// What is wrong with it.
        why: &'static str,
    },
    /// The operating system's random number generator failed.
    Random(String),
    /// A signature has the wrong length.
    SignatureLength {
        /// Which signature.
        algorithm: &'static str,
        /// Its length.
        got: usize,
    },
    /// A file could not be written or read.
    Io(String),
    /// The input image is already signed.
    AlreadySigned,
    /// The input is not a valid unsigned KRN image.
    Input(VerifyError),
    /// The signed image failed its in-process verification: the signer returned something
    /// the loader will refuse.
    SelfCheck(SelfCheckError),
}

/// What failed when the finished image was re-verified as the loader will.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SelfCheckError {
    /// The container is refused outside developer mode.
    Container(onbf_format::Error),
    /// The signature block is refused.
    Block(KrnBlockError),
    /// The finished image fails the KRN checks.
    Krn(VerifyError),
}

impl fmt::Display for SignError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::KeyExists(path) => write!(f, "{} already exists", path.display()),
            Self::KeyFile { path, why } => write!(f, "{}: {why}", path.display()),
            Self::Random(why) => write!(f, "the system random number generator failed: {why}"),
            Self::SignatureLength { algorithm, got } => {
                write!(f, "{algorithm} signature of {got} bytes")
            }
            Self::Io(why) => write!(f, "{why}"),
            Self::AlreadySigned => f.write_str("the input image is already signed"),
            Self::Input(e) => write!(f, "the input is not a valid unsigned KRN image: {e}"),
            Self::SelfCheck(e) => write!(f, "self-check failed, nothing written: {e:?}"),
        }
    }
}

impl std::error::Error for SignError {}

/// `bytes` as lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
    use fmt::Write;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// The first bytes of a key file holding a real key.
const KEY_FILE_MAGIC: [u8; 8] = *b"ONBFKEY1";
/// The first bytes of a key file holding a test key, derived from public text. The file says
/// so, so that a tool can never present it as a real key.
const TEST_KEY_FILE_MAGIC: [u8; 8] = *b"ONBFTKEY";
/// magic ‖ Ed25519 seed ‖ ML-DSA seed ξ ‖ key ID: the key ID lets a damaged file be caught
/// before anything is signed with it.
const KEY_FILE_BYTES: usize = 8 + 32 + 32 + 32;

/// An `origin-kernel` signing key on the build host: a development key. A release signing key
/// belongs in an HSM (ONBF §7.2, CLAUDE.md §60).
pub struct HostKey {
    ed25519_seed: Box<[u8; 32]>,
    mldsa: mldsa87::SigningKey,
    public: PublicKeys,
    is_test_key: bool,
}

impl fmt::Debug for HostKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HostKey(..)")
    }
}

impl Drop for HostKey {
    fn drop(&mut self) {
        wipe(self.ed25519_seed.as_mut_slice());
    }
}

impl HostKey {
    fn from_seeds(ed25519_seed: &[u8; 32], mldsa_seed: &[u8; 32], is_test_key: bool) -> Self {
        let mldsa = mldsa87::SigningKey::from_seed(mldsa_seed);
        let public = PublicKeys {
            ed25519: ed25519::public_key(ed25519_seed),
            mldsa87: mldsa.public_key().to_vec(),
        };
        Self {
            ed25519_seed: Box::new(*ed25519_seed),
            mldsa,
            public,
            is_test_key,
        }
    }

    /// True if this key's file records it as derived from public text (a test key).
    pub fn is_test_key(&self) -> bool {
        self.is_test_key
    }

    /// Loads a key file written by [`generate_keys`], refusing a damaged or foreign one, and
    /// one that anyone but its owner can read: like an SSH private key, it must be mode 0600.
    pub fn load(path: &Path) -> Result<Self, SignError> {
        let permissions = fs::metadata(path)
            .map_err(|e| SignError::Io(format!("cannot read {}: {e}", path.display())))?
            .permissions()
            .mode();
        if permissions & 0o077 != 0 {
            return Err(SignError::KeyFile {
                path: path.to_path_buf(),
                why: "readable by others: a key file must be mode 0600",
            });
        }
        let mut bytes = fs::read(path)
            .map_err(|e| SignError::Io(format!("cannot read {}: {e}", path.display())))?;
        let result = Self::parse(&bytes).map_err(|why| SignError::KeyFile {
            path: path.to_path_buf(),
            why,
        });
        wipe(&mut bytes);
        result
    }

    fn parse(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() != KEY_FILE_BYTES {
            return Err("not a key file: wrong length");
        }
        let (magic, rest) = bytes.split_at(8);
        let is_test_key = match magic {
            m if m == KEY_FILE_MAGIC => false,
            m if m == TEST_KEY_FILE_MAGIC => true,
            _ => return Err("not a key file: bad magic"),
        };
        let (ed25519_seed, rest) = rest.split_at(32);
        let (mldsa_seed, stored_id) = rest.split_at(32);
        let (Ok(ed25519_seed), Ok(mldsa_seed)) = (
            <&[u8; 32]>::try_from(ed25519_seed),
            <&[u8; 32]>::try_from(mldsa_seed),
        ) else {
            return Err("not a key file: wrong length");
        };
        let key = Self::from_seeds(ed25519_seed, mldsa_seed, is_test_key);
        if key_id(&key.public.as_public_key()) != stored_id {
            return Err("damaged: its keys do not match its key ID");
        }
        Ok(key)
    }
}

impl Signer for HostKey {
    fn public_keys(&self) -> &PublicKeys {
        &self.public
    }

    fn sign(&self, statement: &[u8], context: &[u8]) -> Result<Signatures, SignError> {
        let mut rnd = [0u8; 32];
        os_random(&mut rnd).map_err(|e| SignError::Random(e.to_string()))?;
        let mldsa87 = self.mldsa.sign(statement, context, &rnd);
        wipe(&mut rnd);
        let mldsa87 = mldsa87.map_err(|e| SignError::Io(format!("ML-DSA: {e:?}")))?;
        Ok(Signatures {
            ed25519: ed25519::sign(self.ed25519_seed.as_ref(), statement).to_vec(),
            mldsa87: mldsa87.to_vec(),
        })
    }
}

/// Writes a new file with `mode`, refusing to touch an existing path. If writing fails, the
/// partial file is removed.
fn write_new(path: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)?;
    let result = file.write_all(bytes).and_then(|()| file.sync_all());
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result
}

/// Writes every file or none: a later failure removes the files already created.
fn write_all_new(files: &[(&Path, &[u8], u32)]) -> Result<(), SignError> {
    for (done, &(path, bytes, mode)) in files.iter().enumerate() {
        if let Err(e) = write_new(path, bytes, mode) {
            for &(created, _, _) in files.iter().take(done) {
                let _ = fs::remove_file(created);
            }
            return Err(if e.kind() == std::io::ErrorKind::AlreadyExists {
                SignError::KeyExists(path.to_path_buf())
            } else {
                SignError::Io(format!("{}: {e}", path.display()))
            });
        }
    }
    Ok(())
}

/// A 32-byte seed from public text, separated by `domain`.
fn domain_seed(domain: &str, label: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(domain.as_bytes());
    h.update(&[0]);
    h.update(label.as_bytes());
    h.finalize()
}

/// Where [`generate_keys`] writes a new key pair. None of these may exist.
#[derive(Clone, Copy, Debug)]
pub struct KeyPaths<'a> {
    /// The private key (mode 0600).
    pub key: &'a Path,
    /// The raw Ed25519 public key, 32 bytes.
    pub public_ed25519: &'a Path,
    /// The raw ML-DSA-87 public key, 2,592 bytes.
    pub public_mldsa87: &'a Path,
}

/// Makes a key pair: the private key in a new file (mode 0600) and the raw public keys in two
/// more, and returns the public keys. No file is ever overwritten: an existing path, in
/// particular a private key given by mistake as a public-key path, is refused.
///
/// `test_seed` derives both keys from public text, so the result is a **test key by
/// construction**: anyone can recompute it. It exists for boot tests and fixtures only; never
/// give it to anything that holds a real key.
pub fn generate_keys(
    paths: &KeyPaths<'_>,
    test_seed: Option<&str>,
) -> Result<PublicKeys, SignError> {
    let (mut ed25519_seed, mut mldsa_seed, magic) = match test_seed {
        Some(label) => (
            domain_seed("onbf-pack TEST-ONLY Ed25519 key", label),
            domain_seed("onbf-pack TEST-ONLY ML-DSA-87 key", label),
            TEST_KEY_FILE_MAGIC,
        ),
        None => {
            let (mut ed25519_seed, mut mldsa_seed) = ([0u8; 32], [0u8; 32]);
            os_random(&mut ed25519_seed).map_err(|e| SignError::Random(e.to_string()))?;
            os_random(&mut mldsa_seed).map_err(|e| SignError::Random(e.to_string()))?;
            (ed25519_seed, mldsa_seed, KEY_FILE_MAGIC)
        }
    };
    let key = HostKey::from_seeds(&ed25519_seed, &mldsa_seed, test_seed.is_some());
    let mut file = Vec::with_capacity(KEY_FILE_BYTES);
    file.extend_from_slice(&magic);
    file.extend_from_slice(&ed25519_seed);
    file.extend_from_slice(&mldsa_seed);
    file.extend_from_slice(&key_id(&key.public.as_public_key()));
    let written = write_all_new(&[
        (paths.key, &file, 0o600),
        (paths.public_ed25519, &key.public.ed25519, 0o644),
        (paths.public_mldsa87, &key.public.mldsa87, 0o644),
    ]);
    wipe(&mut file);
    wipe(&mut ed25519_seed);
    wipe(&mut mldsa_seed);
    written?;
    Ok(key.public.clone())
}

/// The statement (S3).
fn statement(digest: &[u8; 64], id: &str, version: &str, time: u64) -> Value {
    Value::Map(BTreeMap::from([
        (0, Value::Unsigned(1)),
        (1, Value::Unsigned(Profile::Krn as u64)),
        (2, Value::Bytes(digest.to_vec())),
        (3, Value::Text(id.to_string())),
        (4, Value::Text(version.to_string())),
        (5, Value::Unsigned(Role::Kernel as u64)),
        (6, Value::Unsigned(time)),
    ]))
}

/// One entry of the block (§7.1): role `origin-kernel`, suite OSS-1R, no chain (errata V1).
fn entry(key_id: &[u8; 32], statement: Value, signatures: &Signatures) -> Value {
    Value::Map(BTreeMap::from([
        (1, Value::Unsigned(Role::Kernel as u64)),
        (2, Value::Unsigned(Suite::Oss1R as u64)),
        (3, Value::Bytes(key_id.to_vec())),
        (4, Value::Array(Vec::new())),
        (5, statement),
        (6, Value::Bytes(signatures.ed25519.clone())),
        (7, Value::Bytes(signatures.mldsa87.clone())),
    ]))
}

/// The block around one encoded entry (§7.1).
fn block(entry: &[u8]) -> Result<Vec<u8>, SignError> {
    let size = u32::try_from(entry.len())
        .map_err(|_| SignError::Io("the signature entry is too long".into()))?;
    let mut out = Vec::new();
    out.extend_from_slice(&signature::MAGIC);
    out.extend_from_slice(&signature::VERSION.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(entry);
    Ok(out)
}

/// Appends `block` to `image` at its canonical offset (errata C3) and records it in the
/// header. `content_digest` excludes those header bytes (§4.5), so it is unchanged.
fn attach(image: &[u8], block: &[u8]) -> Result<Vec<u8>, SignError> {
    let layout = || SignError::Io("the signed image does not fit".into());
    let end = u64::try_from(image.len()).map_err(|_| layout())?;
    let offset = align_up(end, SMALL_ALIGN).ok_or_else(layout)?;
    let padded = usize::try_from(offset).map_err(|_| layout())?;
    let size = u64::try_from(block.len()).map_err(|_| layout())?;
    let mut out = image.to_vec();
    out.resize(padded, 0);
    out.extend_from_slice(block);
    put(&mut out, 32, &offset.to_le_bytes()).map_err(|_| layout())?;
    put(&mut out, 40, &size.to_le_bytes()).map_err(|_| layout())?;
    Ok(out)
}

/// Re-verifies a finished image with the code a loader outside developer mode runs: the
/// container and content digest, the signature block, and every section (`krn::verify`). The
/// loader's own further limits are not repeated here: an image that asks for `cpu_features` it
/// cannot test yet, or a boot-info version it does not provide, can be signed and still be
/// refused at boot (ADR-0002 B2 steps 4 and 5).
fn self_check(signed: &[u8], keys: &PublicKeys) -> Result<(), SelfCheckError> {
    let image = Image::parse(signed, ParseOptions::default()).map_err(SelfCheckError::Container)?;
    image
        .verify_content_digest(|parts| {
            let mut h = Sha512::new();
            for part in parts {
                h.update(part);
            }
            h.finalize()
        })
        .map_err(SelfCheckError::Container)?;
    let store = [TrustedKey {
        role: Role::Kernel,
        key: keys.as_public_key(),
    }];
    let block = image
        .signature_block()
        .ok_or(SelfCheckError::Block(KrnBlockError::EntryCount(0)))?;
    verify_krn_block(block, &store, &image.header().content_digest)
        .map_err(SelfCheckError::Block)?;
    krn::verify(signed).map_err(SelfCheckError::Krn)?;
    Ok(())
}

/// Signs a complete unsigned KRN image and returns the signed image, after verifying its
/// signature block and sections with the loader's code ([`self_check`]). The signer's private
/// key never reaches this function.
pub fn sign_krn(
    image: &[u8],
    signer: &dyn Signer,
    options: &SignOptions,
) -> Result<Vec<u8>, SignError> {
    let parse = ParseOptions {
        developer_mode: true,
        ..ParseOptions::default()
    };
    let parsed =
        Image::parse(image, parse).map_err(|e| SignError::Input(VerifyError::Container(e)))?;
    if parsed.signature_block().is_some() {
        return Err(SignError::AlreadySigned);
    }
    krn::verify(image).map_err(SignError::Input)?;

    let keys = signer.public_keys();
    let id = key_id(&keys.as_public_key());
    let statement = statement(
        &parsed.header().content_digest,
        KRN_ID,
        origin_version::VERSION,
        options.claimed_signing_time,
    );
    let statement_bytes = statement.encode();
    let signatures = signer.sign(&statement_bytes, Role::Kernel.context())?;
    if signatures.ed25519.len() != 64 {
        return Err(SignError::SignatureLength {
            algorithm: "Ed25519",
            got: signatures.ed25519.len(),
        });
    }
    if signatures.mldsa87.len() != MLDSA87_SIGNATURE_BYTES {
        return Err(SignError::SignatureLength {
            algorithm: "ML-DSA-87",
            got: signatures.mldsa87.len(),
        });
    }
    let entry_bytes = entry(&id, statement, &signatures).encode();
    ocbor::validate(&entry_bytes)
        .map_err(|e| SignError::Io(format!("the signature entry is not OCBOR-1: {e:?}")))?;
    let signed = attach(image, &block(&entry_bytes)?)?;
    self_check(&signed, keys).map_err(SignError::SelfCheck)?;
    Ok(signed)
}

/// Test access to the byte-level builders.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    pub(crate) fn statement_bytes(
        digest: &[u8; 64],
        id: &str,
        version: &str,
        time: u64,
    ) -> Vec<u8> {
        statement(digest, id, version, time).encode()
    }

    pub(crate) fn block_bytes(
        key_id: &[u8; 32],
        digest: &[u8; 64],
        id: &str,
        version: &str,
        time: u64,
        signatures: &Signatures,
    ) -> Vec<u8> {
        let entry = entry(key_id, statement(digest, id, version, time), signatures).encode();
        block(&entry).unwrap()
    }

    pub(crate) fn attach_block(image: &[u8], signature_block: &[u8]) -> Vec<u8> {
        attach(image, signature_block).unwrap()
    }
}
