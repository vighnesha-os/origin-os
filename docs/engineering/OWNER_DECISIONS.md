# Open owner decisions

| | |
|---|---|
| As of | 2026-11-09, on `main`, after the pre-merge review. Everything that was on a side branch is merged. |
| Checked against | CLAUDE.md v7.1, item by item. That check changed the first version of this sheet. |
| Evidence | Everything below is proven under QEMU only. Nothing has run on physical hardware (CLAUDE.md §74). |
| Answered | 2026-11-08: items 1–29. 2026-11-09: items 33–38 and 40–51. |
| Still open | Items 30–32 and 39 below. Buying the machines ADR-0009 chose. |

The recommendations are Claude's. The decisions are yours (CLAUDE.md §18, §84).

## What has to happen first

CLAUDE.md requires these of the three Proposed ADRs. An ADR can be marked Accepted only once the steps that apply to it are done. Steps A–D are done.

| Step | What | Rule | Applies to |
|---|---|---|---|
| A, **done** | Trade-offs and migration consequences added to all three ADRs, and alternatives to ADR-0006. | §18 | ADR-0006, 0007, 0008 |
| B, **done** | [The OriginFS ledger](../research/2026-11-08-originfs-design.md). **The layout cannot grow into §40:** its inode is full, its block pointers hold only an address, and its inode table cannot move. | §16, §40 | ADR-0006 |
| C, **done** | [The IOMMU threat model](THREAT_MODEL_IOMMU.md): 24 threats, 9 still open. Early-boot DMA and failing open are the most serious. | §58 | ADR-0007 |
| D, **done** | [Every VT-d encoding is checked against Rev 5.20](../research/2026-11-08-vt-d-iommu.md), which answers item 7. Both AMD manuals are byte-identical to AMD's own copies (item 23). | §3, §16 | Items 10–12 |

IOMMU overhead has never been measured (rule 21). QEMU's numbers would mean little, so that waits for hardware (item 24). It does not hold up acceptance.

## OriginFS on-disk format (ADR-0006): blocks mounting any filesystem

| # | Question | Recommendation | Why |
|---|---|---|---|
| 1, answered | A classic inode layout for v0, read-only? | **Accept, as a bring-up format that never ships in an approved release** (new decision F7) | v0 cannot become the §40 filesystem, so it should only prove the read path. Rule 39 would otherwise require a migration path from it. |
| 2, answered | 4 KiB blocks and a file limit of about 2 MiB? | **Accept for v0** | 4 KiB matches the page cache. The limit falls short of §40's large files, so it only works for a format that never ships. |
| 3, answered | Defer writes, journalling and crash consistency? | **Accept** | A read-only format needs no write path. Writing belongs to a successor format with its own ADR (§41). |
| 4, answered | Reserve space now for per-block Merkle integrity? | **No** | §40's integrity belongs to the successor. v0's pointers have no room for it, and reserved space would never be used. |

## IOMMU (ADR-0007): built, boot-proven and Accepted

