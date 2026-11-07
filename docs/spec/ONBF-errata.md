# ONBF v1.0: proposed clarifications and open gaps

| | |
|---|---|
| Applies to | `docs/spec/ONBF.md` v1.0 (design), accepted 2026-11-07 |
| Status | **Proposed.** Needs the owner's approval (CLAUDE.md §84). Until then `onbf-format` must not ship. |
| Found by | Implementing `runtime/onbf/format` (`onbf-format`) against §4, 2026-11-07 |
| Byte layouts | Unchanged. Every item below is a validation rule or a missing definition, not a new field. |

ONBF-R6 requires that a file have exactly one valid reading, and §13 item 6 says that two parsers disagreeing is a spec bug. Each item below is a place where §4 allows more than one reading. `onbf-format` implements the reading given here, and its tests (`runtime/onbf/format/src/tests.rs`) pin each one.

## Clarifications the parser implements

| # | Spec | Ambiguity | Reading implemented |
|---|---|---|---|
| C1 | §4.1 | The spec says "ascending file offset, non-overlapping". It does not say whether table order must equal file order, or how large gaps may be. | Table order is file order. Each section starts at its **canonical offset**: the end of the previous item rounded up to the section's alignment. The first section is at `align_up(table_end, 4096)`. Padding is therefore only the minimum that alignment needs. |
| C2 | §4.1, ONBF-R1, R6 | Padding is covered by no digest, and R1 says that before verification only the header and table are read. | Every padding byte MUST be zero. The parser reads padding before verification **only to compare it with zero**; it never interprets it. Otherwise padding could carry unauthenticated data. |
| C3 | §4.1, §4.2 | The signature block's alignment is not given. | `signature_block_offset = align_up(end of last section, 8)`, and the block ends exactly at EOF. |
| C4 | §4.2 | The spec does not say what `signature_block_size` must be for an unsigned image. | Offset 0 requires size 0. A non-zero offset requires size 1 to 1 MiB. |
| C5 | §4.2 | The `format_minor` rule column says "`0`", but its note says that higher minors only add optional content. | Writers emit 0. A v1 reader **accepts any minor** under major 1 and reports it. |
| C6 | §4.3 | The permission bits (READ, WRITE, EXECUTE, RELRO) are not restricted to any kind. | They are allowed only on SEGMENT, and SEGMENT must be MAPPABLE. |
| C7 | §4.3 | `mem_size` must be at least `content_size`, but nothing is said for non-segments. | For a non-SEGMENT section, `mem_offset = 0` and `mem_size = content_size`. For a SEGMENT, `mem_offset` is 4 KiB aligned, `mem_size ≥ content_size`, and the range does not overflow. |
| C8 | §4.3, App. B 5 | "Merkle for all mapped content" is not tied to the `integrity` field. | A MAPPABLE section uses integrity 1 (Merkle) or 3 (covered by parent), never 0. |
| C9 | §4.3, §4.4 | The rules for MERKLE sections themselves are not stated. | A MERKLE section uses integrity 2, has an all-zero `digest`, is uncompressed, and is referenced by exactly one section (no unreferenced bytes). |
| C10 | §4.3, §4.6 | With content of at most 4 KiB, `levels = 0` and there are no hash blocks, but §4.3 still asks for a MERKLE section index. | When the tree has **no hash blocks**, `merkle_index = 0xFFFF_FFFF` and there is no MERKLE section. Otherwise `merkle_index` names a MERKLE section of exactly `hash_blocks(content_size) × 4096` bytes. See `merkle_hash_blocks` for how blocks are counted. |
| C11 | §4.3 | The value of `digest` under integrity 3 is not stated. | It is all zero. Integrity 3 is accepted only when the caller declares that the image is nested in a verified parent. |
| C12 | §4.4, §5 | MANIFEST compression and size limits are not fully stated. | A MANIFEST has REQUIRED set, uses integrity 0, and has `content_size` ≤ 1 MiB. It may be compressed. |
| C13 | §4.4 | Unknown kinds without REQUIRED are not limited to any profile. | They are allowed in every profile, and the general flag, size and layout rules still apply to them. |
| C14 | §4.3 | The spec does not say whether a section may be empty. | Zero-length sections are allowed, for example a bss-only SEGMENT with `content_size = 0` and `mem_size > 0`. |

## Gaps the parser does not resolve

| # | Spec | Gap | Who needs it |
|---|---|---|---|
| G1 | §6.5, §4.4 | The SYM profile holds "DWARF and line tables", but §4.4 has no section kind for them. A SYM image can currently contain only MANIFEST and MERKLE. | SYM producer and symbolizer (M6) |
| G2 | §4.6 step 2 | In `hL[j] = SHA-256(0x01 ‖ le8(L) ‖ …)`, it is unclear whether L is the level of the hash being produced or the level of the hash block being hashed. | `onbf-merkle` |
| G3 | §4.6 step 3 | `le8(levels)` is not defined. The parser assumes it is the number of hash-block levels: 0 for content of at most one block, 1 for 2–128 blocks, and so on. | `onbf-merkle` |
| G4 | §4.3, §6.1 | Segment `mem_size` alignment and non-overlap of segment memory ranges are not stated. They belong to the EXE/LIB/KRN profile layer, not the container. | KRN loader (M1 step 2), Loader Service |

## Observed by the bit-flip sweep

Flipping every bit of the header and section table of a valid KRN image shows which fields can take another valid value without being rejected. They are `format_minor`, `content_digest`, the entry digests, segment permissions and placement, REQUIRED on optional kinds, the manifest's `compression`, and `profile` (because KRN's sections are also valid in LIB). **All of them are covered by `content_digest`, and the signed statement names the profile (§7.1)**, so none can change undetected. No other bit can change without the image being rejected.
