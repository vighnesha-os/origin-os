# ADR-0006: OriginFS v0, the native filesystem on-disk format

| | |
|---|---|
| Status | **Accepted** by the owner on 2026-11-08: the four questions below answered with the recommended option each, and the ADR accepted with F7 (v0 is a bring-up format that never ships in an approved release). A host-tested read-only parser and an image builder exist; a read-only mount beneath the VFS may now be built. |
| Scope | The on-disk layout of the bring-up filesystem: the superblock, inodes, directories, and how a file's bytes are found. Read only. Writing, crash consistency and the rest of §40 belong to a successor format with its own ADR (F7). |
| Requires | CLAUDE.md §39 (storage architecture, VFS), §40 (native filesystem requirements), §41 (filesystem correctness) |
| Builds on | the block layer (a 4 KiB page cache over NVMe), GPT partitions, `onbf-merkle` (for a future integrity option) |
| Evidence | `docs/research/2026-11-08-originfs-design.md` (v0 against §40 and §41, the alternatives, the NVMe durability contract); the host-tested builder, parser and soak in `framekernel::originfs` |

## Context

M5 gave the kernel a block device (NVMe), partitions (GPT) and a page cache. The runtime and services (§44, §49) need a file namespace with real files on disk; the in-memory VFS (`framekernel::vfs`) is the interface, and an on-disk filesystem must plug in beneath it. CLAUDE.md §40 asks for an "independently authored, standards-informed filesystem design." This ADR fixes the smallest on-disk format that stores a directory tree and file contents, so a read-only mount can be built and tested now. Durable writes, journalling and snapshots belong to a successor format (F7).

This is a format decision (§84). The owner accepted it on 2026-11-08; until then a parser and a builder were implemented and host-tested against the proposal, and the kernel did not mount OriginFS.

## Decisions

### F1. One 4 KiB block, little-endian, in a GPT partition
The block size is 4096 bytes, matching the page cache, so one cache page is one filesystem block. All multi-byte fields are little-endian. A volume lives in a GPT partition of the Origin system or data type (the provisional type GUIDs of the test disk; §21 fixes the real ones). Block 0 of the partition is the superblock.

### F2. The superblock (block 0)
Fields: magic `"ORIGINFS"` (8 bytes), `version` (u32, 1), `block_size` (u32, 4096), `total_blocks` (u64), `inode_count` (u64), `inode_table_start` (u64, the first block of the inode table), `data_start` (u64, the first data block), `root_inode` (u32, normally 1; inode 0 is never used), and a CRC-32 over the first 60 bytes with the CRC field zeroed. A superblock whose magic, version, block size or CRC is wrong is refused.

### F3. Inodes (128 bytes each)
An inode: `kind` (u16: 1 file, 2 directory), `_reserved` (u16), `links` (u32), `size` (u64, file length or directory byte length), `blocks` (u64, allocated blocks), 12 `direct` block pointers (u64 each) and one `single_indirect` block pointer (u64). That fills the 128 bytes: the only spare field is `_reserved`. A block pointer of 0 means "no block" (a hole reads as zeros). The single-indirect block holds `block_size / 8` = 512 further u64 pointers, so a file reaches `(12 + 512) × 4096` = about 2 MiB; a double-indirect pointer is a later revision. Inodes are packed `block_size / 128` = 32 per block, starting at `inode_table_start`; inode *n* is at table offset `n × 128`.

### F4. Directories
A directory's data blocks hold a sequence of entries, each: `inode` (u32; 0 marks a free/last entry), `rec_len` (u16, the bytes this record occupies, 4-aligned), `name_len` (u8), `kind` (u8, mirrors the target inode's kind), then `name_len` UTF-8 bytes. A record never crosses a block boundary; the last record in a block has `rec_len` running to the block end. `.` and `..` are ordinary entries. Names are at most 255 bytes.

### F5. Reading a file
A file's byte `b` is in block index `b / 4096`; block indexes 0–11 are the direct pointers, 12–523 are entries of the single-indirect block. A pointer of 0 yields a zero-filled block (a sparse hole). Reads past `size` return nothing.

