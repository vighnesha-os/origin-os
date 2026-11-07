//! Verifying one data block against a root (§4.6), the page-fault path of ONBF-R2.

use crate::{BLOCK_SIZE, Error, HASH_SIZE, MAX_LEVELS, Shape, leaf_hash, node_hasher, root_hash};

/// Remembers hash blocks that have already been verified, so later blocks stop at the first
/// trusted ancestor instead of re-hashing up to the root (§4.6).
///
/// An entry is valid only for the exact MERKLE-section bytes it was verified against. The
/// caller must keep that memory unchanged, for example in read-only kernel pages, and must
/// clear the cache whenever the bytes are reloaded.
pub trait TrustCache {
    /// True if hash block `index` of `level` was verified before.
    fn is_trusted(&self, level: u8, index: u64) -> bool;
    /// Records that hash block `index` of `level` is authentic.
    fn mark_trusted(&mut self, level: u8, index: u64);
}

/// A cache that trusts nothing: every verification walks up to the root.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoCache;

impl TrustCache for NoCache {
    fn is_trusted(&self, _level: u8, _index: u64) -> bool {
        false
    }
    fn mark_trusted(&mut self, _level: u8, _index: u64) {}
}

/// Checks data block `index` (4096 bytes, or the remainder for the last block) against
/// `root`, using the MERKLE section `hash_blocks`.
///
/// At each level the block's hash must equal its slot in the parent hash block. The walk
/// ends at the root, or at a hash block `cache` already trusts. Only after success are the
/// hash blocks on the path marked trusted.
pub fn verify_block(
    root: &[u8; HASH_SIZE],
    shape: &Shape,
    hash_blocks: &[u8],
    index: u64,
    block: &[u8],
    cache: &mut impl TrustCache,
) -> Result<(), Error> {
    let found = u64::try_from(hash_blocks.len()).unwrap_or(u64::MAX);
    if found != shape.hash_blocks_len() {
        return Err(Error::HashBlocksLength {
            expected: shape.hash_blocks_len(),
            found,
        });
    }
    let expected = shape.block_len(index).ok_or(Error::BlockIndex(index))?;
    if block.len() != expected {
        return Err(Error::BlockLength {
            expected,
            found: block.len(),
        });
    }

    let mut hash = leaf_hash(index, block);
    let mut child = index;
    let mut visited = [(0u8, 0u64); MAX_LEVELS];
    let mut visited_len = 0usize;
    let mut anchored = false;
    for level in 1..=shape.levels() {
        let parent = child >> 7;
        let mismatch = Error::HashMismatch { level };
        let block_bytes = shape
            .block_position(level, parent)
            .and_then(|p| p.checked_mul(BLOCK_SIZE as u64))
            .and_then(|start| usize::try_from(start).ok())
            .and_then(|start| hash_blocks.get(start..start.checked_add(BLOCK_SIZE)?))
            .ok_or(mismatch)?;
        let slot = usize::try_from(child & 127)
            .map_err(|_| mismatch)?
            .saturating_mul(HASH_SIZE);
        let in_slot = block_bytes
            .get(slot..slot.saturating_add(HASH_SIZE))
            .ok_or(mismatch)?;
        if in_slot != hash {
            return Err(mismatch);
        }
        if cache.is_trusted(level, parent) {
            anchored = true;
            break;
        }
        if let Some(v) = visited.get_mut(visited_len) {
            *v = (level, parent);
            visited_len = visited_len.saturating_add(1);
        }
        let mut h = node_hasher(level, parent);
        h.update(block_bytes);
        hash = h.finalize();
        child = parent;
    }
    if !anchored && root_hash(shape.content_size(), shape.levels(), &hash) != *root {
        return Err(Error::RootMismatch);
    }
    for &(level, block_index) in visited.get(..visited_len).unwrap_or(&[]) {
        cache.mark_trusted(level, block_index);
    }
    Ok(())
}
