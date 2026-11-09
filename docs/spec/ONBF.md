# Origin Native Binary Format (ONBF)

| | |
|---|---|
| Document | `docs/spec/ONBF.md` |
| Version | 1.0 (design) |
| Status | **Accepted by the owner on 2026-11-07** (§2; `docs/adr/ADR-0001-onbf.md`). Byte layouts become v1.0 final only after the §13 parser tests pass. Until then, any layout change needs an ADR amendment. |
| Clarifications | `docs/spec/ONBF-errata.md`. C1–C14 and G2–G5 were decided by the owner on 2026-11-07 and are normative. They narrow §4 and define the §4.6 Merkle level numbering, for example: every unknown section kind is rejected (C13); manifests are never compressed (C12); segments are page-sized, sorted and disjoint (G4); REQUIRED is set on every section (G5); SYM's DWARF data has its own kind (G1). |
| Manifest keys | `docs/spec/ONBF-manifest-keys.md`: one shared OCBOR-1 key registry for all profiles. Key 0 is `schema`, CPU features are a bitset, and the KRN keys are assigned (owner decisions K1–K4, 2026-11-07). |
| Research baseline | 2026-11-07 |
| Toolchain | Rust 1.99.1 stable, edition 2024 (CLAUDE.md §8) |
| Implements | CLAUDE.md §19, §42–47, §58–60, §62–64, §70 |
| Evidence | `docs/research/2026-11-07-onbf.md` (sources R1–R20, local experiments E1–E4) |

The keywords MUST, MUST NOT, SHOULD and MAY are used as in RFC 2119.

---

## 1. What ONBF is

ONBF is **one** binary container: one parser, one integrity scheme and one signature scheme for every native Origin artifact. Each artifact kind is a **profile** of the container.

| ID | Profile | Extension | What it is | How it lives on disk |
|---|---|---|---|---|
| 1 | EXE | `.ose` | Native executable | Stored as-is; mapped by the Loader Service |
| 2 | LIB | `.osl` *(added in CLAUDE.md v7.1)* | Shared library: system SDK libraries, app-private plugins | Same as EXE |
| 3 | APP | `.oapp` | Sealed, read-only application image (executables + resources) | Stored as-is; **never extracted** |
| 4 | PKG | `.osi` | Signed installer: one or more APP/LIB/DRV payloads plus install-screen text | Unpacked into the blob store, then discarded |
| 5 | DRV | `.od` | Driver package: driver binary, device-match rules, firmware, privileges | Stored in the driver store |
| 6 | KRN | `.onbf` | Framekernel image, loaded by the Origin boot loader | Part of the system deployment |
| 7 | SYS | `.onbf` | Signed unified release manifest (CLAUDE.md §62–63) | Part of the system deployment |
| 8 | SYM | `.osym` | Detached debug symbols, keyed by build ID; never installed on user machines | Symbol server |

Why one format instead of four: one hardened parser and verifier (P0), one fuzz corpus and test suite (P8), one zero-copy mapping path (P1).

The UEFI boot loader is **not** ONBF. Firmware requires PE32+ and verifies it with Authenticode; see §8.1.

Everything in ONBF is 64-bit x86-64. There is no 32-bit path and no foreign-format path (ONBF-R11).

---

## 2. Owner decisions (2026-11-07)

| # | Decision | Chosen | Where it shows in this spec |
|---|---|---|---|
| D1 | `.ose` payload | **Native ONBF segments.** ELF64 exists only as linker output on the build host. | §6.1.2: `onbf-pack` converts it; the loader never parses ELF |
| D2 | Third-party drivers | **User mode only.** Kernel-mode drivers are first-party, built from Origin source in the unified release. | §6.4, §8.4 |
| D3 | Signature algorithms | **Hybrid Ed25519 + ML-DSA-65.** Root, kernel and release keys use Ed25519 + ML-DSA-87. Both signatures must verify. | §7.1, §7.2 |
| D4 | Who may publish | **Open, in tiers.** Registration is free. Unverified apps need consent and are always sandboxed. Drivers always need Origin attestation. | §7.5, §7.7 |
| D5 | Platform scope | **64-bit only.** No Windows `.exe` or `.msi` support, and no 32-bit support of any kind. There is no foreign-format handler. | ONBF-R11, §4.2 `arch` |

The full alternatives, evidence and trade-offs are in ADR-0001. The Appendix B defaults are approved.

Still open, for the M1 boot ADR: the boot trust anchor (signing under Microsoft UEFI CA 2023, or Origin keys enrolled in firmware). ONBF does not depend on it (§8.1).

---

## 3. Design rules (normative)

