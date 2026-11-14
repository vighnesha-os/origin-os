//! Signing a KRN image on the build host (ONBF §7.1–§7.2, encodings S1–S4; CLAUDE.md §59).
//!
//! `onbf-pack` builds the statement, the entry and the block itself, and then verifies the
//! result with the code the boot loader runs. The two signatures come from a [`Signer`]:
//! today [`HostKey`], which signs with the original Rust code of `onbf-sign` (the owner's
//! decision of 2026-11-08). The private key is a 104-byte file that only this tool's command
//! line reads. Why, and what was rejected: `docs/research/2026-11-08-krn-signature-verification.md`.
//!
//! Nothing here reaches the host (ADR-0011 Amendment 4, T1): a key file comes in and goes out
//! as bytes, and fresh randomness comes from the caller through [`Random`]. The command line
//! reads and writes the files, with their checks (no symbolic link, a regular file, owner-only
//! on reading; 0600, never overwriting, on writing), and fills [`Random`] from the operating
//! system's generator; so
//! does `xtask`, which derives its test keys in memory.
//!
//! Signing the statement is the one step of a build that is not reproducible: ML-DSA signing
//! is hedged with fresh randomness (FIPS 204 §3.4), so its signature differs from run to run.
//! The container, the content digest and the statement stay deterministic; the claimed signing
//! time is an input, never the host's clock (ONBF-R9; errata V6).

use core::cell::RefCell;
use core::fmt;
use std::collections::BTreeMap;

use onbf_format::signature::{self, Role, Suite};
use onbf_format::{Image, ParseOptions, Profile, SMALL_ALIGN, ocbor};
use onbf_sign::{ed25519, mldsa87, wipe};
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

    /// The key ID a trust store names this key by (ONBF §7.2).
    #[must_use]
    pub fn key_id(&self) -> [u8; 32] {
        key_id(&self.as_public_key())
    }
}

/// Why a [`Random`] gave no randomness.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RandomError(pub String);

impl fmt::Display for RandomError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Fresh randomness, which the caller supplies (ADR-0011 Amendment 4, T1): on the build host,
/// the operating system's generator. This crate never reaches for it itself.
///
/// It fails closed. A [`fill`](Random::fill) that returns an error stops what asked for it,
/// and so does one that returns success with its 32 bytes all zero: a generator gives that
/// once in 2^256, and a `fill` that wrote nothing gives it every time. So nothing is generated
/// or signed with a buffer left untouched. A `fill` that writes only part of the buffer and
/// reports success is not caught: filling every byte is the trait's contract, which both
/// `Random`s in the tree keep with `read_exact`.
pub trait Random {
    /// Fills every byte of `out`, or fails.
    ///
    /// # Errors
    ///
    /// Whatever the source reports; the caller stops on it.
    fn fill(&mut self, out: &mut [u8]) -> Result<(), RandomError>;
}

