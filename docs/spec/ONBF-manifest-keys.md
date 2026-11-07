# ONBF v1 manifest key registry (OCBOR-1)

| | |
|---|---|
| Applies to | `docs/spec/ONBF.md` §5 (OCBOR-1) and §6 (profile manifests) |
| Resolves | ADR-0001, "Still open": integer key numbers for the OCBOR-1 schemas |
| Status | **Policy K1–K4 and the KRN keys decided by the owner on 2026-11-07.** Keys for the other profiles are assigned when each profile is implemented, under the same policy. |

## Policy (owner decisions, 2026-11-07)

| # | Decision | Why (performance, security, native SDK and GUI) |
|---|---|---|
| K1 | **One shared registry for all profiles.** A key number means the same field in every profile. Each profile lists which keys it requires and which it allows. | There is one validator table, so the same field can never be read as a different field in another profile. Diagnostics stay consistent in the native SDK and installer UI. The common keys sit in 0–23, where they encode in one byte. |
| K2 | **Key 0 is `schema`, required in every manifest**, and v1 is `1`. A reader rejects a schema it does not know. | ONBF-R8 makes the manifest schema version independent of the container version. A breaking manifest change can never be misread as an old one. Additive changes use keys ≥ 1000 (§5). |
| K3 | **Required CPU features are one unsigned-integer bitset**, defined by the feature registry below. Unknown bits are rejected (fail closed). | The loader computes the CPU's bitset once and checks with a single AND, which is the fastest possible test. The registry also provides plain-language names for the native "this CPU lacks …" dialog (§6.1.3). |
| K4 | **The KRN manifest carries `toolchain`** in addition to its required fields, and nothing that can be derived from other data: no release version and no image span. | Provenance (CLAUDE.md §60) without a second source of truth. The SYS manifest already binds the KRN root to its release, and the SEGMENT sections already define the span. |

## Encoding rules (from §5, restated)

- Maps use unsigned-integer keys, strictly ascending, with no duplicates. An unknown key below 1000 is rejected; keys of 1000 and up are optional extensions and are skipped.
- The same rules apply to nested maps: `toolchain` and `cet` below.
- Integers use the shortest encoding. A `bool` is CBOR `true` or `false`.
- Text is NFC-normalized UTF-8.
- A version pair is a two-element array `[major, minor]` of unsigned integers.

## Top-level keys

| Key | Name | Type | Profiles | Meaning |
|---|---|---|---|---|
| 0 | `schema` | uint | all | Manifest schema version, `1` (K2). |
| 1 | `build_id` | bytes, 32 | EXE, LIB, KRN | SHA-256 of the concatenated content of all SEGMENT sections, in table order (§6.1). |
| 2 | `toolchain` | map | EXE, LIB, KRN | Build provenance; sub-keys below. |
| 3 | `cpu_features` | uint | EXE, LIB, KRN | Required features beyond the `x86-64-v3` baseline (K3). `0` means the baseline only. |
| 4 | `cet` | map | EXE, LIB, KRN | CET settings (§6.1.3); sub-keys below. |
| 5 | `entry` | uint | EXE, KRN | Entry point as an RVA. It must lie inside a SEGMENT that is readable and executable (RX). |
| 6–23 | — | — | — | Reserved for fields shared by several profiles (for example EXE/LIB `abi`, `kind`, `stack`, `tls`, `deps`). |
| 24 | `boot_info_abi` | `[major, minor]` | KRN | The boot-info structure version the kernel expects. Its contents are defined by the M1 boot ADR. The loader refuses a major it does not provide. |
| 25–999 | — | — | — | Reserved for profile-specific fields. |
| ≥ 1000 | — | any | all | Optional extensions; skipped by readers (§5). |

### `toolchain` (key 2) sub-keys

| Key | Name | Type | Meaning |
|---|---|---|---|
| 0 | `rustc` | text | `rustc` release, for example `1.99.1`. |
| 1 | `rustc_commit` | text | `rustc` commit hash, 40 lowercase hex digits. |
| 2 | `llvm` | text | LLVM version, for example `23.1.1`. |
| 3 | `flags_digest` | bytes, 32 | SHA-256 of the canonical compiler and linker flags. `onbf-pack` defines the canonical form. |

### `cet` (key 4) sub-keys

| Key | Name | Type | Meaning |
|---|---|---|---|
| 0 | `shstk` | bool | Shadow stack. `true` by default for every native image (§6.1.3). |
| 1 | `ibt` | bool | Indirect-branch tracking. Must be `false` until stable `rustc` can emit ENDBR64 (CLAUDE.md §14). |

## Profile requirements

| Profile | Required keys | Allowed optional keys |
|---|---|---|
| KRN | 0 `schema`, 1 `build_id`, 2 `toolchain`, 3 `cpu_features`, 4 `cet`, 5 `entry`, 24 `boot_info_abi` | keys ≥ 1000 |
| EXE, LIB, APP, PKG, DRV, SYS, SYM | 0 `schema`; the rest is assigned when the profile is implemented | — |

## CPU feature registry (key 3)

Each bit stands for a feature beyond `x86-64-v3`. The loader must check both CPUID *and* the matching operating-system state enable (for example XCR0 for AVX-512 or APX state) before it accepts an image. The CPUID test for each bit will be taken from the current Intel SDM and AMD APM, verified, and added to this table when the loader's CPU check is implemented. It is deliberately not written from memory here.

| Bit | Name | Feature |
|---|---|---|
| 0 | `avx512f` | AVX-512 Foundation |
| 1 | `avx10.2` | AVX10, version 2 or later |
| 2 | `apx` | Advanced Performance Extensions |
| 3 | `sha` | SHA-NI (SHA-1/SHA-256 instructions) |
| 4 | `gfni` | Galois Field New Instructions |
| 5 | `vaes` | Vector AES |
| 6–63 | — | Reserved. A set reserved bit is rejected. |

Kernel-specific requirements, such as x2APIC or 5-level paging, are added here by the M1 boot ADR.