### F6. What read-only leaves out (the successor format)
No write path, allocation bitmap maintenance, journalling, crash consistency (§41), snapshots, extended attributes, permissions beyond the kind, hard links beyond `links` accounting, or symbolic links. The read path and its on-disk structures are fixed first so a read path can be proven.

### F7. v0 is a bring-up format and never ships in an approved release
The research ledger found that v0 cannot grow into the filesystem §40 requires: its inode is full, its block pointers carry only an address, and its inode table cannot move. So any filesystem that meets §40 is a new format with its own ADR. v0 exists to prove the read path (NVMe, block cache, VFS), and only test images use it. It carries no compatibility promise, because it never reaches an approved release (CLAUDE.md rule 39).

## Consequences

- OriginFS sits beneath `framekernel::vfs`: the same operations (`resolve`, `read`, `list`, `stat`) back onto disk blocks instead of RAM nodes, once mounting is accepted.
- The 2 MiB single-indirect limit is deliberate for v0. Because of F7 it is not extended: large files (§40) come with the successor format.
- Integrity (§40) belongs to the successor format. The ledger's candidate keeps each block's checksum in its parent pointer, which v0's pointers have no room for.

## Alternatives

- **No on-disk filesystem yet: stay on the ustar initramfs.** It is already in the kernel and needs no format decision, but it never exercises NVMe → block cache → VFS, which is what v0 is for.
- **Design the §40 filesystem first.** The right end state, and required eventually, but a far larger design before any real file can be read from disk. It should proceed anyway, separately.
- **A journal on this layout.** Adds crash consistency, but not integrity, access control or snapshots, all of which need a new inode regardless.
- **Soft updates.** NVMe gives no ordering between commands, so every dependency needs a wait or flush. It adds nothing for a read-only format.
- **Copy-on-write tree.** The ledger's leading candidate for the successor. It would replace, not extend, this layout.
- **FAT.** Needed only for the EFI System Partition. It has no inodes and no access control.

## Trade-offs

- *Simple and quick to prove, but disposable.* Everything written for the on-disk layout is thrown away when the successor arrives; the VFS interface above it is not.
- *A fixed inode table is easy to parse and bounds-check, but it rules out moving inodes*, and so snapshots.
- *Block size equals the page-cache page*, so one cache page is one block; but a 4 KiB block can tear on a 512-byte namespace. That is harmless read-only and a design constraint for writing.
- *A CRC on the superblock catches accidents only.* It is not integrity in §40's sense, and it authenticates nothing.

## Migration consequences

None, because nothing has shipped and F7 keeps v0 out of every approved release. Only test images built by xtask use it. The superblock's `version` makes any other format refused cleanly, so the successor can use a new version or magic without being misread. Services depend on the VFS interface, not on this layout, so replacing it changes no ABI. If F7 were ever dropped, rule 39 and §40's safe migrations would require a converter from v0.

## Evidence and comparisons

The layout follows the classic Unix inode filesystem (the BSD FFS and Linux ext2 lineage): a superblock, a fixed inode table, direct and indirect block pointers, and linked directory entries with `rec_len`. These are conceptual references; no source is used (CLAUDE.md §3). The read path is proven by a host-tested builder and parser that round-trip a directory tree and file contents. The comparison with journalling, copy-on-write and the other alternatives, written after this design, is in `docs/research/2026-11-08-originfs-design.md`.

## Questions for the owner (answered 2026-11-08: the recommended option for all four)

1. Accept a classic inode layout (superblock, fixed inode table, 12 direct + 1 single-indirect, linked directory entries) for v0 read-only (F1–F5), as a bring-up format that never ships in an approved release (F7)? **Yes.**
2. Accept the 4 KiB block equal to the page-cache page, and the ~2 MiB single-indirect file limit for v0 (F1, F3)? **Yes.**
3. Accept deferring the write path, journalling and crash consistency to a later revision (F6)? **Yes: to the successor format.**
4. Should v0 reserve space for the optional per-block Merkle integrity leaf now, or add it later? **No reservation: integrity belongs to the successor format.**

v0 must not appear in an approved release (F7).
