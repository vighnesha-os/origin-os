//! Seeded multi-fault soak of Merkle verification (ONBF §4.6; §13 item 3; CLAUDE.md §72).
//!
//! The judge is a model written from §4.6: its own shape arithmetic and its own hash
//! construction, with none of the crate's helpers. It predicts every verdict of
//! [`verify_block`], the exact error and exactly which hash blocks the trust cache gains.
//!
//! Sparse trees make every height affordable. A path needs only the hash blocks from one
//! data block up to the root, and `verify_block` reads nothing else, so a 4-level tree over
//! 8 GiB of content costs one path per case (plus a zeroed buffer of the MERKLE section's
//! length). Small trees are built whole, to check `build` and `root` as well.
//!
//! A failure is reproduced by its seed and case number; `ORIGIN_MUTATION_SCALE` scales the
//! case counts.

use std::collections::HashSet;

use origin_crypto::Sha256;

use crate::testing::{Reached, Rng, cases, mutate};
use crate::*;

// ------------------------------------------------------------------ the model

fn model_leaf(index: u64, block: &[u8]) -> [u8; 32] {
    let mut input = vec![0x00];
    input.extend(index.to_le_bytes());
    input.extend(block);
    input.resize(1 + 8 + BLOCK_SIZE, 0);
    Sha256::digest(&input)
}

fn model_node(level: u8, index: u64, hash_block: &[u8]) -> [u8; 32] {
    let mut input = vec![0x01, level];
    input.extend(index.to_le_bytes());
    input.extend(hash_block);
    Sha256::digest(&input)
}

fn model_root(content_size: u64, levels: u8, top: &[u8; 32]) -> [u8; 32] {
    let mut input = b"ONBF-MERKLE-1".to_vec();
    input.extend(content_size.to_le_bytes());
    input.push(levels);
    input.extend(top);
    Sha256::digest(&input)
}

/// §4.6 and errata G3, computed directly.
#[derive(Clone)]
struct ModelShape {
    content_size: u64,
    leaves: u64,
    levels: u8,
    /// `blocks[L]`: hash blocks at level `L`; `blocks[0]` is unused.
    blocks: Vec<u64>,
}

fn model_shape(content_size: u64) -> ModelShape {
    let leaves = content_size.div_ceil(4096);
    let mut blocks = vec![0];
    let mut hashes = leaves;
    while hashes > 1 {
        hashes = hashes.div_ceil(128);
        blocks.push(hashes);
    }
    ModelShape {
        content_size,
        leaves,
        levels: (blocks.len() - 1) as u8,
        blocks,
    }
}

impl ModelShape {
    fn section_len(&self) -> usize {
        self.blocks.iter().sum::<u64>() as usize * BLOCK_SIZE
    }

    /// Offset of hash block `j` of `level`: the highest level first (errata R3).
    fn offset(&self, level: u8, j: u64) -> usize {
        let above: u64 = self.blocks[usize::from(level) + 1..].iter().sum();
        (above + j) as usize * BLOCK_SIZE
    }

    fn block_len(&self, index: u64) -> Option<usize> {
        (index < self.leaves).then(|| {
            let rest = self.content_size - index * 4096;
            rest.min(4096) as usize
        })
    }
}

/// What `verify_block` must do: the error, or the hash blocks a success makes trusted.
fn model_verify(
    root: &[u8; 32],
    shape: &ModelShape,
    section: &[u8],
    index: u64,
    block: &[u8],
    trusted: &HashSet<(u8, u64)>,
) -> Result<Vec<(u8, u64)>, Error> {
    if section.len() != shape.section_len() {
        return Err(Error::HashBlocksLength {
            expected: shape.section_len() as u64,
            found: section.len() as u64,
        });
    }
    let Some(expected) = shape.block_len(index) else {
        return Err(Error::BlockIndex(index));
    };
    if block.len() != expected {
        return Err(Error::BlockLength {
            expected,
            found: block.len(),
        });
    }
    let mut hash = model_leaf(index, block);
    let mut child = index;
    let mut newly_trusted = Vec::new();
    for level in 1..=shape.levels {
        let j = child >> 7;
        let at = shape.offset(level, j);
        let hash_block = &section[at..at + BLOCK_SIZE];
        let slot = (child & 127) as usize * 32;
        if hash_block[slot..slot + 32] != hash {
            return Err(Error::HashMismatch { level });
        }
        if trusted.contains(&(level, j)) {
            return Ok(newly_trusted);
        }
        newly_trusted.push((level, j));
        hash = model_node(level, j, hash_block);
        child = j;
    }
    if model_root(shape.content_size, shape.levels, &hash) != *root {
        return Err(Error::RootMismatch);
    }
    Ok(newly_trusted)
}

