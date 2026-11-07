//! Whole-image validation: header, section table and file layout (§4.1–§4.5).

use crate::error::{Error, SectionError};
use crate::header::{ENTRY_SIZE, HEADER_SIZE, Header};
use crate::read::{align_up, slice};
use crate::section::{
    Integrity, Kind, NO_MERKLE, PAGE_SIZE, SMALL_ALIGN, SectionEntry, merkle_hash_blocks,
};

/// What the caller vouches for before parsing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ParseOptions {
    /// The image is nested inside a parent whose signature has verified (a PKG payload).
    /// Allows an unsigned image and `integrity = 3` (§4.2, §4.3).
    pub nested_in_verified_parent: bool,
    /// Developer mode: allows an unsigned top-level image (§4.2).
    pub developer_mode: bool,
}

/// A validated ONBF image.
///
/// Holding an `Image` means the header, section table and file layout satisfy §4. It does
/// **not** mean the image is authentic: the content digest and signatures are checked
/// separately, and no section content has been read (ONBF-R1).
#[derive(Clone, Copy, Debug)]
pub struct Image<'a> {
    bytes: &'a [u8],
    header: Header,
    nested: bool,
}

/// One section of a validated image.
#[derive(Clone, Copy, Debug)]
pub struct Section<'a> {
    /// Position in the section table.
    pub index: u16,
    /// The decoded entry.
    pub entry: SectionEntry,
    /// The stored (possibly compressed) bytes, not yet verified against `entry.digest`.
    pub stored: &'a [u8],
}

/// Records an error against the section at `index`.
fn at(index: u16) -> impl Fn(SectionError) -> Error {
    move |error| Error::Section { index, error }
}

/// Bytes `start..end` of the input must exist and be zero.
fn check_padding(bytes: &[u8], start: u64, end: u64) -> Result<(), Error> {
    let pad = slice(bytes, start, end).ok_or(Error::Truncated)?;
    match pad.iter().position(|&b| b != 0) {
        None => Ok(()),
        Some(i) => Err(Error::PaddingNotZero(
            start.saturating_add(u64::try_from(i).unwrap_or(u64::MAX)),
        )),
    }
}

impl<'a> Image<'a> {
    /// Validates `bytes` as one complete ONBF image.
    ///
    /// Reads the header, the section table and the padding bytes between items (to check
    /// that they are zero). It does not read section content. On success every rule of
    /// §4.1–§4.4 holds; on failure nothing is returned (no partial results).
    pub fn parse(bytes: &'a [u8], options: ParseOptions) -> Result<Self, Error> {
        let header_bytes = bytes.get(..128).ok_or(Error::Truncated)?;
        let header = Header::parse(header_bytes)?;
        let nested = options.nested_in_verified_parent;
        let len = u64::try_from(bytes.len()).map_err(|_| Error::Truncated)?;

        let count = header.section_count;
        let table_end = u64::from(count)
            .checked_mul(ENTRY_SIZE)
            .and_then(|t| t.checked_add(HEADER_SIZE))
            .ok_or(Error::Truncated)?;
        let table = slice(bytes, HEADER_SIZE, table_end).ok_or(Error::Truncated)?;

        // §4.1: zero padding from the end of the table to the next 4 KiB boundary.
        let data_start = align_up(table_end, PAGE_SIZE).ok_or(Error::Truncated)?;
        check_padding(bytes, table_end, data_start)?;

        // Pass 1: each entry on its own, then its canonical position in the file.
        let mut pos = data_start;
        let mut manifests = 0u32;
        for (i, raw) in table.as_chunks::<128>().0.iter().enumerate() {
            let index = u16::try_from(i).map_err(|_| Error::SectionCount(u32::MAX))?;
            let entry = SectionEntry::parse(raw, header.profile, nested).map_err(at(index))?;
            if entry.kind() == Kind::Manifest {
                manifests = manifests.saturating_add(1);
            }
            let expected = align_up(pos, entry.file_alignment()).ok_or(Error::Truncated)?;
            if entry.file_offset != expected {
                return Err(at(index)(SectionError::Misplaced {
                    expected,
                    found: entry.file_offset,
                }));
            }
            let end = expected
                .checked_add(entry.stored_size)
                .filter(|&e| e <= len)
                .ok_or(at(index)(SectionError::OutOfBounds))?;
            check_padding(bytes, pos, expected)?;
            pos = end;
        }
        if manifests != 1 {
            return Err(Error::ManifestCount);
        }

        // The signature block, if any, is the last item and ends exactly at EOF (§4.1).
        if header.signature_block_offset == 0 {
            if !(nested || options.developer_mode) {
                return Err(Error::UnsignedNotAllowed);
            }
            if pos != len {
                return Err(Error::TrailingData);
            }
        } else {
            let expected = align_up(pos, SMALL_ALIGN).ok_or(Error::Truncated)?;
            if header.signature_block_offset != expected {
                return Err(Error::SignatureBlockMisplaced {
                    expected,
                    found: header.signature_block_offset,
                });
            }
            check_padding(bytes, pos, expected)?;
            let end = expected
                .checked_add(header.signature_block_size)
                .ok_or(Error::Truncated)?;
            if end > len {
                return Err(Error::Truncated);
            }
            if end < len {
                return Err(Error::TrailingData);
            }
        }

        let image = Self {
            bytes,
            header,
            nested,
        };
        image.check_merkle_references()?;
        Ok(image)
    }

