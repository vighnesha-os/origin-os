# Open owner decisions

| | |
|---|---|
| As of | 2026-11-08, commit `944a276` on `claude/interesting-carson-ae7c83`. The M4 and signed-boot sessions are still running and may add to this list. |
| Checked against | CLAUDE.md v7.1, item by item. That check changed the first version of this sheet. |
| Evidence | Everything below is proven under QEMU only. Nothing has run on physical hardware (CLAUDE.md §74). |
| Answered | 2026-11-08: items 1–9, 13–20 and 25, each with the recommendation below. |
| Still open | Items 10–12 (after item 23). Items 21–24 (need you). |

The recommendations are Claude's. The decisions are yours (CLAUDE.md §18, §84).

## What has to happen first

CLAUDE.md requires these of the three Proposed ADRs. An ADR can be marked Accepted only once the steps that apply to it are done. Steps A–C are done.

| Step | What | Rule | Applies to |
|---|---|---|---|
| A, **done** | Trade-offs and migration consequences added to all three ADRs, and alternatives to ADR-0006. | §18 | ADR-0006, 0007, 0008 |
| B, **done** | [The OriginFS ledger](../research/2026-11-08-originfs-design.md). **The layout cannot grow into §40:** its inode is full, its block pointers hold only an address, and its inode table cannot move. | §16, §40 | ADR-0006 |
| C, **done** | [The IOMMU threat model](THREAT_MODEL_IOMMU.md): 24 threats, 9 still open. Early-boot DMA and failing open are the most serious. | §58 | ADR-0007 |
| D, **partly done** | [Every VT-d encoding is checked against Rev 5.20](../research/2026-11-08-vt-d-iommu.md), which answers item 7. Item 23 (the two AMD manuals) is still yours. | §3, §16 | Items 10–12 |

IOMMU overhead has never been measured (rule 21). QEMU's numbers would mean little, so that waits for hardware (item 24). It does not hold up acceptance.

## OriginFS on-disk format (ADR-0006): blocks mounting any filesystem

| # | Question | Recommendation | Why |
|---|---|---|---|
| 1, answered | A classic inode layout for v0, read-only? | **Accept, as a bring-up format that never ships in an approved release** (new decision F7) | v0 cannot become the §40 filesystem, so it should only prove the read path. Rule 39 would otherwise require a migration path from it. |
| 2, answered | 4 KiB blocks and a file limit of about 2 MiB? | **Accept for v0** | 4 KiB matches the page cache. The limit falls short of §40's large files, so it only works for a format that never ships. |
| 3, answered | Defer writes, journalling and crash consistency? | **Accept** | A read-only format needs no write path. Writing belongs to a successor format with its own ADR (§41). |
| 4, answered | Reserve space now for per-block Merkle integrity? | **No** | §40's integrity belongs to the successor. v0's pointers have no room for it, and reserved space would never be used. |

## IOMMU (ADR-0007): built and boot-proven; Proposed until items 10–12 are answered

| # | Question | Recommendation | Why |
|---|---|---|---|
| 5, answered | Default-deny, one domain per device? | **Accept** | §58's DMA protection; §47's third-party drivers depend on this confinement. |
| 6, answered | DMA addresses from a 1–2 GiB window, never physical addresses? | **Accept** | A translation failure can't pass unnoticed, and it can change later without an ABI break. Addresses aren't reused yet, which must be fixed before any driver maps memory per request. |
| 7, answered | Legacy translation mode now, scalable mode later? | **Accept** | Rev 5.20 still defines legacy mode, with no deprecation notice. It needs second-stage translation: a newer unit that reports `SSTS` clear has none, and the kernel refuses it (T6). Hardware must confirm this (§74). |
| 8, answered | Refuse devices that need firmware-reserved memory (RMRR) until identity maps exist? | **Accept, with a deadline** | It fails closed. USB controllers and integrated graphics commonly need RMRRs, so identity maps must land before real hardware. |
| 9, answered | With no IOMMU present, refuse bus mastering for external and hot-plugged devices? | **Yes** | §32 asks for external-device authorization. An IOMMU should also be part of the declared hardware scope (item 24). |
| 10 | AMD-Vi: blocking device table entries, one interrupt table per device? | **After item 23**, then accept. | The same posture as VT-d, but the encodings came from a mirror copy of AMD's manual. |
| 11 | Allow two QEMU gaps in the AMD-Vi proofs (fault address 0, no record of a blocked interrupt)? | **After item 23**, then accept as QEMU limits. | Hardware qualification must check both (§74). The address bug is fixed after QEMU 11.1, so that check can tighten on upgrade. |
| 12 | Refuse devices that an IVMD names, as for RMRRs? | **After item 23**, then accept. | Same reasoning and deadline as item 8. |
| 25, answered | Rev 5.20 lets firmware ask the OS not to use DMA remapping (`DMA_REMAP_OPT_OUT`) or x2APIC (`X2APIC_OPT_OUT`). Honour them? | **No: report them and carry on** (what the kernel does today) | §58 requires DMA protection, and the kernel depends on x2APIC. A platform that truly needs either is outside the declared hardware scope (§76). |

