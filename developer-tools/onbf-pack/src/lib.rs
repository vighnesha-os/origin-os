//! `onbf-pack`: converts `rust-lld` output into ONBF images (`docs/spec/ONBF.md` §6.1.2,
//! owner decision D1). Host only; it is not part of the shipping OS.
//!
//! This version writes one profile, KRN (§6.5): [`krn::pack`] turns the Framekernel's
//! ELF64 x86-64 PIE into an unsigned (developer-mode) KRN image. It also describes the programs
//! the kernel carries in that image ([`carried`], ADR-0011 L1), which is no ONBF profile.
//!
//! - [`elf`] reads the input against the conversion contract and rejects everything
//!   outside it (§6.1.2 hard errors, ONBF-R11).
//! - [`carried`] lays a ring-3 program out for the kernel's launch path, as Rust the kernel
//!   compiles in (task S4-1; the note to ONBF §8.3).
//! - [`corelib`] lays `origin.core` out for the kernel the same way, and refuses an image that
//!   is anything but the generated system-call stubs (task S4-2; ADR-0011 L2 to L7).
//! - [`relr`] encodes and decodes RELR relocations (§6.1).
//! - [`ocbor`] writes canonical OCBOR-1 (§5).
//! - [`krn`] builds the sections, lays out the container (errata C1–C4, C7, C10, C12,
//!   C14, G4, G5), and verifies the result with `onbf-format` and `onbf-merkle` before
//!   returning it ([`krn::verify`]).
//! - [`sign`] signs a finished KRN image (§7.1–§7.2, S1–S4; CLAUDE.md §59) with an external
//!   signer, and verifies the result with the boot loader's own signature code.
//!
//! Packing is deterministic: the same ELF bytes and options always give the same image,
//! with no timestamps, host paths or other environment input (ONBF-R9, CLAUDE.md §75).
//! Signing is not, because ML-DSA signing is hedged with fresh randomness; the statement is.
//!
//! **The library reaches nothing of the host** (ADR-0011 Amendment 4, T1). It reads and writes
//! no file, runs no program, reads no environment variable, clock or device, and prints
//! nothing: it takes bytes and text from its caller and returns values. What it needs of the
//! host comes from the caller: the toolchain's identity as `rustc -vV` prints it
//! ([`Toolchain::from_rustc_verbose_version`]), randomness ([`sign::Random`]), and the contents
//! of files. Its two callers are the command line (`src/main.rs`), a thin client that does the
//! reading and writing and calls nothing this library does not offer, and `xtask`, which calls
//! it in process. `cargo xtask host-tools-gate` holds the rule.
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

pub mod carried;
mod container;
pub mod corelib;
pub mod elf;
pub mod krn;
pub mod ocbor;
pub mod relr;
pub mod sign;
pub mod toolchain;

use core::fmt;

pub use elf::ElfError;
pub use krn::{KrnOptions, Packed, VerifyError};
pub use relr::RelrError;
pub use sign::{HostKey, NewKey, Random, RandomError, SignError, SignOptions, Signer};
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