/// Fills `out` from `random`, failing closed (see [`Random`]).
fn fill(random: &mut dyn Random, out: &mut [u8; 32]) -> Result<(), SignError> {
    out.fill(0);
    random
        .fill(out)
        .map_err(|e| SignError::Random(e.to_string()))?;
    if out.iter().all(|&b| b == 0) {
        return Err(SignError::Random(
            "it returned 32 zero bytes: nothing was filled".into(),
        ));
    }
    Ok(())
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

/// Why an image could not be signed, or a key could not be made or read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignError {
    /// A key file's bytes are not what [`new_key`] or [`new_test_key`] made, or are damaged;
    /// the value says what is wrong. The caller names the file.
    KeyFile(&'static str),
    /// The caller's [`Random`] gave no randomness; nothing was generated or signed.
    Random(String),
    /// A signature has the wrong length.
    SignatureLength {
        /// Which signature.
        algorithm: &'static str,
        /// Its length.
        got: usize,
    },
    /// The signed image could not be laid out, or ML-DSA refused to sign.
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
            Self::KeyFile(why) => f.write_str(why),
            Self::Random(why) => write!(f, "no randomness: {why}"),
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
pub const KEY_FILE_BYTES: usize = 8 + 32 + 32 + 32;

/// An `origin-kernel` signing key on the build host: a development key. A release signing key
/// belongs in an HSM (ONBF §7.2, CLAUDE.md §60). It signs with ML-DSA's hedge from the
/// caller's [`Random`].
pub struct HostKey {
    ed25519_seed: Box<[u8; 32]>,
    mldsa: mldsa87::SigningKey,
    public: PublicKeys,
    is_test_key: bool,
    random: RefCell<Box<dyn Random>>,
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
    fn from_seeds(
        ed25519_seed: &[u8; 32],
        mldsa_seed: &[u8; 32],
        is_test_key: bool,
        random: Box<dyn Random>,
    ) -> Self {
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
            random: RefCell::new(random),
        }
    }

    /// True if this key's file records it as derived from public text (a test key).
    #[must_use]
    pub fn is_test_key(&self) -> bool {
        self.is_test_key
    }

    /// The key whose file is `bytes`, as [`new_key`] or [`new_test_key`] made it; it signs with
    /// ML-DSA's hedge from `random`. A damaged or foreign file is refused. Where the bytes came
    /// from, and whether that file was safe to read (no symbolic link, a regular file,
    /// owner-only), is the caller's to check: the command line does.
    ///
    /// # Errors
    ///
    /// [`SignError::KeyFile`], saying what is wrong.
    pub fn from_key_file(bytes: &[u8], random: Box<dyn Random>) -> Result<Self, SignError> {
        if bytes.len() != KEY_FILE_BYTES {
            return Err(SignError::KeyFile("not a key file: wrong length"));
        }
        let (magic, rest) = bytes.split_at(8);
        let is_test_key = match magic {
            m if m == KEY_FILE_MAGIC => false,
            m if m == TEST_KEY_FILE_MAGIC => true,
            _ => return Err(SignError::KeyFile("not a key file: bad magic")),
        };
        let (ed25519_seed, rest) = rest.split_at(32);
        let (mldsa_seed, stored_id) = rest.split_at(32);
        let (Ok(ed25519_seed), Ok(mldsa_seed)) = (
            <&[u8; 32]>::try_from(ed25519_seed),
            <&[u8; 32]>::try_from(mldsa_seed),
        ) else {
            return Err(SignError::KeyFile("not a key file: wrong length"));
        };
        let key = Self::from_seeds(ed25519_seed, mldsa_seed, is_test_key, random);
        if key.public.key_id() != stored_id {
            return Err(SignError::KeyFile(
                "damaged: its keys do not match its key ID",
            ));
        }
        Ok(key)
    }

    /// The test key [`new_test_key`] derives from `label`, without a file; it signs with
    /// ML-DSA's hedge from `random`. A **test key by construction**: anyone can recompute it.
    #[must_use]
    pub fn from_test_seed(label: &str, random: Box<dyn Random>) -> Self {
        let (mut ed25519_seed, mut mldsa_seed) = test_seeds(label);
        let key = Self::from_seeds(&ed25519_seed, &mldsa_seed, true, random);
        wipe(&mut ed25519_seed);
        wipe(&mut mldsa_seed);
        key
    }
}

impl Signer for HostKey {
    fn public_keys(&self) -> &PublicKeys {
        &self.public
    }

    fn sign(&self, statement: &[u8], context: &[u8]) -> Result<Signatures, SignError> {
        let mut rnd = [0u8; 32];
        let filled = fill(self.random.borrow_mut().as_mut(), &mut rnd);
        if let Err(e) = filled {
            wipe(&mut rnd);
            return Err(e);
        }
        let mldsa87 = self.mldsa.sign(statement, context, &rnd);
        wipe(&mut rnd);
        let mldsa87 = mldsa87.map_err(|e| SignError::Io(format!("ML-DSA: {e:?}")))?;
        Ok(Signatures {
            ed25519: ed25519::sign(self.ed25519_seed.as_ref(), statement).to_vec(),
            mldsa87: mldsa87.to_vec(),
        })
    }
}

/// A 32-byte seed from public text, separated by `domain`.
fn domain_seed(domain: &str, label: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(domain.as_bytes());
    h.update(&[0]);
    h.update(label.as_bytes());
    h.finalize()
}

/// The two seeds of the test key derived from `label`.
fn test_seeds(label: &str) -> ([u8; 32], [u8; 32]) {
    (
        domain_seed("onbf-pack TEST-ONLY Ed25519 key", label),
        domain_seed("onbf-pack TEST-ONLY ML-DSA-87 key", label),
    )
}

/// The bytes of a private key file, overwritten when dropped (best effort; see `onbf-sign`).
pub struct KeyFile(Vec<u8>);

impl KeyFile {
    /// The [`KEY_FILE_BYTES`] bytes the caller writes, mode 0600.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for KeyFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("KeyFile(..)")
    }
}

