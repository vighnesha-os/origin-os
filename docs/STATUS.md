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
| **M1** bootable Framekernel | done (developer mode) | UEFI loader reads, verifies (content digest, manifest, every section's digest or Merkle root) and places the KRN image, applies RELR, exits boot services and enters the kernel; a tampered image is rejected. **Unsigned images only**: the signature block is parsed and verifiable (`onbf-verify`) but not yet required by the loader; the trust anchor is decided (B7: a Microsoft-signed first stage by default, Origin-owned machines supported) |
| **M2** CPU and memory | done (baseline) | Own GDT/TSS/IDT; exceptions; frame allocator; higher-half kernel with KASLR (ADR-0004) and its own page tables (W^X image from the signed segment table, NX direct map, nothing in the lower half, SMEP/SMAP, guard pages below every stack); kernel heap with `alloc`; CPU feature discovery (x86-64 level graded) and SSE/AVX enabled, with FPU/SIMD state saved and restored across context switches; global kernel pages and TLB shootdowns across CPUs (ADR-0004 H5; PCIDs not yet); x2APIC and a PIT-calibrated timer; a monotonic clock (the TSC rate proven against one RTC second; readings consistent across CPUs) and a UTC wall clock from the CMOS RTC; the HPET discovered and its rate cross-checked against the TSC; loader memory reclaimed |
| **M3** SMP and scheduler | started | All MADT-enabled CPUs started (1–8 tested); per-CPU round-robin with preemption, yield, sleep, exit and stack reaping; GS-based CPU-local data; a console that keeps every CPU's lines whole (checked over the whole boot log); a frame-pointer backtrace for panic diagnostics. kernel wait queues with blocking, timed waits and cross-CPU wake-ups by IPI (no lost wake-ups: a producer/consumer transfer and a ping-pong prove it); weighted fair scheduling (stride scheduling, shares within a few per mille of the weights) with a true idle context on every AP. Not yet: scheduling classes, load balancing, priority inheritance |
| **M4** processes and IPC | started | Per-process address spaces sharing the kernel half, with CR3 switched per thread (two processes at the same addresses stay isolated); ring 3 with one DPL-3 gate and `syscall`; user faults end the thread, not the kernel; RSP0 per switch; NMI/#MC on IST; validated, fault-safe copy from user memory (a write system call). Not yet: process objects, handles and capabilities, IPC, loading `.ose` programs |
| **M8/M9** groundwork | started | An in-memory VFS (paths, directories, files); a 2D software rasterizer (fills, blits, alpha blend) cross-checked host↔kernel by CRC. Not wired to syscalls, disk or a framebuffer yet |
| **M5** storage | started | PCIe enumeration through the ACPI MCFG: the ECAM window mapped uncached (PAT checked on every CPU), every function listed, every BAR sized and restored; a polled NVMe driver (admin and I/O queues, Identify, read, write, flush; an out-of-range read refused), checked against a GPT test disk xtask builds; both GPT copies validated (CRCs, layout, agreement) and the partitions listed; MSI-X interrupt-driven completions for a thread that blocks on a wait queue; one queue pair and vector per CPU, with all CPUs doing I/O at once; a block layer (a 4 KiB page cache over the namespace, LRU eviction and write-back). Not yet: an IOMMU, a filesystem |
| **M10** groundwork | started | PS/2 scancode decoding (set 1, US layout) and i8042 controller bring-up proven in QEMU; live keystrokes await the keyboard IRQ |
| **M10** networking | started | The packet layer (Ethernet/IPv4/ICMP/ARP headers, the Internet checksum), proven by in-memory loopback echo; no NIC driver, TCP/UDP or routing yet |
| **M6** onwards | not started | — |

## Native formats and cryptography

| Component | State |
|---|---|
| ONBF parser (`onbf-format`), Merkle trees (`onbf-merkle`), KRN packer (`onbf-pack`) | done for KRN; EXE/LIB/OSI/OAPP/OD profiles not yet |
| SHA-2, SHA-3/SHAKE, Ed25519 (strict), ML-DSA-65/87 (strict) in `origin-crypto` | done, verification only; **no NIST ACVP or Wycheproof validation yet** (downloads need approval) |
| Signature block, certificate chain, trust store | encodings decided (S1–S4); parser (`onbf-format`) and entry verification (`onbf-verify`, both signatures, proven against reference-signed vectors) done; loader wiring, trust store and chains next; trust anchor decided (B7) |

## Waiting on the owner

every design decision is now final (ADR-0002 with B7, ADR-0003, G1 and the errata readings were decided on 2026-11-07 under the owner's instruction to finalise with the recommended options; ADR-0004 and ADR-0005 were accepted by the owner). Still the owner's, because they are not design: approval to download NIST ACVP / FIPS 204 errata / Wycheproof vectors and the `loom` crate; a project licence; merging into `main` and pushing. Details: `docs/engineering/TASKS.md`.

## Where the evidence is

`docs/engineering/SOURCE_PROVENANCE.md` (per-module evidence, unsafe inventories, negative controls), `docs/engineering/handoffs/2026-11-07-local-handoff.md` (commit list and lessons), the commit messages (each lists its boot-test lines and controls).
