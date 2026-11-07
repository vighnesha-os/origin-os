//! Tests: reference vectors, shape cross-checks, verification of every block, the §13
//! "Merkle block swap" attack class, and the trust cache.

use std::cell::Cell;
use std::collections::HashSet;

use crate::*;

/// The content `tools/merkle_reference.py` hashes for size `n`.
fn content(n: usize) -> Vec<u8> {
    (0..n).map(|i| ((i + n) % 251) as u8).collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Content, its shape, its MERKLE section and its root.
fn tree_for(n: usize) -> (Vec<u8>, Shape, Vec<u8>, [u8; 32]) {
    let data = content(n);
    let shape = Shape::new(n as u64).unwrap();
    let mut section = vec![0u8; shape.hash_blocks_len() as usize];
    let root = build(&data, &mut section).unwrap();
    (data, shape, section, root)
}

fn data_block(data: &[u8], i: u64) -> &[u8] {
    let start = i as usize * BLOCK_SIZE;
    &data[start..(start + BLOCK_SIZE).min(data.len())]
}

#[test]
fn matches_independent_reference_vectors() {
    let file = include_str!("../tests/vectors/merkle.txt");
    let mut count = 0;
    for line in file
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let cols: Vec<&str> = line.split_whitespace().collect();
        let n: usize = cols[0].parse().unwrap();
        let (levels, blocks): (u8, u64) = (cols[1].parse().unwrap(), cols[2].parse().unwrap());
        let shape = Shape::new(n as u64).unwrap();
        assert_eq!(
            (shape.levels(), shape.hash_blocks()),
            (levels, blocks),
            "shape, size {n}"
        );

        let data = content(n);
        let streamed = root(&data).unwrap();
        assert_eq!(hex(&streamed), cols[3], "root, size {n}");
        let mut section = vec![0xEEu8; shape.hash_blocks_len() as usize];
        assert_eq!(
            build(&data, &mut section).unwrap(),
            streamed,
            "build root, size {n}"
        );
        assert_eq!(
            hex(&origin_crypto::Sha256::digest(&section)),
            cols[4],
            "MERKLE section, size {n}"
        );
        count += 1;
    }
    assert_eq!(count, 17);
}

#[test]
fn empty_content_root_follows_the_spec_formula() {
    let mut expected = origin_crypto::Sha256::new();
    expected.update(b"ONBF-MERKLE-1");
    expected.update(&0u64.to_le_bytes());
    expected.update(&[0]);
    expected.update(&[0; 32]);
    assert_eq!(root(&[]).unwrap(), expected.finalize());
}

#[test]
fn shape_agrees_with_the_container_parser() {
    let mut sizes: Vec<u64> = (0..=300).map(|b| b * 4096).collect();
    sizes.extend((0..=300).map(|b| b * 4096 + 1));
    sizes.extend([
        128 * 128 * 4096,
        128 * 128 * 4096 + 1,
        128 * 128 * 128 * 4096 + 1,
        MAX_CONTENT,
    ]);
    for size in sizes {
        let shape = Shape::new(size).unwrap();
        assert_eq!(
            shape.hash_blocks(),
            onbf_format::merkle_hash_blocks(size),
            "size {size}"
        );
    }
    assert_eq!(Shape::new(MAX_CONTENT).unwrap().levels(), 4);
    assert_eq!(Shape::new(MAX_CONTENT + 1), Err(Error::TooLarge));
}

#[test]
fn root_binds_the_content_size() {
    // A trailing zero byte inside the last block leaves every leaf hash unchanged, but the
    // root covers content_size, so the roots must differ.
    let data = content(5000);
    let mut longer = data.clone();
    longer.push(0);
    assert_ne!(root(&data).unwrap(), root(&longer).unwrap());
}

#[test]
fn every_block_verifies_for_one_and_two_levels() {
    for n in [1, 4096, 4097, 128 * 4096, 129 * 4096, 255 * 4096 + 7] {
        let (data, shape, section, root) = tree_for(n);
        for i in 0..shape.leaves() {
            let r = verify_block(
                &root,
                &shape,
                &section,
                i,
                data_block(&data, i),
                &mut NoCache,
            );
            assert_eq!(r, Ok(()), "size {n} block {i}");
        }
    }
}

