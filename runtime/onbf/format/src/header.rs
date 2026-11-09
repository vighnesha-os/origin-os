//! The 128-byte ONBF header (§4.2).

use crate::error::Error;
use crate::read::{Reader, all_zero};

/// `"\x89ONBF\r\n\x1a"`.
pub const MAGIC: [u8; 8] = [0x89, b'O', b'N', b'B', b'F', 0x0D, 0x0A, 0x1A];
/// The only `format_major` this parser reads.
pub const FORMAT_MAJOR: u16 = 1;
/// Size of the header in bytes.
pub const HEADER_SIZE: u64 = 128;
/// Size of one section entry in bytes.
pub const ENTRY_SIZE: u64 = 128;
/// Largest `section_count` (Appendix A).
pub const MAX_SECTIONS: u32 = 256;
/// Largest signature block (Appendix A).
pub const MAX_SIGNATURE_BLOCK: u64 = 1 << 20;
/// `arch` value for x86-64 (LP64), the only one accepted (ONBF-R11).
pub const ARCH_X86_64: u16 = 1;

// The container version this parser implements must be the one origin-version.toml
// records (CLAUDE.md §63), so the two cannot drift apart silently.
const _: () = assert!(origin_version::ONBF_FORMAT_MAJOR == FORMAT_MAJOR);

/// An ONBF profile (§1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    /// `.ose` executable.
    Exe = 1,
    /// `.osl` shared library.
    Lib = 2,
    /// `.oapp` application image.
    App = 3,
    /// `.osi` installer package.
    Pkg = 4,
    /// `.od` driver package.
    Drv = 5,
    /// Framekernel image.
    Krn = 6,
    /// Unified release manifest.
    Sys = 7,
    /// `.osym` detached debug symbols.
    Sym = 8,
}

impl Profile {
    /// The profile with this ID, if any.
    pub const fn from_id(id: u16) -> Option<Self> {
        Some(match id {
            1 => Self::Exe,
            2 => Self::Lib,
            3 => Self::App,
            4 => Self::Pkg,
            5 => Self::Drv,
            6 => Self::Krn,
            7 => Self::Sys,
            8 => Self::Sym,
            _ => return None,
        })
    }

    /// One bit per profile, for the §4.4 allowed-profile sets.
    pub(crate) const fn bit(self) -> u16 {
        1 << (self as u16)
    }
}

/// A decoded and validated header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    /// Container major version; always [`FORMAT_MAJOR`].
    pub format_major: u16,
    /// Container minor version. Higher minors add only optional content (§4.2).
    pub format_minor: u16,
    /// The image's profile.
    pub profile: Profile,
    /// Number of section entries, 1–256.
    pub section_count: u16,
    /// File offset of the signature block; 0 if the image is unsigned.
    pub signature_block_offset: u64,
    /// Size of the signature block; 0 if the image is unsigned.
    pub signature_block_size: u64,
    /// SHA-512 over the header and section table (§4.5).
    pub content_digest: [u8; 64],
}

impl Header {
    /// Decodes and validates the header in `bytes[..128]`.
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let mut r = Reader::new(bytes);
        let magic = r.array::<8>().ok_or(Error::Truncated)?;
        let format_major = r.u16().ok_or(Error::Truncated)?;
        let format_minor = r.u16().ok_or(Error::Truncated)?;
        let profile_id = r.u16().ok_or(Error::Truncated)?;
        let header_size = r.u16().ok_or(Error::Truncated)?;
        let header_flags = r.u32().ok_or(Error::Truncated)?;
        let section_count = r.u32().ok_or(Error::Truncated)?;
        let section_table_offset = r.u64().ok_or(Error::Truncated)?;
        let signature_block_offset = r.u64().ok_or(Error::Truncated)?;
        let signature_block_size = r.u64().ok_or(Error::Truncated)?;
        let content_digest = *r.array::<64>().ok_or(Error::Truncated)?;
        let arch = r.u16().ok_or(Error::Truncated)?;
        let reserved = r.array::<14>().ok_or(Error::Truncated)?;

        if *magic != MAGIC {
            return Err(Error::BadMagic);
        }
        if format_major != FORMAT_MAJOR {
            return Err(Error::UnsupportedFormatMajor(format_major));
        }
        let profile = Profile::from_id(profile_id).ok_or(Error::UnknownProfile(profile_id))?;
        if u64::from(header_size) != HEADER_SIZE {
            return Err(Error::BadHeaderSize(header_size));
        }
        if header_flags != 0 {
            return Err(Error::HeaderFlags(header_flags));
        }
        if !(1..=MAX_SECTIONS).contains(&section_count) {
            return Err(Error::SectionCount(section_count));
        }
        let section_count =
            u16::try_from(section_count).map_err(|_| Error::SectionCount(section_count))?;
        if section_table_offset != HEADER_SIZE {
            return Err(Error::SectionTableOffset(section_table_offset));
        }
        if arch != ARCH_X86_64 {
            return Err(Error::UnsupportedArch(arch));
        }
        if !all_zero(reserved) {
            return Err(Error::HeaderReserved);
        }
        let signed = signature_block_offset != 0;
        let size_ok = if signed {
            (1..=MAX_SIGNATURE_BLOCK).contains(&signature_block_size)
        } else {
            signature_block_size == 0
        };
        if !size_ok {
            return Err(Error::SignatureBlockSize(signature_block_size));
        }

        Ok(Self {
            format_major,
            format_minor,
            profile,
            section_count,
            signature_block_offset,
            signature_block_size,
            content_digest,
        })
    }
}
