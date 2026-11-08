//! Signing a KRN image on the build host (ONBF §7.1–§7.2, encodings S1–S4; CLAUDE.md §59).
//!
//! `onbf-pack` builds the statement, the entry and the block itself, and then verifies the
//! result with the code the boot loader runs. The two signatures come from a [`Signer`]:
//! today [`OpensslSigner`], which runs OpenSSL (3.5 or later, which has ML-DSA) as a
//! subprocess. So this crate contains no signing algorithm, and no private key is read into
//! this process: OpenSSL opens the key files itself. Why, and what was rejected:
//! `docs/research/2026-11-08-krn-signature-verification.md`.
//!
//! Signing the statement is the one step of a build that is not reproducible: OpenSSL's
//! ML-DSA is hedged (FIPS 204 §3.4), so its signature differs from run to run. The
//! container, the content digest and the statement stay deterministic; the claimed signing
//! time is an input, never the host's clock (ONBF-R9; errata V6).

use core::fmt;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{self, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use onbf_format::signature::{self, Role, Suite};
use onbf_format::{Image, ParseOptions, Profile, SMALL_ALIGN, ocbor};
use onbf_verify::{KrnBlockError, PublicKey, TrustedKey, key_id, verify_krn_block};
use origin_crypto::{MLDSA87_PUBLIC_KEY_BYTES, MLDSA87_SIGNATURE_BYTES, Sha256, Sha512};

use crate::container::{align_up, put};
use crate::krn::{self, VerifyError};
use crate::ocbor::Value;

/// The KRN statement's `id` (errata V3): a KRN manifest has no `id` of its own (K4), and the
/// loader does not compare this one. ASCII, so errata O1 does not arise.
pub const KRN_ID: &str = "origin.framekernel";

/// The DER `SubjectPublicKeyInfo` prefix of an Ed25519 key (RFC 8410): the 32 key bytes follow.
const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2A, 0x30, 0x05, 0x06, 0x03, 0x2B, 0x65, 0x70, 0x03, 0x21, 0x00,
];
/// The DER `SubjectPublicKeyInfo` prefix of an ML-DSA-87 key: the 2,592 key bytes follow.
const MLDSA87_SPKI_PREFIX: [u8; 22] = [
    0x30, 0x82, 0x0A, 0x32, 0x30, 0x0B, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x03,
    0x13, 0x03, 0x82, 0x0A, 0x21, 0x00,
];
/// The DER `PrivateKeyInfo` prefix of an Ed25519 key built from a seed (RFC 8410): the 32
/// seed bytes follow. Used for test keys only.
const ED25519_PKCS8_PREFIX: [u8; 16] = [
    0x30, 0x2E, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2B, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
];

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

/// Why an image could not be signed. Nothing is written on any error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignError {
    /// OpenSSL could not be run, or refused the request.
    Openssl(String),
    /// A key file already exists; `keygen` never overwrites a key.
    KeyExists(PathBuf),
    /// A key file does not hold the expected kind of key.
    KeyFormat {
        /// The file.
        path: PathBuf,
        /// What it should hold.
        expected: &'static str,
    },
    /// A signature has the wrong length.
    SignatureLength {
        /// Which signature.
        algorithm: &'static str,
        /// Its length.
        got: usize,
    },
    /// A temporary file could not be written or read.
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
            Self::Openssl(why) => write!(f, "openssl: {why}"),
            Self::KeyExists(path) => write!(f, "{} already exists", path.display()),
            Self::KeyFormat { path, expected } => {
                write!(f, "{} is not {expected}", path.display())
            }
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

/// A directory for the statement and the signature files, removed on drop. Their contents
/// are public, but it is private (mode 0700, created exclusively): no other user may swap the
/// statement that OpenSSL is about to sign.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Result<Self, SignError> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("onbf-pack-sign-{}-{n}", process::id()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|e| SignError::Io(format!("{}: {e}", path.display())))?;
        Ok(Self(path))
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn read_file(path: &Path) -> Result<Vec<u8>, SignError> {
    fs::read(path).map_err(|e| SignError::Io(format!("cannot read {}: {e}", path.display())))
}

