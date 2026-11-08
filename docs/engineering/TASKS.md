# Engineering backlog

| | |
|---|---|
| Required by | CLAUDE.md §79 (persistent engineering memory) |
| Updated | 2026-11-08 |
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
| M3: thread lifecycle | yield, sleep, exit; slot reuse | 100/100 alternations, 5-tick sleep; negative control |
| Hardening | SMEP/SMAP on every CPU; guard pages below every stack; loader memory reclaimed | five more #PF probes; two negative controls |
| Thread cleanup | exited threads' stacks reaped on their own CPU | heap back to its pre-spawn size; negative control |
| `origin-sync` | the spin lock as a freestanding, host-stress-tested crate | 3 multi-thread tests; no-panic check |
| CPU-local area | GS-based per-CPU data; context-switch cost measured at every boot | ~18 % fewer cycles per switch (QEMU) |
| Higher half and KASLR | ADR-0004 H1–H4: boot info 0.3, loader-built tables, RDSEED/RDRAND slides, kernel in the higher half | boot-matrix; three distinct placements; fallback boot; negative control |
| M4: process address spaces | own PML4 per process, shared kernel half, CR3 per thread | two-process isolation proof; negative control |
| M4 groundwork: user mode | ring 3, one DPL-3 system-call gate and `syscall`, RSP0 per switch, user faults end the thread, NMI/#MC on IST | 5 user-program proofs, a self-NMI proof; four negative controls |
| Clocks (§68) | monotonic TSC clock (rate proven against one RTC second), UTC wall clock from the CMOS RTC, TSC consistency across CPUs | boot-matrix; three negative controls |
| Serialized console | whole lines from every CPU; urgent path for exceptions and panics | xtask whole-log check; negative control |
| TLB shootdown, global pages (ADR-0004 H5) | IPIs with per-request generations; kernel half global | every AP's read of an unmapped page faults; two negative controls |
| Kernel wait queues | blocking, cross-CPU wake-ups by IPI, pending wake-ups | 10 000-item transfer in order; 44–73 µs ping-pong; two negative controls |
| PCIe enumeration | MCFG, uncached ECAM, every function and BAR (sized with decoding off, restored) | q35's functions listed; two negative controls |
| NVMe driver | polled admin and I/O queues, Identify, read, write, flush | pattern blocks from the xtask test disk; two negative controls |
| GPT | both copies validated (CRCs, layout, agreement), partitions listed | the xtask disk's partitions; negative control |
| CPU indexes, APIC IDs above 255 (2026-11-08, branch `claude/eloquent-kapitsa-840594`) | dense `CpuIndex` per processor at MADT discovery (ADR-0008, Proposed); per-CPU arrays by index, IPIs and MSI/IRTE destinations by APIC ID; MADT and SRAT lists sized from the table | `cpumap` host tests; `cargo xtask boot-x2apic` (APIC ID 256 of 150 possible); three negative controls |
| CPU topology from CPUID (2026-11-08) | `framekernel::topology`: leaves 1Fh and 0Bh (thread, core, module, tile, die, die group, with unknown domains absorbed), AMD's 80000026h normalized into one layout, core types as raw codes; each processor reads its own leaves and records them under its `CpuIndex`; package, die, module, core and thread of every processor reported and checked against the MADT. 80000026h and core types host-tested only: QEMU 11.1 models neither | 19 `topology` host tests; `boot-matrix`; `boot-x2apic` on a third processor at APIC ID 412, under both leaf 1Fh and leaf 0Bh; 10 negative controls |
| External known-answer vectors, CRYPTO-KAT (2026-11-08, branch `claude/amazing-einstein-4eb320`) | `security/crypto/tests/kat`, host-only: NIST ACVP SHA-2, SHA-3, SHAKE and ML-DSA signature verification, and Wycheproof Ed25519, from vectors converted at pinned revisions and hashes and committed with their notices; FIPS 204 errata sheet applied (one check tightened) | 3,882 vectors pass, each exception justified; negative controls per algorithm; planted faults; `SOURCE_PROVENANCE.md` |

## In progress

| Task | Notes |
|---|---|
| — | nothing in progress at the end of the session |

## Next, in order

