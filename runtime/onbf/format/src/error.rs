//! Typed parse errors. Every rejection names the rule it enforces.

/// Why an image was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The input ends before a structure the header or table requires.
    Truncated,
    /// `magic` is not `89 4F 4E 42 46 0D 0A 1A` (§4.2).
    BadMagic,
    /// `format_major` is not 1 (§4.2).
    UnsupportedFormatMajor(u16),
    /// `profile` is not one of the IDs in §1.
    UnknownProfile(u16),
    /// `header_size` is not 128.
    BadHeaderSize(u16),
    /// `header_flags` has a bit set; all bits are reserved.
    HeaderFlags(u32),
    /// `section_count` is outside 1–256.
    SectionCount(u32),
    /// `section_table_offset` is not 128.
    SectionTableOffset(u64),
    /// `arch` is not 1, x86-64 (ONBF-R11).
    UnsupportedArch(u16),
    /// A reserved header field is not zero (ONBF-R6).
    HeaderReserved,
    /// `signature_block_size` exceeds 1 MiB, or is non-zero while the offset is zero
    /// (or zero while the offset is non-zero).
    SignatureBlockSize(u64),
    /// The signature block does not start at its canonical offset after the last section.
    SignatureBlockMisplaced {
        /// The canonical offset.
        expected: u64,
        /// The offset in the header.
        found: u64,
    },
    /// The image is unsigned, and the caller allowed neither developer mode nor a
    /// verified parent (§4.2).
    UnsignedNotAllowed,
    /// Bytes follow the last item of the file (ONBF-R6).
    TrailingData,
    /// A padding byte is not zero (ONBF-R6). The offset is the first such byte.
    PaddingNotZero(u64),
    /// The image does not contain exactly one MANIFEST section (§4.4).
    ManifestCount,
    /// A section entry breaks a rule; `index` is its position in the section table.
    Section {
        /// Position in the section table.
        index: u16,
        /// The rule it breaks.
        error: SectionError,
    },
    /// The computed content digest differs from the header's `content_digest` (§4.5).
    ContentDigestMismatch,
}

/// Why one section entry was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionError {
    /// A flag bit outside the six defined bits is set.
    UnknownFlags(u32),
    /// WRITE and EXECUTE are both set (ONBF-R5).
    WriteAndExecute,
    /// READ, WRITE, EXECUTE or RELRO on a section that is not a SEGMENT.
    PermissionsOnNonSegment,
    /// A SEGMENT without MAPPABLE.
    SegmentNotMappable,
    /// A kind not in §4.4, with or without REQUIRED (errata C13).
    UnknownKind(u32),
    /// A known kind that §4.4 does not allow in this profile.
    KindNotAllowedInProfile(u32),
    /// `compression` is not 0 or 1.
    UnknownCompression(u8),
    /// A MAPPABLE, MERKLE or MANIFEST section is compressed (errata C9, C12).
    CompressionNotAllowed,
    /// `stored_size` or `content_size` exceeds 1 TiB (Appendix A).
    TooLarge,
    /// An uncompressed section whose `content_size` differs from `stored_size`.
    SizeMismatch,
    /// A section with no content that is not a SEGMENT, or a SEGMENT with `mem_size = 0`
    /// (errata C14).
    EmptySection,
    /// `integrity` is not 0–3.
    UnknownIntegrity(u8),
    /// The integrity mode does not fit the section: Merkle hash blocks (2) on a
    /// non-MERKLE section or the reverse; parent coverage (3) outside a nested image; a
    /// MAPPABLE section without Merkle or parent coverage; or a MANIFEST not using 0.
    IntegrityNotAllowed(u8),
    /// `merkle_index` is set although integrity is not Merkle, or points outside the
    /// table, or at a section that is not MERKLE.
    MerkleIndex(u32),
    /// The MERKLE section's size is not the hash-block count the referencing section's
    /// `content_size` needs.
    MerkleSize {
        /// Expected bytes: hash blocks × 4096.
        expected: u64,
        /// `stored_size` of the MERKLE section.
        found: u64,
    },
    /// A MERKLE section referenced by no section, or by more than one.
    MerkleReferences(u16),
    /// The `digest` field is not zero although the integrity mode carries no digest.
    DigestNotZero,
    /// A MANIFEST without REQUIRED, or larger than 1 MiB (§5).
    ManifestRules,
    /// `mem_offset` and `mem_size` break §4.3: a SEGMENT not 4 KiB aligned, smaller than
    /// its content, or overflowing; or a non-SEGMENT with a memory placement.
    MemoryLayout,
    /// A reserved entry field is not zero (ONBF-R6).
    Reserved,
    /// `file_offset` is not the canonical position after the previous section.
    Misplaced {
        /// The canonical offset.
        expected: u64,
        /// The offset in the entry.
        found: u64,
    },
    /// The section extends past the end of the input.
    OutOfBounds,
}
