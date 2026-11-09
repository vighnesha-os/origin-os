# Research ledger: OriginFS v0 against CLAUDE.md §40 and §41

| | |
|---|---|
| Date | 2026-11-08 |
| For | `docs/adr/ADR-0006-originfs.md` (**Proposed**) and `framekernel::originfs` |
| Requires | CLAUDE.md §16 (research first), §18 (ADR content), §39–§41 (storage and filesystem), §84 (format decisions) |
| Method | Written **after** v0 was designed and built, so it checks a decision instead of informing one; §16 asks for this first. Concepts only, from published papers and the device specification (§3); no filesystem source code was read. |
| Question | Can the crash consistency, integrity and snapshots §40 requires fit v0's layout, or is v0 a format to replace? |

## 1. Sources

| ID | Source | What it establishes |
|---|---|---|
| N1 | NVM Express, *NVM Command Set Specification*, Revision 1.3 (2026-07-31; the §17 baseline), §2.1.4.3, §2.1.4.3.2, and the Write command's FUA field (p. 59) | What a device promises about torn writes, durability and ordering (§6 below). |
| L1 | Hitz, Lau, Malcolm, *File System Design for an NFS File Server Appliance*, USENIX Winter 1994 (WAFL) | Inodes live in a file, and only the root inode's block is at a fixed location. Every block of a new consistency point is written before the root, so the disk image advances atomically. Snapshots share unchanged blocks. |
| L2 | Zhang, Rajimwale, Arpaci-Dusseau, Arpaci-Dusseau, *End-to-end Data Integrity for File Systems: A ZFS Case Study*, FAST 2010 | ZFS keeps each block's checksum in its parent's block pointer. The hierarchy forms a self-validating Merkle tree that detects bit rot, phantom writes and misdirected reads and writes. ZFS is much less resilient to memory corruption. Checksumming does not protect against software bugs. |
| L3 | Rodeh, Bacik, Mason, *BTRFS: The Linux B-Tree Filesystem*, ACM Transactions on Storage 9, 2013, DOI 10.1145/2501620.2501623 | Copy-on-write B-trees as the main on-disk structure, giving efficient snapshots and clones, with checksums built in. |
| L4 | Seltzer, Ganger, McKusick, Smith, Soules, Stein, *Journaling Versus Soft Updates*, USENIX ATC 2000 | The two have comparable performance but different semantic guarantees. Journaling approaches soft updates' performance only if metadata operations need not be durable on return. |
| L5 | Pillai et al., *All File Systems Are Not Created Equal*, OSDI 2014 | Application crash consistency depends on rarely specified "persistence properties", which differ widely between filesystems. 60 vulnerabilities across 11 applications. |
| L6 | Mohan, Martinez, Ponnapalli, Raju, Chidambaram, *Finding Crash-Consistency Bugs with Bounded Black-Box Crash Testing*, OSDI 2018 | Most reported crash bugs reproduce within three operations. CrashMonkey and ACE found 24 of 26 known bugs and 10 new ones in mature filesystems, and one in the verified FSCQ. |
| L7 | Bairavasundaram et al., *An Analysis of Data Corruption in the Storage Stack*, FAST 2008 | Over 400,000 checksum mismatches in 41 months of production data: silent corruption is real. |

**Gaps.**
- No standard defines a native filesystem's on-disk format. "Authoritative specification" here means the device's durability contract.
- The Flush command's text is in the NVMe Base Specification, which was not read. N1 was read for durability, torn writes and FUA; its errata were not reviewed.
- L1, L2 and N1 were read in their own text. L3–L7 were read only at the level of abstracts and summaries.
- Log-structured designs (LFS, F2FS) were not assessed.

## 2. What v0 is

From `framekernel/src/originfs.rs` and ADR-0006:
- A 4 KiB block, a superblock at block 0 (CRC-32 over its first 60 bytes), a **fixed inode table**, then data blocks.
- A 128-byte inode: `kind` 2, `_reserved` 2, `links` 4, `size` 8, `blocks` 8, 12 direct pointers 96, one single-indirect pointer 8. That is exactly 128 bytes, so the only spare field is the 2-byte `_reserved`. ADR-0006 F3's "the rest reserved zero" was wrong and is corrected.
- Block pointers are bare `u64`s, with no checksum, generation or flags. Files reach about 2 MiB.

## 3. §40 and §41 against v0