## CPU indexes (ADR-0008): built and boot-proven

| # | Question | Recommendation | Why |
|---|---|---|---|
| 13, answered | Dense indexes in MADT order, with `CpuIndex` the only per-CPU array index? | **Accept** | The type makes mixing up an APIC ID and an index a compile error. |
| 14, answered | Keep 64 static per-CPU slots for now? | **Keep for now** | Move to sizing at discovery with the scheduling work (TASKS item 4), before any machine with more than 64 logical CPUs enters the declared scope. |
| 15, answered | Add `boot-x2apic` to `boot-matrix`? | **Yes** | About 40 s on a two-minute gate. Today's merges changed the code it guards. |
| 16, answered | Park a silent AP with INIT and drill it on every boot? Per-AP trampoline data too? | **Accept parking; per-AP data later** | Parking closes the hazard it was built for. Per-AP data belongs with parallel start-up. |

## Signature format (ONBF errata O1)

| # | Question | Recommendation | Why |
|---|---|---|---|
| 17, answered | A signature statement's `id` must be NFC, but the parser has no Unicode tables. Restrict it to ASCII, or add tables? | **Restrict to ASCII** | The same call as errata R4. This parser runs before any signature is checked, so it should stay small (§13). An `id` is a machine identifier; display names belong in localized metadata (§45, §51). |

## Downloads

| # | Question | Recommendation | Why |
|---|---|---|---|
| 18, answered | Intel VT-d Rev 5.20 PDF (D51397-019, intel.com, 12.8 MB)? | **Approved; downloaded 2026-11-08**, outside the repository | §16 wants this text before implementation. Every encoding has now been checked against it: seven corrections, none affecting a boot proof. |
| 19, answered | NIST ACVP vectors (ML-DSA, SHA-2/3), the FIPS 204 errata sheet, Wycheproof (Ed25519)? | **Approved; not fetched yet** | §59 requires known-answer tests and independent validation. Host-only test data. Each file's source and size will be stated, and confirmed, when the work that uses them starts. |
| 20, answered | The `loom` crate as a host-only dev-dependency? | **Approved; not added yet** | §73 names Loom, and it never reaches the shipping runtime (§5). It is added with the first Loom model. |

## Only you can do these

| # | Question | Recommendation | Why |
|---|---|---|---|
| 21 | The project licence? | **Apache-2.0 if this is to be open. If closed, make the repository private now.** | The GitHub repository is public with no LICENSE file: anyone can read the code, but nobody else has permission to reuse it. Apache-2.0 includes a patent grant. Much of this code is AI-written, and how copyright applies to that is unsettled, so take legal advice if it matters commercially. |
| 22 | Push this branch and open a PR against PR #1's branch? | **Yes, after item 21** | Everything since PR #1 exists only on this Mac (§79: the repository is the durable record). Because the repository is public, pushing publishes them. A PR is review, not a merge (§80). |
| 23 | Compare two AMD manuals with AMD's own copies? | **Now, before items 10–12** | §3 requires legitimate specifications and §16 a confirmed revision. Both were read from the kib.kiev.ua mirror because AMD's site refuses automated reads. Download each from AMD and run `shasum -a 256` on it. IOMMU spec 48882 Rev 3.11: `f7c375a15db5ed63de760356867211063d164a2ed59f2d38613daec95894ce22`. APM Vol. 3, 24594 Rev 3.39: `b598514b19f204d973040d9867a7ffc48c949009a8b3dc86b20c9188e721ee45`. |
| 24 | Which 2026+ Intel and AMD machines are the declared targets, and what physical x86 test hardware? | **One Intel and one AMD machine to start; several of each before any qualification** | §74 requires multiple Intel and AMD models, with SMT, hybrid and NUMA variations, and §76 a declared scope. CET shadow stacks, hybrid cores and real IOMMU behaviour cannot be shown under QEMU, and this Mac can't run x86 natively. Choosing models needs current research (§16). |

## What the answers changed (2026-11-08)

- ADR-0006 and ADR-0008 are Accepted. ADR-0007 records the answers to its questions 1–5 and 9, and stays Proposed.
- Item 15: `boot-matrix` ends with `boot-x2apic`.
- Item 17: a statement's `id` must be printable ASCII; errata O1 is closed.
- Item 18: the VT-d PDF is downloaded, outside the repository because it is Intel's document.

**Also found by the same check, not decisions for now:** ADR-0002, ADR-0004 and ADR-0005, already Accepted, have no migration consequences either (§18). The system-call boundary M4 is building has no threat model yet (§58).

**Later:** the Microsoft signature for the first-stage loader (ADR-0002 B7), once that stage exists; USB-IF and Thunderbolt licensing (§35), when that work starts.
