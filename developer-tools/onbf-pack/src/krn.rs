//! ELF64 PIE → KRN image (profile 6; ONBF §6.1.2, §6.5), and the in-process verifier.
//!
//! Section table, in order:
//! 1. `[0]` MANIFEST: OCBOR-1, integrity 0 (SHA-256), never compressed (errata C12);
//! 2. one SEGMENT per `PT_LOAD`, ascending RVA: MAPPABLE plus R, R+X or R+W; `mem_offset =
//!    p_vaddr`; `mem_size = p_memsz` rounded up to 4 KiB (G4); content = the `p_filesz`
//!    file bytes with the RELA addends written in; integrity 1 (Merkle);
//! 3. RELOCS, if there are relocations: RELR words (ONBF §6.1), integrity 0. Omitted when
//!    there are none, because an empty section is not allowed (C14);
//! 4. one MERKLE section per segment whose tree has hash blocks, in segment order. A
//!    segment of at most 4 KiB has none and uses `merkle_index = NO_MERKLE` (C10).
//!
//! Every section has REQUIRED set (G5). The image is unsigned (developer mode); signatures
//! are added by a later tool.

use core::fmt;
use std::collections::BTreeMap;

use onbf_format::{
    Flags, Image, Kind, KrnManifest, ManifestError, NO_MERKLE, ParseOptions, Profile, Section,
};
use onbf_merkle::Shape;
use origin_crypto::{Sha256, Sha512};

use crate::container::{self, SectionSpec};
use crate::elf::{self, Elf, Permissions};
use crate::ocbor::Value;
use crate::relr::{self, RelrError};
use crate::toolchain::Toolchain;
use crate::{Error, LayoutError};

/// Section kinds this module writes (ONBF §4.4).
pub mod kind {
    /// MANIFEST.
    pub const MANIFEST: u32 = 0x0001;
    /// SEGMENT.
    pub const SEGMENT: u32 = 0x0010;
    /// RELOCS.
    pub const RELOCS: u32 = 0x0011;
    /// MERKLE.
    pub const MERKLE: u32 = 0x00F0;
}

const INTEGRITY_SHA256: u8 = 0;
const INTEGRITY_MERKLE: u8 = 1;
const INTEGRITY_HASH_BLOCKS: u8 = 2;

/// Every CPU feature bit the registry defines (`docs/spec/ONBF-manifest-keys.md`, K3).
pub const KNOWN_CPU_FEATURES: u64 = onbf_format::CpuFeatures::KNOWN;

/// What the caller supplies besides the ELF file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KrnOptions {
    /// Required CPU features beyond `x86-64-v3` (manifest key 3). Only registry bits
    /// (`0x3F`) are accepted.
    pub cpu_features: u64,
    /// The canonical compiler and linker flags. `toolchain.flags_digest` is the SHA-256 of
    /// exactly these UTF-8 bytes; the build system is responsible for the canonical form.
    pub flags: String,
    /// Toolchain provenance (manifest key 2).
    pub toolchain: Toolchain,
}

/// A packed and verified image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packed {
    /// The complete image.
    pub bytes: Vec<u8>,
    /// Number of sections.
    pub sections: usize,
    /// Number of SEGMENT sections.
    pub segments: usize,
    /// Number of relocation slots.
    pub relocations: usize,
}

