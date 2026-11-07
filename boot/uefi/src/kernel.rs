//! Verifying a KRN image before anything of it runs (ADR-0002 B2; ONBF-R1, §8.1).

use core::fmt;

use onbf_format::{
    Compression, Error as FormatError, Image, Integrity, Kind, KrnManifest, ManifestError,
    ParseOptions, Profile,
};
use origin_crypto::{Sha256, Sha512};

/// Why the loader refused a kernel image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KernelError {
    /// The container is malformed, or unsigned outside developer mode.
    Parse(FormatError),
    /// The image is not a KRN image.
    NotKrn,
    /// The image is signed, but signature verification is not implemented yet. Refused
    /// rather than accepted unverified (ADR-0002 B2 step 3).
    SignaturesUnsupported,
    /// The SHA-512 content digest does not match.
    ContentDigest,
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
}

impl fmt::Display for KernelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "malformed or unsigned image ({e:?})"),
            Self::NotKrn => f.write_str("not a KRN (Framekernel) image"),
            Self::SignaturesUnsupported => {
                f.write_str("signed image, but signature verification is not implemented yet")
            }
            Self::ContentDigest => f.write_str("content digest (SHA-512) mismatch"),
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
}

fn sha512(parts: &[&[u8]; 3]) -> [u8; 64] {
    let mut h = Sha512::new();
    for p in parts {
        h.update(p);
    }
    h.finalize()
}

/// Runs the full verification chain on `bytes`. `developer_mode` allows an unsigned image.
pub fn verify(bytes: &[u8], developer_mode: bool) -> Result<Verified<'_>, KernelError> {
    let options = ParseOptions {
        developer_mode,
        ..ParseOptions::default()
    };
    let image = Image::parse(bytes, options).map_err(KernelError::Parse)?;
    if image.header().profile != Profile::Krn {
        return Err(KernelError::NotKrn);
    }
    if image.signature_block().is_some() {
        return Err(KernelError::SignaturesUnsupported);
    }
    image
        .verify_content_digest(sha512)
        .map_err(|_| KernelError::ContentDigest)?;

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

    // Every byte that will be placed or used is checked against its section digest.
    // MERKLE sections only serve page-level verification later; they are not needed here.
    for s in image.sections() {
        if s.entry.kind() == Kind::Merkle {
            continue;
        }
        if s.entry.compression != Compression::None {
            return Err(KernelError::Compressed(s.index));
        }
        let ok = match s.entry.integrity {
            Integrity::Sha256 => Sha256::digest(s.stored) == s.entry.digest,
            Integrity::Merkle => onbf_merkle::root(s.stored).is_ok_and(|r| r == s.entry.digest),
            Integrity::MerkleHashBlocks | Integrity::CoveredByParent => false,
        };
        if !ok {
            return Err(KernelError::SectionDigest(s.index));
        }
    }
    Ok(Verified { image, manifest })
}
