# Open owner decisions

| | |
|---|---|
| As of | 2026-11-08, commit `944a276` on `claude/interesting-carson-ae7c83`. The M4 and signed-boot sessions are still running and may add to this list. |
| Checked against | CLAUDE.md v7.1, item by item. That check changed the first version of this sheet. |
| Evidence | Everything below is proven under QEMU only. Nothing has run on physical hardware (CLAUDE.md §74). |
| How to answer | Reply with numbers: "yes to 5, 6, 8, 9, 13–20". Items marked "Not yet" or "After item N" wait for what they name. Items 21–24 need you. |

The recommendations are Claude's. The decisions are yours (CLAUDE.md §18, §84).

## What has to happen first

The three Proposed ADRs do not yet meet what CLAUDE.md requires of them. An ADR can be marked Accepted only once the steps that apply to it are done. Steps A–C are agent work.

| Step | What | Rule | Applies to |
|---|---|---|---|
| A | Add migration consequences to all three ADRs. ADR-0006 also needs alternatives and trade-offs. | §18 | ADR-0006, 0007, 0008 |
| B | Research OriginFS and record it under `docs/research/`; there is no ledger. The main question: can the crash consistency, integrity and snapshots §40 requires fit this layout at all? | §16, §40 | ADR-0006 |
| C | Write a threat model for DMA and interrupt remapping. The repository has no threat model of any kind. | §58 | ADR-0007 |
| D | Check the specifications' own text: items 18 and 23. | §3, §16 | Items 7, 10–12 |

IOMMU overhead has never been measured (rule 21). QEMU's numbers would mean little, so that waits for hardware (item 24). It does not hold up acceptance.

## OriginFS on-disk format (ADR-0006): blocks mounting any filesystem

| # | Question | Recommendation | Why |
|---|---|---|---|
| 1 | A classic inode layout for v0, read-only? | **Not yet** (steps A, B). Then accept, as a bring-up format that never ships in an approved release. | §40 requires crash consistency, integrity, large files and safe migrations, which v0 lacks. Rule 39 requires safe migration across approved releases. |
| 2 | 4 KiB blocks and a file limit of about 2 MiB? | **Not yet** (item 1). Then accept for v0. | 4 KiB matches the page cache. The limit falls short of §40's large files, so it only works for a format that never ships. |
| 3 | Defer writes, journalling and crash consistency? | **Not yet** (item 1). Then accept. | A read-only format needs no write path. That path needs its own ADR (§41), free to replace this layout. |
| 4 | Reserve space now for per-block Merkle integrity? | **Not yet** (item 1). Then no, add it later. | §40's integrity belongs to the design that replaces v0. Space reserved for a design that doesn't exist yet is likely to be wrong. |

## IOMMU (ADR-0007): built and boot-proven; needs steps A, C and D before it is Accepted

| # | Question | Recommendation | Why |
|---|---|---|---|
| 5 | Default-deny, one domain per device? | **Accept** | §58's DMA protection; §47's third-party drivers depend on this confinement. |
| 6 | DMA addresses from a 1–2 GiB window, never physical addresses? | **Accept** | A translation failure can't pass unnoticed, and it can change later without an ABI break. Addresses aren't reused yet, which must be fixed before any driver maps memory per request. |
| 7 | Legacy translation mode now, scalable mode later? | **After item 18**, then accept. | §16 puts the authoritative text first. Whether 2026+ platforms keep legacy mode is unconfirmed until it is read. |
| 8 | Refuse devices that need firmware-reserved memory (RMRR) until identity maps exist? | **Accept, with a deadline** | It fails closed. USB controllers and integrated graphics commonly need RMRRs, so identity maps must land before real hardware. |
| 9 | With no IOMMU present, refuse bus mastering for external and hot-plugged devices? | **Yes** | §32 asks for external-device authorization. An IOMMU should also be part of the declared hardware scope (item 24). |
| 10 | AMD-Vi: blocking device table entries, one interrupt table per device? | **After item 23**, then accept. | The same posture as VT-d, but the encodings came from a mirror copy of AMD's manual. |
| 11 | Allow two QEMU gaps in the AMD-Vi proofs (fault address 0, no record of a blocked interrupt)? | **After item 23**, then accept as QEMU limits. | Hardware qualification must check both (§74). The address bug is fixed after QEMU 11.1, so that check can tighten on upgrade. |
| 12 | Refuse devices that an IVMD names, as for RMRRs? | **After item 23**, then accept. | Same reasoning and deadline as item 8. |

