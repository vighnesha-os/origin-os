//! Section entries (§4.3) and section kinds (§4.4).

use crate::error::SectionError;
use crate::header::Profile;
use crate::read::{Reader, all_zero};

/// Mapped sections start on 4 KiB boundaries; so does the first section (§4.1).
pub const PAGE_SIZE: u64 = 4096;
/// Alignment of sections that are not MAPPABLE (§4.3).
pub const SMALL_ALIGN: u64 = 8;
/// Largest `stored_size` or `content_size` (Appendix A).
pub const MAX_SECTION_CONTENT: u64 = 1 << 40;
/// Largest manifest (§5, Appendix A).
pub const MAX_MANIFEST: u64 = 1 << 20;
/// Largest image span: the end of the highest segment, as an RVA (Appendix A; errata G4).
pub const MAX_IMAGE_SPAN: u64 = 1 << 31;
/// Most SEGMENT sections in an EXE or LIB image (Appendix A).
pub const MAX_SEGMENTS_EXE_LIB: u32 = 16;
/// `merkle_index` value when the section has no MERKLE section.
pub const NO_MERKLE: u32 = 0xFFFF_FFFF;
/// Hashes per 4 KiB hash block (§4.6).
const HASHES_PER_BLOCK: u64 = 128;

/// Section flag bits (§4.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Flags(pub u32);

impl Flags {
    /// The reader must understand this section.
    pub const REQUIRED: u32 = 1 << 0;
    /// The section is mapped into memory.
    pub const MAPPABLE: u32 = 1 << 1;
    /// Mapped readable.
    pub const READ: u32 = 1 << 2;
    /// Mapped writable.
    pub const WRITE: u32 = 1 << 3;
    /// Mapped executable.
    pub const EXECUTE: u32 = 1 << 4;
    /// Read-only after relocation.
    pub const RELRO: u32 = 1 << 5;
    const DEFINED: u32 = 0x3F;
    const PERMISSIONS: u32 = Self::READ | Self::WRITE | Self::EXECUTE | Self::RELRO;
    const READ_ONLY: u32 = Self::READ;
    const READ_WRITE: u32 = Self::READ | Self::WRITE;
    const READ_WRITE_RELRO: u32 = Self::READ_WRITE | Self::RELRO;
    const READ_EXECUTE: u32 = Self::READ | Self::EXECUTE;

    /// True if every bit in `bits` is set.
    pub const fn has(self, bits: u32) -> bool {
        self.0 & bits == bits
    }
}

/// A section kind from §4.4.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// OCBOR-1 manifest; exactly one per image.
    Manifest,
    /// Localized string tables.
    Strings,
    /// Install-screen assets.
    Resources,
    /// A mapped segment.
    Segment,
    /// Relative relocations.
    Relocs,
    /// Symbol imports.
    Imports,
    /// Sorted export table.
    Exports,
    /// Thread-local template.
    Tls,
    /// Unwind tables.
    Unwind,
    /// Read-only file tree.
    ImageFs,
    /// One nested ONBF image.
    Payload,
    /// One device-firmware blob.
    Firmware,
    /// Merkle hash blocks.
    Merkle,
    /// Build ID and symbol-file reference.
    DebugLink,
    /// One DWARF section of a detached symbol file (SYM only; errata G1). The SYM
    /// manifest's `dwarf_sections` key names which DWARF section each one is.
    Dwarf,
    /// A kind this version does not know. Allowed only without REQUIRED.
    Unknown(u32),
}

const EXE: u16 = Profile::Exe.bit();
const LIB: u16 = Profile::Lib.bit();
const APP: u16 = Profile::App.bit();
const PKG: u16 = Profile::Pkg.bit();
const DRV: u16 = Profile::Drv.bit();
const KRN: u16 = Profile::Krn.bit();
const SYM: u16 = Profile::Sym.bit();
const ALL: u16 = 0xFFFF;