| # | Question | Recommendation | Why |
|---|---|---|---|
| 5, answered | Default-deny, one domain per device? | **Accept** | §58's DMA protection; §47's third-party drivers depend on this confinement. |
| 6, answered | DMA addresses from a 1–2 GiB window, never physical addresses? | **Accept** | A translation failure can't pass unnoticed, and it can change later without an ABI break. Addresses aren't reused yet, which must be fixed before any driver maps memory per request. |
| 7, answered | Legacy translation mode now, scalable mode later? | **Accept** | Rev 5.20 still defines legacy mode, with no deprecation notice. It needs second-stage translation: a newer unit that reports `SSTS` clear has none, and the kernel refuses it (T6). Hardware must confirm this (§74). |
| 8, answered | Refuse devices that need firmware-reserved memory (RMRR) until identity maps exist? | **Accept, with a deadline** | It fails closed. USB controllers and integrated graphics commonly need RMRRs, so identity maps must land before real hardware. |
| 9, answered | With no IOMMU present, refuse bus mastering for external and hot-plugged devices? | **Yes** | §32 asks for external-device authorization. An IOMMU should also be part of the declared hardware scope (item 24). |
| 10, answered | AMD-Vi: blocking device table entries, one interrupt table per device? | **Accept** | The same posture as VT-d, and item 23 confirmed the encodings come from AMD's own text. |
| 11, answered | Allow two QEMU gaps in the AMD-Vi proofs (fault address 0, no record of a blocked interrupt)? | **Accept as QEMU limits** | Hardware qualification must check both (§74). The address bug is fixed after QEMU 11.1, so that check can tighten on upgrade. |
| 12, answered | Refuse devices that an IVMD names, as for RMRRs? | **Accept** | Same reasoning and deadline as item 8. |
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
| 19, answered | NIST ACVP vectors (ML-DSA, SHA-2/3), the FIPS 204 errata sheet, Wycheproof (Ed25519)? | **Approved; fetched 2026-11-08**, after each file's name, source and size were confirmed with the owner | §59 requires known-answer tests and independent validation. Host-only test data. The applicable vectors are committed under `security/crypto/tests/vectors/` with the upstream notices; sources and hashes are in `SOURCE_PROVENANCE.md`. |
| 20, answered | The `loom` crate as a host-only dev-dependency? | **Approved; added 2026-11-08** after its version, licence, transitive crates and sizes were stated and accepted (`SOURCE_PROVENANCE.md`, "Loom models") | §73 names Loom, and it never reaches the shipping runtime (§5): `cargo xtask check` now fails if a third-party crate enters a shipping crate's dependencies or `Cargo.lock` changes without approval. |

## Only you can do these

| # | Question | Recommendation | Why |
|---|---|---|---|
| 21, answered | The project licence? | **MIT OR Apache-2.0** (your choice, 2026-11-08) | `LICENSE-MIT` and `LICENSE-APACHE` are in the repository. The repository stays public. Much of this code is AI-written, and how copyright applies to that is unsettled, so take legal advice if it matters commercially. |
| 22, answered | Push this branch and open a PR against PR #1's branch? | **Yes, after merging the finished side work** (your choice, 2026-11-08) | Signed boot and the crypto vectors are merged here. The clock and scheduler proof fixes followed on your word, after the PR opened, and the early console and the wait-queue and timer fixes on 2026-11-09. M4, Loom and early-boot DMA stay on their own branches: they were not finished when you chose this, and each needs your word to merge. A PR is review, not a merge (§80). |
| 23, answered | Compare two AMD manuals with AMD's own copies? | **Byte-identical** (done by Claude at your request, 2026-11-08) | §3 requires legitimate specifications and §16 a confirmed revision. AMD's portal serves 48882 Rev 3.11 (released 2026-04-08, 7,705,637 bytes) and 24594 Rev 3.39 (released 2026-10-28, 16,226,519 bytes). Both have the SHA-256 of the mirror copies that were read: `f7c375a15db5ed63de760356867211063d164a2ed59f2d38613daec95894ce22` and `b598514b19f204d973040d9867a7ffc48c949009a8b3dc86b20c9188e721ee45`. They were hashed in memory, and no PDF was saved. |
| 24, answered | Which 2026+ Intel and AMD machines are the declared targets, and what physical x86 test hardware? | **[ADR-0009](../adr/ADR-0009-hardware-targets.md), decided by Claude at your request (2026-11-08).** A platform counts if the vendor launched its generation in 2026 or later. First machines: #2 (ASRock iBOX-358H, Intel), then #5 (Lenovo M75q Gen 6, AMD). **Nothing is bought; that is yours.** | §74 requires multiple Intel and AMD models, with SMT, hybrid and NUMA variations, and §76 a declared scope. CET shadow stacks, hybrid cores and real IOMMU behaviour cannot be shown under QEMU, and this Mac can't run x86 natively. |

## What the answers changed (2026-11-08)