/// The whole MERKLE section and root of `content`, level by level (§4.6 steps 1–4).
fn model_full_tree(content: &[u8]) -> (Vec<u8>, [u8; 32]) {
    let mut hashes: Vec<[u8; 32]> = content
        .chunks(BLOCK_SIZE)
        .enumerate()
        .map(|(i, block)| model_leaf(i as u64, block))
        .collect();
    let mut level = 0u8;
    let mut levels: Vec<Vec<u8>> = Vec::new();
    while hashes.len() > 1 {
        level += 1;
        let mut blocks = Vec::new();
        let mut next = Vec::new();
        for (j, chunk) in hashes.chunks(HASHES_PER_BLOCK).enumerate() {
            let mut hash_block = vec![0u8; BLOCK_SIZE];
            for (i, h) in chunk.iter().enumerate() {
                hash_block[i * 32..i * 32 + 32].copy_from_slice(h);
            }
            next.push(model_node(level, j as u64, &hash_block));
            blocks.extend(hash_block);
        }
        levels.push(blocks);
        hashes = next;
    }
    let top = hashes.first().copied().unwrap_or([0; 32]);
    let section = levels.iter().rev().flatten().copied().collect();
    (section, model_root(content.len() as u64, level, &top))
}

// ------------------------------------------------------------------ the sparse-tree soak

/// A cache that records what the crate asks and what it marks.
#[derive(Default)]
struct RecordingCache {
    trusted: HashSet<(u8, u64)>,
}

impl TrustCache for RecordingCache {
    fn is_trusted(&self, level: u8, index: u64) -> bool {
        self.trusted.contains(&(level, index))
    }
    fn mark_trusted(&mut self, level: u8, index: u64) {
        self.trusted.insert((level, index));
    }
}

/// `content_size` values covering every tree height, biased to each height's boundaries.
fn random_size(rng: &mut Rng) -> u64 {
    match rng.below(16) {
        0 => rng.below(4097) as u64,
        1 => 4096 * rng.pick(&[1, 2, 127, 128]),
        2..=4 => 4097 + rng.below(4096 * 127) as u64,
        5 => 4096 * rng.pick(&[129, 257, 16_383, 16_384]),
        6..=9 => (4096 * 128 + 1) + rng.below(4096 * 16_256) as u64,
        10 | 11 => 4096 * rng.pick(&[16_385, 16_513, 40_000]),
        // A three-level tree: at most 2^20 leaves keeps the zeroed section near 32 MiB.
        12..=14 => 4096 * 16_384 + 1 + rng.below(4096 * (1 << 20)) as u64,
        // The smallest four-level tree.
        _ => 4096 * (128 * 128 * 128) + 1 + rng.below(4096) as u64,
    }
}

/// Hash blocks of the largest tree `random_size` can make, plus room for over-long sections.
const BUFFER_BLOCKS: usize = 16_385 + 129 + 2 + 1 + 2;

/// Writes the path from data block `index` to the root into `section` and returns the
/// root. Bytes of the path's hash blocks outside the child's slot are random, so that
/// nothing depends on them being zero.
fn build_path(
    shape: &ModelShape,
    section: &mut [u8],
    index: u64,
    block: &[u8],
    rng: &mut Rng,
) -> [u8; 32] {
    let mut hash = model_leaf(index, block);
    let mut child = index;
    for level in 1..=shape.levels {
        let j = child >> 7;
        let at = shape.offset(level, j);
        let hash_block = &mut section[at..at + BLOCK_SIZE];
        hash_block.iter_mut().for_each(|b| *b = rng.byte());
        let slot = (child & 127) as usize * 32;
        hash_block[slot..slot + 32].copy_from_slice(&hash);
        hash = model_node(level, j, hash_block);
        child = j;
    }
    model_root(shape.content_size, shape.levels, &hash)
}

const SPARSE_SEED: u64 = 0x4D45_524B_4C45_5031; // "MERKLEP1"

