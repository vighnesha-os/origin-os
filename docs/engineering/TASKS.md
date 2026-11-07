# Engineering backlog

| | |
|---|---|
| Required by | CLAUDE.md §79 (persistent engineering memory) |
| Updated | 2026-11-07 |
| Rule | Status words mean what they say. "Done" means built, tested and committed; QEMU is not hardware (CLAUDE.md §74). |

## Done (branch `m0/toolchain-verification`)

| Task | Result | Evidence |
|---|---|---|
| M0 toolchain | Rust 1.99.1 pinned and verified; QEMU 11.1.3 + edk2-stable202408 | `docs/toolchain/VERIFICATION.md` |
| Workspace and xtask | `cargo xtask check / build / boot-test / run`; lints for §11 and §62; no-panic object check for the freestanding libraries | `SOURCE_PROVENANCE.md` |
| Versioning (§62) | `origin-version.toml` → generated `origin-version` | `SOURCE_PROVENANCE.md` |
| ONBF container parser | `onbf-format`: container, OCBOR-1, KRN manifest; owner decisions C1–C14, G2–G5, K1–K4 | `docs/spec/ONBF-errata.md`, `ONBF-manifest-keys.md` |
| SHA-256 / SHA-512 | `origin-crypto`, FIPS 180-4 | known-answer tests, 310 differential vectors |
| Merkle-SHA256-4K | `onbf-merkle` | independent Python reference, attack tests |
| Boot-info v0.1 | `origin-boot-info` (Proposed ABI) | ADR-0002 |
| Loader, M1 step 2 | reads, verifies, places, relocates and enters KRN | ADR-0002; boot test (see below) |

## In progress

| Task | Notes |
|---|---|
| Framekernel M1 entry | `framekernel/`: COM1 report, validates boot info, halts |
| `onbf-pack` KRN subset | ELF64 PIE → KRN; RELR; manifest encoder; Merkle; self-verifying output |
| SHA-3 / SHAKE (FIPS 202) | `origin-crypto`; needed by ML-DSA |
| Ed25519 verification (RFC 8032) | `origin-crypto`; strict semantics, recorded for the owner |

## Next, in order

1. **M1 exit:** the QEMU boot test proves loader → kernel. Add the tampered-kernel case to CI once CI exists.
2. **Signature block (ONBF §7.1):** blocked on owner decisions S1–S4 below.
3. **ML-DSA-65/87 verification (FIPS 204):** needs SHAKE.
4. **M2 (CPU and memory):** GDT/IDT/TSS, exceptions, the kernel's own page tables (W^X enforced; KASLR per ADR-0002 B3), the physical page allocator from the boot-info regions.
5. **CPU-feature CPUID tests** (key registry bits 0–5), verified against the Intel SDM and AMD APM, plus the XCR0 state checks.
6. **M1 follow-ups:**
   - A/B deployment and boot-attempt recording (CLAUDE.md §20);
   - the boot trust anchor (B7);
   - an executed FFI-bool test (Rosetta).

## Waiting on the owner

| # | Decision | Blocks |
|---|---|---|
| — | Accept, amend or reject **ADR-0002** (Proposed) | Shipping anything built on the boot path |
| B7 | Boot trust anchor: Microsoft UEFI CA 2023, or Origin keys enrolled in firmware | Secure Boot |
| S1 | Signature entry `role` encoding (§7.1): integer enum or text | Signature block parser |
| S2 | Signature suite IDs for OSS-1 and OSS-1R (§7.2) | Signature block parser |
| S3 | Statement map keys: §7.1 writes text names, but K1 requires integer keys | Signature block parser |
| S4 | Origin Key Certificate (OKC) key numbers (§7.3) | Certificate chain |
| G1 | A section kind for SYM's DWARF data | `.osym` (M6) |
| R1–R5 | Readings applied in the errata (overrule if wanted) | — |
| — | A project licence | Every provenance entry |
| — | Merge into `main` and push | Sharing the work |
