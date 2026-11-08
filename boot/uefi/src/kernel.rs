//! Verifying a KRN image before anything of it runs (ADR-0002 B2; ONBF-R1, §8.1).

use core::fmt;

use onbf_format::{
    Compression, Error as FormatError, Image, Integrity, Kind, KrnManifest, ManifestError,
    NO_MERKLE, ParseOptions, Profile, Section,
};
use onbf_verify::{KrnBlockError, TrustedKey, VerifyError, verify_krn_block};
use origin_crypto::{Sha256, Sha512};

/// Why the loader refused a kernel image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KernelError {
    /// The image carries no signature, and this loader is not in developer mode (ONBF §4.2).
    Unsigned,
    /// The container is malformed.
    Parse(FormatError),
    /// The image is not a KRN image.
    NotKrn,
    /// The SHA-512 content digest does not match.
    ContentDigest,
    /// The signature block was refused (ADR-0002 B2 step 3).
    Signature(KrnBlockError),
    /// The manifest section failed its SHA-256 check.
    ManifestDigest(FormatError),
    /// The manifest is invalid, or its entry point is not in executable code.
    Manifest(ManifestError),
    /// The kernel expects a boot-info version this loader does not provide.
    BootInfoVersion {
        /// What the kernel's manifest asks for.
        wanted: (u16, u16),
        /// What this loader provides.
        provided: (u16, u16),
    },
    /// The kernel needs CPU features beyond x86-64-v3; their CPUID tests are not
    /// implemented yet, so the loader fails closed.
    CpuFeatures(u64),
    /// A section is compressed; the loader has no decompressor.
    Compressed(u16),
    /// A section's content does not match its SHA-256 digest or Merkle root.
    SectionDigest(u16),
    /// A MERKLE section is not the hash blocks of the section it serves.
    MerkleBlocks(u16),
}

impl fmt::Display for KernelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsigned => f.write_str(
                "unsigned image: a signature by an origin-kernel key is required outside developer mode",
            ),
            Self::Parse(e) => write!(f, "malformed image ({e:?})"),
            Self::NotKrn => f.write_str("not a KRN (Framekernel) image"),
            Self::ContentDigest => f.write_str("content digest (SHA-512) mismatch"),
            Self::Signature(e) => match e {
                KrnBlockError::Malformed(e) => write!(f, "malformed signature block ({e:?})"),
                KrnBlockError::EntryCount(n) => write!(
                    f,
                    "the signature block holds {n} entries; a KRN image has exactly one"
                ),
                KrnBlockError::Role(role) => {
                    write!(f, "the signature's role ({role:?}) is not origin-kernel")
                }
                KrnBlockError::ChainNotSupported => f.write_str(
                    "the signature carries a certificate chain, which this loader cannot verify yet",
                ),
                KrnBlockError::UntrustedKey => {
                    f.write_str("signed by a key this loader does not trust")
                }
                KrnBlockError::Entry(VerifyError::Digest) => f.write_str(
                    "the signed statement does not name this image (its content digest differs)",
                ),
                KrnBlockError::Entry(VerifyError::Profile) => f.write_str(
                    "the signed statement does not name this image (its profile is not KRN)",
                ),
                KrnBlockError::Entry(VerifyError::Ed25519(e)) => write!(
                    f,
                    "the Ed25519 signature does not verify over the statement ({e:?}): the signature or the statement was altered"
                ),
                KrnBlockError::Entry(VerifyError::MlDsa(e)) => write!(
                    f,
                    "the ML-DSA signature does not verify over the statement ({e:?}): the signature or the statement was altered"
                ),
                KrnBlockError::Entry(VerifyError::KeyId | VerifyError::KeyLength) => {
                    f.write_str("a trust-store key does not fit this signature's suite")
                }
            },
            Self::ManifestDigest(e) => write!(f, "manifest digest mismatch ({e:?})"),
            Self::Manifest(e) => write!(f, "invalid KRN manifest ({e:?})"),
            Self::BootInfoVersion { wanted, provided } => write!(
                f,
                "kernel expects boot info {}.{}, loader provides {}.{}",
                wanted.0, wanted.1, provided.0, provided.1
            ),
            Self::CpuFeatures(bits) => write!(
                f,
                "kernel requires CPU features {bits:#x}; CPUID checks not implemented"
            ),
            Self::Compressed(i) => write!(f, "section {i} is compressed"),
            Self::SectionDigest(i) => write!(f, "section {i} does not match its digest"),
            Self::MerkleBlocks(i) => write!(
                f,
                "section {i} (MERKLE) is not the hash blocks of the section it serves"
            ),
        }
    }
}