fn error_name(e: Error) -> &'static str {
    match e {
        Error::TooLarge => "TooLarge",
        Error::HashBlocksLength { .. } => "HashBlocksLength",
        Error::BlockIndex(_) => "BlockIndex",
        Error::BlockLength { .. } => "BlockLength",
        Error::HashMismatch { level: 1 } => "HashMismatch 1",
        Error::HashMismatch { level: 2 } => "HashMismatch 2",
        Error::HashMismatch { level: 3 } => "HashMismatch 3",
        Error::HashMismatch { .. } => "HashMismatch 4",
        Error::RootMismatch => "RootMismatch",
    }
}

#[test]
fn mutated_paths_verify_exactly_when_the_model_says_so() {
    let mut buffer = vec![0u8; BUFFER_BLOCKS * BLOCK_SIZE];
    let mut reached = Reached::default();
    let total = cases(2500);
    for case in 0..total {
        let mut rng = Rng::for_case(SPARSE_SEED, case);
        let size = random_size(&mut rng);
        let shape = model_shape(size);
        // The pristine tree. An empty tree has no block to verify, so index 0 with no bytes
        // is the first case it must refuse.
        let index = if shape.leaves == 0 {
            0
        } else {
            rng.next() % shape.leaves
        };
        let block_len = shape.block_len(index).unwrap_or(0);
        let mut block: Vec<u8> = (0..block_len).map(|_| rng.byte()).collect();
        let original_block = block.clone();
        let section_len = shape.section_len();
        let root = build_path(&shape, &mut buffer[..section_len], index, &block, &mut rng);
        reached.hit(match shape.levels {
            0 => "levels 0",
            1 => "levels 1",
            2 => "levels 2",
            3 => "levels 3",
            _ => "levels 4",
        });

        // Which hash blocks to clear when this case is done.
        let path: Vec<(u8, u64)> = (1..=shape.levels)
            .map(|level| (level, index >> (7 * u32::from(level))))
            .collect();
        let mut dirty: Vec<usize> = path
            .iter()
            .map(|&(level, j)| shape.offset(level, j))
            .collect();
        let path_offsets = dirty.clone();

        // The cache: empty, or trusting part of the path, as a warm page-fault path does.
        let mut cache = RecordingCache::default();
        if rng.chance(30) {
            for &(level, j) in &path {
                if rng.chance(50) {
                    cache.trusted.insert((level, j));
                }
            }
        }
        let trusted_before = cache.trusted.clone();

        // One to four faults; a tenth of the cases are left pristine, so success stays
        // well covered at every height.
        let (mut use_root, mut use_index, mut use_len, mut use_shape) =
            (root, index, section_len, shape.clone());
        let fault_count = if rng.chance(10) { 0 } else { 1 + rng.below(4) };
        for _ in 0..fault_count {
            match rng.below(11) {
                0 | 1 if !block.is_empty() => {
                    let at = rng.below(block.len());
                    block[at] ^= 1 << rng.below(8);
                }
                2 | 3 if !path.is_empty() => {
                    // The child's own slot at a level of the path.
                    let i = rng.below(path.len());
                    let (level, _) = path[i];
                    let child = index >> (7 * (u32::from(level) - 1));
                    let at = path_offsets[i] + (child & 127) as usize * 32 + rng.below(32);
                    buffer[at] ^= 1 << rng.below(8);
                }
                4 if !path.is_empty() => {
                    // Elsewhere in a hash block on the path: covered by its parent.
                    let i = rng.below(path.len());
                    let at = path_offsets[i] + rng.below(BLOCK_SIZE);
                    buffer[at] ^= 1 << rng.below(8);
                }
                5 if section_len > 0 => {
                    // Anywhere in the section: on the path it matters, off it nothing does.
                    let b = rng.below(section_len / BLOCK_SIZE) * BLOCK_SIZE;
                    dirty.push(b);
                    let at = b + rng.below(BLOCK_SIZE);
                    buffer[at] ^= 1 << rng.below(8);
                }
                6 => use_root[rng.below(32)] ^= 1 << rng.below(8),
                7 => {
                    use_index = match rng.below(4) {
                        0 => index.wrapping_add(1),
                        1 => shape.leaves.wrapping_add(rng.below(3) as u64),
                        2 => rng.edge_u64(),
                        _ => index ^ (1 << rng.below(40)),
                    };
                }
                8 => {
                    let mut data = block.clone();
                    mutate(&mut data, &mut rng);
                    block = data;
                }
                9 => {
                    use_len = match rng.below(3) {
                        0 => section_len.saturating_sub(1 + rng.below(4096)),
                        1 => section_len + 1 + rng.below(4096),
                        _ => section_len.saturating_sub(BLOCK_SIZE),
                    };
                }
                _ => use_shape = model_shape(size + 1 + rng.below(8192) as u64),
            }
        }

        let section = &buffer[..use_len];
        let want = model_verify(
            &use_root,
            &use_shape,
            section,
            use_index,
            &block,
            &trusted_before,
        );
        let crate_shape = Shape::new(use_shape.content_size).unwrap();
        let got = verify_block(
            &use_root,
            &crate_shape,
            section,
            use_index,
            &block,
            &mut cache,
        );
        assert_eq!(
            got,
            want.as_ref().map(|_| ()).map_err(|e| *e),
            "case {case}: size {size}, index {index}, {fault_count} faults"
        );
        match &want {
            Ok(marks) => {
                reached.hit("ok");
                if !trusted_before.is_empty() {
                    reached.hit("ok via trust cache");
                }
                let mut expected_cache = trusted_before.clone();
                expected_cache.extend(marks.iter().copied());
                assert_eq!(cache.trusted, expected_cache, "case {case}");
                // Success means the block is the genuine one, however the rest was hit. Only
                // a cold walk reaches the root, which binds the content size; a trusted
                // ancestor is valid for its own tree (see `TrustCache`), so a warm cache
                // handed another tree's shape is the caller's contract broken, not a forgery.
                if trusted_before.is_empty() {
                    assert_eq!(
                        use_shape.content_size, size,
                        "case {case}: wrong content size"
                    );
                    assert_eq!(use_root, root, "case {case}: a wrong root verified");
                }
                if use_shape.content_size == size {
                    assert_eq!(
                        block, original_block,
                        "case {case}: a tampered block verified"
                    );
                    assert_eq!(
                        use_index, index,
                        "case {case}: another block's hash verified"
                    );
                }
            }
            Err(e) => {
                reached.hit(error_name(*e));
                assert_eq!(
                    cache.trusted, trusted_before,
                    "case {case}: a failure was trusted"
                );
            }
        }

        for &at in &dirty {
            buffer[at..at + BLOCK_SIZE].fill(0);
        }
        if use_len > section_len {
            buffer[section_len..use_len].fill(0);
        }
    }
    reached.require(
        "sparse Merkle soak",
        &[
            "levels 0",
            "levels 1",
            "levels 2",
            "levels 3",
            "levels 4",
            "ok",
            "ok via trust cache",
            "HashBlocksLength",
            "BlockIndex",
            "BlockLength",
            "HashMismatch 1",
            "HashMismatch 2",
            "HashMismatch 3",
            "HashMismatch 4",
            "RootMismatch",
        ],
    );
    assert!(
        reached.count("ok") >= total as u64 / 20,
        "{}",
        reached.count("ok")
    );
}