/// Why a finished image failed verification. For an image `onbf-pack` itself wrote, any
/// of these is a bug in the packer, and nothing is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerifyError {
    /// `onbf-format` rejects the container, the content digest or a section digest.
    Container(onbf_format::Error),
    /// The profile is not KRN.
    NotKrn,
    /// The section order is not MANIFEST, SEGMENT…, RELOCS, MERKLE….
    Layout(&'static str),
    /// A section kind `onbf-pack` does not write.
    UnexpectedSection {
        /// Table index.
        index: u16,
        /// `kind`.
        kind: u32,
    },
    /// `onbf-format` rejects the manifest.
    Manifest(ManifestError),
    /// `onbf-merkle` cannot hash a segment.
    Merkle(onbf_merkle::Error),
    /// A segment's Merkle root differs from its entry's digest.
    SegmentRoot {
        /// Table index.
        index: u16,
    },
    /// A MERKLE section differs from the hash blocks of the segment it serves.
    MerkleBlocks {
        /// Table index of the segment.
        index: u16,
    },
    /// The manifest's `build_id` is not the SHA-256 of the segment contents.
    BuildId,
    /// RELOCS does not decode.
    Relocs(RelrError),
    /// RELOCS is not the canonical (greedy) RELR encoding of its slots.
    RelocsNotCanonical,
    /// A RELOCS slot is not 8-byte aligned inside the content of a writable segment.
    RelocationSlot {
        /// The slot's RVA.
        slot: u64,
    },
    /// The image is valid but differs from what the packer meant to write.
    Mismatch(&'static str),
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Container(e) => write!(f, "onbf-format rejects the image: {e:?}"),
            Self::NotKrn => f.write_str("the profile is not KRN"),
            Self::Layout(why) => write!(f, "section layout: {why}"),
            Self::UnexpectedSection { index, kind } => {
                write!(
                    f,
                    "section {index} has kind {kind:#x}, which onbf-pack does not write"
                )
            }
            Self::Manifest(e) => write!(f, "onbf-format rejects the manifest: {e:?}"),
            Self::Merkle(e) => write!(f, "onbf-merkle: {e:?}"),
            Self::SegmentRoot { index } => {
                write!(
                    f,
                    "section {index}: the Merkle root does not match the digest"
                )
            }
            Self::MerkleBlocks { index } => {
                write!(
                    f,
                    "section {index}: the MERKLE section does not match its content"
                )
            }
            Self::BuildId => f.write_str("build_id is not the SHA-256 of the segments"),
            Self::Relocs(e) => write!(f, "RELOCS: {e}"),
            Self::RelocsNotCanonical => f.write_str("RELOCS is not the canonical RELR encoding"),
            Self::RelocationSlot { slot } => write!(
                f,
                "RELOCS slot {slot:#x} is not inside the content of a writable segment"
            ),
            Self::Mismatch(what) => write!(f, "the verified image differs from the input: {what}"),
        }
    }
}

impl std::error::Error for VerifyError {}

/// One SEGMENT of a verified image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedSegment {
    /// Table index.
    pub index: u16,
    /// `mem_offset`.
    pub rva: u64,
    /// `mem_size`.
    pub mem_size: u64,
    /// Section flags.
    pub flags: u32,
    /// The content (verified against its Merkle root).
    pub content: Vec<u8>,
}

/// A KRN image that passed [`verify`], with its decoded manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedKrn {
    /// Number of sections.
    pub sections: usize,
    /// Entry point RVA (key 5).
    pub entry: u64,
    /// Build ID (key 1).
    pub build_id: [u8; 32],
    /// Toolchain text fields (key 2, sub-keys 0–2).
    pub toolchain: Toolchain,
    /// `toolchain.flags_digest` (key 2, sub-key 3).
    pub flags_digest: [u8; 32],
    /// Required CPU features (key 3).
    pub cpu_features: u64,
    /// `cet.shstk` (key 4, sub-key 0).
    pub shadow_stack: bool,
    /// Boot-info ABI (key 24).
    pub boot_info_abi: (u16, u16),
    /// Segments in table order.
    pub segments: Vec<VerifiedSegment>,
    /// Relocation slots from RELOCS, ascending.
    pub relocations: Vec<u64>,
}

fn sha512_parts(parts: &[&[u8]; 3]) -> [u8; 64] {
    let mut h = Sha512::new();
    for part in parts {
        h.update(part);
    }
    h.finalize()
}

