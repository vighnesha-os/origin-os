# ONBF v1.0: clarifications and open gaps

| | |
|---|---|
| Applies to | `docs/spec/ONBF.md` v1.0 (design), accepted 2026-11-07 |
| Status | **C1–C14 and G2–G5 decided by the owner on 2026-11-07. G1 is open.** |
| Found by | Implementing `runtime/onbf/format` (`onbf-format`) against §4, 2026-11-07 |
| Byte layouts | Unchanged. Every item below is a validation rule or a missing definition, not a new field. |

ONBF-R6 requires that a file have exactly one valid reading, and §13 item 6 says that two parsers disagreeing is a spec bug. Each item below is a place where §4 allowed more than one reading.

- These decisions are **normative**: they bind `onbf-format`, `onbf-pack` and any second, independent parser.
- `runtime/onbf/format/src/tests.rs` pins each one.
- The owner weighed the options for performance, security, and native SDK and GUI use.

## Decided clarifications

| # | Spec | Ambiguity | Decision (2026-11-07) |
|---|---|---|---|
| C1 | §4.1 | The spec says "ascending file offset, non-overlapping". It does not say whether table order must equal file order, or how large gaps may be. | **Canonical offsets.** Table order is file order. Each section starts at the end of the previous item rounded up to the section's alignment; the first section is at `align_up(table_end, 4096)`. There is one byte encoding per image. |
| C2 | §4.1, ONBF-R1, R6 | Padding is covered by no digest, and R1 says that before verification only the header and table are read. | **Zero, checked early.** Every padding byte MUST be zero. The parser reads padding before verification only to compare it with zero; it never interprets it. |
| C3 | §4.1, §4.2 | The signature block's alignment is not given. | **8-byte aligned.** `signature_block_offset = align_up(end of last section, 8)`, and the block ends exactly at EOF. |
| C4 | §4.2 | The spec does not say what `signature_block_size` must be for an unsigned image. | **The size must match.** Offset 0 requires size 0. A non-zero offset requires size 1 to 1 MiB. |
| C5 | §4.2 | The `format_minor` rule column says "`0`", but its note says that higher minors only add optional content. | **Accept higher minors.** Writers emit 0. A v1 reader accepts any minor under major 1. Because of C13, a newer minor stays readable by older systems only if it adds nothing but manifest keys ≥ 1000. |
| C6 | §4.3 | The permission bits (READ, WRITE, EXECUTE, RELRO) are not restricted to any kind. | **Segments only.** Permission bits are allowed only on SEGMENT, and SEGMENT must be MAPPABLE. |
| C7 | §4.3 | `mem_size` must be at least `content_size`, but nothing is said for non-segments. | **`mem_size = content_size`** and `mem_offset = 0` for a non-SEGMENT section. A SEGMENT has `mem_offset` 4 KiB aligned, `mem_size ≥ content_size`, and a range that does not overflow. |
| C8 | §4.3, App. B 5 | "Merkle for all mapped content" is not tied to the `integrity` field. | **Merkle required.** A MAPPABLE section uses integrity 1 (Merkle) or 3 (covered by parent), never 0. |
| C9 | §4.3, §4.4 | The rules for MERKLE sections themselves are not stated. | **Strict rules.** A MERKLE section uses integrity 2, has an all-zero `digest`, is uncompressed, and is referenced by exactly one section. |
| C10 | §4.3, §4.6 | With content of at most 4 KiB there are no hash blocks, but §4.3 still asks for a MERKLE index. | **No MERKLE section.** When the tree has no hash blocks, `merkle_index = 0xFFFF_FFFF`. Otherwise `merkle_index` names a MERKLE section of exactly `hash_blocks(content_size) × 4096` bytes (see `merkle_hash_blocks`). |
| C11 | §4.3 | The value of `digest` under integrity 3 is not stated. | **Zero digest, nested images only.** The digest is all zero, and integrity 3 is accepted only when the caller declares that the image is nested in a verified parent. |
| C12 | §4.4, §5 | MANIFEST compression and size limits are not fully stated. | **Uncompressed only** (a change to the parser). A MANIFEST has REQUIRED set, uses integrity 0, is never compressed, and has `content_size` ≤ 1 MiB. As a result, no decompressor runs on unverified data, and the boot loader needs no zstd. |
| C13 | §4.4 | Unknown kinds without REQUIRED could be skipped ("MAY"). | **Reject all unknown kinds** (the owner's choice; it differs from the recommendation to skip them). An unknown kind is rejected with or without REQUIRED. This is permitted by §4.4, because skipping was only a MAY. Adding a new section kind therefore needs an OS update before files that use it can be opened. |
| C14 | §4.3 | The spec does not say whether a section may be empty. | **Empty SEGMENT only** (a change to the parser). Only a SEGMENT may have `content_size = 0`, as zero-filled memory (bss), and it must have `mem_size > 0`. Any other empty section is rejected, so "nothing" has one encoding: absent. |

## Decided gaps

| # | Spec | Gap | Decision (2026-11-07) | Enforced by |
|---|---|---|---|---|
| G2 | §4.6 step 2 | In `hL[j] = SHA-256(0x01 ‖ le8(L) ‖ le64(j) ‖ hash_block_j)`, it was unclear whether L is the level of the hash being produced or the level of the hashes inside the block. | **L is the level of the hash being produced**, which matches the spec's notation `hL`. Leaf hashes are level 0, so hashing a hash block of level-0 hashes gives `h1[j]` with `L = 1`, and so on upward. `j` is the hash's index within its level. | `onbf-merkle` (next) |
| G3 | §4.6 step 3 | `le8(levels)` in the root was not defined beyond the single-block case. | **`levels` is the level of the top hash**: `top = h_levels[0]`. It is 0 for empty content or a single 4 KiB block, 1 for 2–128 blocks, 2 for 129–16,384, and so on. It equals the number of hash-block levels stored in the MERKLE section and agrees with `merkle_hash_blocks`. | `onbf-merkle` (next); `onbf-format` sizes MERKLE sections this way |
| G4 | §4.3, §6.1 | Segment `mem_size` alignment, and the order and overlap of segment memory ranges, were not stated. | **Page-multiple, sorted, disjoint.** A SEGMENT's `mem_size`, like its `mem_offset`, is a multiple of 4 KiB. SEGMENT sections appear in ascending `mem_offset` order (table order) and never overlap. Unmapped gaps, such as guard pages, are allowed. Every segment ends within the 2 GiB image span (`mem_offset + mem_size ≤ 2^31`) in EXE, LIB and KRN. The Appendix A limit of 16 segments applies to EXE and LIB. | `onbf-format` (`MemoryLayout`, `SegmentOrder`, `SegmentSpan`, `TooManySegments`) |
| G5 | §4.3, §4.4 | After C13, REQUIRED no longer changed anything for a v1 reader except on MANIFEST. | **REQUIRED is set on every section.** That is truthful in v1, because every section must be understood, and gives one encoding. Writers (`onbf-pack`, the SDK tools) always set it. The bit keeps its meaning in case a future major version brings optional kinds back. | `onbf-format` (`RequiredNotSet`) |

## Open gaps

| # | Spec | Gap | Who needs it |
|---|---|---|---|
| G1 | §6.5, §4.4 | The SYM profile holds "DWARF and line tables", but §4.4 has no section kind for them. A SYM image can currently contain only MANIFEST and MERKLE. Under C13, adding a kind is a format change. | SYM producer and symbolizer (M6) |

## Observed by the bit-flip sweep

Flipping every bit of the header and section table of a valid KRN image shows which fields can take another valid value without being rejected:
- `format_minor` and `content_digest`;
- the entry digests;
- segment permissions, and segment placements that stay page-aligned, ordered and disjoint;
- `profile`, because KRN's sections are also valid in LIB.

**All of them are covered by `content_digest`, and the signed statement names the profile (§7.1)**, so none can change undetected. Two fields are no longer on this list: since C12, a manifest's `compression`, and since G5, the REQUIRED bit.