impl Kind {
    /// The kind for a `kind` field value.
    pub const fn from_raw(raw: u32) -> Self {
        match raw {
            0x0001 => Self::Manifest,
            0x0002 => Self::Strings,
            0x0003 => Self::Resources,
            0x0010 => Self::Segment,
            0x0011 => Self::Relocs,
            0x0012 => Self::Imports,
            0x0013 => Self::Exports,
            0x0014 => Self::Tls,
            0x0015 => Self::Unwind,
            0x0020 => Self::ImageFs,
            0x0030 => Self::Payload,
            0x0040 => Self::Firmware,
            0x00F0 => Self::Merkle,
            0x00F1 => Self::DebugLink,
            0x00F2 => Self::Dwarf,
            other => Self::Unknown(other),
        }
    }

    /// The profiles §4.4 allows this kind in, as a bit set; `None` for unknown kinds.
    const fn profiles(self) -> Option<u16> {
        Some(match self {
            Self::Manifest | Self::Merkle => ALL,
            Self::Strings | Self::Resources => EXE | APP | PKG | DRV,
            Self::Segment | Self::Relocs | Self::Unwind | Self::DebugLink => EXE | LIB | KRN,
            Self::Imports | Self::Tls => EXE | LIB,
            Self::Exports => LIB | KRN,
            Self::ImageFs => APP | DRV,
            Self::Payload => PKG,
            Self::Firmware => DRV,
            Self::Dwarf => SYM,
            Self::Unknown(_) => return None,
        })
    }
}

/// How a section's content is stored (§4.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compression {
    /// Stored as-is.
    None,
    /// Zstandard (RFC 8878).
    Zstd,
}

/// How a section's content is protected (§4.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Integrity {
    /// `digest` is the SHA-256 of the content.
    Sha256 = 0,
    /// `digest` is a Merkle-SHA256-4K root; the hash blocks are in section `merkle_index`.
    Merkle = 1,
    /// The section holds Merkle hash blocks for another section.
    MerkleHashBlocks = 2,
    /// Covered by the parent image (nested images only).
    CoveredByParent = 3,
}

/// A decoded section entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SectionEntry {
    /// Raw `kind` field.
    pub kind: u32,
    /// Flag bits.
    pub flags: Flags,
    /// Offset of the stored bytes in the file.
    pub file_offset: u64,
    /// Bytes in the file.
    pub stored_size: u64,
    /// Bytes after decompression.
    pub content_size: u64,
    /// Segment RVA; 0 for non-segments.
    pub mem_offset: u64,
    /// Size in memory; the tail beyond `content_size` is zero-filled.
    pub mem_size: u64,
    /// Storage encoding.
    pub compression: Compression,
    /// Protection mode.
    pub integrity: Integrity,
    /// Index of the MERKLE section, or [`NO_MERKLE`].
    pub merkle_index: u32,
    /// SHA-256 digest or Merkle root.
    pub digest: [u8; 32],
}

impl SectionEntry {
    /// The entry's kind.
    pub const fn kind(&self) -> Kind {
        Kind::from_raw(self.kind)
    }

    /// Required alignment of `file_offset`.
    pub const fn file_alignment(&self) -> u64 {
        if self.flags.has(Flags::MAPPABLE) {
            PAGE_SIZE
        } else {
            SMALL_ALIGN
        }
    }