/// The section flags for a segment's permissions.
fn segment_flags(permissions: Permissions) -> u32 {
    let base = Flags::REQUIRED | Flags::MAPPABLE | Flags::READ;
    match permissions {
        Permissions::Read => base,
        Permissions::ReadExecute => base | Flags::EXECUTE,
        Permissions::ReadWrite => base | Flags::WRITE,
    }
}

/// The KRN manifest (`docs/spec/ONBF-manifest-keys.md`).
fn manifest(build_id: [u8; 32], entry: u64, options: &KrnOptions) -> Value {
    let toolchain = BTreeMap::from([
        (0, Value::Text(options.toolchain.rustc.clone())),
        (1, Value::Text(options.toolchain.rustc_commit.clone())),
        (2, Value::Text(options.toolchain.llvm.clone())),
        (
            3,
            Value::Bytes(Sha256::digest(options.flags.as_bytes()).to_vec()),
        ),
    ]);
    let cet = BTreeMap::from([(0, Value::Bool(true)), (1, Value::Bool(false))]);
    Value::Map(BTreeMap::from([
        (0, Value::Unsigned(1)),
        (1, Value::Bytes(build_id.to_vec())),
        (2, Value::Map(toolchain)),
        (3, Value::Unsigned(options.cpu_features)),
        (4, Value::Map(cet)),
        (5, Value::Unsigned(entry)),
        (
            24,
            Value::Array(vec![
                Value::Unsigned(u64::from(origin_version::BOOT_INFO_MAJOR)),
                Value::Unsigned(u64::from(origin_version::BOOT_INFO_MINOR)),
            ]),
        ),
    ]))
}

/// Segment contents: the file bytes of each `PT_LOAD`, with each RELA addend written into
/// its slot as a little-endian `u64` (the implicit-addend form RELR needs).
///
/// The parser has already placed every segment inside the file and every slot in the
/// file-backed bytes of an RW segment, so the error paths here are unreachable in practice.
fn segment_contents(file: &[u8], elf: &Elf) -> Result<Vec<Vec<u8>>, Error> {
    let truncated = || Error::Elf(elf::ElfError::Truncated("file range of a PT_LOAD"));
    let mut contents = Vec::with_capacity(elf.segments.len());
    for s in &elf.segments {
        let start = usize::try_from(s.file_offset).map_err(|_| truncated())?;
        let len = usize::try_from(s.file_size).map_err(|_| truncated())?;
        let end = start.checked_add(len).ok_or_else(truncated)?;
        contents.push(file.get(start..end).ok_or_else(truncated)?.to_vec());
    }
    let misplaced = || Error::Verify(VerifyError::Mismatch("relocation slot outside its segment"));
    for r in &elf.relocations {
        let Some(addend) = r.addend else { continue };
        let i = elf.segment_containing(r.slot).ok_or_else(misplaced)?;
        let base = elf.segments.get(i).map(|s| s.vaddr).ok_or_else(misplaced)?;
        let at = r
            .slot
            .checked_sub(base)
            .and_then(|d| usize::try_from(d).ok())
            .ok_or_else(misplaced)?;
        let end = at.checked_add(8).ok_or_else(misplaced)?;
        contents
            .get_mut(i)
            .and_then(|c| c.get_mut(at..end))
            .ok_or_else(misplaced)?
            .copy_from_slice(&addend.to_le_bytes());
    }
    Ok(contents)
}