1. **NVMe interrupts:** the PCI capability list, MSI-X, a vector per queue, and completions that wake a wait queue instead of polling; timed waits (deadlines on the monotonic clock).
2. **IOMMU (§32):** VT-d under QEMU's `intel-iommu` — **done 2026-11-08** (ADR-0007, Proposed): the DMAR table, DMA remapping for NVMe and virtio-net with blocked-DMA and fault proofs, and interrupt remapping (see STATUS and `SOURCE_PROVENANCE.md`). Delivery to an APIC ID above 255 is proven live since the CPU-index work (ADR-0008, `boot-x2apic`). I/O APIC entries in remappable format, through IRTEs that check the I/O APIC's source ID — **done 2026-11-08**. AMD-Vi under QEMU's `amd-iommu` — **done 2026-11-08** (ADR-0007's AMD-Vi section, Proposed): the IVRS, default-deny device tables, DMA and per-device interrupt remapping with x2APIC destinations, the same proofs and drivers, `boot-matrix` over both IOMMUs. Remaining: RMRR and IVMD identity maps, scalable mode; an interrupt table for the I/O APIC's DeviceID under AMD-Vi (its pins stay masked there until then); the keyboard IRQ1 to a handler through `iommu::route_ioapic_pin`; a live level-triggered pin (PCI INTx) with its EOI path; the source-ID check on I/O APIC interrupts on hardware (QEMU's I/O APIC sends no requester ID).
3. **Parser hardening (§72):** deterministic mutation runs over every firmware and device parser (ACPI, PCI, NVMe Identify, GPT, RTC), with a seeded generator and no fuzzing crates — **done 2026-11-08**: the whole inventory, with a soak per parser (about 1.3 million mutated inputs in the default 1 s run; `ORIGIN_MUTATION_SCALE` scales it). The soaks found 13 library bugs and several unchecked device values in the kernel; each is fixed with a regression test (see `SOURCE_PROVENANCE.md`, "Parser hardening"). The multi-fault soak over the ONBF parsers is **done 2026-11-08**: the container, OCBOR-1, the KRN manifest, the signature block, Merkle verification and signature verification, each judged by a model written from the specification, about 98,000 cases in the default 1 s; 5 bugs found, each fixed with a regression test (see `SOURCE_PROVENANCE.md`). Remaining: coverage-guided fuzzing and Kani bounds (§73); the malformed-device paths on a fault-injecting device model or hardware (§74).
4. **M3, scheduling proper** (ADR-0003 K7 is the baseline; ADR-0008 indexes its queues): priorities and a fairness policy (EEVDF to be evaluated, §26), load balancing; pad the per-worker counters. Per-CPU state sized at discovery instead of 64 static slots (ADR-0008 C6) and CPU hot-plug (MADT "online capable"). CPUID topology for §27 (dies, modules, per CPU) — **done 2026-11-08** (see Done): the next §27 step is using it for placement, and hybrid-core *capacity*, which no leaf here gives (`docs/research/2026-11-08-cpu-topology-leaf-1fh.md`).
5. **M4:** ADR-0005 is accepted; the `origin-abi` crate, handle tables and channel logic exist. Next: the kernel's system-call layer, user copies against the calling process's tables, `wait_one`, the start record, memory objects, the Loader Service and `.ose` loading; PCIDs (ADR-0004 H5).
6. **Signature block (ONBF §7.1):** `onbf-pack` signing with a host key, the loader requiring a signed KRN outside developer mode, the trust store and OKC chains (the trust anchor is decided: B7).
7. **Block layer and filesystem (§39–§41):** one I/O queue per CPU, a block cache, then a filesystem ADR.
8. **Formal and concurrency checks:** Loom models for `SpinLock` and the wait-queue protocol; Kani bounds for `paging`, `frames`, `heap` (§73).
9. **CPU-feature CPUID tests** (key registry bits 0–5) and XCR0 checks; **M1 follow-ups** (A/B deployment, boot-attempt recording, CI).

## Waiting on the owner

Every design decision was finalised on 2026-11-07, under the owner's instruction to finalise the design with the recommended options: ADR-0002 (with B7: a Microsoft-signed first stage by default, Origin-owned machines supported), ADR-0003, errata G1 (the SYM `DWARF` kind) and the readings R1–R5, P1–P7, E1, M1 and L1. What remains is not design:

| # | Decision | Blocks |
|---|---|---|
| — | The `loom` crate (host-only dev-dependency, §5), **approved 2026-11-08**, not fetched yet; its source and size are confirmed when the work that uses it starts. (The ACVP, errata and Wycheproof downloads of the same approval were fetched on 2026-11-08, file by file; see `SOURCE_PROVENANCE.md`.) | Concurrency models (§73) |
| — | A project licence | Every provenance entry |
| — | ADR-0007 (DMA isolation and interrupt remapping, Intel VT-d and AMD-Vi, Proposed): questions 3, 6, 7 and 8; the rest are answered (`docs/engineering/OWNER_DECISIONS.md`) | Accepting the IOMMU design; it is implemented and boot-proven already |
| — | Compare the AMD IOMMU specification (48882 Rev 3.11) read from a mirror with AMD's own copy (SHA-256 in `docs/research/2026-11-08-amd-vi-iommu.md`) | Confidence that every AMD-Vi encoding comes from AMD's text |
| — | Merge into `main` and push | Sharing the work |