    /// Decodes one 128-byte entry and checks the rules that depend only on the entry,
    /// the profile and whether the image is nested. Layout and cross-references are
    /// checked by the image parser.
    pub(crate) fn parse(
        raw: &[u8; 128],
        profile: Profile,
        nested: bool,
    ) -> Result<Self, SectionError> {
        let mut r = Reader::new(raw);
        // `raw` is exactly 128 bytes, the sum of the field sizes, so no read can fail;
        // `Reserved` is returned only to avoid a panic path.
        let kind = r.u32().ok_or(SectionError::Reserved)?;
        let flags = Flags(r.u32().ok_or(SectionError::Reserved)?);
        let file_offset = r.u64().ok_or(SectionError::Reserved)?;
        let stored_size = r.u64().ok_or(SectionError::Reserved)?;
        let content_size = r.u64().ok_or(SectionError::Reserved)?;
        let mem_offset = r.u64().ok_or(SectionError::Reserved)?;
        let mem_size = r.u64().ok_or(SectionError::Reserved)?;
        let compression = r.u8().ok_or(SectionError::Reserved)?;
        let integrity = r.u8().ok_or(SectionError::Reserved)?;
        let reserved_a = r.array::<2>().ok_or(SectionError::Reserved)?;
        let merkle_index = r.u32().ok_or(SectionError::Reserved)?;
        let reserved_b = r.array::<8>().ok_or(SectionError::Reserved)?;
        let digest = *r.array::<32>().ok_or(SectionError::Reserved)?;
        let reserved_c = r.array::<32>().ok_or(SectionError::Reserved)?;

        if !(all_zero(reserved_a) && all_zero(reserved_b) && all_zero(reserved_c)) {
            return Err(SectionError::Reserved);
        }

        // Flags.
        if flags.0 & !Flags::DEFINED != 0 {
            return Err(SectionError::UnknownFlags(flags.0 & !Flags::DEFINED));
        }
        if flags.has(Flags::WRITE | Flags::EXECUTE) {
            return Err(SectionError::WriteAndExecute);
        }
        let k = Kind::from_raw(kind);
        let is_segment = k == Kind::Segment;
        if !is_segment && flags.0 & Flags::PERMISSIONS != 0 {
            return Err(SectionError::PermissionsOnNonSegment);
        }
        if is_segment && !flags.has(Flags::MAPPABLE) {
            return Err(SectionError::SegmentNotMappable);
        }
        // §6.1: exactly one of R, RW (optionally RELRO) or RX.
        if is_segment
            && !matches!(
                flags.0 & Flags::PERMISSIONS,
                Flags::READ_ONLY | Flags::READ_WRITE | Flags::READ_WRITE_RELRO | Flags::READ_EXECUTE
            )
        {
            return Err(SectionError::SegmentPermissions(flags.0 & Flags::PERMISSIONS));
        }
        let mappable = flags.has(Flags::MAPPABLE);
        // Errata G1: symbol files are never mapped.
        if k == Kind::Dwarf && mappable {
            return Err(SectionError::MappableNotAllowed);
        }

        // Kind against profile. Unknown kinds are rejected whether or not REQUIRED is set
        // (errata C13, owner decision; §4.4 permits this, since skipping is only a MAY).
        match k.profiles() {
            None => return Err(SectionError::UnknownKind(kind)),
            Some(allowed) if allowed & profile.bit() == 0 => {
                return Err(SectionError::KindNotAllowedInProfile(kind));
            }
            Some(_) => {}
        }
        // Errata G5 (owner decision): every v1 section must be understood, so REQUIRED is
        // set on every section, which gives the bit one encoding.
        if !flags.has(Flags::REQUIRED) {
            return Err(SectionError::RequiredNotSet);
        }

        // Sizes and compression.
        let compression = match compression {
            0 => Compression::None,
            1 => Compression::Zstd,
            other => return Err(SectionError::UnknownCompression(other)),
        };
        // Mapped content and hash blocks need random access. A manifest is never compressed
        // (errata C12), so no decompressor runs before it is verified and the boot loader
        // needs no zstd.
        let no_compression = mappable || k == Kind::Merkle || k == Kind::Manifest;
        if compression == Compression::Zstd && no_compression {
            return Err(SectionError::CompressionNotAllowed);
        }
        if stored_size > MAX_SECTION_CONTENT || content_size > MAX_SECTION_CONTENT {
            return Err(SectionError::TooLarge);
        }
        if compression == Compression::None && content_size != stored_size {
            return Err(SectionError::SizeMismatch);
        }
        // Errata C14: "nothing" has one encoding, which is an absent section. Only a
        // SEGMENT may have no content, and then only as zero-filled memory (bss).
        if (content_size == 0 && !is_segment) || (is_segment && mem_size == 0) {
            return Err(SectionError::EmptySection);
        }

        // Integrity.
        let integrity = match integrity {
            0 => Integrity::Sha256,
            1 => Integrity::Merkle,
            2 => Integrity::MerkleHashBlocks,
            3 => Integrity::CoveredByParent,
            other => return Err(SectionError::UnknownIntegrity(other)),
        };
        let raw_integrity = integrity as u8;
        let allowed = match integrity {
            // A MAPPABLE section uses integrity 1 or 3 (errata C8), so hash blocks are never
            // mapped.
            Integrity::MerkleHashBlocks => k == Kind::Merkle && !mappable,
            _ if k == Kind::Merkle => false,
            Integrity::CoveredByParent => nested,
            Integrity::Sha256 => !mappable,
            Integrity::Merkle => k != Kind::Manifest,
        };
        if !allowed {
            return Err(SectionError::IntegrityNotAllowed(raw_integrity));
        }
        if integrity != Integrity::Merkle && merkle_index != NO_MERKLE {
            return Err(SectionError::MerkleIndex(merkle_index));
        }
        if matches!(
            integrity,
            Integrity::MerkleHashBlocks | Integrity::CoveredByParent
        ) && !all_zero(&digest)
        {
            return Err(SectionError::DigestNotZero);
        }

        // Manifest.
        if k == Kind::Manifest {
            let is_sha256 = integrity == Integrity::Sha256;
            if content_size > MAX_MANIFEST || !is_sha256 {
                return Err(SectionError::ManifestRules);
            }
        }

        // Memory placement. A SEGMENT's RVA and size are whole pages (errata G4), so one
        // memory layout has one encoding and the loader maps whole pages without rounding.
        let memory_ok = if is_segment {
            mem_offset % PAGE_SIZE == 0
                && mem_size % PAGE_SIZE == 0
                && mem_size >= content_size
                && mem_offset.checked_add(mem_size).is_some()
        } else {
            mem_offset == 0 && mem_size == content_size
        };
        if !memory_ok {
            return Err(SectionError::MemoryLayout);
        }

        Ok(Self {
            kind,
            flags,
            file_offset,
            stored_size,
            content_size,
            mem_offset,
            mem_size,
            compression,
            integrity,
            merkle_index,
            digest,
        })
    }
}