## CPU indexes (ADR-0008): built and boot-proven; needs step A before it is Accepted

| # | Question | Recommendation | Why |
|---|---|---|---|
| 13 | Dense indexes in MADT order, with `CpuIndex` the only per-CPU array index? | **Accept** | The type makes mixing up an APIC ID and an index a compile error. |
| 14 | Keep 64 static per-CPU slots for now? | **Keep for now** | Move to sizing at discovery with the scheduling work (TASKS item 4), before any machine with more than 64 logical CPUs enters the declared scope. |
| 15 | Add `boot-x2apic` to `boot-matrix`? | **Yes** | About 40 s on a two-minute gate. Today's merges changed the code it guards. |
| 16 | Park a silent AP with INIT and drill it on every boot? Per-AP trampoline data too? | **Accept parking; per-AP data later** | Parking closes the hazard it was built for. Per-AP data belongs with parallel start-up. |

## Signature format (ONBF errata O1)

| # | Question | Recommendation | Why |
|---|---|---|---|
| 17 | A signature statement's `id` must be NFC, but the parser has no Unicode tables. Restrict it to ASCII, or add tables? | **Restrict to ASCII** | The same call as errata R4. This parser runs before any signature is checked, so it should stay small (§13). An `id` is a machine identifier; display names belong in localized metadata (§45, §51). |

## Downloads

| # | Question | Recommendation | Why |
|---|---|---|---|
| 18 | Intel VT-d Rev 5.20 PDF (D51397-019, intel.com, about 10 MB)? | **Approve, first** | §16 wants this text before implementation. The VT-d code was written without it, so it must be checked now. |
| 19 | NIST ACVP vectors (ML-DSA, SHA-2/3), the FIPS 204 errata sheet, Wycheproof (Ed25519)? | **Approve** | §59 requires known-answer tests and independent validation. Host-only test data. Each file's source and size will be stated before fetching. |
| 20 | The `loom` crate as a host-only dev-dependency? | **Approve** | §73 names Loom, and it never reaches the shipping runtime (§5). |

## Only you can do these

| # | Question | Recommendation | Why |
|---|---|---|---|
| 21 | The project licence? | **Apache-2.0 if this is to be open. If closed, make the repository private now.** | The GitHub repository is public with no LICENSE file: anyone can read the code, but nobody else has permission to reuse it. Apache-2.0 includes a patent grant. Much of this code is AI-written, and how copyright applies to that is unsettled, so take legal advice if it matters commercially. |
| 22 | Push this branch and open a PR against PR #1's branch? | **Yes, after item 21** | 26 commits exist only on this Mac (§79: the repository is the durable record). Because the repository is public, pushing publishes them. A PR is review, not a merge (§80). |
| 23 | Compare two AMD manuals with AMD's own copies? | **Now, before items 10–12** | §3 requires legitimate specifications and §16 a confirmed revision. Both were read from the kib.kiev.ua mirror because AMD's site refuses automated reads. Download each from AMD and run `shasum -a 256` on it. IOMMU spec 48882 Rev 3.11: `f7c375a15db5ed63de760356867211063d164a2ed59f2d38613daec95894ce22`. APM Vol. 3, 24594 Rev 3.39: `b598514b19f204d973040d9867a7ffc48c949009a8b3dc86b20c9188e721ee45`. |
| 24 | Which 2026+ Intel and AMD machines are the declared targets, and what physical x86 test hardware? | **One Intel and one AMD machine to start; several of each before any qualification** | §74 requires multiple Intel and AMD models, with SMT, hybrid and NUMA variations, and §76 a declared scope. CET shadow stacks, hybrid cores and real IOMMU behaviour cannot be shown under QEMU, and this Mac can't run x86 natively. Choosing models needs current research (§16). |

## What answering changes

- Answers are recorded in the ADRs. An ADR becomes Accepted only when its steps above are done.
- Item 15 adds `boot-x2apic` to `boot-matrix`. Item 17 restricts a statement's `id` to ASCII and closes errata O1. Items 18–20 download the approved files.
- Nothing is pushed unless you answer yes to item 22.

**Also found by the same check, not decisions for now:** ADR-0002, ADR-0004 and ADR-0005, already Accepted, have no migration consequences either (§18). The system-call boundary M4 is building has no threat model (§58).

**Later:** the Microsoft signature for the first-stage loader (ADR-0002 B7), once that stage exists; USB-IF and Thunderbolt licensing (§35), when that work starts.