/// Converts an ELF64 x86-64 PIE into a KRN image, verifies the result in-process, and
/// returns it. Nothing is returned on any error (no partial output).
pub fn pack(file: &[u8], options: &KrnOptions) -> Result<Packed, Error> {
    if options.cpu_features & !KNOWN_CPU_FEATURES != 0 {
        return Err(Error::CpuFeatures(
            options.cpu_features & !KNOWN_CPU_FEATURES,
        ));
    }
    options.toolchain.validate().map_err(Error::Toolchain)?;
    let elf = elf::parse(file).map_err(Error::Elf)?;
    let contents = segment_contents(file, &elf)?;
    let slots: Vec<u64> = elf.relocations.iter().map(|r| r.slot).collect();
    let words = relr::encode(&slots).map_err(|e| Error::Layout(LayoutError::Relr(e)))?;

    let mut build_id = Sha256::new();
    for c in &contents {
        build_id.update(c);
    }
    let manifest_bytes = manifest(build_id.finalize(), elf.entry, options).encode();

    // Table indices: MANIFEST, segments, RELOCS (if any), then the MERKLE sections.
    let segment_count = contents.len();
    let has_relocs = !words.is_empty();
    let mut next_merkle = segment_count
        .checked_add(1)
        .and_then(|n| n.checked_add(usize::from(has_relocs)))
        .ok_or(Error::Layout(LayoutError::TooManySections))?;
    // Non-segment sections have mem_offset 0 and mem_size = content_size (errata C7).
    let byte_len =
        |b: &[u8]| u64::try_from(b.len()).map_err(|_| Error::Layout(LayoutError::TooLarge));
    let mut sections = vec![SectionSpec {
        kind: kind::MANIFEST,
        flags: Flags::REQUIRED,
        digest: Sha256::digest(&manifest_bytes),
        mem_size: byte_len(&manifest_bytes)?,
        content: manifest_bytes,
        mem_offset: 0,
        integrity: INTEGRITY_SHA256,
        merkle_index: NO_MERKLE,
    }];
    let mut merkle_sections = Vec::new();
    for (segment, content) in elf.segments.iter().zip(contents) {
        let merkle_err = |e| Error::Layout(LayoutError::Merkle(e));
        let shape = Shape::new(byte_len(&content)?).map_err(merkle_err)?;
        let (root, merkle_index) = if shape.hash_blocks() == 0 {
            (onbf_merkle::root(&content).map_err(merkle_err)?, NO_MERKLE)
        } else {
            let size = usize::try_from(shape.hash_blocks_len())
                .map_err(|_| Error::Layout(LayoutError::TooLarge))?;
            let mut blocks = vec![0u8; size];
            let root = onbf_merkle::build(&content, &mut blocks).map_err(merkle_err)?;
            merkle_sections.push(SectionSpec {
                kind: kind::MERKLE,
                flags: Flags::REQUIRED,
                content: blocks,
                mem_offset: 0,
                mem_size: shape.hash_blocks_len(),
                integrity: INTEGRITY_HASH_BLOCKS,
                merkle_index: NO_MERKLE,
                digest: [0; 32],
            });
            let index = u32::try_from(next_merkle)
                .map_err(|_| Error::Layout(LayoutError::TooManySections))?;
            next_merkle = next_merkle
                .checked_add(1)
                .ok_or(Error::Layout(LayoutError::TooManySections))?;
            (root, index)
        };
        sections.push(SectionSpec {
            kind: kind::SEGMENT,
            flags: segment_flags(segment.permissions),
            content,
            mem_offset: segment.vaddr,
            mem_size: segment.mapped_size,
            integrity: INTEGRITY_MERKLE,
            merkle_index,
            digest: root,
        });
    }
    if has_relocs {
        let bytes = relr::words_to_bytes(&words);
        sections.push(SectionSpec {
            kind: kind::RELOCS,
            flags: Flags::REQUIRED,
            digest: Sha256::digest(&bytes),
            mem_size: byte_len(&bytes)?,
            content: bytes,
            mem_offset: 0,
            integrity: INTEGRITY_SHA256,
            merkle_index: NO_MERKLE,
        });
    }
    sections.append(&mut merkle_sections);

    let bytes = container::write(Profile::Krn, &sections).map_err(Error::Layout)?;
    let verified = verify(&bytes).map_err(Error::Verify)?;
    check_matches(&verified, &elf, &sections, &slots, options).map_err(Error::Verify)?;
    Ok(Packed {
        bytes,
        sections: sections.len(),
        segments: segment_count,
        relocations: slots.len(),
    })
}

