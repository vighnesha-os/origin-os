# Source provenance

| | |
|---|---|
| Required by | CLAUDE.md §6 |
| Rule | One entry per original module. Automated similarity scanning cannot prove independent authorship, so this file records how each module was produced. |

**Open item for the owner:** the repository has no LICENSE file yet. Every entry below therefore reads "project licence not chosen yet".

## `rust-toolchain.toml`

- **Content:** copied verbatim from CLAUDE.md §8 (v7.1).
- **Author / date:** Claude (cloud session), 2026-11-07.
- **Licensing:** project licence not chosen yet.

## `developer-tools/toolchain/verify.sh`

- **What:** a host-side verification tool for CLAUDE.md §8–§10. It is not part of the shipping OS.
- **Author / date:** Claude (cloud session), 2026-11-07, written for this project.
- **Specifications:**
  - CLAUDE.md §8–§10.
  - PE/COFF image layout: DOS `MZ` header, `PE\0\0` signature, machine 0x8664, PE32+ magic 0x20B, subsystem 10 (EFI application).
  - ELF gABI: `e_ident`, `e_type`, and `e_machine` 62 (x86-64).
  - The regression conditions in rust-lang/rust issues #163614, #163911 and #163947.
- **Architecture decisions:** CLAUDE.md §9 (results go to `docs/toolchain/VERIFICATION.md`) and ADR-0001 (toolchain targets).
- **Dependencies:**
  - rustup, rustc, cargo, rust-lld and llvm-tools (`llvm-nm`, `llvm-objdump`). These are permitted toolchain infrastructure under CLAUDE.md §4.
  - POSIX utilities: `od`, `grep`, `sed`, `sort`, `mktemp`, `date`, `head`, plus `shasum` (macOS) or `sha256sum` (Linux) for firmware hashes.
- **Toolchain:** bash 3.2 or later (the macOS default), and Rust 1.99.1 per `rust-toolchain.toml`.
- **Review:** not yet independently reviewed.
- **Licensing:** project licence not chosen yet.
- **Test evidence.** Run on 2026-11-07 in the cloud sandbox: Linux x86-64, rustc 1.97.0, no network.
  - The syntax check passes.
  - Failure paths are reported correctly, with the expected exit codes:
    - the pinned toolchain is missing
    - rustup is not on PATH
    - `--install` with downloads blocked (rustup accepted the arguments and failed at the download)
    - an unknown argument (exit 2)
  - With a 1.97.0 override, the checks that can run there behave as expected: edition 2024, running rust-lld directly, FFI bool on `x86_64-unknown-linux-gnu`, hardware-float code generation, and the unstable hardening flags.
  - The FFI-bool check **fails** on the exact miscompiled body from #163911 (`movl %edi,%eax; retq`) and passes on the correct body. Both were assembled with llvm-mc.
  - The PE header checks agree with `llvm-objdump -p` on a real EFI application linked by rust-lld.
  - **First run on the M1 Max, 2026-11-07** (Claude Code, local), rustc 1.99.1 `58cf8f9c3`, macOS 27.0.1:
    - `--install`: 18 PASS, 0 FAIL.
    - Check mode: 17 PASS, 0 FAIL (the install step is skipped).
    - The output is in `docs/toolchain/VERIFICATION.md`.
    - The `wcslen` PASS was cross-checked with a linker map: `wcslen` resolves to the weak definition in `compiler_builtins`.
- **Change, 2026-11-07** (Claude Code, local). The QEMU section now:
  - lists each firmware file once, by its resolved path;
  - includes the `edk2-i386-vars.fd` NVRAM template;
  - records a SHA-256 of each file.

  Re-verified with `bash -n` and a full check-mode run.

## `tests/toolchain/`

Author, date, toolchain, review and licensing are as for `verify.sh`.