- **ONBF-R1 Verify before parse.** Before signature verification, only the fixed 128-byte header and the fixed-size section table are read. Both are hashed as raw bytes. The manifest and every variable-length structure are parsed only after a signature over their digest has verified.
- **ONBF-R2 Continuous integrity.** An image is never trusted just because it "was verified at install". Every page is verified against its Merkle root when it enters memory (§4.6, §8.2).
- **ONBF-R3 No install-time code.** Packages are purely declarative. No installer, script or custom action runs at install, upgrade or uninstall (CLAUDE.md §46). §9.2 shows what replaces them.
- **ONBF-R4 Execute only verified code.** The Framekernel maps a page executable only if it comes from a sealed object whose Merkle root is registered as executable. JIT requires an explicit capability (§8.2).
- **ONBF-R5 W^X always.** No mapping and no section is ever both writable and executable.
- **ONBF-R6 One valid reading.** Reserved fields MUST be zero. Unknown REQUIRED sections are rejected. Trailing or unreferenced bytes are rejected. Non-canonical encodings are rejected. Two correct parsers therefore can never disagree about a file.
- **ONBF-R7 No compiler layouts.** Parsers decode every field explicitly from byte slices: no `transmute`, no casting of untrusted bytes to structs (CLAUDE.md §42). A boolean that crosses an ABI or IPC boundary is a `u8` checked to be 0 or 1. Any other value in a Rust `bool` is undefined behaviour, and the Rust 1.99.1 FFI-bool regression (R2) shows how fragile this boundary is.
- **ONBF-R8 Separate versions.** The container version, Origin userspace ABI version, driver ABI version, manifest schema version and product release version are independent numbers (CLAUDE.md §63).
- **ONBF-R9 Reproducible.** Signed content contains no build timestamps and no host paths. Tables are sorted and encodings deterministic. The same inputs produce bit-identical output (CLAUDE.md §60, §75).
- **ONBF-R10 Bounded.** Every count, size and nesting depth has a hard limit (Appendix A). Parsing is O(input) with no recursion on untrusted depth.
- **ONBF-R11 64-bit only (D5).**
  - The header `arch` field accepts one value: `1`, meaning x86-64 (LP64). There is no IA-32, no x32 (ILP32) and no 16-bit code.
  - `onbf-pack` rejects `ELFCLASS32`, `EM_386` and x32 objects.
  - The loader never parses PE/COFF, MSI or any other foreign format.
  - The Framekernel has no 32-bit compatibility mode and no 32-bit system-call entry.

---

## 4. Container

### 4.1 File layout

```text
offset 0      Header (128 B)
offset 128    Section table: section_count × 128 B
              zero padding to the next 4 KiB boundary
              Sections, in ascending file offset, non-overlapping
                (MAPPABLE sections start on 4 KiB boundaries; other sections on 8 B)
              Signature block: last item, ends exactly at EOF
```

### 4.2 Header (128 bytes, little-endian)

| Offset | Size | Field | Rule |
|---|---|---|---|
| 0 | 8 | `magic` | `89 4F 4E 42 46 0D 0A 1A` ("\x89ONBF\r\n\x1a"). Detects 7-bit stripping, CR/LF rewriting and text-mode truncation. |
| 8 | 2 | `format_major` | `1`. Any other value: reject. |
| 10 | 2 | `format_minor` | `0`. Higher minors only add non-REQUIRED sections or manifest keys ≥ 1000. |
| 12 | 2 | `profile` | IDs from §1 |
| 14 | 2 | `header_size` | `128` |
| 16 | 4 | `header_flags` | `0` (all bits reserved) |
| 20 | 4 | `section_count` | 1–256 |
| 24 | 8 | `section_table_offset` | `128` |
| 32 | 8 | `signature_block_offset` | `0` means unsigned. Allowed only for an image covered by a verified parent, or in developer mode. |
| 40 | 8 | `signature_block_size` | ≤ 1 MiB |
| 48 | 64 | `content_digest` | SHA-512; see §4.5 |
| 112 | 2 | `arch` | `1` = x86-64 (LP64), the only accepted value (ONBF-R11) |
| 114 | 14 | reserved | `0` |

### 4.3 Section entry (128 bytes)

| Offset | Size | Field | Rule |
|---|---|---|---|
| 0 | 4 | `kind` | §4.4 |
| 4 | 4 | `flags` | bit0 REQUIRED, bit1 MAPPABLE, bit2 READ, bit3 WRITE, bit4 EXECUTE, bit5 RELRO. WRITE together with EXECUTE: reject. Other bits: `0`. |
| 8 | 8 | `file_offset` | Aligned to 4 KiB if MAPPABLE, else to 8 B. Within the file; no overlap with other sections or the signature block. |
| 16 | 8 | `stored_size` | Bytes in the file |
| 24 | 8 | `content_size` | Bytes after decompression. Equals `stored_size` when uncompressed. |
| 32 | 8 | `mem_offset` | Segment RVA (EXE/LIB/KRN) aligned to 4 KiB; otherwise `0` |
| 40 | 8 | `mem_size` | ≥ `content_size`; the tail is zero-filled (bss) |
| 48 | 1 | `compression` | `0` none, `1` zstd (RFC 8878). Never on MAPPABLE sections. |
| 49 | 1 | `integrity` | `0` SHA-256 of content; `1` Merkle-SHA256-4K; `2` Merkle hash blocks (MERKLE sections only); `3` covered by parent (nested images only, asserted by the caller) |
| 50 | 2 | reserved | `0` |
| 52 | 4 | `merkle_index` | Index of the MERKLE section when `integrity = 1`; otherwise `0xFFFF_FFFF` |
| 56 | 8 | reserved | `0` |
| 64 | 32 | `digest` | SHA-256 digest or Merkle root |
| 96 | 32 | reserved | `0`. Room for a second-algorithm root without a major version bump. |

### 4.4 Section kinds

