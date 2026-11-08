//! Streaming tree construction: the root, and optionally the MERKLE section.

use core::ops::Range;

use origin_crypto::Sha256;

use crate::{
    BLOCK_SIZE, Error, HASH_SIZE, HASHES_PER_BLOCK, MAX_LEVELS, Shape, ZEROS, leaf_hash,
    node_hasher, root_hash,
};

/// A partly absorbed hash block at one level: the running SHA-256 of `hL[j]`, how many child
/// hashes it holds, and `j`.
struct Node {
    hasher: Sha256,
    count: usize,
    index: u64,
}

impl Node {
    const fn new() -> Self {
        Self {
            hasher: Sha256::new(),
            count: 0,
            index: 0,
        }
    }

    /// Zero-pads the block to 128 hashes, finishes `hL[j]` and moves on to `j + 1`.
    fn complete(&mut self) -> [u8; HASH_SIZE] {
        let missing = HASHES_PER_BLOCK
            .saturating_sub(self.count)
            .saturating_mul(HASH_SIZE);
        self.hasher.update(ZEROS.get(..missing).unwrap_or(&[]));
        let hash = core::mem::replace(&mut self.hasher, Sha256::new()).finalize();
        self.count = 0;
        self.index = self.index.wrapping_add(1);
        hash
    }
}

/// Builds a tree bottom-up from leaf hashes. `on_hash(level, index, hash)` sees every hash
/// in the tree, including the top, in the order it is produced.
struct Builder<F: FnMut(u8, u64, &[u8; HASH_SIZE])> {
    levels: u8,
    /// `nodes[L]` absorbs level-`L` hashes into a level-`L+1` hash block.
    nodes: [Node; MAX_LEVELS],
    /// Number of hashes produced so far at each level.
    produced: [u64; MAX_LEVELS + 1],
    top: [u8; HASH_SIZE],
    on_hash: F,
}

impl<F: FnMut(u8, u64, &[u8; HASH_SIZE])> Builder<F> {
    fn new(levels: u8, on_hash: F) -> Self {
        Self {
            levels,
            nodes: [const { Node::new() }; MAX_LEVELS],
            produced: [0; MAX_LEVELS + 1],
            top: [0; HASH_SIZE],
            on_hash,
        }
    }

    /// Adds a hash at `level`, completing hash blocks upward as they fill (iteratively).
    fn push(&mut self, level: u8, hash: [u8; HASH_SIZE]) {
        let (mut level, mut hash) = (level, hash);
        loop {
            let index = match self.produced.get_mut(usize::from(level)) {
                Some(n) => {
                    let i = *n;
                    *n = n.wrapping_add(1);
                    i
                }
                None => return,
            };
            (self.on_hash)(level, index, &hash);
            if level >= self.levels {
                self.top = hash;
                return;
            }
            let Some(parent_level) = level.checked_add(1) else {
                return;
            };
            let Some(node) = self.nodes.get_mut(usize::from(level)) else {
                return;
            };
            if node.count == 0 {
                node.hasher = node_hasher(parent_level, node.index);
            }
            node.hasher.update(&hash);
            node.count = node.count.saturating_add(1);
            if node.count < HASHES_PER_BLOCK {
                return;
            }
            hash = node.complete();
            level = parent_level;
        }
    }

    /// Completes the partial hash block of every level, lowest first, and returns `top`.
    fn finish(mut self) -> [u8; HASH_SIZE] {
        for level in 0..self.levels {
            let partial = self
                .nodes
                .get(usize::from(level))
                .is_some_and(|n| n.count > 0);
            if !partial {
                continue;
            }
            let Some(hash) = self.nodes.get_mut(usize::from(level)).map(Node::complete) else {
                continue;
            };
            if let Some(parent_level) = level.checked_add(1) {
                self.push(parent_level, hash);
            }
        }
        self.top
    }
}

/// Hashes `content` into a tree and returns the root, calling `on_hash` for every hash.
fn run(
    content: &[u8],
    on_hash: impl FnMut(u8, u64, &[u8; HASH_SIZE]),
) -> Result<[u8; HASH_SIZE], Error> {
    let size = u64::try_from(content.len()).map_err(|_| Error::TooLarge)?;
    let shape = Shape::new(size)?;
    let top = if shape.leaves() == 0 {
        [0; HASH_SIZE]
    } else {
        let mut builder = Builder::new(shape.levels(), on_hash);
        for (i, block) in content.chunks(BLOCK_SIZE).enumerate() {
            builder.push(0, leaf_hash(i as u64, block));
        }
        builder.finish()
    };
    Ok(root_hash(size, shape.levels(), &top))
}