impl Drop for KeyFile {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

/// A new key pair, for the caller to write: the private key's file and the raw public keys.
#[derive(Debug)]
pub struct NewKey {
    /// The private key file's bytes (mode 0600 when written).
    pub key_file: KeyFile,
    /// The raw public keys: the Ed25519 key, 32 bytes, and the ML-DSA-87 key, 2,592 bytes,
    /// each written as a file of its own.
    pub public: PublicKeys,
    /// True if it was derived from public text ([`new_test_key`]).
    pub is_test_key: bool,
}

/// The key file of the key with these seeds.
fn key_from_seeds(ed25519_seed: &[u8; 32], mldsa_seed: &[u8; 32], is_test_key: bool) -> NewKey {
    let mldsa = mldsa87::SigningKey::from_seed(mldsa_seed);
    let public = PublicKeys {
        ed25519: ed25519::public_key(ed25519_seed),
        mldsa87: mldsa.public_key().to_vec(),
    };
    let mut file = Vec::with_capacity(KEY_FILE_BYTES);
    file.extend_from_slice(if is_test_key {
        &TEST_KEY_FILE_MAGIC
    } else {
        &KEY_FILE_MAGIC
    });
    file.extend_from_slice(ed25519_seed);
    file.extend_from_slice(mldsa_seed);
    file.extend_from_slice(&public.key_id());
    NewKey {
        key_file: KeyFile(file),
        public,
        is_test_key,
    }
}

/// Makes a new key pair from the caller's randomness, for the caller to write: the private key
/// file (mode 0600) and the raw public keys (CLAUDE.md §59).
///
/// # Errors
///
/// [`SignError::Random`] if `random` fails or fills nothing (see [`Random`]): no key is made.
pub fn new_key(random: &mut dyn Random) -> Result<NewKey, SignError> {
    let (mut ed25519_seed, mut mldsa_seed) = ([0u8; 32], [0u8; 32]);
    let filled = fill(random, &mut ed25519_seed).and_then(|()| fill(random, &mut mldsa_seed));
    let key = filled.map(|()| key_from_seeds(&ed25519_seed, &mldsa_seed, false));
    wipe(&mut ed25519_seed);
    wipe(&mut mldsa_seed);
    key
}

/// The key pair derived from `label`, public text, so the result is a **test key by
/// construction**: anyone can recompute it. Its file says so. It exists for boot tests and
/// fixtures only; never give it to anything that holds a real key.
#[must_use]
pub fn new_test_key(label: &str) -> NewKey {
    let (mut ed25519_seed, mut mldsa_seed) = test_seeds(label);
    let key = key_from_seeds(&ed25519_seed, &mldsa_seed, true);
    wipe(&mut ed25519_seed);
    wipe(&mut mldsa_seed);
    key
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