// ------------------------------------------------------------------ whole small trees

const WHOLE_SEED: u64 = 0x4D45_524B_4C45_5731; // "MERKLEW1"

/// Whole trees: `build` and `root` agree with the model byte for byte (the encoder's round
/// trip), and multi-fault damage to a real MERKLE section is caught exactly when the path
/// it touches is the one being verified.
#[test]
fn built_trees_match_the_model_and_survive_multi_fault_damage() {
    let mut reached = Reached::default();
    for case in 0..cases(120) {
        let mut rng = Rng::for_case(WHOLE_SEED, case);
        // Mostly a few dozen blocks; hashing a whole tree is the cost of this soak.
        let size = match rng.below(16) {
            0 | 1 => rng.below(4097),
            2 => 4096 * rng.pick(&[2, 128, 129]),
            3 => 4096 * 129 + rng.below(4096 * 40),
            _ => 4097 + rng.below(4096 * 40),
        };
        let content: Vec<u8> = (0..size).map(|_| rng.byte()).collect();
        let (model_section, model_digest) = model_full_tree(&content);
        let shape = Shape::new(size as u64).unwrap();
        assert_eq!(
            shape.hash_blocks_len() as usize,
            model_section.len(),
            "case {case}"
        );
        let mut section = vec![0xEE; model_section.len()];
        assert_eq!(
            build(&content, &mut section),
            Ok(model_digest),
            "case {case}"
        );
        assert!(
            section == model_section,
            "case {case}: `build` differs from the model"
        );
        assert_eq!(root(&content), Ok(model_digest), "case {case}");

        // A wrong-size output buffer is refused before anything is written.
        let mut wrong = vec![0xEE; model_section.len() + 1];
        assert_eq!(
            build(&content, &mut wrong),
            Err(Error::HashBlocksLength {
                expected: model_section.len() as u64,
                found: wrong.len() as u64,
            }),
            "case {case}"
        );
        assert!(
            wrong.iter().all(|&b| b == 0xEE),
            "case {case}: partial write"
        );

        // Damage to real hash blocks.
        let model = model_shape(size as u64);
        let mut damaged = section.clone();
        if !damaged.is_empty() {
            for _ in 0..1 + rng.below(4) {
                let at = rng.below(damaged.len());
                damaged[at] ^= 1 << rng.below(8);
            }
        }
        let mut cache = RecordingCache::default();
        for index in 0..shape.leaves() {
            let block_len = shape.block_len(index).unwrap();
            let start = index as usize * BLOCK_SIZE;
            let block = &content[start..start + block_len];
            let want = model_verify(
                &model_digest,
                &model,
                &damaged,
                index,
                block,
                &cache.trusted,
            );
            let got = verify_block(&model_digest, &shape, &damaged, index, block, &mut cache);
            assert_eq!(
                got,
                want.as_ref().map(|_| ()).map_err(|e| *e),
                "case {case}: block {index}"
            );
            reached.hit(if got.is_ok() { "ok" } else { "damaged" });
        }

        // A flipped content byte changes the root; the root covers the content size too.
        if size > 0 {
            let mut altered = content.clone();
            let at = rng.below(size);
            altered[at] ^= 1 << rng.below(8);
            assert_ne!(root(&altered), Ok(model_digest), "case {case}");
        }
    }
    reached.require("whole-tree soak", &["ok", "damaged"]);
}