/// The Merkle root of `content`, in one streaming pass with no allocation.
pub fn root(content: &[u8]) -> Result<[u8; HASH_SIZE], Error> {
    run(content, |_, _, _| {})
}

/// The slot hash `index` of `level` occupies in the MERKLE section: hash block `(level + 1,
/// index / 128)`, slot `index % 128`. `None` for the top hash, which no hash block holds.
fn hash_slot(shape: &Shape, level: u8, index: u64) -> Option<Range<usize>> {
    let position = shape.block_position(level.checked_add(1)?, index >> 7)?;
    let start = position
        .checked_mul(BLOCK_SIZE as u64)?
        .checked_add((index & 127).checked_mul(HASH_SIZE as u64)?)?;
    let start = usize::try_from(start).ok()?;
    Some(start..start.checked_add(HASH_SIZE)?)
}

/// The zero padding after the last hash of hash-block `level`, or `None` if that block is
/// full. The padding is part of the block that was hashed, but no hash is ever written there.
fn padding(shape: &Shape, level: u8) -> Option<Range<usize>> {
    let hashes = match level.checked_sub(1)? {
        0 => shape.leaves(),
        below => shape.blocks_at(below),
    };
    let used = hashes % HASHES_PER_BLOCK as u64;
    if used == 0 {
        return None;
    }
    let position = shape.block_position(level, shape.blocks_at(level).checked_sub(1)?)?;
    let start = position
        .checked_mul(BLOCK_SIZE as u64)?
        .checked_add(used.checked_mul(HASH_SIZE as u64)?)?;
    let end = position.checked_add(1)?.checked_mul(BLOCK_SIZE as u64)?;
    Some(usize::try_from(start).ok()?..usize::try_from(end).ok()?)
}

/// Checks `content` against `root` and, in the same pass, that `hash_blocks` is byte for byte
/// the MERKLE section [`build`] writes for `content`, padding included. It hashes exactly what
/// [`root`] hashes and allocates nothing: each stored hash is compared as it is produced.
///
/// A wrong root is reported first ([`Error::RootMismatch`]): then the content is at fault,
/// and every hash block above the change would differ anyway. Otherwise a bad hash block is
/// [`Error::HashMismatch`], at the level of the first one found.
pub fn verify_tree(
    root: &[u8; HASH_SIZE],
    content: &[u8],
    hash_blocks: &[u8],
) -> Result<(), Error> {
    let size = u64::try_from(content.len()).map_err(|_| Error::TooLarge)?;
    let shape = Shape::new(size)?;
    let found = u64::try_from(hash_blocks.len()).unwrap_or(u64::MAX);
    if found != shape.hash_blocks_len() {
        return Err(Error::HashBlocksLength {
            expected: shape.hash_blocks_len(),
            found,
        });
    }
    let mut mismatch: Option<u8> = None;
    let computed = run(content, |level, index, hash| {
        let Some(slot) = hash_slot(&shape, level, index) else {
            return;
        };
        if hash_blocks.get(slot).is_none_or(|stored| stored != hash) {
            mismatch.get_or_insert(level.saturating_add(1));
        }
    })?;
    if computed != *root {
        return Err(Error::RootMismatch);
    }
    if let Some(level) = mismatch {
        return Err(Error::HashMismatch { level });
    }
    for level in 1..=shape.levels() {
        let clean = padding(&shape, level)
            .and_then(|range| hash_blocks.get(range))
            .is_none_or(|bytes| bytes.iter().all(|&b| b == 0));
        if !clean {
            return Err(Error::HashMismatch { level });
        }
    }
    Ok(())
}

/// Writes the MERKLE section for `content` into `hash_blocks`, which must be exactly
/// `Shape::hash_blocks_len()` bytes, and returns the root.
pub fn build(content: &[u8], hash_blocks: &mut [u8]) -> Result<[u8; HASH_SIZE], Error> {
    let size = u64::try_from(content.len()).map_err(|_| Error::TooLarge)?;
    let shape = Shape::new(size)?;
    let found = u64::try_from(hash_blocks.len()).unwrap_or(u64::MAX);
    if found != shape.hash_blocks_len() {
        return Err(Error::HashBlocksLength {
            expected: shape.hash_blocks_len(),
            found,
        });
    }
    hash_blocks.fill(0);
    run(content, |level, index, hash| {
        let Some(dst) = hash_slot(&shape, level, index).and_then(|s| hash_blocks.get_mut(s)) else {
            return;
        };
        for (d, s) in dst.iter_mut().zip(hash) {
            *d = *s;
        }
    })
}