/// A KRN image that passed every check in ADR-0002 B2.
#[derive(Clone, Copy, Debug)]
pub struct Verified<'a> {
    /// The parsed container.
    pub image: Image<'a>,
    /// The decoded manifest.
    pub manifest: KrnManifest<'a>,
    /// The key ID of the verified signer, or `None` for an unsigned image, which only a
    /// developer-mode loader accepts.
    pub signer: Option<[u8; 32]>,
}

fn sha512(parts: &[&[u8]; 3]) -> [u8; 64] {
    let mut h = Sha512::new();
    for p in parts {
        h.update(p);
    }
    h.finalize()
}

/// Runs the full verification chain on `bytes` (ADR-0002 B2). `developer_mode` allows an
/// unsigned image; a signed one is verified in every mode (errata V5). `store` holds the
/// keys this loader trusts ([`crate::trust::STORE`]).
pub fn verify<'a>(
    bytes: &'a [u8],
    developer_mode: bool,
    store: &[TrustedKey<'_>],
) -> Result<Verified<'a>, KernelError> {
    let options = ParseOptions {
        developer_mode,
        ..ParseOptions::default()
    };
    let image = Image::parse(bytes, options).map_err(|e| match e {
        FormatError::UnsignedNotAllowed => KernelError::Unsigned,
        other => KernelError::Parse(other),
    })?;
    if image.header().profile != Profile::Krn {
        return Err(KernelError::NotKrn);
    }
    image
        .verify_content_digest(sha512)
        .map_err(|_| KernelError::ContentDigest)?;
    let signer = check_signature(&image, developer_mode, store)?;

    let manifest_section = image
        .manifest()
        .ok_or(KernelError::Parse(FormatError::ManifestCount))?;
    manifest_section
        .verify_sha256(Sha256::digest)
        .map_err(KernelError::ManifestDigest)?;
    let manifest = KrnManifest::decode(manifest_section.stored).map_err(KernelError::Manifest)?;
    manifest
        .check_against(&image)
        .map_err(KernelError::Manifest)?;
    if manifest.boot_info_abi != origin_boot_info::VERSION {
        return Err(KernelError::BootInfoVersion {
            wanted: manifest.boot_info_abi,
            provided: origin_boot_info::VERSION,
        });
    }
    if manifest.cpu_features.0 != 0 {
        return Err(KernelError::CpuFeatures(manifest.cpu_features.0));
    }

    check_sections(&image)?;
    Ok(Verified {
        image,
        manifest,
        signer,
    })
}

/// Checks every section against its digest, so that every byte of the image is verified
/// before any of it is used (ONBF §4.5). A MERKLE section is not hashed on its own: it is
/// compared with the hash blocks of the section it serves, which `parse` has shown exists and
/// is unique (errata C9), in the pass that hashes that section.
fn check_sections(image: &Image<'_>) -> Result<(), KernelError> {
    for s in image.sections() {
        if s.entry.kind() == Kind::Merkle {
            continue;
        }
        if s.entry.compression != Compression::None {
            return Err(KernelError::Compressed(s.index));
        }
        match s.entry.integrity {
            Integrity::Sha256 if Sha256::digest(s.stored) == s.entry.digest => {}
            Integrity::Merkle if s.entry.merkle_index != NO_MERKLE => check_tree(image, &s)?,
            Integrity::Merkle if onbf_merkle::root(s.stored).is_ok_and(|r| r == s.entry.digest) => {
            }
            _ => return Err(KernelError::SectionDigest(s.index)),
        }
    }
    Ok(())
}

/// Checks section `s`, whose tree has hash blocks, and its MERKLE section together.
fn check_tree(image: &Image<'_>, s: &Section<'_>) -> Result<(), KernelError> {
    let merkle = usize::try_from(s.entry.merkle_index)
        .ok()
        .and_then(|i| image.section(i))
        .ok_or(KernelError::SectionDigest(s.index))?;
    onbf_merkle::verify_tree(&s.entry.digest, s.stored, merkle.stored).map_err(|e| match e {
        onbf_merkle::Error::HashMismatch { .. } => KernelError::MerkleBlocks(merkle.index),
        _ => KernelError::SectionDigest(s.index),
    })
}

/// B2 step 3: the signature block against the trust store. The digest it checks is the
/// header's, which step 2 has just proven to be the image's. Nothing of the kernel beyond
/// the header and the section table has been read before this returns. An image with no
/// block is accepted in developer mode only (ADR-0002 B8).
fn check_signature(
    image: &Image<'_>,
    developer_mode: bool,
    store: &[TrustedKey<'_>],
) -> Result<Option<[u8; 32]>, KernelError> {
    match image.signature_block() {
        None if developer_mode => Ok(None),
        None => Err(KernelError::Unsigned),
        Some(block) => verify_krn_block(block, store, &image.header().content_digest)
            .map(Some)
            .map_err(KernelError::Signature),
    }
}

#[cfg(test)]
mod tests;