| Kind | Name | Profiles | Notes |
|---|---|---|---|
| `0x0001` | MANIFEST | all | Exactly one; REQUIRED; integrity 0; OCBOR-1 (§5) |
| `0x0002` | STRINGS | EXE, APP, PKG, DRV | Localized string tables for UI |
| `0x0003` | RESOURCES | EXE, APP, PKG, DRV | Icons and small assets the install screen needs |
| `0x0010` | SEGMENT | EXE, LIB, KRN | MAPPABLE; permissions in `flags` |
| `0x0011` | RELOCS | EXE, LIB, KRN | Relative relocations only (§6.1) |
| `0x0012` | IMPORTS | EXE, LIB | Symbol imports bound eagerly at load |
| `0x0013` | EXPORTS | LIB, KRN | Sorted export table |
| `0x0014` | TLS | EXE, LIB | Thread-local template |
| `0x0015` | UNWIND | EXE, LIB, KRN | Unwind tables (DWARF CFI) |
| `0x0020` | IMAGE_FS | APP, DRV | Read-only file tree (§6.2) |
| `0x0030` | PAYLOAD | PKG | One nested ONBF image |
| `0x0040` | FIRMWARE | DRV | One device-firmware blob |
| `0x00F0` | MERKLE | any | Merkle hash blocks |
| `0x00F1` | DEBUG_LINK | EXE, LIB, KRN | Build ID and symbol-file reference |
| `0x00F2` | DWARF | SYM | One DWARF section of a detached symbol file; the SYM manifest's `dwarf_sections` names it (errata G1) |

A reader MUST reject an unknown kind that has REQUIRED set. It MAY skip an unknown kind without REQUIRED; that kind is still covered by the content digest. ONBF v1 has no vendor-defined section kinds.

### 4.5 Content digest

`content_digest = SHA-512( header[0..32] ‖ header[112..128] ‖ section_table )`

The signature location (bytes 32–47) and the digest field itself (48–111) are excluded. That lets a notary append a counter-signature without changing the digest. Each section entry carries that section's own digest or Merkle root, so every byte of every section is covered transitively. The signed statement (§7.1) contains `content_digest`.

### 4.6 Merkle-SHA256-4K

This is used for every section that is mapped or read lazily. It follows the approach of Fuchsia blobfs and Linux fs-verity (R10, R11), with explicit domain separation. Only the design is borrowed; no code is reused:

1. Split the content into 4096-byte blocks and zero-pad the last one. Leaf hash: `h0[i] = SHA-256(0x00 ‖ le64(i) ‖ block_i)`.
2. Pack hashes 128 per 4 KiB **hash block**, zero-padding the last. The next level's hash is `hL[j] = SHA-256(0x01 ‖ le8(L) ‖ le64(j) ‖ hash_block_j)`. Repeat until one hash, `top`, remains. With a single data block, `top = h0[0]` and `levels = 0`.
3. `root = SHA-256("ONBF-MERKLE-1" ‖ le64(content_size) ‖ le8(levels) ‖ top)`. For empty content, `levels = 0` and `top` is 32 zero bytes.
4. The MERKLE section stores the hash blocks with the highest level first.

