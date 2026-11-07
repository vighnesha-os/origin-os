# ADR-0006: OriginFS v0, the native filesystem on-disk format

| | |
|---|---|
| Status | **Proposed** (2026-11-08). A format decision: it needs the owner's approval before it is Accepted (CLAUDE.md §84). A host-testable, read-only parser and an image builder are implemented against this proposal (allowed by §84); nothing is mounted in the shipping kernel until the owner accepts it. |
| Scope | The on-disk layout of the native filesystem: the superblock, inodes, directories, and how a file's bytes are found. Read first; write and crash-consistency are a later revision. |
| Requires | CLAUDE.md §39 (storage architecture, VFS), §40 (native filesystem requirements), §41 (filesystem correctness) |
| Builds on | the block layer (a 4 KiB page cache over NVMe), GPT partitions, `onbf-merkle` (for a future integrity option) |

## Context

M5 gave the kernel a block device (NVMe), partitions (GPT) and a page cache. The runtime and services (§44, §49) need a file namespace with real files on disk; the in-memory VFS (`framekernel::vfs`) is the interface, and an on-disk filesystem must plug in beneath it. CLAUDE.md §40 asks for an "independently authored, standards-informed filesystem design." This ADR fixes the smallest on-disk format that stores a directory tree and file contents, so a read-only mount can be built and tested now; durable writes, journalling and snapshots come in a later revision once the read path is proven.

Because this is a format decision (§84), the format below is **Proposed**. A parser and a builder are implemented and host-tested against it, but the kernel does not mount OriginFS until the owner accepts this ADR.

## Proposed decisions

### F1. One 4 KiB block, little-endian, in a GPT partition
The block size is 4096 bytes, matching the page cache, so one cache page is one filesystem block. All multi-byte fields are little-endian. A volume lives in a GPT partition of the Origin system or data type (the provisional type GUIDs of the test disk; §21 fixes the real ones). Block 0 of the partition is the superblock.

### F2. The superblock (block 0)
Fields: magic `"ORIGINFS"` (8 bytes), `version` (u32, 1), `block_size` (u32, 4096), `total_blocks` (u64), `inode_count` (u64), `inode_table_start` (u64, the first block of the inode table), `data_start` (u64, the first data block), `root_inode` (u32, normally 1; inode 0 is never used), and a CRC-32 over the first 60 bytes with the CRC field zeroed. A superblock whose magic, version, block size or CRC is wrong is refused.

### F3. Inodes (128 bytes each)
An inode: `kind` (u16: 1 file, 2 directory), `_reserved` (u16), `links` (u32), `size` (u64, file length or directory byte length), `blocks` (u64, allocated blocks), 12 `direct` block pointers (u64 each) and one `single_indirect` block pointer (u64), the rest reserved zero. A block pointer of 0 means "no block" (a hole reads as zeros). The single-indirect block holds `block_size / 8` = 512 further u64 pointers, so a file reaches `(12 + 512) × 4096` = about 2 MiB; a double-indirect pointer is a later revision. Inodes are packed `block_size / 128` = 32 per block, starting at `inode_table_start`; inode *n* is at table offset `n × 128`.

### F4. Directories
A directory's data blocks hold a sequence of entries, each: `inode` (u32; 0 marks a free/last entry), `rec_len` (u16, the bytes this record occupies, 4-aligned), `name_len` (u8), `kind` (u8, mirrors the target inode's kind), then `name_len` UTF-8 bytes. A record never crosses a block boundary; the last record in a block has `rec_len` running to the block end. `.` and `..` are ordinary entries. Names are at most 255 bytes.

### F5. Reading a file
A file's byte `b` is in block index `b / 4096`; block indexes 0–11 are the direct pointers, 12–523 are entries of the single-indirect block. A pointer of 0 yields a zero-filled block (a sparse hole). Reads past `size` return nothing.

### F6. What read-only leaves out (a later revision)
No write path, allocation bitmap maintenance, journalling, crash consistency (§41), snapshots, extended attributes, permissions beyond the kind, hard links beyond `links` accounting, or symbolic links. The read path and its on-disk structures are fixed first so writing is designed against a proven layout.

## Consequences

- OriginFS sits beneath `framekernel::vfs`: the same operations (`resolve`, `read`, `list`, `stat`) back onto disk blocks instead of RAM nodes, once mounting is accepted.
- The 2 MiB single-indirect limit is deliberate for v0; double- and triple-indirect (or extents) come with the write path.
- An integrity option can later store each data block's Merkle leaf (`onbf-merkle`) for tamper detection, reusing the ONBF Merkle work.

## Evidence and comparisons

The layout follows the classic Unix inode filesystem (the BSD FFS and Linux ext2 lineage): a superblock, a fixed inode table, direct and indirect block pointers, and linked directory entries with `rec_len`. These are conceptual references; no source is used (CLAUDE.md §3). The read path is proven by a host-tested builder and parser that round-trip a directory tree and file contents.

## Questions for the owner

1. Accept a classic inode layout (superblock, fixed inode table, 12 direct + 1 single-indirect, linked directory entries) for v0 read-only (F1–F5)?
2. Accept the 4 KiB block equal to the page-cache page, and the ~2 MiB single-indirect file limit for v0 (F1, F3)?
3. Accept deferring the write path, journalling and crash consistency to a later revision (F6)?
4. Should v0 reserve space for the optional per-block Merkle integrity leaf now, or add it later?

Until this ADR is Accepted, the parser and builder stay host-only; the kernel does not mount OriginFS (CLAUDE.md §84).
