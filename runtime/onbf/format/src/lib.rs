//! `onbf-format`: the Origin Native Binary Format container parser (`docs/spec/ONBF.md`).
//!
//! - **Container:** [`Image::parse`] validates the 128-byte header (§4.2), the section
//!   table (§4.3, §4.4) and the file layout (§4.1), and exposes the content-digest input
//!   (§4.5). It reads only the header, the table and the padding between items (ONBF-R1).
//! - **OCBOR-1 (§5):** [`ocbor::validate`] checks a deterministic-CBOR document, and
//!   [`ocbor::Cursor`] reads one.
//! - **KRN manifest:** [`KrnManifest::decode`] implements the KRN keys of
//!   `docs/spec/ONBF-manifest-keys.md`.
//!
//! The order a loader follows (ONBF-R1):
//! 1. `Image::parse`;
//! 2. `verify_content_digest`;
//! 3. verify the signatures (not yet implemented);
//! 4. `Section::verify_sha256` on the manifest;
//! 5. `KrnManifest::decode`;
//! 6. `KrnManifest::check_against`.
//!
//! The signature block (§7.1) and Merkle verification (§4.6, crate `onbf-merkle`) are
//! separate layers.
//!
//! Parser contract (ONBF §12): `no_std`, no allocation, no `unsafe`, no panics, typed
//! errors, no partial results. Where the specification allowed more than one reading,
//! this crate implements the owner's decisions C1–C14 in `docs/spec/ONBF-errata.md`
//! (2026-11-07). ONBF byte layouts become v1.0 final only after the remaining §13 tests:
//! fuzzing, Kani bounds and an independent second parser.
//!
//! SHA-512 is not implemented here: [`Image::verify_content_digest`] takes the hash
//! function from the caller, so `origin-crypto` can supply it.
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented,
        clippy::cast_possible_truncation
    )
)]

mod error;
mod header;
mod image;
mod manifest;
pub mod ocbor;
mod read;
mod section;

pub use error::{Error, SectionError};
pub use header::{
    ARCH_X86_64, ENTRY_SIZE, FORMAT_MAJOR, HEADER_SIZE, Header, MAGIC, MAX_SECTIONS,
    MAX_SIGNATURE_BLOCK, Profile,
};
pub use image::{Image, ParseOptions, Section};
pub use manifest::{Cet, CpuFeatures, FieldPath, KrnManifest, ManifestError, Toolchain};
pub use section::{
    Compression, Flags, Integrity, Kind, MAX_IMAGE_SPAN, MAX_MANIFEST, MAX_SECTION_CONTENT,
    MAX_SEGMENTS_EXE_LIB, NO_MERKLE, PAGE_SIZE, SMALL_ALIGN, SectionEntry, merkle_hash_blocks,
};

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_ocbor;