    /// Pass 2: every Merkle-protected section whose tree has hash blocks points at a
    /// MERKLE section of exactly the right size; one whose tree has none (content of at
    /// most 4 KiB, `levels = 0`) uses `NO_MERKLE`. Every MERKLE section serves exactly one
    /// section (ONBF-R6: no unreferenced bytes).
    fn check_merkle_references(&self) -> Result<(), Error> {
        let mut references = [0u8; 256];
        for s in self.sections() {
            if s.entry.integrity != Integrity::Merkle {
                continue;
            }
            let target = s.entry.merkle_index;
            let blocks = merkle_hash_blocks(s.entry.content_size);
            if blocks == 0 {
                if target != NO_MERKLE {
                    return Err(at(s.index)(SectionError::MerkleIndex(target)));
                }
                continue;
            }
            let merkle = usize::try_from(target)
                .ok()
                .and_then(|t| self.section(t))
                .filter(|m| m.entry.kind() == Kind::Merkle)
                .ok_or(at(s.index)(SectionError::MerkleIndex(target)))?;
            let expected = blocks.saturating_mul(PAGE_SIZE);
            if merkle.entry.stored_size != expected {
                return Err(at(merkle.index)(SectionError::MerkleSize {
                    expected,
                    found: merkle.entry.stored_size,
                }));
            }
            if let Some(n) = references.get_mut(usize::from(merkle.index)) {
                *n = n.saturating_add(1);
            }
        }
        for s in self.sections() {
            let refs = references.get(usize::from(s.index)).copied().unwrap_or(0);
            if s.entry.kind() == Kind::Merkle && refs != 1 {
                return Err(at(s.index)(SectionError::MerkleReferences(u16::from(refs))));
            }
        }
        Ok(())
    }

    /// The validated header.
    pub const fn header(&self) -> &Header {
        &self.header
    }

    /// True if the caller declared this image nested in a verified parent.
    pub const fn is_nested(&self) -> bool {
        self.nested
    }

    /// Number of sections.
    pub fn section_count(&self) -> usize {
        usize::from(self.header.section_count)
    }

    /// The section at `index`, or `None` if `index >= section_count()`.
    pub fn section(&self, index: usize) -> Option<Section<'a>> {
        if index >= self.section_count() {
            return None;
        }
        let start = u64::try_from(index)
            .ok()?
            .checked_mul(ENTRY_SIZE)?
            .checked_add(HEADER_SIZE)?;
        let raw: &[u8; 128] = slice(self.bytes, start, start.checked_add(ENTRY_SIZE)?)?
            .try_into()
            .ok()?;
        // Already validated by `parse`; decoding again cannot fail.
        let entry = SectionEntry::parse(raw, self.header.profile, self.nested).ok()?;
        let end = entry.file_offset.checked_add(entry.stored_size)?;
        let stored = slice(self.bytes, entry.file_offset, end)?;
        Some(Section {
            index: u16::try_from(index).ok()?,
            entry,
            stored,
        })
    }

    /// All sections, in table (and file) order.
    pub fn sections(&self) -> impl Iterator<Item = Section<'a>> + '_ {
        (0..self.section_count()).filter_map(|i| self.section(i))
    }

    /// The MANIFEST section. `parse` guarantees there is exactly one.
    pub fn manifest(&self) -> Option<Section<'a>> {
        self.sections().find(|s| s.entry.kind() == Kind::Manifest)
    }

    /// The signature block bytes, or `None` for an unsigned image.
    pub fn signature_block(&self) -> Option<&'a [u8]> {
        let h = &self.header;
        if h.signature_block_offset == 0 {
            return None;
        }
        slice(
            self.bytes,
            h.signature_block_offset,
            h.signature_block_offset
                .checked_add(h.signature_block_size)?,
        )
    }

    /// The three byte ranges `content_digest` covers, in order (§4.5):
    /// `header[0..32]`, `header[112..128]` and the section table.
    pub fn content_digest_input(&self) -> [&'a [u8]; 3] {
        let table_end = u64::from(self.header.section_count)
            .saturating_mul(ENTRY_SIZE)
            .saturating_add(HEADER_SIZE);
        [
            self.bytes.get(0..32).unwrap_or(&[]),
            self.bytes.get(112..128).unwrap_or(&[]),
            slice(self.bytes, HEADER_SIZE, table_end).unwrap_or(&[]),
        ]
    }

    /// Checks `content_digest` with a caller-supplied SHA-512 over the three ranges of
    /// [`Self::content_digest_input`]. The digest is public, so a plain comparison is fine.
    pub fn verify_content_digest(
        &self,
        sha512: impl FnOnce(&[&[u8]; 3]) -> [u8; 64],
    ) -> Result<(), Error> {
        if sha512(&self.content_digest_input()) == self.header.content_digest {
            Ok(())
        } else {
            Err(Error::ContentDigestMismatch)
        }
    }
}
