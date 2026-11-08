# ONBF v1.0: clarifications and open gaps

| | |
|---|---|
| Applies to | `docs/spec/ONBF.md` v1.0 (design), accepted 2026-11-07 |
| Status | **C1–C14 and G2–G5 decided by the owner on 2026-11-07. G1 and the readings below were decided the same day under the owner's instruction of 2026-11-07 to finalise the design with the recommended options.** V1–V6 (2026-11-08) are **Proposed**: see "Proposed readings of §7". |
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
| G1 | §6.5, §4.4 | The SYM profile holds "DWARF and line tables", but §4.4 had no section kind for them. | **A new kind, `0x00F2 DWARF`, allowed only in SYM.** One section per DWARF section, never MAPPABLE (SYM is never mapped), so it may be compressed; REQUIRED like every section (G5). The SYM manifest's `dwarf_sections` key (25) lists `[section_index, name]` pairs in ascending index order, each name one of `.debug_info`, `.debug_abbrev`, `.debug_line`, `.debug_line_str`, `.debug_str`, `.debug_str_offsets`, `.debug_addr`, `.debug_rnglists`, `.debug_loclists`, `.debug_frame` and `.debug_aranges`, each at most once. ONBF stays 1.0: no SYM image existed before this decision. | `onbf-format` (`Kind::Dwarf`, SYM only) |
| G5 | §4.3, §4.4 | After C13, REQUIRED no longer changed anything for a v1 reader except on MANIFEST. | **REQUIRED is set on every section.** That is truthful in v1, because every section must be understood, and gives one encoding. Writers (`onbf-pack`, the SDK tools) always set it. The bit keeps its meaning in case a future major version brings optional kinds back. | `onbf-format` (`RequiredNotSet`) |
| O1 | S3, §5 | A statement's `id` is `text (NFC)`, but the signature-block parser has no Unicode normalization tables. Found by the multi-fault soak (2026-11-08). | **Decided 2026-11-08: non-empty printable ASCII (0x20–0x7E)**, the rule R4 applies to the manifest's text, which is trivially NFC. An `id` is a machine identifier; human-readable names belong in localized metadata. | `onbf-format` signature-block parser (`SignatureBlock::parse`); the Package Service's ID-namespace checks |

## Readings applied without a separate decision

These follow directly from the text, but are written down so that a second, independent implementation cannot drift from them. **Accepted as normative on 2026-11-07, under the owner's instruction of 2026-11-07 to finalise the design with the recommended options.**

| # | Spec | Reading | Applied in |
|---|---|---|---|
| R1 | §4.6 step 3 | `"ONBF-MERKLE-1"` is its 13 ASCII bytes, with no terminator or length prefix. | `onbf-merkle`, `tools/merkle_reference.py` |
| R2 | §4.6 steps 1–2 | The zero padding of the last data block, and of each level's last hash block, is hashed as part of that block, so leaf and node hash inputs are always a full 4 KiB plus the prefix. | `onbf-merkle` |
| R3 | §4.6 step 4 | "Highest level first" applies across levels. Within a level, hash blocks are stored in ascending index order. | `onbf-merkle` |
| R4 | §5; key registry | The KRN `toolchain` text fields (`rustc`, `rustc_commit`, `llvm`) are non-empty printable ASCII (0x20–0x7E). That is stricter than §5's NFC rule, and trivially NFC, so the boot loader needs no Unicode normalization tables. `rustc_commit` is exactly 40 lowercase hex digits. | `onbf-format` (`KrnManifest`) |
| R5 | §5 | "Items per container ≤ 65,536" counts elements for an array and key/value entries for a map. "Depth ≤ 16" counts nested containers, so the top-level map is depth 1, and an empty container still counts. | `onbf-format` (`ocbor`) |

## Readings proposed by the implementations

Applied in code on 2026-11-07 and pinned by tests. **Accepted as normative the same day, under the owner's instruction of 2026-11-07 to finalise the design with the recommended options.** Each is the reading a second implementation (or the host signer) must match.

| # | Where | Reading |
|---|---|---|
| P1 | `onbf-pack`, ELF input | `PT_NULL` and `PT_GNU_PROPERTY` are errors; a second `PT_DYNAMIC` or `PT_GNU_STACK` is an error; a missing `PT_GNU_STACK` is accepted; `PT_PHDR`, `PT_NOTE` and `PT_GNU_EH_FRAME` are ignored. |
| P2 | `onbf-pack`, segments | Order uses the page-rounded end: the next `p_vaddr` must be at least the previous `p_vaddr + align_up(p_memsz, 4096)`, so two segments never share a page; gaps are allowed; `p_memsz = 0` is rejected (C14). |
| P3 | `onbf-pack`, span and entry | The page-rounded end of the highest segment is at most 2^31 (ending exactly at 2 GiB is allowed, as in `onbf-format`); `e_entry` lies in the file-backed bytes of an RX segment. |
| P4 | `onbf-pack`, dynamic table | Read up to the first `DT_NULL` (required); `DT_INIT`, `DT_FINI` and the init, fini and preinit arrays are rejected whatever their size; `DT_RELA`/`RELASZ`/`RELAENT` and `DT_RELR`/`RELRSZ`/`RELRENT` appear all together or not at all; duplicates are errors; each table lies in the file-backed bytes of one PT_LOAD. |
| P5 | `onbf-pack`, relocations | Only `R_X86_64_RELATIVE` with symbol index 0; identical duplicate RELA entries merge; the same slot with different addends, or in both RELA and RELR, is an error; decoded RELR slots strictly ascend. Addends are written as u64 two's complement, so the loader adds with wrapping. |
| P6 | `onbf-pack`, manifest | `flags_digest` is SHA-256 over exactly the UTF-8 bytes of `--flags`, with no normalization (the build system owns the canonical form); toolchain fields default to `rustc -vV`, each validated against R4. |
| P7 | `onbf-pack`, sections | MERKLE sections are REQUIRED only (not MAPPABLE), 8-byte aligned, placed after RELOCS in segment order; RELOCS uses the greedy SHT_RELR encoding, and `verify` rejects any other encoding. |
| E1 | `origin-crypto`, Ed25519 | Strict RFC 8032 verification, in this order: A canonical (y < p, a root exists, not x = 0 with the sign bit) and not small order ([8]A ≠ identity); the same for R; S < L as a 256-bit integer; then the **cofactorless** equation [S]B = R + [k]A. Mixed-order A and R (order 2L, 4L, 8L) are accepted, with no [L]A check. The host signer must produce only signatures this accepts. |
| M1 | `origin-crypto`, ML-DSA | Strict FIPS 204 verification, first failure wins: context > 255 bytes; HintBitUnpack per polynomial (count range, then strictly increasing positions), then non-zero padding; ‖z‖∞ ≥ γ1 − β; c̃ mismatch. SampleInBall uses all λ/4 bytes of c̃; the ExpandA nonce is ρ ‖ column ‖ row; rejection loops are unbounded (no Appendix C bound). The host signer must produce only signatures this accepts. |
| L1 | loader, boot info 0.2 | A RELRO segment is passed to the kernel without write access, because the loader has applied every relocation (ADR-0002 B4). |

