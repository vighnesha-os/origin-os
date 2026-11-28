# ONBF v1.0: clarifications and open gaps

| | |
|---|---|
| Applies to | `docs/spec/ONBF.md` v1.0 (design), accepted 2026-11-07 |
| Status | **C1–C14 and G2–G5 decided by the owner on 2026-11-07. G1 and the readings below were decided the same day under the owner's instruction of 2026-11-07 to finalise the design with the recommended options. O1, V1–V6 and O2–O5 were decided by the owner on 2026-11-08. A1–A13, A21, A22, A24, A25 and A27–A32 were decided by the owner on 2026-11-28 (decision items 385 to 407).** |
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

## Decided readings of §7 (owner, 2026-11-08)

Found while wiring signature verification into the boot loader (`docs/research/2026-11-08-krn-signature-verification.md`). **Decided by the owner on 2026-11-08; normative.** Each was the recommended option; the column on the right is what was rejected.

| # | Spec | Gap | Decision (applied) | Rejected |
|---|---|---|---|---|
| V1 | §7.1 `chain`, §7.4 | Whether an entry may have no certificates, and what that means. The parser accepts a chain of length 0, and §7.1 only says the chain ends "below a trust-store root". | **An empty chain means the signer's own key is in the trust store.** It is direct trust, for that key and role. A non-empty chain is refused until OKC validation exists; it is never ignored. | Forbid empty chains, once a Kernel CA exists. Then every signed KRN would need a chain, and no key could be trusted directly. |
| V2 | §7.5, §7.1 | How many entries a KRN block holds, and whether others are ignored. | **Exactly one entry, in the role `origin-kernel`.** Any other block is refused, so nothing in the block goes unchecked (ONBF-R6). The rule can be relaxed later; a looser one could not be tightened without breaking images. | Accept extra entries if one trusted entry verifies. |
| V3 | S3 key 3–4, K4 | The statement carries the "artifact's ID from its manifest", but a KRN manifest has no `id` and no version (K4). | **`id` is `origin.framekernel`; `version` is the release version from `origin-version.toml`.** The loader compares neither: §7.1 lists no check on them, and the namespace check belongs to chains. | Derive `id` from a new manifest key. That is an ABI change for no present use. |
| V4 | §7.4, §8.1 | What a "root compiled into the loader" is in practice before Origin has keys. | **The trust store is a compiled-in list of (role, public key).** A release build's list is empty until Origin's own kernel key exists, so it refuses every kernel (the safe failure of B8). A build with the `test-trust-store` feature adds the test key of `boot/uefi/trust/` and warns on every boot, and such a build cannot be compiled with a non-zero build number (`origin-version.toml`), so a released loader cannot hold it. | Require a real key before any build can boot. Then no signed boot could be tested. |
| V5 | ADR-0002 B8 | What developer mode does with a *signed* image. | **A signed image is verified in every mode.** Developer mode only allows an image with no signature at all: a signature that does not verify is a refusal, not a warning. | Skip verification in developer mode. |
| V6 | §7.1, ONBF-R9 | `claimed_signing_time` is in the signed statement, and R9 forbids build timestamps in signed content. | **The caller supplies it** (`onbf-pack sign --signing-time`), and no tool reads the clock. It stays informational (§7.6). | Forbid the field in KRN statements. That changes S3. |

## Decided for certificate chains (owner, 2026-11-08)

These four were open gaps blocking chain validation. **Decided by the owner on 2026-11-08; normative when chains are built.** Chains are not implemented yet (V1 still refuses them).