/// Number of 4 KiB hash blocks a MERKLE section holds for content of `content_size` bytes
/// (§4.6). Empty or single-block content has no hash blocks (`levels = 0`).
pub const fn merkle_hash_blocks(content_size: u64) -> u64 {
    let mut hashes = content_size.div_ceil(PAGE_SIZE);
    let mut total: u64 = 0;
    while hashes > 1 {
        let blocks = hashes.div_ceil(HASHES_PER_BLOCK);
        total = total.saturating_add(blocks);
        hashes = blocks;
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merkle_block_counts() {
        assert_eq!(merkle_hash_blocks(0), 0);
        assert_eq!(merkle_hash_blocks(1), 0);
        assert_eq!(merkle_hash_blocks(4096), 0);
        assert_eq!(merkle_hash_blocks(4097), 1); // 2 leaves -> 1 block
        assert_eq!(merkle_hash_blocks(128 * 4096), 1); // 128 leaves -> 1 block
        assert_eq!(merkle_hash_blocks(128 * 4096 + 1), 3); // 129 leaves -> 2 blocks -> 1 block
        // 1 TiB = 2^28 leaves -> 2^21 + 2^14 + 2^7 + 1 blocks.
        assert_eq!(
            merkle_hash_blocks(1 << 40),
            (1 << 21) + (1 << 14) + (1 << 7) + 1
        );
    }

    #[test]
    fn kind_table_matches_section_4_4() {
        assert_eq!(Kind::from_raw(0x10), Kind::Segment);
        assert_eq!(Kind::from_raw(0x99), Kind::Unknown(0x99));
        let in_profile = |k: Kind, p: Profile| k.profiles().is_some_and(|m| m & p.bit() != 0);
        assert!(in_profile(Kind::Manifest, Profile::Sym));
        assert!(in_profile(Kind::Segment, Profile::Krn));
        assert!(!in_profile(Kind::Segment, Profile::App));
        assert!(!in_profile(Kind::Imports, Profile::Krn));
        assert!(in_profile(Kind::Exports, Profile::Krn));
        assert!(!in_profile(Kind::Payload, Profile::Exe));
        assert!(in_profile(Kind::Firmware, Profile::Drv));
        // Errata G1: DWARF sections exist only in detached symbol files.
        assert_eq!(Kind::from_raw(0x00F2), Kind::Dwarf);
        assert!(in_profile(Kind::Dwarf, Profile::Sym));
        for p in [Profile::Exe, Profile::Lib, Profile::Krn, Profile::App] {
            assert!(!in_profile(Kind::Dwarf, p), "{p:?}");
        }
    }
}
