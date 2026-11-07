//! `onbf-pack`: converts `rust-lld` output into ONBF images (`docs/spec/ONBF.md` §6.1.2,
//! owner decision D1). Host only; it is not part of the shipping OS.
//!
//! This version writes one profile, KRN (§6.5): [`krn::pack`] turns the Framekernel's
//! ELF64 x86-64 PIE into an unsigned (developer-mode) KRN image.
//!
//! - [`elf`] reads the input against the conversion contract and rejects everything
//!   outside it (§6.1.2 hard errors, ONBF-R11).
//! - [`relr`] encodes and decodes RELR relocations (§6.1).
//! - [`ocbor`] writes canonical OCBOR-1 (§5).
//! - [`krn`] builds the sections, lays out the container (errata C1–C4, C7, C10, C12,
//!   C14, G4, G5), and verifies the result with `onbf-format` and `onbf-merkle` before
//!   returning it ([`krn::verify`]).
//!
//! Output is deterministic: the same ELF bytes and options always give the same image,
//! with no timestamps, host paths or other environment input (ONBF-R9, CLAUDE.md §75).
#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation
    )
)]

mod container;
pub mod elf;
pub mod krn;
pub mod ocbor;
pub mod relr;
pub mod toolchain;

use core::fmt;

pub use elf::ElfError;
pub use krn::{KrnOptions, Packed, VerifyError};
pub use relr::RelrError;
pub use toolchain::{Toolchain, ToolchainError};

/// Why the container could not be laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutError {
    /// More than 256 sections (Appendix A).
    TooManySections,
    /// A size or offset does not fit in memory or in 64 bits.
    TooLarge,
    /// `onbf-merkle` cannot build a segment's tree.
    Merkle(onbf_merkle::Error),
    /// The relocation slots cannot be RELR-encoded.
    Relr(RelrError),
}

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManySections => f.write_str("more than 256 sections"),
            Self::TooLarge => f.write_str("the image is too large"),
            Self::Merkle(e) => write!(f, "onbf-merkle: {e:?}"),
            Self::Relr(e) => write!(f, "RELR encoding: {e}"),
        }
    }
}

/// Why a conversion failed. Nothing is written on any error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The ELF input breaks the conversion contract.
    Elf(ElfError),
    /// `cpu_features` sets bits outside the registry (`0x3F`); the value is those bits.
    CpuFeatures(u64),
    /// The toolchain description is missing or invalid.
    Toolchain(ToolchainError),
    /// The container could not be laid out.
    Layout(LayoutError),
    /// The finished image failed in-process verification (a packer bug).
    Verify(VerifyError),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Elf(e) => write!(f, "ELF input: {e}"),
            Self::CpuFeatures(bits) => write!(
                f,
                "cpu features {bits:#x} are not in the registry (allowed bits: 0x3f)"
            ),
            Self::Toolchain(e) => write!(f, "{e}"),
            Self::Layout(e) => write!(f, "layout: {e}"),
            Self::Verify(e) => write!(f, "self-check failed, nothing written: {e}"),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests;