#[test]
fn three_level_tree_verifies_with_and_without_cache() {
    let n = 128 * 128 * 4096 + 1;
    let (data, shape, section, root) = tree_for(n);
    assert_eq!(shape.levels(), 3);
    for i in [0, 1, 127, 128, 16383, 16384] {
        assert_eq!(
            verify_block(
                &root,
                &shape,
                &section,
                i,
                data_block(&data, i),
                &mut NoCache
            ),
            Ok(()),
            "block {i}"
        );
    }
    let mut cache = SetCache::default();
    for i in 0..shape.leaves() {
        assert_eq!(
            verify_block(&root, &shape, &section, i, data_block(&data, i), &mut cache),
            Ok(()),
            "block {i}"
        );
    }
    // Every hash block on some path was cached: 129 + 2 + 1.
    assert_eq!(cache.trusted.len() as u64, shape.hash_blocks());
}

/// A HashSet-backed cache that counts lookups.
#[derive(Default)]
struct SetCache {
    trusted: HashSet<(u8, u64)>,
    lookups: Cell<u32>,
}

impl TrustCache for SetCache {
    fn is_trusted(&self, level: u8, index: u64) -> bool {
        self.lookups.set(self.lookups.get() + 1);
        self.trusted.contains(&(level, index))
    }
    fn mark_trusted(&mut self, level: u8, index: u64) {
        self.trusted.insert((level, index));
    }
}

#[test]
fn cache_stops_at_the_first_trusted_block_and_never_trusts_failures() {
    let (data, shape, section, root) = tree_for(129 * 4096);
    let mut cache = SetCache::default();
    assert_eq!(
        verify_block(&root, &shape, &section, 0, data_block(&data, 0), &mut cache),
        Ok(())
    );
    assert_eq!(cache.trusted.len(), 2, "both hash blocks on block 0's path");
    // Block 1 shares block 0's level-1 hash block: one lookup, then it stops.
    cache.lookups.set(0);
    assert_eq!(
        verify_block(&root, &shape, &section, 1, data_block(&data, 1), &mut cache),
        Ok(())
    );
    assert_eq!(cache.lookups.get(), 1);
    // A tampered block still fails against a warm cache, and adds nothing to it.
    let mut bad = data_block(&data, 2).to_vec();
    bad[7] ^= 1;
    let before = cache.trusted.clone();
    assert_eq!(
        verify_block(&root, &shape, &section, 2, &bad, &mut cache),
        Err(Error::HashMismatch { level: 1 })
    );
    // A block under the other level-1 hash block fails without touching the cache.
    let mut bad = data_block(&data, 128).to_vec();
    bad[0] ^= 1;
    assert_eq!(
        verify_block(&root, &shape, &section, 128, &bad, &mut cache),
        Err(Error::HashMismatch { level: 1 })
    );
    assert_eq!(cache.trusted, before);
}

// ------------------------------------------------------------------ attacks (§13 item 4)

#[test]
fn tampered_data_is_rejected_at_every_tree_height() {
    for (n, want) in [
        (4096, Error::RootMismatch),
        (8192, Error::HashMismatch { level: 1 }),
        (129 * 4096, Error::HashMismatch { level: 1 }),
    ] {
        let (data, shape, section, root) = tree_for(n);
        let mut bad = data_block(&data, 0).to_vec();
        bad[4095] ^= 0x80;
        assert_eq!(
            verify_block(&root, &shape, &section, 0, &bad, &mut NoCache),
            Err(want),
            "size {n}"
        );
    }
}

#[test]
fn swapped_data_blocks_are_rejected() {
    // Block contents are bound to their index (le64(i) in the leaf hash).
    let (data, shape, section, root) = tree_for(4 * 4096);
    let r = verify_block(
        &root,
        &shape,
        &section,
        1,
        data_block(&data, 2),
        &mut NoCache,
    );
    assert_eq!(r, Err(Error::HashMismatch { level: 1 }));
}

