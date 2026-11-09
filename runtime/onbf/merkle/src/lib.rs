//! `onbf-merkle`: Merkle-SHA256-4K trees for ONBF sections (`docs/spec/ONBF.md` §4.6).
//!
//! - [`root`] computes a section's Merkle root in one streaming pass. The boot loader uses
//!   it to verify a whole KRN segment.
//! - [`build`] writes the MERKLE section, for writers such as `onbf-pack`.
//! - [`verify_tree`] checks a section and its MERKLE section together, in one pass with no
//!   more hashing than [`root`]. The boot loader uses it so every stored byte is covered.
//! - [`verify_block`] checks one 4 KiB block against the root by walking one hash block per
//!   level, using a caller-supplied cache of trusted hash blocks (§4.6). This is the
//!   page-fault path (ONBF-R2).
//!
//! Tree rules, with the owner's decisions G2 and G3 (`docs/spec/ONBF-errata.md`):
//! - leaf `h0[i] = SHA-256(0x00 ‖ le64(i) ‖ block_i)`, the last block zero-padded to 4 KiB;
//! - node `hL[j] = SHA-256(0x01 ‖ le8(L) ‖ le64(j) ‖ hash_block_j)`, where `L ≥ 1` is the level
//!   of the hash produced and the hash block holds up to 128 level-`L-1` hashes,
//!   zero-padded;
//! - `levels` is the level of the top hash, so `top = h_levels[0]`;
//! - `root = SHA-256("ONBF-MERKLE-1" ‖ le64(content_size) ‖ le8(levels) ‖ top)`, where the
//!   label is its 13 ASCII bytes; empty content has `levels = 0` and an all-zero `top`;
//! - the MERKLE section stores hash blocks highest level first, ascending within a level.
//!
//! Contract (ONBF §12): `no_std`, no allocation, no `unsafe`, no panics. No 4 KiB buffers
//! either: child hashes stream into one SHA-256 state per level, so the stack use is a few
//! hundred bytes, which suits kernel stacks.
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

mod shape;
mod tree;
mod verify;

pub use shape::Shape;
pub use tree::{build, root, verify_tree};
pub use verify::{NoCache, TrustCache, verify_block};

use origin_crypto::Sha256;

/// Data block and hash block size.
pub const BLOCK_SIZE: usize = 4096;
/// Size of one SHA-256 hash.
pub const HASH_SIZE: usize = 32;
/// Hashes per hash block.
pub const HASHES_PER_BLOCK: usize = BLOCK_SIZE / HASH_SIZE;
/// Largest content (ONBF Appendix A: section content ≤ 1 TiB).
pub const MAX_CONTENT: u64 = 1 << 40;
/// Most levels a tree can have: 1 TiB is 2^28 blocks, which is 128^4.
pub const MAX_LEVELS: usize = 4;

/// Why a tree operation failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The content is larger than [`MAX_CONTENT`].
    TooLarge,
    /// The MERKLE section (input or output buffer) has the wrong length for the content.
    HashBlocksLength {
        /// `Shape::hash_blocks_len()`.
        expected: u64,
        /// The length supplied.
        found: u64,
    },
    /// The data block index is not below `Shape::leaves()`.
    BlockIndex(u64),
    /// The data block has the wrong length: 4096 bytes, or the remainder for the last one.
    BlockLength {
        /// The length block `index` must have.
        expected: usize,
        /// The length supplied.
        found: usize,
    },
    /// A hash on the path to the root does not match its slot in the hash block at `level`
    /// (the level of the hash block, 1 to `levels`).
    HashMismatch {
        /// Level of the hash block whose slot did not match.
        level: u8,
    },
    /// The computed root differs from the expected root.
    RootMismatch,
}

const ZEROS: [u8; BLOCK_SIZE] = [0; BLOCK_SIZE];
const ROOT_LABEL: &[u8; 13] = b"ONBF-MERKLE-1";

/// `h0[i]`. `block` is at most 4096 bytes; it is zero-padded to a full block.
fn leaf_hash(index: u64, block: &[u8]) -> [u8; HASH_SIZE] {
    let mut h = Sha256::new();
    h.update(&[0x00]);
    h.update(&index.to_le_bytes());
    h.update(block);
    h.update(ZEROS.get(block.len()..).unwrap_or(&[]));
    h.finalize()
}

/// Starts `hL[j]`: the caller appends the hash block's 4096 bytes.
fn node_hasher(level: u8, index: u64) -> Sha256 {
    let mut h = Sha256::new();
    h.update(&[0x01, level]);
    h.update(&index.to_le_bytes());
    h
}

/// `root = SHA-256("ONBF-MERKLE-1" ‖ le64(content_size) ‖ le8(levels) ‖ top)`.
fn root_hash(content_size: u64, levels: u8, top: &[u8; HASH_SIZE]) -> [u8; HASH_SIZE] {
    let mut h = Sha256::new();
    h.update(ROOT_LABEL);
    h.update(&content_size.to_le_bytes());
    h.update(&[levels]);
    h.update(top);
    h.finalize()
}

#[cfg(test)]
#[path = "../../format/src/testing.rs"]
mod testing;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_soak;
