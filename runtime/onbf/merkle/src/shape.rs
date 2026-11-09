//! The shape of a tree: leaves, levels, and where each level's hash blocks are stored.

use crate::{BLOCK_SIZE, Error, HASHES_PER_BLOCK, MAX_CONTENT, MAX_LEVELS};

const BLOCK: u64 = BLOCK_SIZE as u64;
const FANOUT: u64 = HASHES_PER_BLOCK as u64;

/// Everything about a tree that follows from `content_size` alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shape {
    content_size: u64,
    leaves: u64,
    levels: u8,
    /// `blocks[L]` is the number of hash blocks at level `L` (1..=levels): blocks hashed to
    /// produce level-`L` hashes. `blocks[0]` is unused.
    blocks: [u64; MAX_LEVELS + 1],
}

impl Shape {
    /// The shape for `content_size` bytes of content.
    pub fn new(content_size: u64) -> Result<Self, Error> {
        if content_size > MAX_CONTENT {
            return Err(Error::TooLarge);
        }
        let leaves = content_size.div_ceil(BLOCK);
        let mut blocks = [0u64; MAX_LEVELS + 1];
        let mut levels: u8 = 0;
        let mut hashes = leaves;
        while hashes > 1 {
            hashes = hashes.div_ceil(FANOUT);
            levels = levels.checked_add(1).ok_or(Error::TooLarge)?;
            *blocks.get_mut(usize::from(levels)).ok_or(Error::TooLarge)? = hashes;
        }
        Ok(Self {
            content_size,
            leaves,
            levels,
            blocks,
        })
    }

    /// Content size in bytes.
    pub const fn content_size(&self) -> u64 {
        self.content_size
    }

    /// Number of 4 KiB data blocks.
    pub const fn leaves(&self) -> u64 {
        self.leaves
    }

    /// Level of the top hash (errata G3).
    pub const fn levels(&self) -> u8 {
        self.levels
    }

    /// Hash blocks at `level` (1..=levels); 0 outside that range.
    pub fn blocks_at(&self, level: u8) -> u64 {
        if level == 0 || level > self.levels {
            return 0;
        }
        self.blocks.get(usize::from(level)).copied().unwrap_or(0)
    }

    /// Total hash blocks in the MERKLE section.
    pub fn hash_blocks(&self) -> u64 {
        self.blocks
            .iter()
            .fold(0u64, |acc, &b| acc.saturating_add(b))
    }

    /// Byte length of the MERKLE section.
    pub fn hash_blocks_len(&self) -> u64 {
        self.hash_blocks().saturating_mul(BLOCK)
    }

    /// Position (in blocks) of hash block `index` of `level` within the MERKLE section,
    /// which stores the highest level first.
    pub fn block_position(&self, level: u8, index: u64) -> Option<u64> {
        if index >= self.blocks_at(level) {
            return None;
        }
        let above = (level.checked_add(1)?..=self.levels)
            .fold(0u64, |acc, l| acc.saturating_add(self.blocks_at(l)));
        above.checked_add(index)
    }

    /// Byte length of data block `index`: 4096, or the remainder for the last block.
    pub fn block_len(&self, index: u64) -> Option<usize> {
        if index >= self.leaves {
            return None;
        }
        let start = index.checked_mul(BLOCK)?;
        let len = self.content_size.checked_sub(start)?.min(BLOCK);
        usize::try_from(len).ok()
    }
}