/// Checks that the verified image says exactly what the packer meant to write.
fn check_matches(
    v: &VerifiedKrn,
    elf: &Elf,
    sections: &[SectionSpec],
    slots: &[u64],
    options: &KrnOptions,
) -> Result<(), VerifyError> {
    let mismatch = VerifyError::Mismatch;
    if v.entry != elf.entry {
        return Err(mismatch("entry"));
    }
    if v.relocations != slots {
        return Err(mismatch("relocation slots"));
    }
    if v.cpu_features != options.cpu_features || v.toolchain != options.toolchain {
        return Err(mismatch("manifest options"));
    }
    if v.flags_digest != Sha256::digest(options.flags.as_bytes()) {
        return Err(mismatch("flags digest"));
    }
    let specs = sections.iter().filter(|s| s.kind == kind::SEGMENT);
    if v.segments.len() != elf.segments.len() || specs.clone().count() != v.segments.len() {
        return Err(mismatch("segment count"));
    }
    for ((got, want), elf_segment) in v.segments.iter().zip(specs).zip(&elf.segments) {
        let same = got.rva == elf_segment.vaddr
            && got.mem_size == elf_segment.mapped_size
            && got.flags == segment_flags(elf_segment.permissions)
            && got.content == want.content;
        if !same {
            return Err(mismatch("segment contents or placement"));
        }
    }
    Ok(())
}