- ADR-0006 and ADR-0008 are Accepted. ADR-0007 recorded the answers to its questions 1–5 and 9, and was Accepted later the same day (below).
- Item 15: `boot-matrix` ends with `boot-x2apic`.
- Item 17: a statement's `id` must be printable ASCII; errata O1 is closed.
- Item 18: the VT-d PDF is downloaded, outside the repository because it is Intel's document.
- Item 23: both AMD manuals match AMD's own copies, so items 10–12 are ready to answer.
- Item 24: ADR-0009 records the hardware scope and the first machines.
- Items 10–12: ADR-0007 is Accepted.

## New since this sheet was written

| # | Question | Recommendation | Why |
|---|---|---|---|
| 26, answered | Errata V1–V6 and open gaps O2–O5 from the signed-boot work, and OpenSSL 3.5+ as the host signing tool (`docs/spec/ONBF-errata.md`, `docs/research/2026-11-08-krn-signature-verification.md`) | **V1–V6 accepted. O2: `origin/onbf/1/okc`. O3: check against signed time. O4: exact IDs only. O5: bare public keys. Host signing: original Rust (`onbf-sign`), not OpenSSL. Zeroization: best effort for host development keys** (your choices, 2026-11-08, in the signed-boot session) | Certificate chains are now specified and wait for the SYS profile's signed time. Release keys belong in an HSM (§60). O1 was already answered as item 17. |

**Also found by the same check, not decisions for now:** ADR-0002, ADR-0004 and ADR-0005, already Accepted, have no migration consequences either (§18). The system-call boundary M4 is building has no threat model yet (§58).

## Early console (ADR-0010): built, boot-proven, Accepted and merged

ADR-0009 H6 asked for a console that does not depend on a 16550 at `0x3F8`. The kernel has it since the merge of 2026-11-09, proven under QEMU only: every kernel line drawn on the UEFI framebuffer, the UART taken from the ACPI SPCR when firmware names one, 0x3F8 as the fallback, and a boot that goes on with neither. Evidence: `docs/adr/ADR-0010-early-console.md`, `docs/engineering/SOURCE_PROVENANCE.md`, "The early console".

| # | Question | Recommendation | Why |
|---|---|---|---|
| 27, answered | Boot info 0.4: the framebuffer appended as 48 bytes (`Framebuffer`, E1) | **Accepted by the owner, 2026-11-08** | This is the §84 ABI decision for this work. It adds one field group, with no compatibility promise at major 0, and matches ADR-0002 B4 (the loader translates firmware structures). Alternatives and trade-offs are in the ADR |
| 28, answered | The framebuffer mapped uncached in the first 2 GiB of the MMIO window, by the loader, and kept at that address by the kernel (E2) | **Accepted by the owner, 2026-11-08** | Draws from the first kernel line and across the page-table switch. ADR-0004's layout keeps MMIO out of the direct map; drivers' MMIO now starts 2 GiB higher |
| 29, answered | The UART: COM1 until the ACPI tables can be read, then the SPCR's UART with earlier output replayed; 0x3F8 only when firmware gives nothing better (E4) | **Accepted by the owner, 2026-11-08** | A hang before the tables are readable still shows on the screen and on COM1; once firmware names a UART, COM1 is not used |

## From the pre-merge review (2026-11-09)

| # | Question | Recommendation | Why |
|---|---|---|---|
| 30 | ADR-0007 question 10: clear bus mastering on every function at enumeration (I12), bridges included, and leave an AMD IOMMU's own function alone until hardware shows what it needs? | **Accept** | §32 and §58. It is built and boot-proven, and it merged on your word. It may stop firmware-owned controllers (USB legacy, a firmware framebuffer) until drivers or exceptions exist, which hardware must settle (§74). |
| 31 | The Loom download went beyond what you approved: 26 crate files and 3.36 MiB, against the 24 and 3.09 MiB you were told, and `memchr` and `aho-corasick` were said not to be fetched | **Accept** | §5. All are host-only and no shipping crate reaches them, and since the review they sit outside the kernel's workspace. The record should match what happened. |
| 32 | Add CI: a GitHub workflow running `cargo xtask check` and the boot tests on every pull request? | **Yes** | §8 and §82 put CI in M0, and today every gate depends on one Mac. §80 forbids silent external costs, so it needs your word. GitHub's standard runners are free for public repositories. |