Verifying block *i* walks one hash block per level up to `root`. Hash blocks that have been verified are cached as trusted. SHA-256 uses the SHA-NI instructions when CPUID reports them, with a portable fallback (CLAUDE.md §24: discover, don't assume).

---

## 5. Manifest encoding: OCBOR-1

The manifest is a constrained **deterministic CBOR** profile: RFC 8949 "core deterministic encoding" plus these limits.

- **Allowed:** unsigned and negative integers, byte strings, UTF-8 text strings, arrays, maps, `false`, `true`, `null`.
- **Not allowed:** floats, tags, indefinite lengths, `undefined`, or non-shortest integer forms.
- **Map keys** are unsigned integers defined by the profile schema, strictly ascending, with no duplicates. An unknown key below 1000 is rejected. An unknown key at 1000 or above is an optional extension and is skipped.
- **Identifiers** (app IDs, paths) MUST be NFC-normalized UTF-8.
- **Limits:** depth ≤ 16, ≤ 65,536 items per container, text ≤ 64 KiB, manifest ≤ 1 MiB.

Developers write a TOML source manifest (`origin.toml`). The host tool `onbf-pack` compiles it to OCBOR-1, so no device-side code ever parses TOML. Integer key numbers are assigned when `onbf-format` is implemented (an open item in ADR-0001). The field lists below are normative in content.

---

## 6. Profiles

### 6.1 EXE `.ose` and LIB `.osl`

**Image descriptor (manifest):**

- `abi` = Origin userspace ABI `{major, minor}` (CLAUDE.md §42)
- `kind` = exe | lib
- `entry` RVA (exe only)
- `image_span` ≤ 2 GiB, which keeps the x86-64 small code model valid
- `stack` reserve and initial commit
- `isa` (§6.1.3)
- `cet` (§6.1.3)
- `tls` = {size, align, init_size}
- `deps` = list of {library, abi_major, min_abi_minor}
- `build_id` = SHA-256 over the segment contents
- `toolchain` = {rustc version + commit hash, LLVM version, flags digest}, for provenance (CLAUDE.md §60)
- `capabilities` (standalone `.ose` only; inside an `.oapp`, the app manifest governs)

**Segments.**

- 1–16 SEGMENT sections.
- Each is MAPPABLE, with a 4 KiB-aligned file offset and RVA, in ascending, non-overlapping RVA order.
- Permissions are exactly one of R, RW (optionally RELRO) or RX.
- Padding never puts two segments with different permissions in the same page.

**Relocations.**

- Only base-relative relocations (`*(u64)(base + rva) += base`), encoded RELR-style: an address word followed by bitmap words that cover the next 63 slots.
- Targets MUST lie in RW or RELRO segments and be 8-byte aligned. There are no text relocations.
- RELRO segments become read-only once binding completes.

**Imports and exports.**

- Imports are `{dep_index, symbol, min_abi_minor, slot_rva}`, bound eagerly at load: full RELRO, no lazy PLT.
- Exports are sorted by name: `{name, rva, kind, since_abi_minor}`.
- A missing symbol fails the launch with a typed error that the shell shows as "needs Origin version X". It never crashes later.

**Syscalls are not ABI.** System-call numbers are private to a release. The only stable binary interface is the C ABI of the Origin system libraries (`origin.core` and the SDK libraries; CLAUDE.md §49). The kernel accepts system calls only from `origin.core`'s code range (syscall-origin pinning).

#### 6.1.1 TLS and unwinding
- x86-64 thread pointer in FS. Static TLS for executables and system libraries; descriptor-based TLS for plugins. The details belong to the user-ABI ADR (CLAUDE.md §42).
- All Origin code is built with `-C force-frame-pointers=yes`, so crash stacks and profiling work without unwinding.
- UNWIND sections carry DWARF CFI for runtimes that unwind.
- Until the Origin `std` port exists, userspace Rust uses `panic=abort`: stable `no_std` cannot define an unwinding personality (research note, T4).

#### 6.1.2 Conversion from linker output (`onbf-pack`, D1)
Input: an ELF64 x86-64 `ET_DYN` (PIE) produced by `rust-lld`.

**Hard errors (the image is rejected if any is present):**
- `PT_INTERP`
- any writable and executable segment
- an executable stack
- text relocations
- any relocation type other than RELATIVE, GLOB_DAT, JUMP_SLOT (converted to eager binding) and the TLS types allowed by the ABI ADR
- non-empty `.init_array`, `.fini_array` or `.preinit_array` (v1 has no static constructors)
- an undefined symbol not exported by a declared Origin library
- an image span over 2 GiB
- an unknown dynamic tag

**Output:**
- segments, page-aligned and never sharing pages across permissions
- RELR-encoded relocations
- import and export tables
- build ID
- DWARF moved into a `.osym`

Determinism test: the same ELF and the same manifest must produce byte-identical ONBF.

#### 6.1.3 CPU features and CET
- **Baseline: `x86-64-v3`** (AVX2, BMI2, FMA, MOVBE) for all first-party code. It is supported by every Intel and AMD client platform in scope.
- **Not baseline: `x86-64-v4` / AVX-512.** Panther Lake lacks it. Nova Lake adds AVX10.2 and APX (R12). AMD Zen 5 has AVX-512.
- Images list any further required features: `avx512f`, `avx10.2`, `apx`, `sha`, `gfni`, `vaes`, and so on. The loader checks CPUID before mapping. On a mismatch it refuses with a plain-language dialog, never a `#UD` crash.
- **Shadow stack (CET-SS)** is on by default for every native image (`cet.shstk = true`). Intel has supported it since Tiger Lake and AMD since Zen 3 (R13). It needs no special code generation for code without custom stack switching.
- **IBT** stays off: `cet.ibt = false`, reserved for later. It needs ENDBR64 code generation (`-Z cf-protection`), which is still unstable in Rust (R14, E3). AMD processors have so far implemented shadow stacks but not IBT (R13), so the loader enables each feature only where CPUID reports it. It becomes default-on once stable `rustc` can emit it.

### 6.2 APP `.oapp`

**Manifest:**
- `id` (reverse-DNS). It MUST fall inside a namespace that the publisher's certificate owns, which prevents ID squatting.
- `version` (SemVer 2.0 plus a monotonic build number)
- localized `name` and `description`
- `main` executable path
- `entry_points`:
  - window app
  - background agent
  - user service
  - system service (admin consent and a verified publisher only)
  - out-of-process extension handlers (file previewers, share targets, and so on)
- `file_types` and `url_schemes`
- `capabilities`, each with a localized plain-language reason
- `launch` policy
- `update` source (repository ID + channel)
- `data_schema` version
- `requires` (minimum Origin ABI and release; ISA; GPU/NPU features)
- `privacy` declarations

The publisher name shown anywhere in the UI comes **only** from the verified certificate, never from the manifest.

**IMAGE_FS** is a read-only tree.
- Directory table: sorted NFC names; no `..`, absolute paths or duplicates (including case-folded collisions); depth ≤ 16.
- Every file starts on a 4 KiB boundary, so contained `.ose`/`.osl` files map straight from the page cache.
- One Merkle tree covers the whole IMAGE_FS. Nested images use `integrity = 3`.

**Icons:** a small binary vector-path format (specified in the UI ADR) is preferred for high-DPI; PNG is allowed. All images are decoded only **after** signature verification, and only in an unprivileged decoder process.

An `.oapp` can be run straight from Downloads without installing (the "Try" action). It is verified the same way and gets a temporary data container.

### 6.3 PKG `.osi`

**Manifest:**
- package `id`, `version`
- `payloads[]`, each {profile, id, version, Merkle root, content_size, required/optional, ISA variant}
- `scope` (user | system)
- `depends` (minimum Origin ABI and release; runtime packs by id and version range; no arbitrary app-to-app graphs)
- `disk_space` upper bound
- localized `title`, `summary`, `release_notes`; optional `license` text, shown before installing
- `replaces` / `upgrade_from`

**Payloads** are PAYLOAD sections, zstd-compressed for transport. Each is a complete nested ONBF image whose root is listed in the signed PKG manifest. Installing verifies the PKG signature, then each payload against its listed root, then stores each payload as a content-addressed blob. The `.osi` file itself is not kept.

A bare `.oapp` that the user opens is handled as a PKG with one payload.

### 6.4 DRV `.od`

**Manifest:**
- `id`, `version`
- `driver_abi` {major, minor}
- `class`: user-mode, or built-in (first-party only, per D2)
- `match[]`, with deterministic precedence (exact device > subsystem > class; vendor > Origin generic; then highest version):
  - PCI vendor/device/subsystem/class/revision
  - USB VID/PID/bcdDevice/class/subclass/protocol/interface
  - ACPI `_HID`/`_CID`
  - USB4/Thunderbolt router IDs
  - HID usages
- `resources`:
  - BARs by index with maximum size
  - MSI-X vector count
  - DMA, **always through an IOMMU domain**
  - mediated config-space access
  - an ACPI method allow-list
  - power states
- `firmware[]` = {name, SHA-256, size, license, redistribution status} (CLAUDE.md §38)
- `provides` (typed driver interfaces and versions)
- `depends` (other driver interfaces)
- `recovery` = {restart limit, backoff, fallback driver}

IMAGE_FS contains the driver `.ose` and its firmware files.

### 6.5 KRN, SYS, SYM

- **KRN:**
  - SEGMENT sections for the Framekernel. Relocation and KASLR are decided in the boot ADR.
  - Entry point, boot-info ABI version, required CPU features.
  - First-party built-in drivers are inside this image and are measured with it.
- **SYS** lists every first-party component of one release:
  - boot loader digest
  - KRN root
  - each system APP/LIB/DRV root
  - trust-store roots digest
  - policy
  - release version and ABI identifiers
  - a **security version number** for anti-rollback

  It is signed by release keys with a threshold. Unchanged payloads are reused by root (CLAUDE.md §63).
- **SYM:** DWARF and line tables keyed by `build_id`, used for crash symbolization and never installed on user machines.

---

## 7. Signatures and trust

### 7.1 Signature block

```text
SignatureBlock = "ONBFSIG1" (8 B) ‖ le32 version = 1 ‖ le32 entry_count (1–8) ‖ Entry*
Entry          = le32 entry_size ‖ OCBOR map {
   1: role         publisher | notary | origin-driver-attest | origin-kernel | origin-release
   2: suite        signature suite ID (§7.2)
   3: key_id       SHA-256 of the signer's canonical public-key encoding
   4: chain        [OKC certificates], leaf first, ending below a trust-store root
   5: statement    OCBOR map {format:"onbf-sig-1", profile, content_digest, id, version,
                              role, claimed_signing_time}
   6: sig_ed25519  64 B
   7: sig_mldsa    3309 B (ML-DSA-65) or 4627 B (ML-DSA-87)
   8: log_proof    reserved: transparency-log inclusion proof (phase 2)
}
```

An entry is valid only if **all** of these hold:
- `statement.content_digest` equals the recomputed digest.
- `statement.profile` equals the header's profile.
- **Both** signatures verify over the exact statement bytes. ML-DSA uses the FIPS 204 context string `origin/onbf/1/<role>`.
- The chain reaches a trusted root, with roles, ID namespaces and validity all satisfied.

Ed25519 uses RFC 8032 verification with strict canonical-encoding checks, so the host signer and the OS verifier always agree.

### 7.2 Signature suites (D3)

| Suite | Use | Public key | Signature |
|---|---|---|---|
| OSS-1: Ed25519 + ML-DSA-65 | publishers, notary, driver attestation | 32 + 1,952 B | 64 + 3,309 B |
| OSS-1R: Ed25519 + ML-DSA-87 | Origin root, kernel and release keys | 32 + 2,592 B | 64 + 4,627 B |

- **Hashes:** SHA-512 for content digests, SHA-256 for Merkle trees, SHAKE-128/256 inside ML-DSA (R7–R9).
- **Verification only:** Origin OS contains verification code only. Verification handles only public data, so it has no secret-dependent timing to defend. It is implemented originally from FIPS 180-4, FIPS 202, FIPS 204 and RFC 8032, with known-answer tests and differential tests (CLAUDE.md §59).
- **Signing:** happens in Origin's release infrastructure (HSM) and in host developer tools.

### 7.3 Origin Key Certificates (no X.509 in the package path)

An OKC is an OCBOR map, signed by its issuer with both algorithms:

`{version, serial, issuer_key_id, subject {publisher_id, verified legal name, country}, keys {ed25519, ml-dsa}, roles[], id_namespaces[], not_before, not_after, constraints}`

ASN.1/X.509 parsing never runs in the package trust path. X.509 remains only where the outside world requires it: UEFI Authenticode and TLS.

### 7.4 Hierarchy

```text
Origin Root (offline, k-of-n, OSS-1R)
 ├─ Kernel CA ─────────────── origin-kernel keys
 ├─ Release CA ────────────── origin-release keys (threshold)
 ├─ Driver Attestation CA ─── origin-driver-attest keys
 ├─ Notary CA ─────────────── notary keys
 └─ Publisher CA ──────────── publisher certificates (identity-verified, free; D4)
                               └─ optional CI sub-keys, same namespaces
```

Root rotation uses TUF-style chained root metadata: each new root is signed by a threshold of the old root. Revocation and policy are signed metadata with expiry, which protects against freeze attacks (CLAUDE.md §64, R16).

### 7.5 Required signatures per profile

| Profile | Required |
|---|---|
| KRN | origin-kernel |
| SYS | origin-release, with threshold |
| DRV (user-mode) | publisher (verified tier) **and** origin-driver-attest |
| DRV built-in | not separate; covered by KRN |
| Third-party APP, PKG, EXE, LIB | publisher. A notary counter-signature adds the "Notarized" badge. For unverified publishers, see §7.7. |
| First-party APP, LIB | covered by SYS |

### 7.6 Time, revocation, rollback

- The claimed signing time is informational only.
- Validity uses trusted time from signed timestamp metadata, which is monotonic between syncs.
- Key *expiry* does not invalidate software that is already installed.
- Key *revocation* (compromise) quarantines the affected software and tells the user.
- **Apps never downgrade silently.** Rollback is possible only to the previous version kept locally (§9.3).
- **The OS never boots a release whose security version number is below the stored minimum** (boot ADR: TPM NV index or authenticated variable).

### 7.7 Publisher tiers (D4)

| Tier | How a publisher gets it | What its software may do |
|---|---|---|
| Verified | Free registration. The Publisher CA checks identity; the certificate binds the publisher's ID namespace. | Everything its capabilities declare, through the normal install sheet |
| Notarized | Verified, plus an Origin notary counter-signature after automated checks | The same, with the "Notarized" badge |
| Unverified | A self-generated key with no Origin certificate | Installs only after an explicit consent screen, per-user scope only. Always sandboxed: no system services, drivers, extension handlers or `codegen.jit`. Updates must be signed by the same key, which is pinned at first install. |

Drivers (`.od`) are never accepted from the Unverified tier, and they always need the origin-driver-attest signature. Tier policy is signed data, so it can be tightened or relaxed without changing the format.

---

## 8. Framekernel integration

### 8.1 Chain of trust

```text
UEFI firmware (Secure Boot db)
  └─ verifies ─ Origin Boot Loader (PE32+, Authenticode; signer decided in the boot ADR)
        └─ verifies ─ KRN (origin-kernel) and SYS (origin-release), using Origin roots compiled into the loader
              ├─ checks the anti-rollback security version
              ├─ measures KRN and SYS digests into OS-designated TPM PCRs
              └─ passes boot info + SYS root ─ Framekernel
                    └─ Package Service verifies apps and drivers against the trust store
```

The first link stays classical (RSA/SHA-256 Authenticode) until firmware supports post-quantum algorithms. Every link Origin controls uses the D3 suite.

Microsoft's 2011 Secure Boot CAs expired in June and October 2026. Third-party loaders are now signed under Microsoft UEFI CA 2023, which requires an annual independent security audit. Secured-core PCs may not trust that CA by default (R5). This is why the signer is a boot-ADR decision.

### 8.2 Kernel objects and rules

- **Sealed object:**
  - immutable content plus a Merkle root
  - pages are verified when filled; a failure delivers an integrity fault to the process, logs it, and asks the Package Service to repair
- **Code Integrity Table (CIT):** Merkle roots allowed to be mapped executable.
  - Initialized from SYS at boot.
  - Extended only by the Package Service, which holds the `cit.grant` capability, after signature verification.
- **Mapping rules:** permissions ∈ {R, RW, RX}.
  - RX only from sealed objects whose root is in the CIT.
  - RW never on sealed pages; copy-on-write private pages are allowed for data segments and are never RX.
  - Anonymous RX memory requires the `codegen.jit` capability (verified publishers only; it changes RW↔RX, never both).
- **CPU and FPU:** shadow stacks are allocated per thread by the kernel. SHA-NI hashing in the kernel needs FPU/SIMD state save and restore, which is an OSTD-level service. In a framekernel, the SIMD intrinsics are `unsafe` and therefore belong to the TCB crate set.
- **The kernel never parses ONBF for userspace.** Only the boot loader parses KRN/SYS, and it uses the same `onbf-format` crate.

### 8.3 Process launch (Loader Service, user mode)

The shell asks the Loader Service (through the SDK) to launch an app. The Loader Service then:
1. resolves the installed image
2. creates the process
3. maps segments from sealed objects at a randomized base
4. applies relocations on private pages
5. binds imports against the shared system libraries, then seals RELRO
6. sets up TLS and the shadow stack
7. passes initial capability handles in the start record
8. starts the first thread

Startup cost is mapping only: no extraction, no decompression and no copying, because installed images are uncompressed and 4 KiB aligned.

### 8.4 Driver hosting (Device Service, user mode)

1. A device appears.
2. The Device Service picks the best `.od` from the driver store by the precedence in §6.4.
3. It spawns the driver process with capabilities bound to that device only:
   - BAR windows, validated against PCI config space, never against the driver's claims
   - an IOMMU domain; DMA buffers are allocated by the kernel and mapped only there
   - MSI-X vectors delivered as event objects
   - mediated config-space access
   - power-state calls

If there is no IOMMU, third-party DMA drivers are refused.

When a driver crashes:
1. The device is reset (FLR where supported).
2. The driver restarts with backoff.
3. After the restart limit, the Device Service falls back to the generic class driver or disables the device, and tells the user.

The kernel never panics because of a third-party driver.

---

## 9. Installation

### 9.1 Transaction (Package Service)

1. **Inspect.** The installer UI asks the Package Service to inspect the file. The service verifies signatures, then parses, and returns a typed `InstallPlan`:
   - publisher from the certificate, with tier
   - name and icon
   - version, and whether this is an upgrade or a downgrade
   - size
   - capabilities in plain language
   - scope
   - dependency status
   - free-space check
2. **Consent.** Per-user installs need no admin. System scope asks once, through the trusted system prompt drawn by the compositor.
3. **Stage.** Payloads are decompressed and verified into the content-addressed blob store; existing blobs are reused (deduplicated). Progress streams to the UI. Staging runs at background I/O priority, and its interference is measured (CLAUDE.md §53).
4. **Commit.** One atomic database transaction records:
   - the app record: blob roots, granted capabilities
   - every registration: file types, URL schemes, launcher entry, background tasks, services

   The previous version is retained for rollback.
5. **Activate.** The shell, Device Service and update service are notified. Activation is idempotent and is replayed from the database after a crash.

If power is lost during staging, the staging area is garbage-collected at next boot. Commit is atomic. Activation is replayable.

### 9.2 What replaces installer scripts

| Windows installers do this with code | Origin does it declaratively |
|---|---|
| Registry keys, file associations, protocols | `file_types` and `url_schemes` in the manifest |
| Services and scheduled tasks | `entry_points` with restart and power policy |
| Shortcuts | Generated from the manifest |
| Firewall rules | Network capabilities with declared ports; the user can see and revoke them |
| Prerequisite runtimes | `depends` on runtime packs |
| Shell extensions in-process | Out-of-process, sandboxed extension handlers |
| Drivers | An `.od` inside the same `.osi` |
| Settings migration | The app migrates its own data at first launch, inside its sandbox, after an automatic pre-upgrade snapshot |
| Kernel components (anti-cheat, antivirus) | Not supported. Typed user-mode security and attestation APIs instead (D2) |

### 9.3 Uninstall, repair, rollback, update

- **Uninstall** removes the app record atomically, so every registration disappears with it. Blobs are garbage-collected when nothing references them. The user chooses to keep, delete or export the app's data container. Nothing is left behind by construction.
- **Repair** re-verifies the Merkle trees and re-fetches only the corrupted chunks.
- **Rollback** is one click and restores the previous code and the pre-upgrade data snapshot.
- **Update:** one update service covers the OS, apps and drivers, using TUF-style signed metadata (R16). There are no per-app updater services. Updates apply when the app closes. User-mode driver updates need no reboot. A boot-critical change goes into the next deployment, with automatic fallback to the previous one (CLAUDE.md §20, §65).
- **Moving and migration:** apps are only blobs plus a record, so they can move to another volume or migrate to a new PC by root hash.

---

## 10. Native GUI surfaces (SDK-first, CLAUDE.md §48)

All of these are first-party Origin UI apps. Each calls typed SDK services and never a CLI:
- **Install sheet:**
  - icon, name, verified publisher badge (Verified / Notarized / Unverified)
  - version, size
  - "What this app can do"
  - "Just me / Everyone"
  - staged progress (Verifying → Copying → Registering), Cancel, Open
- **App page in Settings:**
  - permission toggles
  - storage (app / data / cache)
  - Verify, Repair, Roll back, Reset, Uninstall (keep / delete / export data)
- **Devices & Drivers:**
  - device tree
  - driver, version, signer
  - privileges (DMA, interrupts, firmware)
  - Restart, Roll back, Use generic driver, Disable
  - crash history
  - native notification when a driver is installed automatically
- **Updates:** one list for OS, apps and drivers, with release notes and scheduling.
- **Security:** the publishers you allowed, revoked items, integrity events.

Typed APIs, sketched:
- `origin::install`: `inspect`, `begin`, `progress`, `cancel`, `commit`, `uninstall`, `rollback`, `repair`
- `origin::apps`: `list`, `info`, `permissions`, `set_permission`
- `origin::devices`: `tree`, `driver_info`, `restart_driver`, `rollback_driver`
- `origin::updates`: `check`, `plan`, `apply`, `schedule`

---

## 11. Compared with Windows 11

The Windows facts below come from Microsoft's own documentation as of November 2026 (research note R17–R20). Microsoft has not announced a "Windows 12"; the newest release is Windows 11 26H2 (R20). The comparison is therefore with Windows 11.

| Area | Windows 11 today (sourced) | Origin with ONBF |
|---|---|---|
| Install-time code | MSI custom actions can run elevated. Microsoft's PowerShell team is leaving MSI because custom actions and scripts cause inconsistent behaviour (R17). | No install-time code at all (ONBF-R3) |
| Number of formats | WinGet has to handle EXE, MSI, WiX, Burn, Inno, Nullsoft, MSIX, ZIP and portable installers (R17) | One container family, one verifier |
| Modern packaging | MSIX cannot carry drivers, per-user services or in-process shell extensions (R17) | One `.osi` carries apps, libraries, services, out-of-process extensions and drivers in one signed transaction |
| Kernel drivers | Microsoft must sign them through its Dev Portal, which requires an EV certificate. Trust in legacy cross-signed drivers is being removed from April 2026 (R18). | Third-party code never runs in the kernel (D2). Drivers run in user mode, confined by the IOMMU, with Origin attestation. |
| Driver failures | A third-party kernel component can stop the whole machine, as in the July 2024 CrowdStrike outage. Microsoft's 2026 Driver Quality Initiative now moves third-party kernel drivers to user mode or Microsoft class drivers (R18). | A crashed driver is restarted. The kernel never panics because of it (§8.4). |
| Code integrity | Memory integrity (HVCI) is on by default only for clean installs on compatible hardware. The vulnerable-driver blocklist is not guaranteed to block every driver (R18). | Every executable page is verified against a signed Merkle root, on every machine, all the time (ONBF-R2, R4) |
| Publisher trust | No OV or EV certificate bypasses SmartScreen reputation (R17) | Free publisher registration. The tier (Verified / Notarized / Unverified) is shown plainly (D4). |
| Admin prompts | Administrator protection exists but is off by default outside security baselines (R19) | Per-user installs never need admin. System scope asks once, through the trusted prompt. |
| Repair | MSI repair re-runs installer logic and can raise UAC prompts when it runs elevated custom actions (R17) | Merkle verification; only damaged chunks are re-fetched |
| Uninstall and rollback | Each classic installer runs its own uninstall logic | Nothing remains by construction; one-click rollback of code and data |
| Updates | Windows Update for the OS and drivers. The Store, WinGet or each app's own updater for apps. | One update service for OS, apps and drivers, using TUF-style metadata |

---

## 12. Implementation plan and contracts

| Crate | Location (CLAUDE.md §81) | Environment | `unsafe` | When |
|---|---|---|---|---|
| `onbf-format`: header, sections, OCBOR-1, signature block | `runtime/onbf/` (new shared dir) | `no_std`, no alloc | `forbid` | M0–M2, host-testable (§84 step 13) |
| `onbf-merkle` | `runtime/onbf/` | `no_std` | `forbid` | M0–M2 |
| `origin-crypto`: SHA-2, SHA-3/SHAKE, Ed25519 verify, ML-DSA verify | `security/crypto/` | `no_std` | only in one reviewed SIMD module | M0–M2 |
| `onbf-pack`, `onbf-sign`, `onbf-inspect` | `developer-tools/` | host, `std` | `forbid` | M6–M7 |
| Loader Service and linker | `runtime/ose/`, `runtime/linker/` | Origin userspace | as needed, documented | M6 |
| Package Service | `services/packages/` | Origin userspace | — | M7 |
| Driver store and Device Service | `services/devices/` | Origin userspace | — | M7, M10 |

The boot loader uses `onbf-format`, `onbf-merkle` and `origin-crypto` to verify KRN and SYS by the end of M1–M2, so the boot chain is never "temporarily unsigned" in release builds.

Parser contract for `onbf-format`:
- no panics: `deny(clippy::panic, unwrap_used, indexing_slicing, arithmetic_side_effects)`
- no allocation
- typed errors
- no partial results

Host developer tools MAY use approved crates (CLAUDE.md §5). If the tools later ship on Origin itself, those crates must be replaced with original code.

## 13. Test obligations (exit criteria for M6/M7)

1. **Known-answer tests:** RFC 8032 (Ed25519), NIST ACVP (ML-DSA), FIPS 180-4 / FIPS 202 vectors, and Wycheproof Ed25519 edge cases.
2. **Differential tests:** `origin-crypto` against reference implementations, on the host only. Round-trip property tests: pack → parse → pack is byte-identical.
3. **Structure-aware fuzzing** of the header, section table, OCBOR-1, signature block and IMAGE_FS. Fuzz tooling that needs nightly runs only in an isolated host test path (CLAUDE.md §12).
4. **Malicious corpus**, each case rejected with the right error:
   - overlapping or misaligned sections; counts beyond limits; integer overflow; truncation; trailing data
   - W+X segments
   - non-canonical or duplicate-key CBOR
   - wrong profile in the signature statement; wrong role
   - expired or out-of-namespace certificate
   - decompression bomb
   - Merkle block swap
5. **Kani:** for inputs up to a bound, `onbf-format` never panics and never reads out of bounds. State precisely what is proved (CLAUDE.md §73).
6. **N-version check:** a second parser, written independently from this spec by a different agent, must agree with the first on the whole corpus. Disagreements are spec bugs.
7. **QEMU:**
   - W^X enforced; executing from an unregistered root is denied
   - a tampered byte on disk → integrity fault → repair
   - CPUID-mismatch dialog
8. **Crash consistency:** power cut injected at every install, upgrade and uninstall step; the database stays consistent and no blobs leak.
9. **Performance**, measured against budgets set on the reference hardware:
   - verification throughput with and without SHA-NI
   - added cold-launch latency (budget ≤ 5%)
   - install throughput
   - background-install frame-pacing impact

---

## Appendix A — Limits (v1)

| Item | Limit |
|---|---|
| Sections per file | 256 |
| Manifest | 1 MiB; depth 16; 65,536 items per container; text 64 KiB |
| Signature block | 1 MiB; 8 entries; chain length 4 |
| Segments (EXE, LIB) | 16; image span 2 GiB |
| Imports, exports | 65,536 each; symbol 255 B |
| IMAGE_FS | 262,144 entries; path 1,024 B; depth 16 |
| Section content | 1 TiB; decompressed size must equal `content_size` exactly |

## Appendix B — Defaults (approved by the owner on 2026-11-07)

1. ONBF is the umbrella format; the 8 profiles are in §1, with `.osl` added for shared libraries.
2. The Framekernel image and the release manifest use ONBF profiles (KRN, SYS).
3. Manifest encoding is OCBOR-1, compiled from a TOML source.
4. No X.509/ASN.1 in the package trust path; Origin Key Certificates instead.
5. Merkle-SHA256-4K for all mapped content; SHA-512 for content digests.
6. zstd (decompression only, original code from RFC 8878) for transport; installed images are uncompressed.
7. `x86-64-v3` ISA baseline; shadow stack on by default; IBT reserved until stable `rustc` supports it.
8. Frame pointers on everywhere; `panic=abort` in userspace until the Origin `std` port.
9. Eager binding and full RELRO; no lazy PLT; no static constructors in ABI v1.
10. System calls private to each release; only `origin.core` is a stable ABI; syscall-origin pinning.
11. No downgrade except to the locally retained previous version.
12. `.oapp` can run without installing ("Try").
