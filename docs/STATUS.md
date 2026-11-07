# Origin OS status

| | |
|---|---|
| Updated | 2026-11-07, evening |
| Branch | `m0/toolchain-verification` (local only, not pushed, not merged into `main`) |
| Reproduce | `cargo xtask check`, `cargo xtask boot-test`, and `cargo xtask boot-matrix` (1, 2 and 4 CPUs; `ORIGIN_QEMU_SMP=1`…`8` for others) |
| Rule | "Done" below means built, tested and committed, with a boot proof where it runs in the kernel. **Everything runs in QEMU 11.1.3 (TCG) + OVMF only; nothing is qualified on physical hardware yet (CLAUDE.md §74).** |

## By milestone

| Milestone | State | What is proven |
|---|---|---|
| **M0** toolchain | done | Rust 1.99.1 pinned and verified; QEMU and OVMF; `xtask` check, build, boot-test and run; no-panic object check on the freestanding libraries |
| **M1** bootable Framekernel | done (developer mode) | UEFI loader reads, verifies (content digest, manifest, every section's digest or Merkle root) and places the KRN image, applies RELR, exits boot services and enters the kernel; a tampered image is rejected. **Unsigned images only**: the signature block is parsed and verifiable (`onbf-verify`) but not yet required by the loader; the trust anchor waits on B7 |
| **M2** CPU and memory | done (baseline) | Own GDT/TSS/IDT; exceptions; frame allocator; higher-half kernel with KASLR (ADR-0004) and its own page tables (W^X image from the signed segment table, NX direct map, nothing in the lower half, SMEP/SMAP, guard pages below every stack); kernel heap with `alloc`; x2APIC and a PIT-calibrated timer; loader memory reclaimed |
| **M3** SMP and scheduler | started | All MADT-enabled CPUs started (1–8 tested); per-CPU round-robin with preemption, yield, sleep, exit and stack reaping; GS-based CPU-local data. Not yet: priorities, fairness policy, load balancing, wake-ups across CPUs |
| **M4** processes and IPC | started | Per-process address spaces sharing the kernel half, with CR3 switched per thread (two processes at the same addresses stay isolated); ring 3 with one DPL-3 gate and `syscall`; user faults end the thread, not the kernel; RSP0 per switch; NMI/#MC on IST; validated, fault-safe copy from user memory (a write system call). Not yet: process objects, handles and capabilities, IPC, loading `.ose` programs |
| **M5** onwards | not started | — |

## Native formats and cryptography

| Component | State |
|---|---|
| ONBF parser (`onbf-format`), Merkle trees (`onbf-merkle`), KRN packer (`onbf-pack`) | done for KRN; EXE/LIB/OSI/OAPP/OD profiles not yet |
| SHA-2, SHA-3/SHAKE, Ed25519 (strict), ML-DSA-65/87 (strict) in `origin-crypto` | done, verification only; **no NIST ACVP or Wycheproof validation yet** (downloads need approval) |
| Signature block, certificate chain, trust store | encodings decided (S1–S4); parser (`onbf-format`) and entry verification (`onbf-verify`, both signatures, proven against reference-signed vectors) done; loader wiring, trust store and chains next; trust anchor waits on B7 |

## Waiting on the owner

ADR-0002 and ADR-0003 (Proposed); ADR-0004 was accepted and implemented on 2026-11-07; ADR-0005 (process ABI and IPC) was accepted on 2026-11-07 and is being implemented; B7; G1; the readings P1–P7, E1, M1 and L1 in `docs/spec/ONBF-errata.md`; approval to download NIST ACVP / FIPS 204 errata / Wycheproof vectors and the `loom` crate; a project licence; merging into `main` and pushing. Details: `docs/engineering/TASKS.md`.

## Where the evidence is

`docs/engineering/SOURCE_PROVENANCE.md` (per-module evidence, unsafe inventories, negative controls), `docs/engineering/handoffs/2026-11-07-local-handoff.md` (commit list and lessons), the commit messages (each lists its boot-test lines and controls).