| File | Purpose | Origin |
|---|---|---|
| `uefi_smoke.rs` | UEFI release link test, and the `wcslen` regression (#163614) | Original |
| `none_smoke.rs` | Freestanding x86-64 link test; also the direct-`rustc` run for #163947 | Original |
| `ffi_bool.rs` | FFI `bool` regression (#163911) | The function body is the public three-line reproducer from that issue, kept verbatim so the test matches the reported bug |
| `float_abi.rs` | Float-ABI probe (research experiment E2; CLAUDE.md §11) | Original |

## `Cargo.toml`, `.cargo/config.toml` (workspace)

- **What:** the virtual workspace (`resolver = "3"`), with shared lints (`unsafe_op_in_unsafe_fn`, `clippy::undocumented_unsafe_blocks`, `missing_safety_doc` and `multiple_unsafe_ops_per_block` all deny), a release profile with `codegen-units = 1`, the `cargo xtask` alias, and `-C force-frame-pointers=yes` for `x86_64-unknown-uefi` (CLAUDE.md §14).
- **Author / date:** Claude Code (local, M1 Max), 2026-11-07. Handoff task T4.
- **Architecture decisions:**
  - `default-members` is the host `xtask` only. Freestanding crates are built for their own target through the xtask, because per-package targets are unstable in Cargo.
  - No `version` field: generated version metadata comes in T5 (CLAUDE.md §62).
- **Dependencies:** none. `Cargo.lock` lists only workspace crates.
- **Licensing:** project licence not chosen yet.

## `boot/uefi/` (`origin-boot-uefi`), the UEFI loader, M1 step 1

- **What:** an EFI application (`origin-boot.efi`, 8.7 KB PE32+). It:
  - validates the system table: null, alignment, signature, revision ≥ 2.0, and `HeaderSize` covering every field read;
  - copies the table once;
  - prints a banner, the UEFI revision and the firmware vendor and revision through `ConOut`;
  - returns `EFI_UNSUPPORTED`, because Framekernel loading is not implemented yet.

  It does not load, verify or jump to anything, and it does not call `ExitBootServices`.
- **Author / date:** Claude Code (local, M1 Max), 2026-11-07. Original code written for this project.
- **Specifications:** UEFI 2.11:
  - §4.1, the image entry point;
  - §4.2, `EFI_TABLE_HEADER` and how a revision is displayed;
  - §4.3, `EFI_SYSTEM_TABLE`;
  - §12.4, `EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL.OutputString`;
  - Appendix D, status codes.

  It also follows the Microsoft x64 calling convention, through Rust's `extern "efiapi"`. Fields the loader does not use are opaque pointers, not guessed signatures.
- **Architecture decisions:**
  - CLAUDE.md §10: a `BOOLEAN` is a `u8`; none crosses the boundary yet.
  - CLAUDE.md §19.
  - No third-party crates (CLAUDE.md §5; handoff §3).
- **Dependencies:** `core`, and the first-party `origin-version` crate for the banner's release identity (since T5).
- **Toolchain:** Rust 1.99.1, `x86_64-unknown-uefi`, rust-lld (msvc flavour).
- **Unsafe inventory, reviewed 2026-11-07 by the author (not independently):**
  - Four `unsafe fn` with `# Safety` contracts: `efi_main`, `uefi::read_system_table`, `uefi::copy_char16_string`, `console::ConOut::new`.
  - Eight one-operation `unsafe` blocks, each with a SAFETY comment. Clippy enforces both rules.
  - No Rust references to firmware memory: the system table is copied after validation, and the protocol is used through raw pointers only.
  - `ConOut` is `!Send` and `!Sync`.
  - The panic handler can call `OutputString` only from Rust code, never re-entrantly from inside a firmware call.
  - Open for step 2: clear `PANIC_CONSOLE` before `ExitBootServices`.
- **Test evidence (2026-11-07):**
  - 20 host unit tests (`cargo test -p origin-boot-uefi --lib`):
    - UCS-2 chunking, CR/LF, replacement characters;
    - revision display;
    - header acceptance and rejection;
    - null and misaligned pointers;
    - bounded and unaligned `CHAR16` copy.
  - Compile-time layout assertions for the x86-64 offsets of the three structures.
  - Clippy at `-D warnings`, on the host and on `x86_64-unknown-uefi`.
  - `cargo xtask boot-test` passed in 1.8 s on QEMU 11.1.3 + edk2-stable202408 (q35, TCG):
    - The loader printed `UEFI 2.7 system table` and `firmware EDK II, revision 0x00010000`.
    - OVMF reported `failed to start Boot0001 …: Unsupported`, so the returned status reached the firmware intact.
  - After T5 the banner reads `Origin OS 0.1.0-dev UEFI loader (build 0)`. `boot-test` builds the expected banner from the same generated constants, and it passes (1.8 s).
  - QEMU only. No physical hardware yet (CLAUDE.md §74).
- **Licensing:** project licence not chosen yet.

## `boot/info/` (`origin-boot-info`), the boot-info contract v0.1 (Proposed)

- **What:** the one structure the loader hands the Framekernel (ADR-0002 B4):
  - `BootInfo`: `repr(C)`, 80 bytes, every field explicitly sized, offsets pinned at compile time;
  - `MemoryRegion`: 24 bytes;
  - `RegionKind`: 13 Origin memory kinds, so the kernel never sees UEFI structures;
  - `BootInfo::check`: validates magic, version, size and region count before anything else is trusted.

  The version is `[compatibility] boot-info = "0.1"` in `origin-version.toml`. Major 0 means unstable until ADR-0002 is accepted.
- **Author / date:** Claude Code (local, M1 Max), 2026-11-07. Original code.
- **Dependencies:** `core` and `origin-version`. `no_std`, `forbid(unsafe_code)`.
- **Test evidence (2026-11-07):**
  - 4 unit tests: magic, header acceptance and rejection, region-kind round trip, version source.
  - Clippy on the host, UEFI and kernel targets.
  - **No panic paths:** the kernel-target object references 0 external symbols.
- **Licensing:** project licence not chosen yet.

## `framekernel/` (`framekernel`), the Framekernel M1 entry

- **What:** a `no_std`, `no_main` ELF for `x86_64-unknown-none` (24 KB static PIE). The UEFI loader jumps to it after `ExitBootServices`. `_start` (`extern "sysv64"`, boot info in `rdi`):
  - programs COM1 (0x3F8) as a polled 16550 at 115200 8N1, after a bounded wait for the transmitter to drain;
  - checks that the loader applied the image's relocations, using a static pointer read volatile and compared with its RIP-relative target;
  - validates the boot info: pointer, `BootInfo::check`, and a region array that is aligned, sorted and non-overlapping;
  - prints the banner, the boot-info version, the region count, usable MiB, the load address and span, and the ACPI RSDP;
  - halts with `cli; hlt`.

  The panic handler prints to COM1 and halts. The kernel installs no page tables, GDT or IDT yet (M2).
- **Author / date:** Claude Code agent (local, M1 Max), 2026-11-07, in its own worktree, merged by the coordinator. Original code.
- **Specifications:**
  - PC16550D UART datasheet: registers and divisor;
  - the COM1 base, 0x3F8;
  - System V x86-64 psABI;
  - Intel SDM: `IN`, `OUT`, `CLI`, `HLT`;
  - ELF gABI and x86-64 psABI;
  - ONBF §6.1, §6.1.2 and §6.5;
  - boot-info v0.1.
- **Architecture decisions:**
  - lib + bin split, with the host-testable logic in the library (`forbid(unsafe_code)`);
  - `build.rs` passes rust-lld `--entry=_start --no-dynamic-linker -ztext -zseparate-loadable-segments -zmax-page-size=4096 -zcommon-page-size=4096 -znorelro -znoexecstack` for the kernel target only;
  - frame pointers through `.cargo/config.toml` (CLAUDE.md §14).
- **Dependencies:** `core`, `origin-boot-info` and `origin-version`.
- **Toolchain:** Rust 1.99.1, `x86_64-unknown-none` (soft-float, kernel code model, PIE), rust-lld (gnu flavour).
- **Unsafe inventory, reviewed by the author (not independently):**
  - Five `unsafe fn`, each with a `# Safety` contract: `_start`, `read_boot_info`, `Com1::new`, `outb`, `inb`.
  - Eleven one-operation `unsafe` blocks.
  - Port I/O touches only 0x3F8–0x3FD, confirmed in the disassembly.
- **Test evidence (2026-11-07):**
  - 20 host unit tests (`cargo test -p framekernel --lib`): boot-info and region validation, usable-MiB rounding, the exact report lines, and the UART init sequence and bounded polling.
  - Clippy on the host and on the kernel target.
  - The release ELF, checked with `llvm-readobj --elf-output-style=GNU`:
    - ET_DYN, with its entry in the RX segment;
    - PT_LOADs R, RX and RW, all page-aligned;
    - no INTERP, TLS or GNU_RELRO;
    - 38 RELATIVE relocations.
  - Two clean builds in the same path were byte-identical.
  - Boot evidence is in the `boot/uefi` step 2 entry.
- **Licensing:** project licence not chosen yet.

## `origin-version.toml` and `runtime/version/` (`origin-version`), T5

- **What:**
  - `origin-version.toml` is the canonical release identity (CLAUDE.md §62–§63) and the only place a version is written by hand. It holds:
    - the release version, SemVer 2.0 without build metadata (ONBF §6.2);
    - a monotonic `build` number;
    - the anti-rollback `security-version` (ONBF §6.5, §7.6);
    - the ONBF container version, `1.0` (ONBF §4.2).
  - These numbers are independent of each other (ONBF-R8). No other ABI identifiers exist yet; each is added when its interface first exists.
  - `origin-version` is a `no_std`, `forbid(unsafe_code)` crate. Its build script parses the file with a strict TOML-subset parser (`manifest.rs`) and generates the constants: `VERSION`, its parts, `BUILD`, `SECURITY_VERSION`, `ONBF_FORMAT_MAJOR` and `ONBF_FORMAT_MINOR`. `PRODUCT` is a plain constant.
- **Author / date:** Claude Code (local, M1 Max), 2026-11-07. Original code. The SemVer grammar follows semver.org 2.0.0.
- **Architecture decisions:**
  - The generated output depends only on `origin-version.toml`: no timestamps, host paths or git state (ONBF-R9, CLAUDE.md §75).
  - Strict parsing: unknown sections or keys, duplicates, missing keys, escapes, leading zeros and trailing text are all errors. One file has one reading (ONBF-R6).
  - The starting values (`0.1.0-dev`, build 0, security version 0) are a development default. The owner may change them.
- **Dependencies:** `core` in the library; `std` in the build script (host only).
- **Test evidence (2026-11-07):**
  - 7 unit tests, covering 33 cases. One of them checks that the generated constants equal a fresh parse of the repository file.
  - Negative checks by hand, with the files restored and compared with `cmp` afterwards:
    - changing the version to `0.2.0-rc.1` reaches the rebuilt `.efi`, which proves the build script's `rerun-if-changed` works;
    - `version = "0.2"` fails the build with `origin-version.toml:16: … expected MAJOR.MINOR.PATCH`;
    - a hand-written `version` in a crate manifest fails `xtask check`, naming the file and line.
- **Licensing:** project licence not chosen yet.

## `runtime/onbf/format/` (`onbf-format`), the ONBF parser: container, OCBOR-1, KRN manifest

- **What:**
  - **Container layer** (T9 part 1). It validates:
    - the header (§4.2), the section table (§4.3) and the kind/profile table (§4.4);
    - the file layout (§4.1): canonical section offsets, zero padding, and the signature block at EOF;
    - the Merkle cross-references (sizes and exactly one referrer).

    It exposes the §4.5 content-digest input, a digest check that takes a caller-supplied SHA-512, and `Section::verify_sha256` for SHA-256-protected sections. `Image::parse` never interprets section content (ONBF-R1).
  - **OCBOR-1 (§5), `ocbor` module.** `validate` checks a whole document in one pass with an explicit 16-frame stack and no recursion (ONBF-R10). It enforces:
    - allowed types only;
    - shortest integers and lengths;
    - unsigned, strictly ascending map keys;
    - depth, item, text and size limits;
    - strict UTF-8;
    - no trailing data.

    `Cursor` reads a validated document item by item.
  - **KRN manifest** (`KrnManifest`), from `docs/spec/ONBF-manifest-keys.md` (K1–K4) and errata R4–R5:
    - all seven keys are required;
    - unknown keys below 1000 are rejected, and extensions are skipped, including in nested maps;
    - field types, lengths and ranges are checked;
    - reserved feature bits and IBT are rejected;
    - `check_against(&Image)` confirms the profile is KRN and that `entry` lies in the content of an RX segment.
  - **Not yet included:** the signature block's contents (§7.1), Merkle verification of sections (crate `onbf-merkle`), and the other profiles' manifests.
- **Author / date:** Claude Code (local, M1 Max), 2026-11-07. Original code written from `docs/spec/ONBF.md` v1.0.
- **Architecture decisions:**
  - ADR-0001.
  - The ONBF §12 parser contract: `no_std`, no allocation, `forbid(unsafe_code)`, typed errors, no partial results. In non-test builds `clippy::{panic, unwrap_used, expect_used, indexing_slicing, arithmetic_side_effects, unreachable, todo, unimplemented, cast_possible_truncation}` are denied. A planted index and a planted unchecked addition were rejected, which confirms the lints are active.
  - Where §4 allowed more than one reading, the parser implements the owner's decisions C1–C14 in `docs/spec/ONBF-errata.md` (2026-11-07):
    - 12 keep the reading first implemented;
    - C12 makes manifests uncompressed only;
    - C14 allows empty sections only for SEGMENT;
    - C13 rejects every unknown kind. That was the owner's choice, against the recommendation to skip optional ones.
  - The owner's G4 and G5 decisions are enforced here:
    - segments are page-sized, in ascending memory order, disjoint, and end within the 2 GiB span, with at most 16 in EXE/LIB;
    - REQUIRED is set on every section.

    G2 and G3, the Merkle level numbering, bind `onbf-merkle`. MERKLE section sizes already follow G3.
  - A compile-time assertion ties the accepted `format_major` to `origin-version.toml`.
- **Dependencies:** `core`, and the first-party `origin-version` crate.
- **Toolchain:** Rust 1.99.1. Clippy-clean on the host, `x86_64-unknown-uefi` and `x86_64-unknown-none`.
- **Test evidence (2026-11-07, after the C1–C14 and G2–G5 decisions):** 17 tests.
  - Valid images: a KRN image with Merkle and bss segments; a minimal image in every profile; unsigned images in developer mode and nested; `format_minor` > 0; the digest plumbing; 16 segments in EXE and LIB; 17 in KRN; a guard gap between segments.
  - Rejected by design: unknown kinds with or without REQUIRED (C13); empty sections other than bss segments (C14).
  - **Malicious corpus, 81 cases** (ONBF §13 item 4, container part), each rejected with the exact expected error. It covers:
    - overlapping, misaligned and misplaced sections;
    - counts beyond limits, integer overflow, truncation and trailing data;
    - W+X and reserved fields;
    - Merkle mis-references and padding;
    - compressed manifests and empty sections;
    - REQUIRED not set (G5);
    - segment size, order, overlap and span, and 17 segments in an EXE (G4).
  - Sweeps:
    - every truncation of a 20,648-byte image is rejected;
    - every one of 10,732 padding bytes is checked;
    - all 5,120 single-bit flips of the header and table are rejected, except in fields with other valid values, all of which are digest-covered (errata, "Observed by the bit-flip sweep").
  - Since the SHA-2 work, content digests in the tests are real SHA-512 from `origin-crypto`, a dev-dependency only.
  - **OCBOR-1 and KRN manifest** (15 more tests, 32 in all, in `tests_ocbor.rs` plus two in `tests.rs`):
    - **RFC 8949 Appendix A:** 34 examples OCBOR-1 accepts decode to the right items. 13 that the profile excludes (floats, tags including a bignum, `undefined`, simple values, indefinite lengths, text keys) are rejected for the right reason.
    - **§13 item 4 manifest corpus:**
      - 10 non-shortest encodings;
      - 15 malformed items (reserved additional information, a stray break, truncations, trailing data, an overlong NUL, a surrogate);
      - 6 key-order cases (descending, duplicate, negative, bytes and bool keys, a nested map);
      - limits at their exact boundaries: depth 16 and 17, 65,536 and 65,537 items, 64 KiB and 64 KiB + 1 of text, 1 MiB + 1 of document.
    - **KRN schema:** every required key missing; unknown keys 6, 23, 25 and 999; extensions skipped at the top level and in nested maps; 20 invalid-field cases.
    - **Loader order:** parse, content digest, manifest SHA-256, decode, entry check, end to end with real SHA-256 and SHA-512. A tampered manifest fails its digest before decoding. The entry check is tested at 7 boundaries, plus a KRN manifest inside an EXE image.
    - **Canonical-encoding property (ONBF-R6):** all 1,208 single-bit flips of a KRN manifest were tried. Every flip the decoder accepts re-encodes to exactly the flipped bytes, so no accepted document has a second reading. Every truncation is rejected.
  - **No panic paths:** the release object for `x86_64-unknown-none` references only `memset` and `core::str::from_utf8` (`xtask check`, no-panic step). The latter was added to the allowlist with a justification: it is total and RFC 3629-strict, and under `forbid(unsafe_code)` it is the only way to turn bytes into `&str`.
  - Not done yet: fuzzing (§13 item 3), Kani bounds (item 5) and the N-version check (item 6).
- **Licensing:** project licence not chosen yet.

## `runtime/onbf/merkle/` (`onbf-merkle`), Merkle-SHA256-4K

- **What:** the ONBF §4.6 trees, with the owner's G2 and G3 decisions and readings R1–R3 (`docs/spec/ONBF-errata.md`):
  - `Shape`: leaves, levels and MERKLE-section positions, highest level first;
  - `root`: one streaming pass, for whole-section verification such as the boot loader checking KRN segments;
  - `build`: writes the MERKLE section, for `onbf-pack`;
  - `verify_block`: checks one 4 KiB block by walking one hash block per level, stopping at the first hash block a caller-supplied `TrustCache` already trusts. This is the ONBF-R2 page-fault path.
- **Design:**
  - Child hashes stream into one SHA-256 state per level (at most 4 levels for 1 TiB), so there are **no 4 KiB buffers**: the stack use is a few hundred bytes, which suits kernel stacks.
  - Iterative, with no recursion (ONBF-R10).
  - Hash blocks are marked trusted only after the walk reaches the root or a trusted ancestor.
- **Author / date:** Claude Code (local, M1 Max), 2026-11-07. Original code written from ONBF §4.6. The design approach is credited in the spec to Fuchsia blobfs and fs-verity; no code from either.
- **Architecture decisions:** the ONBF §12 contract (`no_std`, no alloc, `forbid(unsafe_code)`, no-panic lints) and ADR-0001.
- **Dependencies:**
  - `core` and the first-party `origin-crypto` (SHA-256).
  - The first-party `onbf-format` as a dev-dependency, to cross-check MERKLE section sizes.
- **Toolchain:** Rust 1.99.1. Clippy-clean on the host, `x86_64-unknown-uefi` and `x86_64-unknown-none`. `[profile.dev.package.*] opt-level = 3` for `origin-crypto` and `onbf-merkle`, so that the tests (about 200 MB hashed) take 1.1 s instead of 16.7 s.
- **Test evidence (2026-11-07):** 12 tests, plus 1 ignored benchmark.
  - **Independent reference:** `tools/merkle_reference.py` implements §4.6 differently (whole levels as lists, materialized blocks) over Python `hashlib` (OpenSSL). For 17 sizes up to 64 MiB + 1 (3 levels), `root`, `build` and `Shape` agree with it on every root and on the SHA-256 of every MERKLE section's bytes.
  - **Shape:** equals `onbf_format::merkle_hash_blocks` for 606 sizes up to 1 TiB, which has 4 levels. Larger content is rejected.
  - **Every block verifies:** for 6 sizes across 1–2 levels, and for all 16,385 blocks of a 3-level tree with a warm cache, which ends up trusting all 132 hash blocks exactly.
  - **Attacks (§13 item 4, "Merkle block swap"):** each fails with the exact expected error.
    - Tampered data at 0, 1 and 2 levels.
    - Swapped data blocks (each leaf is bound to its index).
    - Swapped hash blocks.
    - Tampered hash-block padding, an upper-level slot and an unused slot.
    - A wrong root, or the same bytes under a different `content_size`.
    - A bad index, a bad block length and a bad section length.
  - **Cache:** after the first verification, a sibling block stops after one lookup. A tampered block fails against a warm cache and adds nothing to it.
  - **The root binds `content_size`:** a trailing zero byte inside the last block changes the root.
  - **No panic paths:** the release object for `x86_64-unknown-none` references only `memcpy`, `memset` and `origin_crypto`'s `Sha256::update`/`finalize`, which are themselves checked. A planted latent `split_at` passed Clippy and the tests, and was caught only by the no-panic step (`core::panicking::panic_fmt`).
  - **Throughput on the M1 Max host, portable SHA-256, release:** `root` 279 MiB/s, against 285 MiB/s for plain SHA-256. That is about 2% tree overhead, as expected from 1 extra hash per 128 blocks.
  - Not done: a second Merkle implementation by a different author (§13 item 6), integration of `verify_block` with `onbf-format` sections, and Kani bounds.
- **Licensing:** project licence not chosen yet.

## `security/crypto/` (`origin-crypto`), SHA-2, SHA-3 and SHAKE

- **What:** streaming and one-shot SHA-256 and SHA-512 (FIPS 180-4 §6.2, §6.4), with the §5.1 padding shared through a generic block buffer. The schedule is a 16-word ring.
  - These are the hashes ONBF needs: SHA-512 for content digests, SHA-256 for section digests and Merkle trees.
  - Portable Rust only. SHA-NI with CPUID detection is planned (ONBF §4.6, CLAUDE.md §24).
- **Author / date:** Claude Code (local, M1 Max), 2026-11-07. Original code written from FIPS 180-4.
- **Constants:**
  - Not typed from memory. They were generated by `security/crypto/tools/derive_sha2_constants.py` from their definition: the fractional bits of the square and cube roots of the first 80 primes, computed with exact integer roots.
  - The script asserts that each 32-bit table is the high half of the 64-bit one.
  - The tests re-derive the SHA-256 tables independently in Rust (`u128` integer roots).
- **Architecture decisions:**
  - The ONBF §12 contract: `no_std`, no allocation, `forbid(unsafe_code)`. The same no-panic Clippy lints as `onbf-format` are denied in non-test builds.
  - Verification only, with no signing (ADR-0001 D3).
  - `Debug` prints no internal state (CLAUDE.md §61, secret-safe logging).
- **Side-channel assessment (CLAUDE.md §59):**
  - No data-dependent branches or memory indices. Round constants are indexed by round number, and branches depend only on public lengths.
  - In Origin's verification paths SHA-2 hashes public data.
  - **Not done:** zeroizing the state on drop, which needs volatile writes and therefore a reviewed `unsafe`. Add it before any secret is hashed (for example HMAC keys or password hashing, §57).
- **Dependencies:** `core` only.
- **Toolchain:** Rust 1.99.1. Clippy-clean on the host, `x86_64-unknown-uefi` and `x86_64-unknown-none`.
- **Test evidence (2026-11-07):** 8 tests, plus 1 ignored benchmark.
  - **Known-answer tests:** the FIPS 180-4 example messages, five per algorithm (empty, "abc", 448-bit, 896-bit, one million "a"). Each expected digest was confirmed beforehand against Python `hashlib` (OpenSSL) and macOS `shasum` (Perl Digest::SHA).
  - **Differential tests (ONBF §13 item 2):** 310 lengths (0–300 plus 511–1,000,003) in `tests/vectors/sha2-differential.txt`. The file was generated by `tools/gen_sha2_vectors.py`, which writes a line only where `hashlib` and `shasum` agree.
  - **Streaming:** every two-piece split, and 1-, 7-, 64- and 129-byte pieces, of 15 boundary lengths match the one-shot digest.
  - **Other:** clone forks the stream, and `Debug` hides the state.
  - **No panic paths:** the release object for `x86_64-unknown-none` references only `memcpy`, `memset` and `Formatter::write_str`.
    - The first version referenced `copy_from_slice`'s length-mismatch panic, which no lint flags. It was replaced with a non-panicking copy.
    - Putting it back makes `xtask check` fail and name the symbol.
  - **Throughput on the M1 Max host (aarch64, portable code, release, single thread):** SHA-256 285 MiB/s, SHA-512 462 MiB/s. This is indicative only: the target is x86-64, and SHA-NI is not used yet.
  - Not done: NIST CAVP/ACVP vector files (downloading them needs the owner's approval), SHA-NI, and an independent review.
- **Licensing:** project licence not chosen yet.

### SHA-3 and SHAKE (FIPS 202)

- **What:**
  - Streaming SHA3-256 and SHA3-512.
  - SHAKE128 and SHAKE256, with `finalize_xof() -> ShakeReader`, whose `squeeze` can be called repeatedly.
  - Keccak-f[1600] on `[u64; 25]`.
  - The sponge reuses the SHA-2 block buffer, which gains the FIPS 202 byte-oriented padding: suffix 0x06 or 0x1F, then the last byte `|= 0x80`.

  ML-DSA (FIPS 204), half of every ONBF signature suite, is built on SHAKE (ONBF §7.2).
- **Author / date:** Claude Code agent (local, M1 Max), 2026-11-07, in its own worktree, merged by the coordinator. Original code from FIPS 202.
- **Constants:**
  - None are typed in. Const fns compute the round constants (Algorithm 5 LFSR, placed per Algorithm 6), the ρ offsets (the §3.2.2 walk) and the π mapping (Algorithm 3).
  - The tests re-derive all three another way:
    - the round constants as x^t mod x^8+x^6+x^5+x^4+1 over GF(2);
    - the ρ positions from the matrix power [[0,1],[2,3]]^t mod 5;
    - π in the forward form.
- **Side channels:** the permutation uses fixed lane positions only (ρ compiles to immediate rotates, π to no table loads). Branches depend only on public lengths. State is not zeroized on drop yet.
- **Test evidence (2026-11-07):** 11 tests, plus 1 ignored benchmark.
  - **Known answers:** "" and "abc", and SHAKE at 32, 64 and 1000 bytes.
  - **Differential tests:** 304 lengths, written only where hashlib's OpenSSL 3.5.7 and HACL* backends agree.
  - **Streaming:** splits around every rate, and SHAKE squeezed in pieces.
  - **Mutation check:** five planted faults, one at a time; all were caught.
  - **No panic paths:** the kernel-target object references only `memcpy`, `memset` and `Formatter::write_str`.
  - **Throughput:** SHA3-256 581 MiB/s and SHAKE256 about 580 MiB/s on the M1 host (aarch64, portable code). Indicative only.
  - Not done: NIST CAVP/ACVP files, and an independent review.

## `developer-tools/xtask/` (`xtask`)

- **What:** host-side build orchestration, with four commands:
  - `check`:
    - rustfmt, clippy and tests per crate and target. The freestanding libraries (`onbf-format`, `onbf-merkle`, `origin-crypto`) are checked on the host, `x86_64-unknown-uefi` and `x86_64-unknown-none`;
    - the UEFI release build;
    - a **no-panic step**: each freestanding library is rebuilt for the kernel target in release mode. Its object may reference only:
      - `memcpy`, `memmove`, `memset` or `memcmp`;
      - the total `core` functions `Formatter::write_str` and `str::from_utf8`;
      - symbols *defined by another freestanding library's object*, since those pass the same check;
    - two lints:
      - CLAUDE.md §11: no `target_os = "linux"` in any Rust file outside `developer-tools/`;
      - CLAUDE.md §62: no `version` in the `[package]` or `[workspace.package]` section of any Cargo manifest.
  - `build`.
  - `boot-test`: stages a fresh ESP (`\EFI\BOOT\BOOTX64.EFI`, UEFI 2.11 §3.5.1.1) and a fresh NVRAM copy; boots QEMU with serial on a pipe; requires the loader's lines and OVMF's status line in order; then stops QEMU.
  - `run`.

  It is not part of the shipping OS.
- **Author / date:** Claude Code (local, M1 Max), 2026-11-07. Original code.
- **Dependencies:** Rust `std` (CLAUDE.md §4) and the first-party `origin-version` crate; no third-party crates. `#![forbid(unsafe_code)]`. At run time it needs QEMU and the EDK2 firmware, found next to `qemu-system-x86_64` or through `ORIGIN_OVMF_CODE` and `ORIGIN_OVMF_VARS`.
- **Test evidence (2026-11-07):**
  - 3 unit tests: escape-sequence stripping and in-order matching.
  - Failure paths checked by hand:
    - a timeout below the boot time fails with exit 1 and QEMU is killed;
    - missing firmware is reported by path;
    - an unknown command exits 2.
- **Licensing:** project licence not chosen yet.