| Requirement | v0 | Could this layout reach it? |
|---|---|---|
| Crash consistency, power-loss recovery | None (read-only) | Partly: a journal region and allocation bitmaps could be added, keeping the layout. |
| Data integrity | None | Not as laid out: a bare `u64` pointer has no room for a checksum, and no table exists for them. |
| Metadata integrity | Superblock CRC only | No: the inode has no room for a checksum. |
| Atomic rename | None | Only with crash consistency. |
| Access control, encryption context, quotas | None | No: the inode has no room for an owner, a mode, a key reference or a project. |
| Sparse files | Yes (0 pointers) | Yes. |
| Large files | About 2 MiB | No: it needs a new pointer form, so a new inode. |
| Memory-mapped files, file locking | Above the on-disk format | Not format questions. |
| Snapshots (where feasible) | None | No: sharing blocks needs inodes and blocks that move (L1), and the inode table is fixed. |
| Repair tools | None | No redundancy to repair from. |
| Versioned on-disk structures | Yes (`version` in the superblock) | Yes. |
| Safe migrations | None | Nothing to migrate from yet. |

## 4. Answer

**No. v0 cannot grow into the filesystem §40 requires.** Only crash consistency could be added without changing the layout. Everything else needs a larger inode, or block pointers that carry more than an address, or inodes and blocks that move. So v0 is a read-path bring-up format, and any filesystem that meets §40 will be a new format. Keeping that honest needs one explicit rule: v0 never ships in an approved release (rule 39 would otherwise require a migration path from it).

## 5. Alternatives (§16 step 7)

| Approach | Crash consistency | Integrity and snapshots | Fit with v0 | Cost |
|---|---|---|---|---|
| A. v0 layout with a write-ahead journal | Yes | Neither follows; a new inode is needed anyway | Partial | Metadata written twice. |
| B. Soft updates | Yes, by ordering writes (L4) | Neither follows | Partial | NVMe has no implied ordering (N1), so each dependency needs a completion wait or flush. Complex. |
| C. Copy-on-write tree (L1, L2, L3) | Yes: one atomic root update | Both follow from the same mechanism: parent-held checksums (L2) and shared blocks (L1) | None: inodes must move | Path copying on every update, free-space management. Unmeasured here. |
| D. Log-structured | Not assessed | Not assessed | None | Not assessed. |
| E. No on-disk filesystem yet: stay on the ustar initramfs | Not applicable | Not applicable | Replaces v0 | Needs no format decision, but never exercises NVMe → block → VFS. |
| F. v0 as a bring-up read path (ADR-0006) | Not applicable | Not applicable | It is v0 | Throwaway format. |

## 6. The device contract any writing filesystem must honour (§41, from N1)

- **Durability.** With a volatile write cache enabled, a completed write can be lost on power failure unless FUA was set or a Flush completed (§2.1.4.3.2). Completion alone is not durability.
- **Ordering.** FUA has "no implied ordering with other commands". Order must come from waiting for completions and flushing.
- **Torn writes.** A write up to AWUPF/NAWUPF that crosses no atomic boundary reads back all old or all new. Beyond that there is no guarantee (§2.1.4.3). A controller that does not guarantee atomicity across boundaries reports AWUPF 0h, which is **one logical block**. A 4 KiB block on a 512-byte namespace can tear, so a commit record must be self-validating and have a second copy.

## 7. Risks (§16 step 9)

- **v0 hardening into the default**, because it works. The rule in §4 is the guard.
- **Hostile media.** The parser takes untrusted bytes; parser hardening found a real overflow in it. CRC-32 detects accidents, and proves nothing about who wrote the data.
- **Successor scope.** §40 is a large design, and it will need its own ADR and owner acceptance (§84).

## 8. Selected design, contracts and tests (§16 steps 10–12)

- **Selected:** F, v0 as a bring-up read path that never ships in an approved release. This is a recommendation; the owner decides (ADR-0006).
- **For the successor (not decided here):** start from C and test it against A; meet §6 above; adopt §41's semantics as a written persistence contract (L5).
- **Tests before v0 is mounted:** the host round-trip and soak that exist, plus a boot proof reading a known tree through NVMe, the block cache and the VFS.
- **Tests before any write path:** bounded crash testing of every short operation sequence (L6), fault injection on disk and memory (L2), and corruption and recovery testing (§40).

## Sources

- N1: <https://nvmexpress.org/wp-content/uploads/NVM-Express-NVM-Command-Set-Specification-Revision-1.3-Ratified-2026.07.31.pdf>
- L1: <https://static.usenix.org/publications/library/proceedings/sf94/full_papers/hitz.a>
- L2: <https://static.usenix.org/events/fast10/tech/full_papers/zhang.pdf>
- L3: <https://api.crossref.org/works/10.1145%2F2501620.2501623>
- L4: <https://www.usenix.org/conference/2000-usenix-annual-technical-conference/journaling-versus-soft-updates-asynchronous-meta>
- L5: <https://www.usenix.org/node/186195>
- L6: <https://arxiv.org/abs/1810.02904>
- L7: <https://www.usenix.org/conference/fast-08/analysis-data-corruption-storage-stack>