| # | Spec | Gap | Decision | Consequence |
|---|---|---|---|---|
| O2 | S4 key 10 | An OKC is signed with both algorithms, but S4 defined no ML-DSA context string for that signature (§7.1's `origin/onbf/1/<role>` is for signature-block entries). | **The context string is `origin/onbf/1/okc`.** One fixed string in the same family as the roles, so a certificate signature can never pass as a statement signature. | Issuance and validation use it. |
| O3 | §7.6, §7.3 | `not_before`/`not_after` need trusted time, and the boot loader has only the firmware's RTC, which an attacker can set. | **Validity is checked against a signed time carried in signed release metadata**, as §7.6 asks for trusted time; never against the firmware clock. | Chain validation at boot waits for the SYS profile (§6.5), which carries that time. A chain cannot be checked before SYS exists. |
| O4 | §7.1, S4 key 6 | How an `id_namespaces` entry matches a statement's `id` was undefined. | **Exact IDs only:** a certificate lists every ID it covers, and an `id` matches one entry by equality. | A publisher needs a new certificate for each new ID. §6.2's "falls inside a namespace" now means membership in that list. |
| O5 | §7.4, §8.1 | What a trust-store *root* is. | **Bare public keys,** per role, compiled into the loader; a chain ends at one of them. | Root rotation (§7.4) can be layered on with signed metadata later. |

## Decided readings from the two parsers' comparison (owner, 2026-11-28)

TASKS item 164 compared `onbf-format` with a second parser, `onbf-second`, written from this specification alone. Its author recorded 30 places that allow more than one reading (`tests/onbf-nversion/READINGS.md`, A1 to A30). The owner decided each open one on 2026-11-28 (decision items 385 to 407). Nine were already settled, so they have no row here:
- A14 by §4.6 and C10;
- A15 by C11;
- A16 by §4.2 and V5;
- A17 by S2 and §7.2: one required suite per role;
- A18 by §5;
- A19 by §4.3;
- A20 by C7;
- A23 by ONBF-R6, C3 and C4;
- A26 by key 25 and G1.

A31 and A32 were found by the coordinating session's review of the production code. An **addition** is a rule that no clause states.

| # | Spec | Ambiguity | Decision (2026-11-28) |
|---|---|---|---|
| A1 | Key registry: top-level keys; profile requirements | The first table lists keys 1–5 for EXE and LIB; the second gives them only key 0. | **The profile-requirements table governs.** Until a profile's schema is assigned, an EXE, LIB, APP, PKG, DRV or SYS manifest holds key 0 and keys ≥ 1000 only, and any other key is refused. (Item 385.) |
| A2 | Key 2 `toolchain` | Which of its four sub-keys are required. | **All four:** `rustc`, `rustc_commit`, `llvm` and `flags_digest`, in KRN, and in SYM when key 2 is present. (Item 386.) |
| A3 | Key 4 `cet` | Which sub-keys are required, and which values `shstk` may take. | **Both are required.** `ibt` must be `false`. `shstk` may be `true` or `false`: "`true` by default" is the writer's default. (Item 387.) |
| A4 | Key 5 `entry` | Whether "inside" a SEGMENT means its file-backed bytes or its whole memory. | **The file-backed bytes,** `[mem_offset, mem_offset + content_size)`, of a readable and executable SEGMENT, as P3 rules for `e_entry`. (Item 388.) |
| A5 | Key 1 `build_id` | Whether a reader checks the definition or only takes the value. | **Checked.** A reader that reads every SEGMENT refuses an image whose `build_id` is not the SHA-256 of their content, concatenated in table order. For KRN that includes the loader. Where EXE and LIB images are checked is decided with their loader (M6). (Item 389.) |
| A6 | S3 key 4 `version` | Whether the SemVer grammar is checked. | **Checked** against Semantic Versioning 2.0.0's grammar. (Item 390.) |
| A7 | §7.1 entry key 8 `log_proof` | Whether a v1 entry may carry it. | **Absent in v1.** An entry holding key 8 is refused. (Item 391.) |
| A8 | §7.1 entry keys 1–7; S3 statement keys 0–6 | Which keys are required. | **All are required.** `chain` is present and may be the empty array, so direct trust has one encoding. (Item 392.) |
| A9 | §5, keys ≥ 1000 | Whether a skipped value must be valid OCBOR-1, and whether it counts toward the limits. | **Valid and counted.** A skipped value must be valid OCBOR-1: shortest forms, no floats or tags, valid UTF-8, and unsigned map keys in strictly ascending order. It counts toward the depth and item limits. NFC of its text is the writer's duty, and no v1 reader checks it. (Item 393.) |
| A10 | §7.1 `entry_size` | Whether it counts its own four bytes, and whether the map must fill it. | **The map alone.** The map fills `entry_size` exactly, and the entries fill the block exactly. (Item 394.) |
| A11 | §6.1, §6.5, G4 | How many SEGMENT sections a KRN image holds. | **At least one,** refused at the section-table check. The limit of 16 is EXE's and LIB's (G4). (Item 395.) |
| A12 | §4.4, Profiles column | Whether the column lists what a profile may hold, and whether MANIFEST's place is fixed. | **A list of what is allowed.** A kind outside its profiles is refused: STRINGS in KRN, for example. MERKLE is allowed in every profile. MANIFEST may stand anywhere in the table. (Item 396.) |
| A13 | C9; P7 | What references a MERKLE section, and whether P7's placement binds readers. | **`merkle_index` alone.** A MERKLE section is referenced by one section's `merkle_index`, under integrity 1, from anywhere in the table. P7's placement binds `onbf-pack`, not readers. (Item 397.) |
| A21 | §4.3 MAPPABLE; §6.2; C6, C8 | Which kinds may be MAPPABLE. | **SEGMENT must be, IMAGE_FS may be, and no other kind may be.** Whether IMAGE_FS must be is decided when IMAGE_FS is built. The refusal on the other kinds is an addition. (Item 398.) |
| A22 | §6.1, permissions | Whether "exactly one of R, RW (optionally RELRO) or RX", written under EXE and LIB, binds KRN. | **Every SEGMENT in every profile.** (Item 399.) |
| A24 | §6.1, RELR; P5; P7 | The word format, and whether P5's ascending slots and P7's greedy encoding bind readers. | **Little-endian u64 words, as in SHT_RELR.** A word with bit 0 clear is an address. A word with bit 0 set is a bitmap whose bits 1–63 cover the 63 slots after the running base. Readers refuse:<br>- slots that do not strictly ascend (P5);<br>- an encoding that is not greedy (P7): an empty bitmap, or an address word that the previous bitmap could have reached;<br>- a slot outside an RW or RELRO SEGMENT's file-backed bytes. (Item 400.) |
| A25 | §4.4; ONBF-R6 | How many sections of each kind a table may hold. | **At most one each** of STRINGS, RESOURCES, RELOCS, IMPORTS, EXPORTS, TLS, UNWIND, IMAGE_FS and DEBUG_LINK. Exactly one MANIFEST. SEGMENT, MERKLE, PAYLOAD, FIRMWARE and DWARF may repeat, within the other limits. For STRINGS, RESOURCES and UNWIND the limit is an addition. (Item 401.) |
| A27 | §4.3 `compression`; C14; Appendix A | Whether a compressed section may store no bytes. | **Refused at the section table:** with compression 1, `stored_size` is at least 1. (Item 402.) |
| A28 | V1; §7.1 | Where a non-empty `chain` is refused. | **By the signature-block parser,** in every profile and mode, until OKC validation exists (V1). A verifier's own refusal may stay as a second line. (Item 403.) |
| A29 | §7.1 `entry_count`; V2 | Whether one block may hold two entries with the same role and key ID. | **Refused.** No two entries of a block share both role and key ID. An addition. (Item 404.) |
| A30 | S3 key 1 `profile` | Whether a value outside §1's profile IDs is refused where the block is parsed, or only by equality with the header where it is bound. | **By the signature-block parser:** a statement `profile` outside 1–8 is refused in every mode. Equality with the header's profile is still required where the block is bound to its image. (Item 405.) |
| A31 | The signature encodings' "Follows" row (K1–K4) | Whether keys ≥ 1000 in an entry or a statement are skipped as extensions. | **Strict in v1.** An entry or statement holding a key outside its listed keys is refused, 1000 and above included. An extension of either comes with a format minor version. (Item 406.) |
| A32 | Key 24 `boot_info_abi` | The range of `major` and `minor`. | **Each is a u16,** 0 to 65,535. A larger value is refused. (Item 407.) |

## Open gaps

None at present. O1 is under "Decided gaps" and O2–O5 under "Decided for certificate chains".

## Observed by the bit-flip sweep

Flipping every bit of the header and section table of a valid KRN image shows which fields can take another valid value without being rejected:
- `format_minor` and `content_digest`;
- the entry digests;
- segment permissions, and segment placements that stay page-aligned, ordered and disjoint;
- `profile`, because KRN's sections are also valid in LIB.

**All of them are covered by `content_digest`, and the signed statement names the profile (§7.1)**, so none can change undetected. Two fields are no longer on this list: since C12, a manifest's `compression`, and since G5, the REQUIRED bit.
