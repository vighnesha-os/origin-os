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
| Boot info 0.2 | `origin-boot-info` (Proposed ABI): regions, kernel segment table | ADR-0002 B4 |
| Loader, M1 step 2 | reads, verifies, places, relocates and enters KRN; rejects a tampered KRN | `cargo xtask boot-test`, both scenarios |
| `onbf-pack` KRN subset | ELF64 PIE → KRN; RELR; manifest; Merkle; self-verifying output | 61 tests; packs the real kernel |
| SHA-3 / SHAKE | `origin-crypto`, FIPS 202 | known answers, 304 differential vectors |
| Ed25519 verification | `origin-crypto`, RFC 8032, strict (errata E1) | RFC vectors, 300 differential, 1,765 negative |
| Framekernel M1 entry | COM1 report, boot-info validation, relocation probe | boot test |
| M2: CPU tables | GDT, TSS (IST1 for #DF), IDT, exception dispatch | `int3` handled and resumed |
| M2: physical memory | bitmap frame allocator, frame runs, strict frees | unit tests; boot proof before and after paging |
| M2: virtual memory | own page tables: W^X image from the signed segment table, NX direct map, no page 0 | four #PF probes; negative control |
| M2: interrupts | x2APIC, PIT-calibrated APIC timer, periodic interrupts | 50 at 100 Hz in ~502 ms; two negative controls |
| M2: kernel heap | slabs + frame runs, `GlobalAlloc`, `alloc` works | leak-free proof; negative control |
| ML-DSA-65/87 verification | `origin-crypto`, FIPS 204, strict (errata M1) | 104 differential, 2,631 negative; OpenSSL cross-check |
| M3: processor start-up | ACPI MADT, INIT-SIPI-SIPI, per-CPU GDT/TSS, x2APIC on every AP | 2, 4 and 8 CPUs online; negative control |
| M3: preemptive threads | per-CPU round-robin, timer-driven switching on every CPU | 51 switches per CPU in 500 ms, fair shares; negative control |

## In progress

| Task | Notes |
|---|---|
| — | nothing in progress at the end of the session |

## Next, in order

1. **M3, scheduling proper:** a scheduler ADR (§18), then thread exit and sleeping, wake-ups and IPIs, priorities and a fairness policy (EEVDF to be evaluated, §26), load balancing, guard pages below thread stacks, and per-CPU data without APIC-ID indexing. Pad the per-worker counters (cache-line sharing showed at 4 CPUs).
2. **Security hardening of the address space:** SMEP/SMAP (with probes), the higher-half layout and KASLR (ADR-0002 B3), reclaiming `LoaderReclaimable` once nothing needs it, guard pages below the kernel stacks.
3. **Signature block (ONBF §7.1):** blocked on S1–S4; then Ed25519 + ML-DSA in the loader and the trust anchor (B7).
4. **Formal and concurrency checks:** Loom models for `SpinLock`; Kani bounds for `paging`, `frames` and `heap` (CLAUDE.md §73).
5. **CPU-feature CPUID tests** (key registry bits 0–5) and XCR0 checks.
6. **M1 follow-ups:** A/B deployment and boot-attempt recording (§20); an executed FFI-bool test (Rosetta); CI.

## Waiting on the owner

| # | Decision | Blocks |
|---|---|---|
| — | Accept, amend or reject **ADR-0002** (Proposed) | Shipping anything built on the boot path |
| B7 | Boot trust anchor: Microsoft UEFI CA 2023, or Origin keys enrolled in firmware | Secure Boot |
| — | Accept, amend or reject **ADR-0003** (kernel memory, interrupts, SMP start-up, first scheduler; Proposed) | Building M4 on it |
| S1 | Signature entry `role` encoding (§7.1): integer enum or text | Signature block parser |
| S2 | Signature suite IDs for OSS-1 and OSS-1R (§7.2) | Signature block parser |
| S3 | Statement map keys: §7.1 writes text names, but K1 requires integer keys | Signature block parser |
| S4 | Origin Key Certificate (OKC) key numbers (§7.3) | Certificate chain |
| G1 | A section kind for SYM's DWARF data | `.osym` (M6) |
| R1–R5 | Readings applied in the errata (overrule if wanted) | — |
| P1–P7, E1, L1 | Readings proposed by `onbf-pack`, Ed25519 and the loader (errata) | A second implementation, the host signer |
| — | Downloads: NIST ACVP (ML-DSA, SHA-2/3), the FIPS 204 errata sheet, Wycheproof (Ed25519) | External known-answer validation (§59) |
| — | A project licence | Every provenance entry |
| — | Merge into `main` and push | Sharing the work |