## The native SDK (ADR-0011): Accepted 2026-11-09, nothing built

Every recommendation below was checked against the whole of CLAUDE.md. The sections each one touches are in the ADR's `Requires` row and in its decisions.

| # | Question | Recommendation | Why |
|---|---|---|---|
| 33, answered | Command-line tools, the shell, the terminal and the recovery console are SDK clients with no private doors (N1, N2)? | **Yes** | Goes further than §48 and §70, which bind only the GUI. Removes shell injection and text-format coupling, and gives permissions and versioning one home (P3, rules 10–12). |
| 34, answered | Stable promises per capability, with every module having a typed interface, rather than a stable SDK per module (N3, N4)? | **Yes** | A stable promise on every internal module freezes what must change (§7, P8) and multiplies versioned units against one release identity (§62, §63). |
| 35, answered | The Framekernel's privileged crate first, with every other kernel crate `forbid(unsafe_code)` (N6, N14)? | **Yes**, and it delays M5 and M6 work | ADR-0001's security argument depends on it, and the review found it does not exist (rule 5). It is the kernel's part of your SDK direction. |
| 36, answered | A small original interface language (N7)? | **Yes** | One source of truth for Rust, C, IPC, the kernel's calls and documentation (§42, §49). Adopting one would put third-party generated code in the runtime (§3, §5). |
| 37, answered | Kernel-stamped caller identity on every channel message (N9, an ADR-0005 amendment)? | **Yes** | §30 and §48 require authenticated IPC, and a capability says what may be done, not who asked. |
| 38, answered | Each system-call number accepted only from its own address in `origin.core` (N5)? | **Yes** | Enforces §42's private door. It is hardening, not authorization (§58). |
| 39 | If accepted, will you amend CLAUDE.md §48 and §70 to say this? | Your text, your call | Claude has not changed the directive. |

## The interface language (OIDL): accepted 2026-11-09, as recommended

The specification is `docs/spec/OIDL.md`, accepted by the owner on 2026-11-09. The generator and the first definition exist and are host-tested; the kernel does not use the output yet. Each question is answered there, in §19, with its reasoning and the sections of CLAUDE.md it was checked against. Summary:

| # | Question | Recommendation |
|---|---|---|
| 40 (Q1), answered | Accept wire format revision 1 as specified (§7)? | **Accept** |
| 41 (Q2), answered | Handles in the body as a slot equal to the handle's position? | **Accept** |
| 42 (Q3), answered | A hello on every connection, negotiating the minor (§7.7)? | **Accept** |
| 43 (Q4), answered | Exact size at the negotiated minor, refusing unknown trailing bytes? | **Exact** |
| 44 (Q5), answered | A library whose version is already in `origin-version.toml` names that key? | **Accept** |
| 45 (Q6), answered | A named `capability` per library and `requires` mandatory on every method? | **Accept** |
| 46 (Q7), answered | A method's errors frozen for the whole major? | **Accept** |
| 47 (Q8), answered | Have `channel_read` report each handle's type and rights, with ADR-0005 B1? | **Accept, with B1** |
| 48 (Q9), answered | A `string` is valid UTF-8 with no NUL? | **Accept** |
| 49 (Q10), answered | C symbols are `<library>_<method>`, with no special case for the kernel? | **Accept** |
| 50 (Q11), answered | Keep `event_create` and `object_signal` as `@reserved`? | **Keep them** |
| 51 (Q12), answered | A type from another library is read with all its members known, because only the interface's own library is negotiated? | **Accept** |

**Later:** the Microsoft signature for the first-stage loader (ADR-0002 B7), once that stage exists; USB-IF and Thunderbolt licensing (§35), when that work starts.
