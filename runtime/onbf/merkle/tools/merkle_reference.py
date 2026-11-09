#!/usr/bin/env python3
"""Reference Merkle-SHA256-4K (docs/spec/ONBF.md §4.6, errata G2 and G3) for onbf-merkle.

A deliberately different structure from the Rust crate: whole levels are held as lists
and hash blocks are materialized, with no streaming. It uses Python's hashlib (OpenSSL)
for SHA-256, so the vectors check onbf-merkle and origin-crypto together.

Each vector hashes a deterministic message: byte i of the n-byte content is
(i + n) mod 251. src/tests.rs rebuilds the same content.

Output, one line per size: size levels hash_blocks root sha256(MERKLE section).
Usage: python3 -I merkle_reference.py > ../tests/vectors/merkle.txt
Host-only engineering tool; nothing here ships.
"""

import hashlib
import struct

BLOCK = 4096
FANOUT = 128

SIZES = [
    0, 1, 100, 4095, 4096, 4097, 8192, 12288,
    128 * BLOCK - 1, 128 * BLOCK, 128 * BLOCK + 1, 129 * BLOCK,
    255 * BLOCK + 7, 256 * BLOCK, 257 * BLOCK,
    128 * 128 * BLOCK, 128 * 128 * BLOCK + 1,
]


def content(n: int) -> bytes:
    period = bytes((j + n) % 251 for j in range(251))
    return (period * (n // 251 + 1))[:n]


def sha256(*parts: bytes) -> bytes:
    h = hashlib.sha256()
    for p in parts:
        h.update(p)
    return h.digest()


def tree(data: bytes):
    """Returns (levels, top, hash_blocks_by_level) where hash_blocks_by_level[L] lists the
    4 KiB hash blocks hashed to produce level-L hashes (L >= 1)."""
    n = len(data)
    if n == 0:
        return 0, bytes(32), {}
    level_hashes = []
    for i in range(0, n, BLOCK):
        block = data[i : i + BLOCK].ljust(BLOCK, b"\0")
        level_hashes.append(sha256(b"\x00", struct.pack("<Q", i // BLOCK), block))
    blocks_by_level = {}
    level = 0
    while len(level_hashes) > 1:
        level += 1  # G2: L is the level of the hash being produced
        blocks = [
            b"".join(level_hashes[k : k + FANOUT]).ljust(BLOCK, b"\0")
            for k in range(0, len(level_hashes), FANOUT)
        ]
        blocks_by_level[level] = blocks
        level_hashes = [
            sha256(b"\x01", struct.pack("<B", level), struct.pack("<Q", j), b)
            for j, b in enumerate(blocks)
        ]
    return level, level_hashes[0], blocks_by_level  # G3: levels = level of top


def main() -> None:
    print("# Merkle-SHA256-4K vectors for onbf-merkle, from tools/merkle_reference.py (hashlib).")
    print("# Content n: byte i = (i + n) mod 251.")
    print("# Columns: size levels hash_blocks root sha256(MERKLE section)")
    for n in SIZES:
        levels, top, by_level = tree(content(n))
        root = sha256(b"ONBF-MERKLE-1", struct.pack("<Q", n), struct.pack("<B", levels), top)
        # §4.6 step 4: highest level first; ascending within a level.
        section = b"".join(b for lvl in range(levels, 0, -1) for b in by_level[lvl])
        count = sum(len(v) for v in by_level.values())
        print(n, levels, count, root.hex(), hashlib.sha256(section).hexdigest())


if __name__ == "__main__":
    main()