#[test]
fn swapped_hash_blocks_are_rejected() {
    // 257 blocks: one level-2 block at position 0, level-1 blocks at 1, 2 and 3.
    let (data, shape, mut section, root) = tree_for(257 * 4096);
    let (a, b) = (BLOCK_SIZE, 2 * BLOCK_SIZE);
    let first: Vec<u8> = section[a..a + BLOCK_SIZE].to_vec();
    section.copy_within(b..b + BLOCK_SIZE, a);
    section[b..b + BLOCK_SIZE].copy_from_slice(&first);
    for i in [0, 128] {
        let r = verify_block(
            &root,
            &shape,
            &section,
            i,
            data_block(&data, i),
            &mut NoCache,
        );
        assert_eq!(r, Err(Error::HashMismatch { level: 1 }), "block {i}");
    }
}

#[test]
fn tampered_hash_block_padding_and_upper_levels_are_rejected() {
    let (data, shape, section, root) = tree_for(129 * 4096);
    // The last level-1 hash block holds one hash; its padding is covered by its parent.
    let last_l1 = 2 * BLOCK_SIZE;
    let mut padded = section.clone();
    padded[last_l1 + 4000] = 1;
    assert_eq!(
        verify_block(
            &root,
            &shape,
            &padded,
            128,
            data_block(&data, 128),
            &mut NoCache
        ),
        Err(Error::HashMismatch { level: 2 })
    );
    // A slot in the level-2 block.
    let mut upper = section.clone();
    upper[5] ^= 1;
    assert_eq!(
        verify_block(&root, &shape, &upper, 0, data_block(&data, 0), &mut NoCache),
        Err(Error::HashMismatch { level: 2 })
    );
    // An unused slot of the level-2 block is covered by the root.
    let mut unused = section.clone();
    unused[3 * 32] = 1;
    assert_eq!(
        verify_block(
            &root,
            &shape,
            &unused,
            0,
            data_block(&data, 0),
            &mut NoCache
        ),
        Err(Error::RootMismatch)
    );
}

#[test]
fn wrong_root_size_index_or_lengths_are_rejected() {
    let (data, shape, section, root) = tree_for(3 * 4096 + 10);
    let block = |i| data_block(&data, i);
    let mut other = root;
    other[0] ^= 1;
    assert_eq!(
        verify_block(&other, &shape, &section, 0, block(0), &mut NoCache),
        Err(Error::RootMismatch)
    );
    // The same bytes under a different content_size.
    let wrong = Shape::new(3 * 4096 + 11).unwrap();
    assert_eq!(
        verify_block(&root, &wrong, &section, 0, block(0), &mut NoCache),
        Err(Error::RootMismatch)
    );
    assert_eq!(
        verify_block(&root, &shape, &section, 4, block(3), &mut NoCache),
        Err(Error::BlockIndex(4))
    );
    assert_eq!(
        verify_block(
            &root,
            &shape,
            &section,
            3,
            &data[3 * 4096..3 * 4096 + 9],
            &mut NoCache
        ),
        Err(Error::BlockLength {
            expected: 10,
            found: 9
        })
    );
    assert_eq!(
        verify_block(&root, &shape, &section[..4095], 0, block(0), &mut NoCache),
        Err(Error::HashBlocksLength {
            expected: 4096,
            found: 4095
        })
    );
    let mut too_small = vec![0u8; 100];
    assert_eq!(
        build(&data, &mut too_small),
        Err(Error::HashBlocksLength {
            expected: 4096,
            found: 100
        })
    );
}

/// Throughput of `root` on the build host (portable SHA-256; indicative only). Run with
/// `cargo test -p onbf-merkle --release -- --ignored --nocapture throughput`.
#[test]
#[ignore = "benchmark; run explicitly in release mode"]
fn throughput() {
    let data = vec![0x5Au8; 256 << 20];
    let start = std::time::Instant::now();
    let r = root(std::hint::black_box(&data)).unwrap();
    let secs = start.elapsed().as_secs_f64();
    println!(
        "Merkle root: {:.0} MiB/s (root starts {:02x})",
        256.0 / secs,
        r[0]
    );
}