## Proposed readings of §7: awaiting the owner

Found while wiring signature verification into the boot loader (2026-11-08; `docs/research/2026-11-08-krn-signature-verification.md`). **Status: Proposed. They are not normative until the owner decides (CLAUDE.md §84).** The code applies each one, because nothing ships from this branch; each is the choice that fails closed or changes the least.

| # | Spec | Gap | Proposed reading (applied) | Alternative |
|---|---|---|---|---|
| V1 | §7.1 `chain`, §7.4 | Whether an entry may have no certificates, and what that means. The parser accepts a chain of length 0, and §7.1 only says the chain ends "below a trust-store root". | **An empty chain means the signer's own key is in the trust store.** It is direct trust, for that key and role. A non-empty chain is refused until OKC validation exists; it is never ignored. | Forbid empty chains, once a Kernel CA exists. Then every signed KRN would need a chain, and no key could be trusted directly. |
| V2 | §7.5, §7.1 | How many entries a KRN block holds, and whether others are ignored. | **Exactly one entry, in the role `origin-kernel`.** Any other block is refused, so nothing in the block goes unchecked (ONBF-R6). The rule can be relaxed later; a looser one could not be tightened without breaking images. | Accept extra entries if one trusted entry verifies. |
| V3 | S3 key 3–4, K4 | The statement carries the "artifact's ID from its manifest", but a KRN manifest has no `id` and no version (K4). | **`id` is `origin.framekernel`; `version` is the release version from `origin-version.toml`.** The loader compares neither: §7.1 lists no check on them, and the namespace check belongs to chains. | Derive `id` from a new manifest key. That is an ABI change for no present use. |
| V4 | §7.4, §8.1 | What a "root compiled into the loader" is in practice before Origin has keys. | **The trust store is a compiled-in list of (role, public key).** A release build's list is empty until Origin's own kernel key exists, so it refuses every kernel (the safe failure of B8). A build with the `test-trust-store` feature adds the test key of `boot/uefi/trust/` and warns on every boot, and such a build cannot be compiled with a non-zero build number (`origin-version.toml`), so a released loader cannot hold it. | Require a real key before any build can boot. Then no signed boot could be tested. |
| V5 | ADR-0002 B8 | What developer mode does with a *signed* image. | **A signed image is verified in every mode.** Developer mode only allows an image with no signature at all: a signature that does not verify is a refusal, not a warning. | Skip verification in developer mode. |
| V6 | §7.1, ONBF-R9 | `claimed_signing_time` is in the signed statement, and R9 forbids build timestamps in signed content. | **The caller supplies it** (`onbf-pack sign --signing-time`), and no tool reads the clock. It stays informational (§7.6). | Forbid the field in KRN statements. That changes S3. |

## Open gaps

| # | Spec | Gap | Who needs it |
|---|---|---|---|
| O2 | S4 key 10 | An OKC is signed with both algorithms, but S4 defines no ML-DSA context string for that signature (§7.1's `origin/onbf/1/<role>` is for signature-block entries). | OKC issuance and chain validation |
| O3 | §7.6, §7.3 | `not_before`/`not_after` need trusted time, and the boot loader has only the firmware's RTC, which is not trusted. How does a chain's validity get checked at boot? | Chain validation in the loader |
| O4 | §7.1, S4 key 6 | How an `id_namespaces` entry matches a statement's `id` (prefix, label boundary, wildcard) is undefined. | Chain validation; the Package Service |
| O5 | §7.4, §8.1 | What a trust-store *root* is: a bare key, a self-signed OKC, or TUF-style root metadata (§7.4 mentions rotation by chained root metadata). | The release trust store; root rotation |

## Observed by the bit-flip sweep

Flipping every bit of the header and section table of a valid KRN image shows which fields can take another valid value without being rejected:
- `format_minor` and `content_digest`;
- the entry digests;
- segment permissions, and segment placements that stay page-aligned, ordered and disjoint;
- `profile`, because KRN's sections are also valid in LIB.

**All of them are covered by `content_digest`, and the signed statement names the profile (§7.1)**, so none can change undetected. Two fields are no longer on this list: since C12, a manifest's `compression`, and since G5, the REQUIRED bit.