/// Runs OpenSSL and returns its stdout. Fails with its stderr on a non-zero exit.
fn run_openssl(openssl: &Path, args: &[OsString]) -> Result<Vec<u8>, SignError> {
    let out = Command::new(openssl)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| SignError::Openssl(format!("cannot run {}: {e}", openssl.display())))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(SignError::Openssl(format!(
            "{} {}: {}",
            openssl.display(),
            args.first()
                .map(|a| a.to_string_lossy().into_owned())
                .unwrap_or_default(),
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

/// Refuses anything but OpenSSL 3.5 or later, the first with ML-DSA. LibreSSL, which macOS
/// installs as `/usr/bin/openssl`, has no ML-DSA.
fn check_version(openssl: &Path) -> Result<(), SignError> {
    let out = run_openssl(openssl, &[OsString::from("version")])?;
    let text = String::from_utf8_lossy(&out);
    let mut words = text.split_whitespace();
    let name = words.next().unwrap_or("");
    let mut numbers = words.next().unwrap_or("").split('.');
    let major: u32 = numbers.next().unwrap_or("").parse().unwrap_or(0);
    let minor: u32 = numbers.next().unwrap_or("").parse().unwrap_or(0);
    if name == "OpenSSL" && (major, minor) >= (3, 5) {
        Ok(())
    } else {
        Err(SignError::Openssl(format!(
            "{} is `{}`; signing needs OpenSSL 3.5 or later (ML-DSA)",
            openssl.display(),
            text.trim()
        )))
    }
}

/// The raw public key in `key_file`: its DER SPKI must start with `prefix` and hold exactly
/// `prefix.len() + key_len` bytes, which fixes the algorithm and parameter set.
fn extract_public_key(
    openssl: &Path,
    key_file: &Path,
    prefix: &[u8],
    key_len: usize,
    expected: &'static str,
) -> Result<Vec<u8>, SignError> {
    let args = [
        OsString::from("pkey"),
        OsString::from("-in"),
        key_file.into(),
        OsString::from("-pubout"),
        OsString::from("-outform"),
        OsString::from("DER"),
    ];
    let der = run_openssl(openssl, &args).map_err(|error| match error {
        SignError::Openssl(_) => SignError::KeyFormat {
            path: key_file.to_path_buf(),
            expected,
        },
        other => other,
    })?;
    let key = der
        .strip_prefix(prefix)
        .filter(|key| key.len() == key_len)
        .ok_or_else(|| SignError::KeyFormat {
            path: key_file.to_path_buf(),
            expected,
        })?;
    Ok(key.to_vec())
}

/// The public keys for the private keys in the two files.
fn read_public_keys(
    openssl: &Path,
    ed25519_key: &Path,
    mldsa87_key: &Path,
) -> Result<PublicKeys, SignError> {
    let ed = extract_public_key(
        openssl,
        ed25519_key,
        &ED25519_SPKI_PREFIX,
        32,
        "an Ed25519 private key",
    )?;
    let mldsa87 = extract_public_key(
        openssl,
        mldsa87_key,
        &MLDSA87_SPKI_PREFIX,
        MLDSA87_PUBLIC_KEY_BYTES,
        "an ML-DSA-87 private key",
    )?;
    Ok(PublicKeys {
        ed25519: ed.as_slice().try_into().map_err(|_| SignError::KeyFormat {
            path: ed25519_key.to_path_buf(),
            expected: "an Ed25519 private key",
        })?,
        mldsa87,
    })
}

/// Signs with OpenSSL. The key files stay on disk, read only by OpenSSL.
#[derive(Debug)]
pub struct OpensslSigner {
    openssl: PathBuf,
    ed25519_key: PathBuf,
    mldsa87_key: PathBuf,
    public: PublicKeys,
}

impl OpensslSigner {
    /// A signer for the private keys in the two PEM files, checking that OpenSSL can do it.
    pub fn new(openssl: &Path, ed25519_key: &Path, mldsa87_key: &Path) -> Result<Self, SignError> {
        check_version(openssl)?;
        Ok(Self {
            public: read_public_keys(openssl, ed25519_key, mldsa87_key)?,
            openssl: openssl.to_path_buf(),
            ed25519_key: ed25519_key.to_path_buf(),
            mldsa87_key: mldsa87_key.to_path_buf(),
        })
    }

    /// `openssl pkeyutl -sign -rawin` of `message` with `key`: the signature, or an error.
    fn sign_one(
        &self,
        key: &Path,
        message: &Path,
        out: &Path,
        extra: &[OsString],
    ) -> Result<Vec<u8>, SignError> {
        let mut args: Vec<OsString> = ["pkeyutl", "-sign", "-rawin", "-inkey"]
            .into_iter()
            .map(OsString::from)
            .collect();
        args.push(key.into());
        args.push(OsString::from("-in"));
        args.push(message.into());
        args.push(OsString::from("-out"));
        args.push(out.into());
        args.extend_from_slice(extra);
        run_openssl(&self.openssl, &args)?;
        read_file(out)
    }
}

impl Signer for OpensslSigner {
    fn public_keys(&self) -> &PublicKeys {
        &self.public
    }

    fn sign(&self, statement: &[u8], context: &[u8]) -> Result<Signatures, SignError> {
        let dir = TempDir::new()?;
        let message = dir.file("statement.bin");
        fs::write(&message, statement)
            .map_err(|e| SignError::Io(format!("{}: {e}", message.display())))?;
        let ed25519 = self.sign_one(&self.ed25519_key, &message, &dir.file("ed25519.sig"), &[])?;
        let context = [
            OsString::from("-pkeyopt"),
            OsString::from(format!("hexcontext-string:{}", hex(context))),
        ];
        let mldsa87 = self.sign_one(
            &self.mldsa87_key,
            &message,
            &dir.file("mldsa87.sig"),
            &context,
        )?;
        Ok(Signatures { ed25519, mldsa87 })
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

/// Where [`generate_keys`] writes a new key pair. None of these may exist.
#[derive(Clone, Copy, Debug)]
pub struct KeyPaths<'a> {
    /// The Ed25519 private key (PEM).
    pub ed25519_key: &'a Path,
    /// The ML-DSA-87 private key (PEM).
    pub mldsa87_key: &'a Path,
    /// The raw Ed25519 public key, 32 bytes.
    pub public_ed25519: &'a Path,
    /// The raw ML-DSA-87 public key, 2,592 bytes.
    pub public_mldsa87: &'a Path,
}

/// Creates an Ed25519 and an ML-DSA-87 private key in new files (mode 0600) and the raw public
/// keys in two more, and returns the public keys. No file is ever overwritten: an existing
/// path, in particular a private key given by mistake as a public-key path, is refused.
///
/// `test_seed` derives both keys from public text, so the result is a **test key by
/// construction**: anyone can recompute it. It exists for boot tests and fixtures only; never
/// give it to anything that holds a real key.
pub fn generate_keys(
    openssl: &Path,
    paths: &KeyPaths<'_>,
    test_seed: Option<&str>,
) -> Result<PublicKeys, SignError> {
    let (ed25519_key, mldsa87_key) = (paths.ed25519_key, paths.mldsa87_key);
    check_version(openssl)?;
    for path in [
        ed25519_key,
        mldsa87_key,
        paths.public_ed25519,
        paths.public_mldsa87,
    ] {
        if path.exists() {
            return Err(SignError::KeyExists(path.to_path_buf()));
        }
    }
    let os = OsString::from;
    match test_seed {
        None => {
            run_openssl(
                openssl,
                &[
                    os("genpkey"),
                    os("-algorithm"),
                    os("ED25519"),
                    os("-out"),
                    ed25519_key.into(),
                ],
            )?;
            run_openssl(
                openssl,
                &[
                    os("genpkey"),
                    os("-algorithm"),
                    os("ML-DSA-87"),
                    os("-out"),
                    mldsa87_key.into(),
                ],
            )?;
        }
        Some(label) => {
            let dir = TempDir::new()?;
            let seed = domain_seed("onbf-pack TEST-ONLY Ed25519 key", label);
            let der = dir.file("ed25519-seed.der");
            fs::write(&der, [&ED25519_PKCS8_PREFIX[..], &seed[..]].concat())
                .map_err(|e| SignError::Io(format!("{}: {e}", der.display())))?;
            run_openssl(
                openssl,
                &[
                    os("pkey"),
                    os("-inform"),
                    os("DER"),
                    os("-in"),
                    der.into(),
                    os("-out"),
                    ed25519_key.into(),
                ],
            )?;
            let xi = domain_seed("onbf-pack TEST-ONLY ML-DSA-87 key", label);
            run_openssl(
                openssl,
                &[
                    os("genpkey"),
                    os("-algorithm"),
                    os("ML-DSA-87"),
                    os("-pkeyopt"),
                    OsString::from(format!("hexseed:{}", hex(&xi))),
                    os("-out"),
                    mldsa87_key.into(),
                ],
            )?;
        }
    }
    let keys = read_public_keys(openssl, ed25519_key, mldsa87_key)?;
    for (path, bytes) in [
        (paths.public_ed25519, &keys.ed25519[..]),
        (paths.public_mldsa87, &keys.mldsa87[..]),
    ] {
        fs::write(path, bytes).map_err(|e| SignError::Io(format!("{}: {e}", path.display())))?;
    }
    Ok(keys)
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

    /// The permission bits of a new [`TempDir`].
    pub(crate) fn temp_dir_mode() -> u32 {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        fs::metadata(&dir.0).unwrap().permissions().mode() & 0o777
    }
}
