//! `onbf-format`: the Origin Native Binary Format container parser (`docs/spec/ONBF.md`).
//!
//! This crate validates the container layer: the 128-byte header (§4.2), the section
//! table (§4.3, §4.4) and the file layout (§4.1), and exposes the content-digest input
//! (§4.5). Per ONBF-R1 it reads only the header, the table and the padding between items;
//! it never interprets section content. OCBOR-1 manifests (§5), the signature block
//! (§7.1) and Merkle verification (§4.6) are separate layers.
//!
//! Parser contract (ONBF §12): `no_std`, no allocation, no `unsafe`, no panics, typed
//! errors, no partial results. Where the specification allows more than one reading,
//! this crate implements the reading recorded in `docs/spec/ONBF-errata.md` (Proposed).
//! Until the owner approves those readings, this parser must not ship (CLAUDE.md §84).
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
mod read;
mod section;

pub use error::{Error, SectionError};
pub use header::{
    ARCH_X86_64, ENTRY_SIZE, FORMAT_MAJOR, HEADER_SIZE, Header, MAGIC, MAX_SECTIONS,
    MAX_SIGNATURE_BLOCK, Profile,
};
pub use image::{Image, ParseOptions, Section};
pub use section::{
    Compression, Flags, Integrity, Kind, MAX_MANIFEST, MAX_SECTION_CONTENT, NO_MERKLE, PAGE_SIZE,
    SMALL_ALIGN, SectionEntry, merkle_hash_blocks,
};

#[cfg(test)]
mod tests;