// ------------------------------------------------------------------ shape arithmetic

/// `Shape` is total over every `content_size`, agrees with the model, and places hash
/// blocks without collisions. It sizes MERKLE sections from an untrusted table field.
#[test]
fn shapes_are_total_and_match_the_model() {
    let mut rng = Rng::new(0x4D45_524B_5348_5031);
    for case in 0..cases(30_000) {
        let size = rng.edge_u64();
        let shape = Shape::new(size);
        if size > MAX_CONTENT {
            assert_eq!(shape, Err(Error::TooLarge), "case {case}");
            continue;
        }
        let shape = shape.unwrap();
        let model = model_shape(size);
        assert_eq!(shape.content_size(), size, "case {case}");
        assert_eq!(shape.leaves(), model.leaves, "case {case}");
        assert_eq!(shape.levels(), model.levels, "case {case}");
        assert!(usize::from(shape.levels()) <= MAX_LEVELS, "case {case}");
        assert_eq!(
            shape.hash_blocks_len() as usize,
            model.section_len(),
            "case {case}: {size}"
        );
        assert_eq!(
            shape.hash_blocks(),
            onbf_format::merkle_hash_blocks(size),
            "case {case}"
        );

        // Positions follow the storage order (R3), and stop at each level's end.
        for level in 1..=shape.levels() {
            let count = model.blocks[usize::from(level)];
            assert_eq!(shape.blocks_at(level), count, "case {case}");
            for j in [0, count / 2, count - 1, rng.next() % count] {
                assert_eq!(
                    shape
                        .block_position(level, j)
                        .map(|p| p as usize * BLOCK_SIZE),
                    Some(model.offset(level, j)),
                    "case {case}"
                );
            }
            assert_eq!(shape.block_position(level, count), None, "case {case}");
            assert_eq!(
                shape.block_position(level, rng.edge_u64() | count),
                None,
                "case {case}"
            );
        }
        assert_eq!(shape.blocks_at(0), 0, "case {case}");
        assert_eq!(shape.blocks_at(shape.levels() + 1), 0, "case {case}");

        // Data blocks: the last holds the remainder; nothing lies beyond it.
        if shape.leaves() > 0 {
            let last = shape.leaves() - 1;
            assert_eq!(shape.block_len(last), model.block_len(last), "case {case}");
            let probe = rng.next() % shape.leaves();
            assert_eq!(
                shape.block_len(probe),
                model.block_len(probe),
                "case {case}"
            );
        }
        assert_eq!(shape.block_len(shape.leaves()), None, "case {case}");
        assert_eq!(shape.block_len(u64::MAX), None, "case {case}");
    }
}