/// Checks the section order `onbf-pack` writes: MANIFEST, then at least one SEGMENT, then
/// RELOCS (optional), then MERKLE sections in the order of the segments they serve.
fn check_order(sections: &[Section<'_>]) -> Result<(), VerifyError> {
    // Phases: 0 MANIFEST, 1 SEGMENT, 2 RELOCS, 3 MERKLE. Only 1 and 3 repeat.
    let mut phase: Option<u8> = None;
    let mut last_merkle: Option<u32> = None;
    for s in sections {
        let p = match s.entry.kind() {
            Kind::Manifest => 0,
            Kind::Segment => 1,
            Kind::Relocs => 2,
            Kind::Merkle => 3,
            _ => {
                return Err(VerifyError::UnexpectedSection {
                    index: s.index,
                    kind: s.entry.kind,
                });
            }
        };
        let in_order = match phase {
            None => p == 0,
            // RELOCS and MERKLE need a SEGMENT before them.
            Some(prev) => (p > prev && (prev >= 1 || p == 1)) || (p == prev && (p == 1 || p == 3)),
        };
        if !in_order {
            return Err(VerifyError::Layout(
                "sections must be MANIFEST, SEGMENT..., RELOCS, MERKLE...",
            ));
        }
        phase = Some(p);
        if p == 1 && s.entry.merkle_index != NO_MERKLE {
            if last_merkle.is_some_and(|m| s.entry.merkle_index <= m) {
                return Err(VerifyError::Layout(
                    "MERKLE sections must follow the order of their segments",
                ));
            }
            last_merkle = Some(s.entry.merkle_index);
        }
    }
    if phase.is_none_or(|p| p == 0) {
        return Err(VerifyError::Layout(
            "a KRN image needs at least one SEGMENT",
        ));
    }
    Ok(())
}

/// True if `[slot, slot + 8)` lies 8-byte aligned inside the content of a writable segment.
fn writable_slot(segments: &[VerifiedSegment], slot: u64) -> bool {
    slot & 7 == 0
        && segments.iter().any(|s| {
            let end = u64::try_from(s.content.len())
                .ok()
                .and_then(|len| s.rva.checked_add(len));
            s.flags & Flags::WRITE != 0
                && slot >= s.rva
                && slot
                    .checked_add(8)
                    .zip(end)
                    .is_some_and(|(slot_end, end)| slot_end <= end)
        })
}

/// Verifies a KRN image as the boot loader will, plus the layout rules of `onbf-pack`:
/// - `Image::parse` in developer mode (the image is unsigned);
/// - the content digest (SHA-512);
/// - the manifest's SHA-256, `KrnManifest::decode` and `check_against`;
/// - every segment's Merkle root, its MERKLE section, and the `build_id`;
/// - the RELOCS SHA-256, its decoding, its canonical encoding, and every slot.
pub fn verify(bytes: &[u8]) -> Result<VerifiedKrn, VerifyError> {
    use VerifyError as V;
    let options = ParseOptions {
        developer_mode: true,
        ..ParseOptions::default()
    };
    let image = Image::parse(bytes, options).map_err(V::Container)?;
    image
        .verify_content_digest(sha512_parts)
        .map_err(V::Container)?;
    if image.header().profile != Profile::Krn {
        return Err(V::NotKrn);
    }
    let sections: Vec<Section<'_>> = image.sections().collect();
    check_order(&sections)?;

    let manifest_section = image.manifest().ok_or(V::Layout("no MANIFEST"))?;
    manifest_section
        .verify_sha256(Sha256::digest)
        .map_err(V::Container)?;
    let manifest = KrnManifest::decode(manifest_section.stored).map_err(V::Manifest)?;
    manifest.check_against(&image).map_err(V::Manifest)?;

    let mut build_id = Sha256::new();
    let mut segments = Vec::new();
    for s in sections.iter().filter(|s| s.entry.kind() == Kind::Segment) {
        let root = onbf_merkle::root(s.stored).map_err(V::Merkle)?;
        if root != s.entry.digest {
            return Err(V::SegmentRoot { index: s.index });
        }
        if s.entry.merkle_index != NO_MERKLE {
            let merkle = usize::try_from(s.entry.merkle_index)
                .ok()
                .and_then(|i| image.section(i))
                .ok_or(V::MerkleBlocks { index: s.index })?;
            let shape = Shape::new(s.entry.content_size).map_err(V::Merkle)?;
            let len = usize::try_from(shape.hash_blocks_len())
                .map_err(|_| V::MerkleBlocks { index: s.index })?;
            let mut expected = vec![0u8; len];
            onbf_merkle::build(s.stored, &mut expected).map_err(V::Merkle)?;
            if expected != merkle.stored {
                return Err(V::MerkleBlocks { index: s.index });
            }
        }
        build_id.update(s.stored);
        segments.push(VerifiedSegment {
            index: s.index,
            rva: s.entry.mem_offset,
            mem_size: s.entry.mem_size,
            flags: s.entry.flags.0,
            content: s.stored.to_vec(),
        });
    }
    if build_id.finalize() != manifest.build_id {
        return Err(V::BuildId);
    }

    let relocations = match sections.iter().find(|s| s.entry.kind() == Kind::Relocs) {
        None => Vec::new(),
        Some(r) => {
            r.verify_sha256(Sha256::digest).map_err(V::Container)?;
            let words = relr::words_from_bytes(r.stored).map_err(V::Relocs)?;
            let slots = relr::decode(&words).map_err(V::Relocs)?;
            if relr::encode(&slots).map_err(V::Relocs)? != words {
                return Err(V::RelocsNotCanonical);
            }
            if let Some(&slot) = slots.iter().find(|&&s| !writable_slot(&segments, s)) {
                return Err(V::RelocationSlot { slot });
            }
            slots
        }
    };

    let t = manifest.toolchain;
    Ok(VerifiedKrn {
        sections: sections.len(),
        entry: manifest.entry,
        build_id: manifest.build_id,
        toolchain: Toolchain {
            rustc: t.rustc.to_string(),
            rustc_commit: t.rustc_commit.to_string(),
            llvm: t.llvm.to_string(),
        },
        flags_digest: t.flags_digest,
        cpu_features: manifest.cpu_features.0,
        shadow_stack: manifest.cet.shstk,
        boot_info_abi: manifest.boot_info_abi,
        segments,
        relocations,
    })
}
