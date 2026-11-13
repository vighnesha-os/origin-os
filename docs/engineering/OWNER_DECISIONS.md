# Open owner decisions

| | |
|---|---|
| As of | 2026-11-10 for items 71–160, 2026-11-11 for items 161 to 190, 2026-11-12 for items 191 to 246 and 252 to 270, and 2026-11-13 for items 271 to 282. Items up to 70: 2026-11-09, after the pre-merge review. |
| Checked against | Items 1–70: CLAUDE.md v7.1, item by item. That check changed the first version of this sheet. Items 71–97: v7.2, and the 7.3 revision they produced. Items 98–105: as their sections say. Items 106–123: v7.3 and every accepted ADR, in the design review that put them, and the 7.4 revision they produced. Items 124–141: v7.4 and ADR-0001 to ADR-0015 as they stand on the records branch, by the task that put them (TASKS item 19a). Items 145–151: v7.4, the accepted ADRs and NIST's SP 800-90A, 90B and 90C, by the task that put them (TASKS item 18a). Items 152–154: ADR-0017 and ADR-0013, by the task that built ADR-0017's first half (TASKS item 19b) and the session that keeps the review batch. Item 161: items 70 and 106. Items 142–144, 155–160, 162–164, 166 and 188: as their sections say. Items 165 and 168: your own text for CLAUDE.md §1, in its second and third versions, read against the whole of revision 7.5 and the ADRs each change names; the check and the list of changes are `ROLES_CHECK_2026-11-11.md`. Item 167: items 70 and 113 and CLAUDE.md §78 and §80, by the session that keeps the second step-3 batch. Item 189: proposal 11 of `ROLES_CHECK_2026-11-11.md`. Items 169–187 and 190: as their sections say. Items 191–196: CLAUDE.md 7.6's §1, by TASKS item 35's session, with one rule added to item 191 by the coordinating session. Items 197–220: CLAUDE.md 7.6, the accepted ADRs and `docs/spec/OIDL.md`, by the session that wrote step 4's ledger, and again by the coordinating session against ADR-0005 A6, ADR-0011 N8 and the system threat model. Items 221–223: items 191–196 and the matrix as built, by the coordinating session, which ran its gate. Items 224–234: the bare-metal plan's sources, as its ledger records them, and four of its statements about the code, checked by the coordinating session. Item 235: the rental tool at `70e596d`, read in full by the coordinating session, whose own run of its offline tests passed. Item 236: rental A's findings (`docs/research/2026-11-12-rental-a-findings.md`, point 6). Item 237: OIDL §13 and the lock of `origin.kernel` 0.7, by the session that built S4-0 and by the coordinating session. Items 238–246: CLAUDE.md 7.6 and ADR-0001 to ADR-0017 as they stand on `main` at `04ec842`, by the architecture-review session, which wrote them. Items 252–263: CLAUDE.md 7.6 and the requirements matrix only, by the coordinating session; it had not then read the accepted ADRs in full. Items 264–268: CLAUDE.md 7.6, ADR-0001 to ADR-0018, the four threat models and the specifications, read in full by the same session (`DESIGN_READ_2026-11-12.md`). Items 269 and 270: each proposed row's scope in the matrix, and the texts of ADR-0011 N13 and ADR-0013, by the coordinating session and by the session that wrote the roadmap. Items 271–275: CLAUDE.md 7.6 §10, §11, §17, §42, §53, §60, §75 and §80; ADR-0005 P4, P6, P10, P12, B2 and Amendment 8; ADR-0011 Amendment 2; OIDL §10's register decoding; ONBF §6.1 and ONBF-R9; ADR-0018 V3; decision item 32; the matrix's README. Every accepted ADR and the specifications had been read in full the day before (`DESIGN_READ_2026-11-12.md`) and were searched again for the psABI and for extending an argument; they were not read again in full. Items 276 and 277: `docs/ROADMAP.md`'s M6 and its gate; matrix rows A05-H1 and A11-L13; OIDL §3, §4.2 and §10's register decoding; ADR-0011 Amendment 3; and the generator's rule and the kernel's decoder as task S4-2's session and the independent review of its build half each read them at `ac22d57`, a commit that is on no pushed branch. Items 278 and 279: CLAUDE.md 7.7 §1, §8 to §15 and §79; decision items 70 and 106; the official stable channel's manifest and the release notes of Rust 1.100.0, read on 2026-11-13; and `rust-toolchain.toml`, `.cargo/config.toml` and the build tool's reading of Cargo's build directory as they stand on `main` at `c05797c`. Items 280 and 281: ADR-0017 Z2, Z10, question 6 (item 129) and Amendment 1; CLAUDE.md §2, §12, §14 and §15; issue 162235 of `rust-lang/rust`, read on 2026-11-13; and the upgrade task's first build with the pin at 1.100.0, reported by its session and not run by the coordinating session. |
| Evidence | Everything below is proven under QEMU only. No Origin code has run on physical hardware (CLAUDE.md §74): rental A (items 224–235) read a rented server with the provider's own Linux. |
| Answered | 2026-11-08: items 1–29. 2026-11-09: items 30–70. 2026-11-10: items 71–160 (item 97 delegated to Claude). 2026-11-11: items 161 to 190 (item 168 delegated to Claude). 2026-11-12: items 191 to 246 and 252 to 270. 2026-11-13: items 271 to 282. |
| Still open | No numbered item. Item 168's changes to your text are Claude's, and you have not read them. Buying machines is off for now (ADR-0009's update of 2026-11-09). Items 77 and 79 leave you two things to do, and items 106, 121 and 122 three more (see their sections). |

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
| 145, answered | Six files from NIST for the kernel's random-number generator (TASKS 18a, ADR-0016)? To read: SP 800-90A Rev. 1 (2,417,007 bytes), SP 800-90B (1,043,696) and its errata sheet (107,606), SP 800-90C (10,056,754) and FIPS 198-1 (129,454), from nvlpubs.nist.gov and csrc.nist.gov. To convert and commit: the ACVP vectors for HMAC_DRBG, `hmacDRBG-1.0/internalProjection.json` (1,160,048 bytes), from github.com/usnistgov/ACVP-Server at commit `975de31`, the release item 19's vectors are pinned to. | **Approved 2026-11-10: "Approve all six"**, after each file's name, source and size had been put to you; your words reached the generator's session through the coordinating session. The vector file was fetched after that and checked against the hash GitHub lists; its SHA2-512 vectors are committed under `security/crypto/tests/vectors/acvp/` with NIST's notice. The five documents had already been read in memory, with no file written, before your answer reached the session; they are not in the repository. Two copies of SP 800-90A that the fetch tool had saved unasked were deleted unopened | §16 wants the standards' text before a design is accepted, and §59 known-answer tests. Host-only test data. Sizes, hashes and the pages read are in `docs/research/2026-11-10-kernel-random-numbers.md`; the vectors' provenance is in `SOURCE_PROVENANCE.md`, "HMAC-SHA-512 and HMAC_DRBG". The number 145 leaves 142 to 144 to the branches that use them |
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
| 52, answered | ADR-0010 Amendment 1: each line to the UART first, then drawn at most 256 pixels per interrupts-off section, instead of E3's screen first | **Accepted by the owner, 2026-11-09** | The UART is the log of last resort and no longer waits behind pixels. Under KVM on an AMD EPYC 9V74 a section holds interrupts off for about 24 µs; a whole line took 4.3 to 4.6 ms on a different EPYC model. No real machine has been measured |
| 53, answered | ADR-0010 Amendment 2: the UART sent at most 16 bytes (one FIFO load) per interrupts-off section, the wait for the wire between sections; a line from an interrupt handler leaves with the next writer or report | **Accepted by the owner, 2026-11-09** | At most 17 register accesses with interrupts off, whatever the line speed, against about 8.7 ms per 100 bytes at 115 200 baud. What a section costs on a real UART is not measured yet; the kernel prints it on every boot |

## From the pre-merge review (2026-11-09)

| # | Question | Recommendation | Why |
|---|---|---|---|
| 30, answered | ADR-0007 question 10: clear bus mastering on every function at enumeration (I12), bridges included, and leave an AMD IOMMU's own function alone until hardware shows what it needs? | **Accepted by the owner, 2026-11-09** | §32 and §58. It is built and boot-proven, and it merged on your word. It may stop firmware-owned controllers (USB legacy, a firmware framebuffer) until drivers or exceptions exist, which hardware must settle (§74). |
| 31, answered | The Loom download went beyond what you approved: 26 crate files and 3.36 MiB, against the 24 and 3.09 MiB you were told, and `memchr` and `aho-corasick` were said not to be fetched | **Accepted by the owner, 2026-11-09** | §5. All are host-only and no shipping crate reaches them, and since the review they sit outside the kernel's workspace. The record should match what happened. |
| 32, answered | Add CI: a GitHub workflow running `cargo xtask check` and the boot tests on every pull request? | **Yes: the owner's decision, 2026-11-09** | §8 and §82 put CI in M0, and until then every gate depended on one Mac. §80 forbids silent external costs, so it needed the owner's word. It uses only GitHub's standard runners, free for this public repository. Built in `.github/workflows/ci.yml`, with the boots on a real processor through KVM as well as on QEMU's software CPU. A trial branch first booted the kernel on a real AMD EPYC on those runners (`docs/research/2026-11-09-testing-without-buying-hardware.md`) and showed costs QEMU's software CPU had hidden. |

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
| 39, answered | If accepted, will you amend CLAUDE.md §48 and §70 to say this? | **Yes, with Claude drafting the wording and the owner approving it before it goes in (2026-11-09)** | The owner approved the wording the same day; it is CLAUDE.md revision 7.2, and 7.1 is kept at `docs/engineering/CLAUDE-v7.1.md`. |

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

## The monotonic clock (ADR-0012): built, boot-proven, Accepted 2026-11-09

CI caught one processor's TSC stepping back by 16 ticks on an EPYC 9V45 under KVM. Neither Intel nor AMD promises that two processors' counters agree, so the kernel now makes that promise itself, with a floor every read passes through. Proven under QEMU only (§74).

| # | Question | Recommendation | Why |
|---|---|---|---|
| 54, answered | Accept ADR-0012: the monotonic clock never goes backwards, whatever the processors' TSCs say (C1), through a floor on every read, always on (C2); a machine without an invariant TSC keeps order but not rate (C3); `IA32_TSC_ADJUST` unused until a physical machine shows a constant offset (C5)? | **Accepted by the owner, 2026-11-09** ("Accept both", answering "Accept ADR-0012 (clock) and ADR-0013 (boot-check policy)?") | §68 and §66. Nothing else makes the promise on every machine, and no caller is hot today: alone a read costs one more variable access, and it does not scale when everything reads at once (C8). `ADR-0013` is item 55. |

## What a failed boot proof means (ADR-0013): built, boot-proven, Accepted 2026-11-09

`kernel_main` stopped for five proofs and ignored the rest, so a real machine did nothing about a failed one. Every proof now has a row in one table: 13 stop the boot, 6 withdraw one capability, 31 are reported, and every boot ends with a verdict line. A proof can stop the boot only if nothing can run without it, or it covers a P0 property and a correct kernel cannot fail it through timing, load or being a different machine. CI's boots on real processors are why that second test exists. Proven under QEMU only.

| # | Question | Recommendation | Why |
|---|---|---|---|
| 55, answered | Accept ADR-0013: the Refuse, Withdraw and Report classes and R1's test for Refuse (questions 1 and 2), that a refusal halts rather than resets until a boot manager records boot attempts (3), no "continue past a refusal" switch (4), and which proofs a production boot keeps decided before M9 (5). | **Accepted by the owner, 2026-11-09** ("Accept both", for ADR-0012 and ADR-0013 together) | The smallest policy that does not stop working machines |

## ADR-0011 N14 step 3: caller identity, the start record, the directory, memory objects and ports (Accepted 2026-11-09)

Step 3 adds system calls and structures, so these are ABI and format decisions (CLAUDE.md §84). The owner accepted all thirteen with the recommended answer on 2026-11-09 ("Accept all 13"). They are ADR-0005 Amendment 4, C1–C13; the scoping is `docs/research/2026-11-09-n14-step3-scope.md`. C1–C3 are built (task S3-2, 2026-11-10: a message's writer and handle records), C4–C6 (task S3-3, 2026-11-10: the start record and its launch path), C11 and C12 (task S3-4, 2026-11-10: `not_found` and the service directory, `origin.directory` 0.1), and C7 and C8, with C6's rights for a memory object (task S3-5b, 2026-11-10: memory objects, `origin.kernel` 0.5, as items 101–105 decide); C9, C10 and C13 are not.

| # | Question | Answer |
|---|---|---|
| 56, answered | C1: a message's identity is `{u64 process; u64 principal}`: process IDs unique for the boot and never reused (0 the kernel), the principal opaque, set at creation, inherited and immutable? | **Accept** |
| 57, answered | C2: the identity reaches the reader through `channel_read`'s growing `read_sizes` result, old-size callers unaffected? | **Accept** |
| 58, answered | C3: OIDL allows plain structs as system-call vector and array elements (a change to OIDL's format), for B3's handle records? | **Accept** |
| 59, answered | C4: the start record v1 layout (32-byte header, 16-byte handle entries, then arguments and environment; kernel-written, read-only, random address; at most 64 handles and 64 KiB)? | **Accept** |
| 60, answered | C5: add the DIRECTORY role and retire SERVICE, never reusing its number? | **Accept** |
| 61, answered | C6: the default rights per object type, with `write` on a process handle needed to change its address space? | **Accept** |
| 62, answered | C7: memory committed at creation and charged to the creator; whole-mapping unmap and protect; no fixed-address replacement; no execute until the Code Integrity Table exists? | **Accept** |
| 63, answered | C8: `memory_map(process, object, map_request) -> address`? | **Accept** |
| 64, answered | C9: one-shot, level-triggered port bindings with reserved storage; a silent explicit cancel that also removes queued packets? | **Accept** |
| 65, answered | C10: events now, with `object_signal` on events only and `SIGNALED` only in v1? | **Accept** |
| 66, answered | C11: append `not_found = -14`? | **Accept** |
| 67, answered | C12: `origin.directory` 0.1, keyed by OIDL interface ID and major, with a pipelined `connect`? | **Accept** |
| 68, answered | C13: a `clock_read` system call now, a shared clock page later? | **Accept** |

## Memory the firmware sets apart (ADR-0002, boot info 0.5; Accepted 2026-11-09)

UEFI added two memory attributes after 2.7 that a 2026+ kernel must respect, and the loader read only the memory type. Changing what the loader hands the kernel is a boot-info format change (§84). Research: `docs/research/2026-11-09-uefi-2.8-to-2.11.md`.

| # | Question | Answer |
|---|---|---|
| 69, answered | After the landing, make the loader honour `EFI_MEMORY_SP` (UEFI 2.8) and `EFI_MEMORY_HOT_PLUGGABLE` (UEFI 2.11): conventional memory carrying either becomes its own region kind, `SpecificPurpose` or `HotPluggable`, which the kernel neither allocates from nor direct-maps (boot info 0.5)? | **Yes, after the landing (2026-11-09)** |

## How work lands (2026-11-09)

Work reaches `main` through pull requests, and merging each one takes the owner's word. Until this decision each task opened its own pull request, so the owner was asked to merge pull requests of 1 to 14 commits.

| # | Question | Answer |
|---|---|---|
| 70, answered | Offer a pull request for review only once it carries at least 100 commits? | **Yes: "create PRs with minimum 100 commits" (2026-11-09)** |

What this means in practice:

- Tasks collect on one landing branch per batch, as a draft pull request. CI runs on every push to it, so each task is tested, including the real-CPU boots, as it arrives. The first batch is the first part of ADR-0011 N14 step 3, pull request 13: merged on the owner's word on 2026-11-11, at 110 commits (`c11e6f4`). The second batch, the rest of step 3, is the landing branch `land/s3-part2`.
- Commits are small and self-contained: each one builds and passes its tests.
- Merge commits from `main` don't count toward the 100, and nothing is padded to reach it (CLAUDE.md §7).
- Merging still takes the owner's word.

**Later:** the Microsoft signature for the first-stage loader (ADR-0002 B7), once that stage exists and an organisation can hold the signature (Amendment 1, item 78); USB-IF and Thunderbolt licensing (§35), when that work starts.

## Editions, device standards and the public repository (ADR-0014, ADR-0015, ADR-0002 Amendment 1): Accepted 2026-11-10, nothing built

On 2026-11-10 you said you are building a Desktop and a Server OS. You want:

- both with the native GUI, "not terminal";
- the server running the full native desktop;
- the kernel, NVMe, USB4, Thunderbolt 4/5, Wi-Fi 6/7 and Bluetooth "with all features as Native SDK".

Claude recommended these seventeen items, each checked against CLAUDE.md v7.2. Items 1–5 were revised twice as your direction became clearer. You answered **"Accept all"** in the same session. They are recorded in CLAUDE.md revision 7.3.

| # | Question | Answer |
|---|---|---|
| 71, answered | Server is an edition of the same release and installs the full desktop by default; editions differ only in defaults (ADR-0014 S1)? | **Accept** |
| 72, answered | Desktop through M9 first; server-only work after M9 (S2)? | **Accept** |
| 73, answered | The first server platform is a KVM guest with its GUI on the VM's display; physical servers later on 2026 platforms (S3)? | **Accept** |
| 74, answered | Administration only through the native GUI (console, management app, remote desktop); remote desktop built before the server ships; no terminal needed; no web administration page (S4)? | **Accept** |
| 75, answered | Server sessions cheap and closed: login screen only when idle, unprivileged, nothing in the UI listening on a network (S5)? | **Accept** |
| 76, answered | Software written for other systems decided later: Rust `std` first, a POSIX library only through its own ADR at M6 (S6)? | **Accept** |
| 77, answered | A trademark search for the product name before the first public release (CLAUDE.md §76)? vivo has shipped "OriginOS" worldwide since 2025-10-15. | **Accept.** The search is yours to arrange |
| 78, answered | Secure Boot defaults to Origin-owned keys; the Microsoft-signed first stage waits for an organisation (ADR-0002 Amendment 1)? This reverses B7's default order of 2026-11-07, which Claude had not pointed out when recommending it. | **Accept** |
| 79, answered | Add `CONTRIBUTING.md` (DCO sign-off, an original-source declaration), `SECURITY.md` and `CODEOWNERS` (CLAUDE.md §6)? | **Accept.** Turning on GitHub's private vulnerability reporting is a repository setting, so it is yours |
| 80, answered | DisplayPort first; read the Thunderbolt licence before its specification; keep restricted documents out of Git (ADR-0015 D7)? | **Accept** |
| 81, answered | Add remote administration, service management, hardware health and remote desktop to §49 (ADR-0014 S7)? | **Accept** |
| 82, answered | Three typed layers per device technology (class, the whole standard, documented vendor extras), with each device reporting its features truthfully (ADR-0015 D1, D2)? | **Accept** |
| 83, answered | Separate rights for dangerous operations; raw access for administrator diagnostics only; unprivileged radio stacks; fuzzed parsers (D3, D4)? | **Accept** |
| 84, answered | Thunderbolt 4/5 inside the USB4 interface; the name only after certification (D5)? | **Accept** |
| 85, answered | Wi-Fi and Bluetooth drivers wait for vendor documentation or an approved clean-room ADR; host stacks first; licensing and regulation checked before shipping (D6)? | **Accept** |
| 86, answered | Add IEEE 802.11-2024 with 802.11be, and Bluetooth Core 6.2 or later, to §17 (D8)? | **Accept** |
| 87, answered | NVMe and xHCI first; USB4, Thunderbolt, Wi-Fi and Bluetooth at M10 on physical hardware (D9)? | **Accept** |

**Yours to do:** the trademark search (item 77), and turning on private vulnerability reporting under the repository's Settings → Security (item 79).

## A strict hardware scope (ADR-0009 Amendment 1): Accepted 2026-11-10, nothing built

You asked for a strict rule: "we are building for 2026+ hardware and in AMD Zen5 and Zen5+ / For Intel also 2026+ hardware". Then, since a CPU reports no year, you asked whether Panther Lake, Nova Lake, Zen 5, Zen 6 and so on could be checked instead. Claude checked the CPUID family and model numbers and recommended these. You answered **"Accept all"**.

| # | Question | Answer |
|---|---|---|
| 88, answered | The scope: Intel platforms launched in 2026 or later; AMD platforms launched in 2026 or later with Zen 5 or newer cores; everything else is bench equipment only (A1)? Zen 5 itself launched in 2024. Counting Zen 5 from any year was offered and not chosen. | **Accept** |
| 89, answered | The kernel names the core generation from CPUID (AMD family 1Ah or higher; Intel family 18 or higher, or family 6 model CCh, D5h, DDh or E5h), from one table built from Intel's and AMD's documents, and reports the scope at boot without ever refusing to boot (A2)? | **Accept** |
| 90, answered | At M9, decide whether the production installer warns on or refuses machines outside the scope, with developer builds exempt (A3)? | **Accept**, to be decided at M9 |

## Linux and other open-source code as a reference only (CLAUDE.md §3, §6, §16): decided by the owner 2026-11-10

Claude had proposed five items allowing Linux drivers to be studied for how they handle hardware. You narrowed that in your own words: "in any case if need to refer Linux code that should be completely as reference, Don't copy just to understand if anything missing from Vendor or official specifications or standards". That replaces Claude's proposal.

| # | Question | Answer |
|---|---|---|
| 91, answered | When may Linux source be read? | **Only as a reference, to understand what the vendor's documents or the official specifications and standards leave out. Never copied** (your rule, 2026-11-10) |
| 92, answered | So that code is never written with Linux source open: the reading step writes the missing facts into Origin's specification or research ledger in Origin's own words, citing file, commit and licence; the code is then written from that, in a separate session, without the Linux source? | **Yes, as recommended** ("keep 92 as before you said", 2026-11-10). Claude writes Origin's code and was trained on Linux's code, so this split is what keeps "don't copy" true. It is also the clean-room process ADR-0015 D6 asked for |
| 93, answered | Record per driver which Linux files were consulted, their licence, and which gap each one filled (§6)? | **Yes** ("accept 93", 2026-11-10) |

You then asked whether public sources, Linux and other open-source operating systems, Ubuntu included, could cover these drivers without spending, as of today rather than from Claude's training data. You answered **"Accept all"** to these.

| # | Question | Answer |
|---|---|---|
| 94, answered | Extend items 91–93 to other open-source projects on the same terms (gaps only, never copied, Origin's own specification first, licence recorded)? <br>• **Ubuntu:** its kernel patches, packaged firmware and user-space stacks such as wpa_supplicant, BlueZ and fwupd. <br>• **Others:** the BSDs, Haiku, Redox, Fuchsia, illumos, EDK2, coreboot, SeaBIOS, QEMU's device models, SPDK, `libspdm`, Zephyr and NimBLE. <br>• **Excluded:** ReactOS (§3 names it, and it reimplements Windows). Leaked or proprietary code is never opened. | **Accept** |
| 95, answered | The source order, with no spending: free official specifications and vendor documents first; then open-source code the vendor wrote itself (EDK2, SPDK, `libspdm`, Intel's Linux drivers); then other open-source systems? The paid PCIe specification stays unread, and a feature with no free legitimate source is reported "not supported" (ADR-0015 D2). | **Accept** |
| 96, answered | Always today's version: read each reference from the newest release on the day (on 2026-11-10 that was Linux 7.4-rc2 or 7.3.3, and Ubuntu 26.10 or 26.04.1 LTS); record its tag or commit and the date; re-check specification revisions the same day; never rely on Claude's memory of older code? | **Accept** |
| 97, answered | Where vendor documents already cover a topic, may Linux (or another allowed project) still be read? | **Delegated to Claude**: "If vendor fie already covers, then your choice why do you want to read, take decision appropriately" (2026-11-10). <br>**Claude's decision:** only for a named reason, recorded with the reference (§6): <br>• device quirks, errata workarounds or firmware behaviour the documents do not describe; <br>• a specification ambiguity, where interoperability depends on how deployed systems read it; <br>• research into what Linux supports; <br>• debugging a difference between Origin and Linux on the same hardware. <br>Never just to see how Linux structured something the documents already explain: Origin's design differs, so that adds copying risk and nothing else. Items 92, 93 and 96 still apply. In CLAUDE.md §3 |

## ADR-0004 H2: the heap, the stacks and the probe page (Accepted 2026-11-10)

The independent review of S3-5a found that ADR-0004 H2's table puts the kernel heap and stacks at `0xFFFF_C000_0000_0000`, while the code keeps both in the direct map. Changing an accepted ADR is the owner's decision. The owner answered **"Accept all"** on 2026-11-10. These are ADR-0004 Amendment 1, A1–A3. They are numbered from 98 because items 71–97 are in use on the `docs/editions-device-sdk` branch, which has not been merged yet.

| # | Question | Answer |
|---|---|---|
| 98, answered | A1: amend H2 so the kernel heap stays in the direct map, as the code has it (no mapping work or shootdown when it is freed, and the large pages kept)? | **Accept** |
| 99, answered | A2: keep H2 for the stacks, and move them to `0xFFFF_C000_0000_0000` with guard pages in virtual space, as a task of their own that the framework session starts now? | **Accept** |
| 100, answered | A3: record PML4 slot 480 (`0xFFFF_F000_0000_0000`), the kernel-half proof's probe page, in H2's table? | **Accept** |

## S3-5b: memory objects' interface (Accepted 2026-11-10)

Task S3-5b (memory objects) needs five interface decisions that C6–C8 (items 61–63) leave open. They are ABI decisions, so they are the owner's (CLAUDE.md §84). The session building S3-5b proposed them, with the options it rejected, and the coordinating session checked them against CLAUDE.md, ADR-0004 and ADR-0005 before asking. The owner answered **"Accept all"** on 2026-11-10. They are ADR-0005 Amendment 5, D1–D5, all built by task S3-5b on 2026-11-10 (`origin.kernel` 0.5, `user-abi` 0.5). Items 71–97 are in use on the unmerged `docs/editions-device-sdk` branch.

| # | Question | Answer |
|---|---|---|
| 101, answered | D1: creation calls take their arguments in a sized request, so `memory_object_create(request: object_request { length })`, and a creation call with no argument yet takes none? | **Accept** |
| 102, answered | D2: `map_request` holds the access (read, write, execute: read required, never write with execute, execute refused for now), a `fixed` flag (otherwise a random address), and the offset, length and address in bytes, in whole 4 KiB pages, never rounded? | **Accept** |
| 103, answered | D3: `memory_unmap` and `memory_protect` name the process, the mapping's start and its length; in v1 exactly one whole mapping; protect only reduces access? | **Accept** |
| 104, answered | D4: quotas set by the creator: committed bytes, mappings, and a rate for unmap and protect; `limit` past a quota, `no_memory` without frames, `should_wait` past the rate with nothing changed? | **Accept** |
| 105, answered | D5: the rest: `write` on the process handle for every call, the caller's own included; existing statuses only, in a fixed order; ADR-0004 H1's addresses and the start records' random region; a mapping holds its object; two new constants; `user-abi` 0.5 in one step? | **Accept** |

Items 71–97 and 98–105 were written on two branches that had not met. They are in one sheet since the records branch merged them on 2026-11-10 (item 106).

## The design review of 2026-11-10 (items 106–123): Accepted 2026-11-10, recorded, almost nothing built

On 2026-11-10 you asked for a very careful review of CLAUDE.md and the design documents: "Extreme highest security, performance, design, native SDK no calling of shell commands from GUI or UI. Check very carefully each and everything."

The review is `docs/engineering/DESIGN_REVIEW_2026-11-10.md`. It read the documents at `land/s3-part2` (`02d9ba8`) and `docs/editions-device-sdk` (`a95d05b`), checked each of its 19 findings against the code, and put 18 decisions to you. Each had a recommended answer, checked against CLAUDE.md 7.3 and every accepted ADR. You answered **"Accept all"** in the same session.

Five of them change something you had already accepted. The review said so before you answered:

- item 106 changes item 70;
- item 109 changes ADR-0005 P1;
- item 113 changes one row of ADR-0013's table;
- item 114's second step would change ADR-0012 C2, and only if the experiment supports it;
- item 117 rewords item 74.

| # | Question | Answer | Where it is recorded, and its state |
|---|---|---|---|
| 106, answered | Commit and push the editions branch, and land directive, ADR and decision-sheet changes on `main` in pull requests of their own, outside item 70's 100-commit rule (finding 1)? | **Accept** | CLAUDE.md §79. **Done so far:** the editions branch is committed and pushed, and merged with the landing line on the records branch. Reaching `main` waits for your word on that pull request |
| 107, answered | Drop the commit that holds Intel's VT-d PDF, keep the gem5 ledger, and add an ignore rule and a gate (finding 2)? | **Accept** | `.gitignore`. **Done except the gate:** the commit is on no branch and was never pushed, the ledger is on the records branch, and the PDF is a local file again |
| 108, answered | Make "no first-party component runs a command" structural: starting a program is a capability; a release gate over first-party manifests; no command interpreter in the base image; an empty environment by default; a run-time test; no interface that takes text to execute (finding 3)? | **Accept** | CLAUDE.md §48; ADR-0011 Amendment 1; ADR-0005 Amendment 6, E2. Not built. The capability's name is decided with the Loader Service's interface (§84) |
| 109, answered | 64-bit handles, while `user-abi` is at major 0 (finding 4)? | **Accept** | ADR-0005 Amendment 6, E1. Not built |
| 110, answered | Remove `preserves_flags` from the twelve assembly blocks and add a gate (finding 5)? | **Accept** | CLAUDE.md §13. Not built |
| 111, answered | An ADR on what lives in the kernel; move the modules that are not the kernel's; fix which files may hold `unsafe` (finding 6)? | **Accept** | CLAUDE.md §13. The ADR is to be written |
| 112, answered | A system threat model, and an ADR on speculation and processor hardening, both before ADR-0011 N14 step 4 (finding 7)? | **Accept** | CLAUDE.md §18, §24, §58. Both are to be written |
| 113, answered | A kernel random-number generator (HMAC_DRBG over SHA-512) before S3-5b lands, with the entropy proof moved to Withdraw (finding 8)? | **Accept** | CLAUDE.md §17, §59; ADR-0013 Amendment 1; ADR-0004 H3's note. Not built: the row moves with the generator |
| 114, answered | Run the fenced-read experiment on the clock now, and decide the fast path from its result (finding 9)? | **Accept** | ADR-0012's note. C1 and C2 stand until the result is in and you decide |
| 115, answered | Provisional performance budgets, recorded from CI as trends (finding 10)? | **Accept** | CLAUDE.md §53. The file is to be written |
| 116, answered | Security rules for the GUI (finding 11)? | **Accept** | CLAUDE.md §52 |
| 117, answered | Reword "GUI only" as "the GUI is always enough, and nothing needs a terminal", with declarative provisioning and a process for each remote session (finding 12)? | **Accept** | CLAUDE.md §48; ADR-0014 Amendment 1 |
| 118, answered | ADRs for locking and for the rest of the cryptographic profile; the trade-offs and migration sections missing from ADR-0002 to ADR-0005 (finding 13)? | **Accept** | CLAUDE.md §59. To be written |
| 119, answered | ADR-0002 Amendment 2: measured boot, the rollback floor, firmware runtime calls (finding 14)? | **Accept** | ADR-0004 H3's note. To be written, with the SYS profile |
| 120, answered | Make the four statements the build makes true, or reword them (finding 15)? | **Accept** | Not built |
| 121, answered | Three more rules for reference code, with a person's sign-off on the first driver written that way (finding 16)? | **Accept** | CLAUDE.md §3; ADR-0015 D6 |
| 122, answered | A checked provenance table, task contracts from S3-5b on, and exit tests for each milestone (finding 17)? | **Accept** | CLAUDE.md §6, §82. Not built |
| 123, answered | Overflow checks in the kernel and loader; x86-64-v3's integer features once the loader checks the baseline; `verify.sh` and `--locked` in CI; a wider §11 lint (finding 18)? | **Accept** | CLAUDE.md §14. Not built |

Finding 19's smaller points were not put as decisions. They stay in the review as tidying.

The work these create is in `TASKS.md`, items 17 to 30.

**Two later answers changed this revision's wording, on the same day.** You accepted the six questions of ADR-0016, the kernel's random-number generator, as items 146 to 151. They are recorded with that ADR, in their own section below. Two of them are in draft 7.4:

- item 149: §59 says the health tests are on what each source returns, catch gross failure only, and claim no SP 800-90B conformance;
- item 151: §17 names SP 800-90A Rev. 1, SP 800-90B (January 2018), SP 800-90C (September 2025) and FIPS 198-1.

**Yours to do:**

- Read revision 7.4's wording and say whether the records pull request may merge (item 106). CLAUDE.md is your text, and merging takes your word.
- Confirm ADR-0009's text, which you delegated and have not reviewed, and ADR-0007's two additions marked "for the owner's review" (item 122).
- Name the person who signs off the first driver written from reference reading, when one exists (item 121).

## Speculation and processor hardening (ADR-0017), and what the system threat model accepts (items 124–141): Accepted 2026-11-10, nothing built

Item 112 asked for a system threat model and an ADR on speculation and processor hardening, both before ADR-0011 N14 step 4. TASKS item 19a wrote them on 2026-11-10:

- `docs/engineering/THREAT_MODEL_SYSTEM.md`;
- `docs/adr/ADR-0017-speculation-processor-hardening.md`;
- the ledger they rest on, `docs/research/2026-11-10-speculation-processor-hardening.md`, from Intel's and AMD's own documents read that day.

No processor in scope was run, and no cost is measured. Each recommendation was checked against CLAUDE.md 7.4 and the accepted ADRs before it was put. You answered **"Accept all"** in that session, to one question that covered both lists below.

Three of the answers reach past ADR-0017. The ADR said so before you answered:

- item 127 has ADR-0003 K8 and ADR-0005 P4 amended when the FRED entry path is built;
- item 131 adds a Report row to ADR-0013's table when the proof exists;
- item 129 uses a compiler feature that is not nightly and not supported either, which CLAUDE.md §12 does not foresee.

**ADR-0017's nine questions.**

| # | Question | Answer |
|---|---|---|
| 124, answered | Z1 to Z12 as the policy, built as TASKS item 19b with its own contract? | **Accept** |
| 125, answered | An IBPB at every change of process, until trust classes exist and the cost is measured (Z3)? IBPB only between different principals was offered and not chosen. | **Accept** |
| 126, answered | SMT on, with branch isolation between sibling threads, and the remaining timing channels accepted by name (Z5; item 137)? SMT off by default on Origin Server was offered and not chosen. | **Accept** |
| 127, answered | FRED wherever every processor reports it, with the IDT path kept for the rest, AMD's Zen 5 among them (Z6); ADR-0003 K8 and ADR-0005 P4 amended to say so when it is built? | **Accept** |
| 128, answered | On AMD, FRED waits for AMD's own text or for proofs on a machine that has it (Z6)? | **Accept** |
| 129, answered | An INT3 after every RET, through a target feature the stable compiler accepts with a warning, with a gate that reads the kernel's image (Z10)? Hardening only the hand-written assembly was offered and not chosen. | **Accept** |
| 130, answered | Supervisor shadow stacks under FRED first, and on the IDT path only on evidence (Z8)? | **Accept** |
| 131, answered | A processor that lacks a control boots and the boot says so, as a Report row in ADR-0013's table (Z1); whether the production installer refuses such a machine is decided with item 90 at M9? | **Accept** |
| 132, answered | Index masking on AMD and on Intel family 6, LFENCE on Intel family 18 and above and on anything unknown (Z9)? LFENCE on every Intel processor was offered and not chosen. | **Accept** |

**The system threat model's nine acceptances.** Each says what the product does not promise.

| # | What is accepted | Answer |
|---|---|---|
| 133, answered | AC1: the platform firmware, system management mode and the processor's microcode are trusted; so is the hypervisor under an Origin guest, and a server's baseboard controller | **Accept** |
| 134, answered | AC2: while a machine is locked or asleep its secrets are in memory, and someone who can read memory with equipment gets them | **Accept for now.** Decided again in the storage-encryption ADR |
| 135, answered | AC3: tampering with tools: implants, debug ports, bus probes, chips rewritten off the board, and anything measured at the machine | **Accept** |
| 136, answered | AC4: a program that induces faults in memory hardware | **Accept.** Revisited when the list of declared machines exists |
| 137, answered | AC5: timing and contention channels between programs, the two threads of one core included | **Accept for programs in general.** First-party code that handles secrets must be constant-time, and that is not waived |
| 138, answered | AC6: on a processor that lacks the controls ADR-0017 needs, programs are not protected from each other's speculation | **Accept, for bench machines only** |
| 139, answered | AC7: an administrator of an Origin Server can read whatever the system can read | **Accept,** with attribution and an audit record as the defence |
| 140, answered | AC8: whoever operates the host of an Origin Server guest, or the building a server stands in, can read and change everything | **Accept** |
| 141, answered | AC9: a compromised update server can withhold updates until the signed metadata it serves has expired | **Accept** |

Nothing is built. The work is TASKS item 19b, which writes its contract first (item 122). ADR-0011 N14 step 4 still waits for Z2, Z3, Z5, Z9 and Z10.

**Left for the next revision of CLAUDE.md, which is your text:** §14 and §24 do not yet name ADR-0017.

**Coming back to you:** item 134, with the storage-encryption ADR; and the trust classes that would let item 125 cost less, with measurements.

## S3-5b: the status when a map runs out (Accepted 2026-11-10)

The review of S3-5b found that `memory_map` returns `limit` for three causes that D4 (item 104) does not tell apart (`REVIEW_S3-5B_2026-11-10.md`, finding C). What a status means is an ABI decision, so it is the owner's (CLAUDE.md §84). The session that built S3-5b put it to the owner in its own session, with three recommendations about the order of work, and the owner answered **"Accept all"** there on 2026-11-10. That session relayed the answer; the coordinating session read that session's record of it, then numbered and recorded the decision here. It is a note on D4 in ADR-0005 Amendment 5. Not built yet. Items 106–123 are in use on the unmerged `docs/design-review-2026-11-10` branch, and items 124–141 on the unmerged `claude/intelligent-dhawan-a7f6d4` branch (ADR-0017).

| # | Question | Answer |
|---|---|---|
| 142, answered | When a map runs out: `limit` when the address space's 32 page-table frames are used up, and `no_memory` when the machine has no free frame or the kernel heap cannot grow? `memory_map` already declares both, so `user-abi` stays 0.5. | **Accept** |

## H2-stacks: ADR-0004's wording, and a slide for the stack region (Accepted 2026-11-10)

Task H2-stacks built ADR-0004 Amendment 1's A2 on its own branch and left two things to the owner: the ADR's wording for what it built, and whether the stack region is slid. The coordinating session put both to the owner with a recommendation each, and the owner answered **"Accept all"** on 2026-11-10. Item 143 changes no decision; its five edits are applied when H2-stacks lands on the step-3 batch. Item 144 is ADR-0004 Amendment 2 (A4–A6). Not built yet.

| # | Question | Answer |
|---|---|---|
| 143, answered | Update ADR-0004 to say what H2-stacks built, when it lands: A2 is built; the region is 2048 slots of 128 KiB (256 MiB), each stack at the top of its slot with at least 64 KiB below it never mapped; the boot processor's interrupt stacks stay in the kernel image and the boot stack is the loader's? | **Accept** |
| 144, answered | Give the kernel stack region a random base: drawn by the kernel when it builds its tables, 2 MiB-aligned within PML4 slots 384–447 (about 24 bits), from RDSEED, else RDRAND, and with neither the fixed base, reported; never printed after boot; built as a framework task of its own after H2-stacks lands? | **Accept** |

## The kernel's random-number generator (ADR-0016): Accepted 2026-11-10; built by tasks 18a and 18b, under QEMU only

Decision item 113 accepted one generator in outline. [ADR-0016](../adr/ADR-0016-kernel-random-numbers.md) filled the outline in and ended with seven questions, each with a recommended answer checked against CLAUDE.md 7.4, the accepted ADRs and NIST's SP 800-90A, 90B and 90C. The seventh, a download, is item 145 above. You answered **"Accept all"** to the other six on 2026-11-10, in the session that wrote the ADR. The coordinating session put the same six to you the same day, in shorter words, and records the same answer (its commit `44de977`).

Three of them change something you had already accepted. The ADR said so before you answered:

- item 147 changes ADR-0002 B4: boot info goes to 0.6;
- items 147 and 148 change ADR-0004 H3: the loader's slides come from a generator of its own, as two requests, and the firmware's generator is never a source by itself;
- item 146 carries out ADR-0013 Amendment 1, which you accepted with item 113: one row of its table moves when the generator lands.

| # | Question | Answer | Where it is recorded, and its state |
|---|---|---|---|
| 146, answered | The design (R1 to R9, R11 to R13): one HMAC_DRBG over SHA-512; a root generator and one for each processor, in the shape of SP 800-90C's tree of generators with two named differences; health tests on the instructions' outputs; no generator and no randomness without a healthy seed; overwriting in three layers; a new crate for the policy? | **Accept** | ADR-0016. The mechanism is built and tested on the host. Everything else is task 18b |
| 147, answered | Boot info 0.6 (§84): one appended field, `boot_seed`, the physical address of 64 secret bytes inside the boot-info region or 0, which the kernel overwrites once it has taken them? | **Accept** | ADR-0016 R10; ADR-0002 B4. Built by task 18b (2026-11-11). Item 166 below makes the secret area 72 bytes |
| 148, answered | What may seed, and what happens when the sources fail? RDRAND with no RDSEED: the generator runs, the boot says so, and requests for prediction resistance are refused. The firmware's seed with neither instruction: no generator. Every instruction failing after boot: the root stops and the processors' generators go on with the seeds they have. | **Accept, as stated.** The stricter alternative to the third part, stopping all randomness, was offered and not chosen | ADR-0016 R3, R6, R8; ADR-0004 H3's note. Not built |
| 149, answered | What "health tests on the raw source" (CLAUDE.md §59) can mean: tests on the instructions' outputs, because the noise behind them cannot be reached; gross failure only; no SP 800-90B conformance claimed? | **Accept** | ADR-0016 R4. Not built. Draft 7.4's §59 says so since `44de977` on `docs/design-review-2026-11-10` |
| 150, answered | Keep `unsafe` out of `origin-crypto`: best-effort overwriting there, and the guarantee in the framework, by volatile writes over the state and an overwrite of the stack after every request? | **Accept** | ADR-0016 R9. The best-effort part is built; the framework's part is 18b |
| 151, answered | CLAUDE.md §17: pin SP 800-90A to Rev. 1 (June 2015) and SP 800-90B to January 2018, and add SP 800-90C (September 2025) and FIPS 198-1 (July 2008)? | **Accept** | ADR-0016 R1. CLAUDE.md is not changed on this branch. Draft 7.4 on `docs/design-review-2026-11-10` names them in §17 since `44de977`; it reaches `main` on your word (item 106) |

**Yours to do:** nothing new. The wording of §17 and §59 is in draft 7.4, which you read with its pull request (item 106).

The numbers start at 146 because 142 to 144 are in use on `land/s3-part2` and 145 is the download above. The header of this sheet was brought up to date on 2026-11-11, when the branches that hold items 124 to 154 had met in the review batch.

### Where the kernel stack region's base gets its randomness (item 166): Accepted 2026-11-11

Building the generator found that the next batch draws the base of the kernel stacks' region in early boot, when the kernel builds its page tables: before the heap and the clock, and so before the kernel's generator can exist. That draw could not simply ask the generator. The coordinating session put three recommendations to you on 2026-11-11 and you answered **"Accept all"**; it relayed your words. This is the second of them. The third, how task 18b lands (item 167, through pull request 16), is recorded by that batch's lander.

It changes two things you accepted on 2026-11-10, and the recommendation said so: ADR-0004 Amendment 2's "the kernel chooses this slide, and boot info does not change", and the 64-byte size of boot info 0.6's secret area (item 147). Boot info 0.6 has not landed, so there is no second version bump.

| # | Question | Answer | Where it is recorded, and its state |
|---|---|---|---|
| 166, answered | Where does the stack region's base get its randomness? Recommended: the loader's generator makes one more 8-byte request and puts it directly after its 64-byte seed; the kernel reads it when it builds its page tables and overwrites it with the seed. The kernel still places the region; only the random value comes from the loader. With no loader generator the region stays at its fixed base | **Accept** | ADR-0016 Amendment 1; ADR-0002 B4. On the generator's branch: the loader's third request, the 72-byte area and its checks, `origin_frame::random::take_stack_value`, and a boot proof that it is taken once and overwritten. The stack region is on the next batch, whose lander makes `draw_region` the caller and notes ADR-0004 Amendment 2 (A4, A5) **Done on 2026-11-11:** the batch's merge of 18b (`c2245ef`) makes `draw_region` the caller, and ADR-0004 Amendment 2 has its note. |

**Offered and not chosen:**

- derive the value from the seed with a key-derivation function: no boot-info change, but the seed gets a second use and the directive would need a new standard;
- start the kernel's generator before the heap and the clock: that moves most of its machinery into early boot;
- place the region after the generator starts: the generator's own stacks live in that region.

**Yours to do:** nothing.

### What a dry RDSEED costs a processor with no healthy RDRAND (item 188): Accepted 2026-11-11

An independent read of the generator's policy crate found that one row of ADR-0016's threat model said too little. T5 is a program in ring 3 that runs RDSEED in a loop and keeps it dry. The row said reseeds are then "delayed and fall back to RDRAND". Where RDRAND is not healthy on a processor there is nothing to fall back to, and that processor's reseed waits for as long as RDSEED is kept dry. That is how R6 was accepted and how it is built; only the row was short. The session that built the generator put it to you on 2026-11-11 with a recommendation, and you answered **"Yes, amend T5"**.

The number is 188 because 168 is in use on `docs/directive-7.5` and 169 to 187 were given out that morning for the events-and-ports shapes.

| # | Question | Answer | Where it is recorded, and its state |
|---|---|---|---|
| 188, answered | Amend T5's row in ADR-0016 to say that where RDRAND is not healthy on a processor, its reseed waits for as long as RDSEED is kept dry, and that T7's bound of 60 seconds or 2^20 requests does not hold for that processor meanwhile? Recommended: yes, and build nothing | **Yes, amend T5** | ADR-0016 Amendment 2, and T5's row. Nothing is built and nothing changes in R6 |

**Offered and not chosen:** send such a processor to the root once its reseed has been delayed for some periods. That changes R6, and on a machine whose processors are alike it is a reseed in name only: the root's own reseed needs the same instruction.

**Yours to do:** nothing. The limit is stated, not closed. How long a program can keep RDSEED dry on a real processor is unmeasured.

## What building ADR-0017's first half found (items 152 to 154): Accepted 2026-11-10

TASKS item 19b built Z1, Z2, Z3, Z5, Z9 and Z10 of ADR-0017, and its gate on the kernel's image found two things the ADR had not foreseen. Both were put to you in the coordinating session, with six of ADR-0016's (items 146 to 151, recorded by the session that wrote that ADR). You answered **"Accept all"** to one question covering the eight. The coordinating session relayed the answer, and the words each was put in, to the session that built 19b, which records the two here. Items 142 to 151 are on other branches.

| # | Question | Answer |
|---|---|---|
| 152, answered | The toolchain's prebuilt `core`, `alloc` and `compiler_builtins` are outside ADR-0017 Z10's "an INT3 after every RET" and "no jump table": stable Rust cannot rebuild them, and in the kernel of that day they hold 113 returns, 33 without an INT3, and 2 jump tables. Accept the gap by name, with the count printed on every run and looked at again at each toolchain upgrade, as ADR-0017 Amendment 1? A pinned nightly compiler under §12 was offered and not chosen. | **Accept** |
| 153, answered | More than half the kernel's calls go through the global offset table, because the compiler emits a relocation the linker cannot turn into a direct call. Make it a task of its own, not a condition of ADR-0011 N14 step 4: find a fix on stable Rust (link-time optimization is the candidate, untried), measure before and after, then have the image gate hold the count? | **Accept.** TASKS item 33; not started |
| 154, answered | Under QEMU's software processor every boot now ends `degraded: 58 of 59 proofs passed; failed: speculation controls`, because that processor offers none of the controls and the "speculation controls" row fails on a processor that lacks one (item 131). Keep it as written? A row that passes when the kernel set everything the machine offers, and only prints what is lacking, was offered and not chosen. | **Keep as written.** Answered twice the same way: in the session that built 19b, with the boot output beside it, and as one of three recommendations accepted in the coordinating session. No code changes: the verdict already behaves this way |

## S3-5b: two statuses D5 does not name (Accepted 2026-11-10)

The review of S3-5b's low observations left two cases whose status D5 (item 105) does not name (`REVIEW_S3-5B_2026-11-10.md`). What a status means is the owner's (CLAUDE.md §84). The coordinating session put both to the owner with a recommendation each, and the owner answered **"Accept all"** on 2026-11-10. They are a note on D5 in ADR-0005 Amendment 5. Item 155 is not built yet; item 156 is what the kernel does. Items 145–154 are in use on unmerged branches (ADR-0016, ADR-0017, the design review).

| # | Question | Answer |
|---|---|---|
| 155, answered | `memory_unmap` or `memory_protect` on a mapping another thread is changing: return `should_wait`, since nothing changed and a retry gives the true answer, instead of today's `invalid_args`? | **Accept** |
| 156, answered | A random placement whose eight draws all collide: keep `limit` and write it into D5, with `no_memory` staying for frames and the kernel heap? | **Accept** |

## The stack region's slide: what A6 leaves open (Accepted 2026-11-10)

The framework session building ADR-0004 Amendment 2 (item 144) found four cases its no-printing rule, A6, does not settle, and two more boot lines that fall under them (`sched.rs`'s stack-guard line, under 159; `kernel.rs`'s NMI stack line, under 158). The coordinating session put them to the owner with a recommendation each, and the owner answered **"Accept all"** on 2026-11-10. They are notes A6.1–A6.4 in ADR-0004 Amendment 2. Not built yet.

| # | Question | Answer |
|---|---|---|
| 157, answered | A6 allows printing in developer mode and the kernel has none: add none now, so A6 means never until one exists for its own reasons? | **Accept** |
| 158, answered | Crash reports: print offsets in every mode (`stack region +0x…`, `image +0x…`, a backtrace as image offsets, no base), the image part included, which brings it under H3's rule? | **Accept** |
| 159, answered | The boot log: print neither the stack region's base nor an address in it, during boot either? | **Accept** |
| 160, answered | Showing that boots differ with nothing printed: required, a host test of the draw, a true-or-false boot line, a forced no-entropy boot and controls; in addition a read through QEMU's monitor across the matrix, dropped and recorded as not done if it is unreliable on CI? | **Accept** |

## The review batch may be offered at 76 commits (item 161): Accepted 2026-11-11

Item 70 offers a code pull request for review once it carries 100 commits of real work. Pull request 15, the review batch, holds four finished tasks (TASKS items 17, 18a, 19 and 31) and one fix to a boot proof, in 76 commits that are not merges. On 2026-11-11 you asked that the open pull requests be ready to merge when they are ready, and were asked whether this one may be marked ready as it is.

| # | Question | Answer |
|---|---|---|
| 161, answered | Mark pull request 15 ready at 76 commits, once pull requests 13 and 14 are in? Waiting for task 18b, the generator in the kernel, to bring it past 100 was offered and not chosen. | **Yes, at 76** |

This is for pull request 15 only. Item 70 stands for every later code pull request, and no commit was added to raise the count (CLAUDE.md §7). Merging it still takes your word, after pull requests 13 and 14, each with a merge commit.

## S3-5b: three cases D5 does not place (Accepted 2026-11-11)

The review of S3-5b's fixes left three cases that D5 (item 105) does not place (`REVIEW_S3-5B_2026-11-10.md`, step 2). What a status means is the owner's (CLAUDE.md §84). The coordinating session put them to the owner with one recommendation for each, to keep what is built, and the owner answered **"Accept all"** on 2026-11-11. They are the second note on D5 in ADR-0005 Amendment 5. All three are what the kernel does since `f56263a`.

They were put to the owner as items 161 to 163. Item 161 was already in use on the unmerged review batch (`land/review-tasks`, pull request 15: marking that pull request ready at 76 commits), so they are recorded here as 162 to 164. The questions and the answers are unchanged.

| # | Question | Answer |
|---|---|---|
| 162, answered | `memory_map` at a fixed place that runs over a page already mapped, in an address space that is also at its bound of page tables: keep `invalid_args` (the place is taken), since the space looks at its records before it makes tables, and not `limit`? | **Accept** |
| 163, answered | `memory_protect` on a mapping another call is changing: keep `should_wait` ahead of the comparison of the access asked with what the mapping allows, since that comparison is not settled while the mapping changes? | **Accept** |
| 164, answered | Keep counting as "being changed" (item 155) a mapping whose own `memory_map` has not yet written its address to its caller, so that `memory_unmap` or `memory_protect` on it returns `should_wait`? | **Accept** |

## The roles in CLAUDE.md §1 (items 165 and 168): your text, and what you left to Claude, 2026-11-11

On 2026-11-11 you gave text for the start of CLAUDE.md §1, "Your mission", three times:

- first with the words "update CLAUDE.md with following at line: 35": three lead roles and 28 specialist roles;
- then with the words "Update CLAUDE.md at line: 35 with following, check carefully, if anything missing": six lead roles, 50 specialist roles and two closing paragraphs;
- then, after the check of the second, with the words "Consider following, and improve if you can. Consider extreme security, performance, Native SDK, Native Desktop, Native GUI or UI, CPU, GPU, NPU consider these also. You can do extremely very good, You decide.": seven opening paragraphs, 64 specialist roles and 22 engineering requirements that apply to every role.

Each replaced the one before it, and none had reached `main`. Revision 7.5 holds the third, with the changes of item 168.

| # | What | Where |
|---|---|---|
| 165, your text | The sentence that opened §1 ("You are the Principal Operating System Architect, ... and Multi-Agent Engineering Coordinator for Origin OS.") is replaced by your text, in its third version | CLAUDE.md 7.5, §1. Revision 7.4 is kept at `docs/engineering/CLAUDE-v7.4.md` |
| 168, delegated to Claude ("improve if you can ... You decide") | Your third text is improved where it could be: 47 changes, among them three new roles and four new requirements. **You have not read them. You can change any of them** | CLAUDE.md 7.5, §1. Every change, with the words before and after and the reason: `docs/engineering/ROLES_CHECK_2026-11-11.md`, its last section |

What your third text keeps and adds:

- **The completion target** is still 100% coverage of approved requirements. It is now measured against a versioned requirements matrix. That matrix does not exist yet: TASKS item 35.
- **Roles 1 to 50 keep their numbers.** All but two are reworded. Roles 51 to 64 are new: a Chief Systems Architect, system integration, resource governance, platform reliability, and ten roles for driver families, with the GPU and the NPU among them.
- **Requirements for every role** replace the two closing paragraphs.
- **It answers most of the check of the second text.** What became of each of the twelve proposals is in the check's last section. The last one, §77's list of 26 engineering agents, you answered later the same day (item 189).

What item 168 changed, by the areas you named:

- **Security.**
  - A role for the compositor as a security boundary between programs (65).
  - Role 16 is the Chief Security Architect, and accepts no change that weakens a security invariant for performance or convenience.
  - Two requirements: deny by default, and the owner's decisions stay the owner's.
  - Named in the roles that own them: speculation controls on every processor (4); `unsafe` confined to the listed files (3); third-party drivers in user mode only (9); the firewall, TLS and VPN (12); constant time and zeroization (17); verify before parse (18); sessions and elevation (21); no install-time code (22); Secure Boot, the TPM and measured boot (30).
- **Performance.** Role 23 is the Chief Performance Architect, and performance budgets are routed through it. The NPU joins role 53's resource policies.
- **Native SDK.** A requirement states the SDK-only rule in full, and role 19 enforces it. Role 19 defines each interface once in the interface language, with a stable C ABI. Role 18 owns the runtime library and the route to Rust `std`.
- **Native desktop and GUI.**
  - Role 20 owns the rest of the UI toolkit and allows no web view, browser engine or foreign toolkit.
  - A role for the text engine, fonts and input methods (66).
  - "The desktop shell", search and notifications in role 49.
  - A management GUI that covers every administrative operation, remote administration and remote desktop in role 50.
- **CPU, GPU, NPU.**
  - A requirement to discover capabilities and report them truthfully.
  - A fallback display path, and no claim of acceleration without a working driver (55).
  - No client can read memory another client left behind.
- **Left without an owner by the directive's own sections.** The boot manager (7); ACPI runtime evaluation (8); continuous integration and its gates (25); milestone exit tests (45); original source, provenance and licensing (67).
- **One merge.** Your first requirement and your sixth opening paragraph said the same thing. They are one sentence now, with every element of both.
- **Form.** Role titles and requirement headings are bold, and the apostrophe is the plain one.

How the text was put in:

- **Your third text was taken from your message** and compared with the second, role by role. The changes were then applied by a script, each exactly once, so the list of 47 is the whole difference in words.
- **Line 35 is where the old sentence stands in revision 7.1,** which the main checkout still held that day. In revision 7.4 it was line 36. Your text replaces that sentence; the nine roles it named are all among your roles.
- **Nothing else in the directive changes.** The rest of §1 follows your text as before.
- **The first and second versions are kept only in this branch's history,** at commits `fc92eca` and `453fded`.

## Task 18b lands through the second step-3 batch (item 167): Accepted 2026-11-11

Item 113 asks for the kernel's random-number generator before S3-5b lands. S3-5b is in the second step-3 batch (pull request 16), and that batch still draws straight from RDSEED and RDRAND in two places: the random place of a mapping (`memory_object.rs`) and the base of the kernel stacks' region (the framework's `stack.rs`). Task 18b, the generator in the kernel and the loader, was built on a branch of its own from `main` (`claude/18b-kernel-random`).

On 2026-11-11 the architecture-review session put three recommendations to the owner, and the owner answered **"Accept all"**. Two of them concern this batch; the third was about pull request 17. The session that keeps this batch read the questions and the answer in that session's record before writing this.

| # | Question | Answer |
|---|---|---|
| 167, answered | How 18b lands: through the next batch, pull request 16, and not a pull request of its own? That batch cannot be offered until the generator is in it (item 113), and the two direct draws it adds are converted there. Before it is offered, the architecture-review session finishes reading the parts of 18b it has not read. | **Accept** |

- **Item 166** was accepted with it: the stack region's base takes its random value from the loader's generator, 8 bytes directly after the 64-byte seed in boot info 0.6's secret area. The session that builds 18b records it on `claude/18b-kernel-random`, and it reaches this sheet when that branch is merged here. What it changes in ADR-0004 Amendment 2 is the note on A4 and A5 there.
- **Item 165** is the owner's own text for CLAUDE.md §1 (revision 7.5). It came to `main` with pull request 17, and its record is the section above.
- **Done on 2026-11-11:** 18b (`a11202a`) is merged into this batch at `c2245ef`, and the merge moves both draws to the generator. Checked under QEMU only; no person has read it.
- Merging pull request 16 still takes your word, and item 70's 100 commits stand.

## §77 points to §1 (item 189): Accepted 2026-11-11, revision 7.6

| # | Question | Answer |
|---|---|---|
| 189, answered | Replace §77's list of 26 agents with a pointer to §1? This was proposal 11 of the roles check (`ROLES_CHECK_2026-11-11.md`). | **Yes: "Yes, point to §1"** |

What it changes, in CLAUDE.md 7.6:

- **§77 now reads:** "The specialist roles and their responsibilities are in §1. Agents are given bounded tasks from them, as §1 and §78 say."
- **Its last line stays:** "Do not execute every agent simultaneously without task independence."
- **Nothing else changes.** Revision 7.5 is kept at `docs/engineering/CLAUDE-v7.5.md`.

Why: §1 has listed 67 specialist roles since revision 7.5, and §77's 26 agents overlapped them with another count. Every one of the 26 has a role in §1: the NPU is role 56, source provenance is role 67, and context management is role 28 with the requirement to keep durable engineering records.

## The randomized model lands with 18b (item 190): Accepted 2026-11-11

A randomized model of the generator's policy was built as a host test after 18b's final commit (TASKS item 37 (c)), and the architecture-review session put one question to the owner. The owner answered that session on 2026-11-11: "Yes, with 18b". The landing session did not hear the answer; it read it in that session's transcript.

| # | Question | Answer |
|---|---|---|
| 190, answered | Does the randomized model of the generator's policy (TASKS item 37 (c), `claude/18b-random-model` at `dabe70b`, nine commits on 18b's final commit) land with 18b in pull request 16, or later on its own? | **With 18b: "Yes, with 18b"** |

- **Done on 2026-11-11:** merged into the second step-3 batch at `fe457f2`, right after 18b's merge (`c2245ef`).
- It adds a host test file, nine negative controls in `plant_faults.py` and its records. It changes no code of the generator.
- No person has read the model.
- Merging pull request 16 still takes your word.

## S3-6: what events, ports and `clock_read` leave open (items 169 to 187): Accepted 2026-11-11

ADR-0005 C9 (ports), C10 (events) and C13 (`clock_read`) were accepted on 2026-11-09. Task S3-6's research ledger, `docs/research/2026-11-11-events-ports.md`, found 19 points those three leave open. Each fixes what a program sees at the system-call boundary, which is the owner's (CLAUDE.md §84). The coordinating session checked every shape, put them to the owner as items 169 to 187 with one recommendation each and four marked "Look" (169, 178, 179, 187), and the owner answered **"Accept all"** on 2026-11-11. They are ADR-0005 Amendment 7, F1 to F19, in the same order; the full text of each, and what was rejected, is there and in the ledger.

| # | Question | Answer |
|---|---|---|
| 169, answered | Is `port_packet` a fixed 32-byte record of C9's six fields with no `size` field, as an exception to ADR-0005 P4 for this record? (**Look**) | **Accept** |
| 170, answered | Does `object_wait_async(port, object, key, signals)` need `write` on the port and `wait` on the object, take no options argument, and fail in D5's order? | **Accept** |
| 171, answered | Does `port_wait(port, deadline) -> packet` need `read`, use `wait_one`'s deadline, answer `timed_out` and `canceled` as `wait_one` does, and consume a packet when it is taken even if the copy then faults? | **Accept** |
| 172, answered | Does `port_cancel(port, object, key)` need only `write` on the port, remove every matching binding and queued packet silently, and answer `not_found` when nothing matched? | **Accept** |
| 173, answered | Is `object_signal` events-only and `signaled`-only in v1, one atomic `(old & !clear) | set`, with `wrong_type` for a non-event? | **Accept** |
| 174, answered | Are the default rights named `event_rights` and `port_rights`, with C6's rights? | **Accept** |
| 175, answered | Three new per-process count quotas, `events`, `ports` and `bindings`, with a binding charged to the process that bound it? | **Accept** |
| 176, answered | Does `wait_one` work on an event and answer `access_denied` on a port, with no signals on ports in v1? | **Accept** |
| 177, answered | May the same port, object and key be bound twice, giving two bindings and two packets? | **Accept** |
| 178, answered | Does `clock_read() -> nanos` read the monotonic clock only, take no argument, and have the one status `fault`? (**Look**: the task contract had said "no errors") | **Accept** |
| 179, answered | When a binder exits while another process holds the port, do its packets stay queued and stay charged to its account until read, cancelled or the port closes? (**Look**: the task contract had asked for no charge left at once) | **Accept** |
| 180, answered | Is a binding's storage refunded when its packet is read, when it is explicitly cancelled and when the port closes, and not on an implicit cancel? | **Accept** |
| 181, answered | Is the mask left unchecked against the object's type, as with `wait_one`? | **Accept** |
| 182, answered | Is `trigger` the mask asked, and `observed` all of the object's signals when the packet was queued? | **Accept** |
| 183, answered | Do packets leave in the order queued, with no bound per port beyond the binders' quotas? | **Accept** |
| 184, answered | Does an implicit cancel follow the handle the binding was made through, not the object? | **Accept** |
| 185, answered | Do `event_create()` and `port_create()` take no argument and answer `fault`, `limit` or `no_memory`? | **Accept** |
| 186, answered | Can any object `wait_one` can wait on be bound: channel ends, processes, threads and events? | **Accept** |
| 187, answered | Does S3-6 leave out packets a program queues itself and edge-triggered or repeating bindings, and leave the `Timer` object owed under P3 as TASKS item 36? (**Look**) | **Accept** |

- **Two of them correct the coordinating session's own task contract** (TASKS, contract S3-6): item 178 ("no errors" was a mistake: a call with a result has `fault`) and item 179 (an exited binder's charge can outlast it on a shared port).
- **Item 169 is an exception to an accepted sentence**, ADR-0005 P4's rule that every structure crossing the boundary begins with a size, for `port_packet` only.
- **Item 187 leaves P3's `Timer` object owed**, not dropped. TASKS item 36 tracks it.
- **Recorded with them, and no decision:** the call numbers (9, 10, 15 to 19), the change to two reserved calls' error sets, and `origin.kernel` and `user-abi` going from 0.5 to 0.6.
- **Nothing of this is built into the batch on the day of the answer.** No behaviour of the seven calls has run. Two pieces were built ahead under §84 and are not merged: the framework's protocol core (`claude/s3-6a-port-core`) and the count quotas of item 175 (`claude/s3-6-events-ports` at `b40d569`). Each lands only after the coordinating session has read it and run the gates on the merge.
- **Built since, in this batch:** the protocol core at merge `9889fcc`, a fix to the channel quotas at `d8e9d49`, and the seven calls with their quotas at `15baed9` (task S3-6 stage 2). The coordinating session read each against Amendment 7 and ran the check, the boot test and the boot matrix on the merge, under QEMU only. No person has read them.
- Merging pull request 16 still takes your word.
## The requirements matrix's shape (items 191 to 196): Accepted 2026-11-12

CLAUDE.md §1 makes 100% coverage of approved requirements the completion target, measured against a versioned requirements matrix (revision 7.5, item 165). TASKS item 35 builds it. Its session found six questions of shape that the directive leaves open, and the coordinating session put them to you on 2026-11-11 with one recommendation each, adding one rule to item 191. You answered **"Accept all 30"** on 2026-11-12, which covers these six and items 197 to 220 below.

| # | Question | Answer |
|---|---|---|
| 191, answered | What one row is: one row per statement of CLAUDE.md 7.6 (1,382) and per decision of an accepted ADR (221), 1,603 rows when counted on `main` at `405495c`. Decision-sheet items are cited by rows and are not rows. A statement that asks for nothing keeps a row marked as information, and is not counted. With the coordinating session's addition: every answered decision item is named by at least one row or listed as asking for nothing (a process rule such as item 70 or 190), and the gate checks it. | **Accept** |
| 192, answered | Where and in what form: Markdown tables under `docs/requirements/`, one file per PART of the directive and one per ADR, and a README with the rules, the directive's revision and the scope changes. The tables are the only copy. | **Accept** |
| 193, answered | `built` and `done`: `built` is implemented with evidence but short of the row's scope (QEMU only where the scope is hardware); `done` is acceptance criteria passing within the declared scope. Each row names where its evidence ran (host tests, emulation, hardware-assisted virtualization, physical devices), and the gate refuses `done` below the row's scope. The figure counts `done` only, with `built` beside it. | **Accept** |
| 194, answered | Evidence: references the gate finds in the tree (a file, a test by file and name, a boot proof by name, a `cargo xtask` command). A `built` or `done` row names at least one. Each row says who reviewed it (nobody, an AI reviewer, a named person), and the gate prints how many a person reviewed. | **Accept** |
| 195, answered | Acceptance criteria: a `not-started` row may lack them, and cannot leave that state without them, written by its task's §78 contract. | **Accept** |
| 196, answered | Scope changes: a table in the README, one line per approved change (item, date, what changed, rows touched). A removed row stays, marked `removed` with its item, and is not counted. The table starts with the changes already approved, among them the Server edition (items 71–76) and the hardware scope (item 88). | **Accept** |

- **What item 193 means today:** no kernel, loader or driver row can be `done`. Their evidence is QEMU's and CI's virtual machines, and their scope is hardware.
- **Item 191's addition is the coordinating session's.** Some answers are recorded only as a note, such as item 163 in ADR-0005's "Second note on D5", and a count of ADR rows alone would miss them.
- **Nothing is built.** TASKS item 35's session builds the matrix from `main`. Its gate will refuse the second step-3 batch's tree until rows exist for the batch's ADR decisions, and the coordinating session plans that order.

## N14 step 4: `origin.core` and the Diagnostics domain (items 197 to 220): Accepted 2026-11-12

ADR-0011 N14's step 4 is "`origin.core`, then one end-to-end service domain: Diagnostics first, because it is the smallest. Measure it (N8)." Its research ledger, `docs/research/2026-11-11-n14-step4-scope.md`, found 24 points that the accepted texts leave open. The coordinating session checked each against CLAUDE.md 7.6, ADR-0005 A6, ADR-0011 N8 and the system threat model, numbered them 197 to 220 (the ledger's candidate n is item 196 + n), and put them to you on 2026-11-11, six marked "Look". You answered **"Accept all 30"** on 2026-11-12.

| # | Question | Answer |
|---|---|---|
| 197, answered | Do step 4's programs reach ring 3 inside the kernel's image, laid out at build time and parsed by nothing at run time, until the Loader Service exists? (**Look**) | **Accept** |
| 198, answered | Is `origin.core` one stateless image per release, mapped read-execute into every process at a random place before its first thread, once, and never unmapped or changed? | **Accept** |
| 199, answered | Does the launch path bind each program's imports from `origin.core` from a table made at build time, read-only before the first thread? | **Accept** |
| 200, answered | Does `origin.core` hold only the generated stubs in step 4, with P8's helper deferred until a kernel fast path is designed with N8's figures? (**Look**) | **Accept** |
| 201, answered | Does `origin.core` export exactly OIDL's generated entry points, versioned by `user-abi`, with the psABI revision pinned before the first export? | **Accept** |
| 202, answered | Are the stubs naked functions, with a gate holding one `syscall` per number in `origin.core` and none in any other image the kernel carries? | **Accept** |
| 203, answered | Does the kernel know each call's offset from the build and record `origin.core`'s base when it maps it (Zircon's method), rather than take a table from a loader (OpenBSD's)? | **Accept** |
| 204, answered | Does a call from anywhere but its own stub stop the thread as a fault does, rather than answer `access_denied` as A6 does? (**Look**) | **Accept** |
| 205, answered | Do the kernel's own proof processes keep A6's range, while every other process is pinned per call? | **Accept** |
| 206, answered | Does O2 close for step 4 by absence, with the thread and process calls built in step 5 with their quotas? (**Look**) | **Accept** |
| 207, answered | Is the ledger's candidate 11 (one create-and-start call, a `threads` count, a reserve of run-queue slots for the kernel) where step 5 starts? | **Accept**, decided again with step 5 |
| 208, answered | Is the ledger's candidate 12 (creation and start behind a named capability; a change rate above 10^9 refused) where the Loader Service starts? | **Accept**, decided again with the Loader Service |
| 209, answered | Is the kernel-wide bound the worst-case figure of a process's limits, paid out of its creator's own, with the kernel's root sized at boot less a reserve? | **Accept** |
| 210, answered | No implicit default quotas: each kind given deliberately, step 4's limits explicit, and the Loader Service's defaults decided with its manifest? | **Accept** |
| 211, answered | Does a launcher fill a directory through a `setup` interface on its BOOTSTRAP, with `add`, `limit` for a full table and a new appended status for a key already held? | **Accept** |
| 212, answered | Does a provider receive connections as a one-way `accept(interface_id, major, server)` on a `provider` interface? | **Accept** |
| 213, answered | Does step 4 go ahead with I2 "Partly", with I2's closure moved to "before any claim about real hardware, and before any program that is not first-party runs in ring 3"? (**Look**) | **Accept**. Nothing is rented |
| 214, answered | Does ring 3's clock stay at full resolution, with K15 closed as accepted under AC5? | **Accept** |
| 215, answered | Is the `Timer` object left out of step 4, still owed under P3? | **Accept** |
| 216, answered | Does `origin.diagnostics` 0.1 serve the boot report (the verdict and each processor's facts, behind a capability), with the kernel log in a later minor? | **Accept** |
| 217, answered | Does the Diagnostics service read the report through one kernel call, gated by a kernel capability only it holds? | **Accept** |
| 218, answered | Does N8's measurement cover the round trip on one processor and on two, with p50 to p99.9, beside the ping-pong, a null call and `clock_read`, against a hypothesis written first? | **Accept** |
| 219, answered | Is N8's figure on declared hardware owed before any performance claim and before release qualification, with step 5 not waiting for it? (**Look**) | **Accept** |
| 220, answered | No call to drop a message: does a service close a connection whose next message it cannot take? | **Accept** |

Where they are written:

- **ADR-0011 Amendment 2, L1 to L12:** items 197–203, 205, 213, 216, 218 and 219. ONBF §8.3 gains a note for item 197, and the system threat model's "What must exist" and AC6 change with items 206 and 213.
- **ADR-0005 Amendment 8, G1 to G12:** items 204, 206–212, 214, 215, 217 and 220. It follows Amendment 7, which is only in the second step-3 batch, so it is written there and reaches `main` with that batch. So do the system-call threat model's rows E8, O2, D11, K5 and K15 and its list "What must be closed", whose K rows are only there.

What the six marked "Look" change in accepted text:

- **Item 197:** ONBF §8.3 gives mapping to the Loader Service; until it exists, the kernel carries step 4's first-party programs. CLAUDE.md §11's sentence that `onbf-pack` "converts the linker output to `.ose`" is your text and is not changed; under item 197 that conversion waits for M6. Whether §11 should say so is yours to ask for.
- **Item 200:** ADR-0005 P8's helper in `origin.core` waits for a fast path designed with N8's figures.
- **Item 204:** for every process pinned per call, a call from outside its stub stops the thread, where A6 answered `access_denied`.
- **Item 206:** O2 closes for step 4 by absence, where the system-call model said it closes "with the calls that create them".
- **Item 213:** I2 stays "Partly". Renting a machine to read ADR-0017's controls back on silicon would be external spending, which is yours, and nothing is rented.
- **Item 219:** N8's "before a second one is built" becomes "before any performance claim and before release qualification".

Also:

- **Items 207 and 208 are drafts.** Their exact shapes are put to you again when step 5 and the Loader Service begin.
- **Owed before code:** the System V x86-64 psABI revision, pinned before the first export of `origin.core` (item 201, CLAUDE.md §17).
- **Recorded, and no decision:** S4-0, ADR-0005 E1's 64-bit handles (item 109), lands before `origin.core` and takes `user-abi` to 0.7; `origin.directory` goes to 0.2 in S4-4 and `origin.diagnostics` starts at 0.1 in S4-5.
- **Nothing is built.** Step 4 starts with S4-0 once the second step-3 batch is on `main`. When built, it runs under QEMU and as KVM guests in CI, and nothing will have run on a declared machine.

## The requirements matrix: two more states, the owed list and quoting by title (items 221 to 223): Accepted 2026-11-12

Building the matrix (TASKS item 35) took three things that items 191 to 196 do not say. The coordinating session numbered them 221 to 223 and put the first two to you on 2026-11-12, recommending both; the matrix session put all three to you in its own session. You answered **"Accept both"** in the coordinating session the same day, and then **"Accept 223"** when the coordinating session put the third to you.

| # | Question | Answer |
|---|---|---|
| 221, answered | Two more states for a row: `partial`, where part of it is implemented with evidence and its limitations say what is missing, and `not-verified`, where something is in the tree and the gate can find no evidence for it. Neither counts as coverage; both are printed beside the figure, with `built`. | **Accept** |
| 222, answered | A second kind of entry in the README's list of answered items without rows: `owed`, for an item whose deciding text is not in the tree yet. Today that is items 204, 207–212, 214, 215, 217 and 220, whose text is ADR-0005 Amendment 8, in pull request 16. Rows replace the entry when the text lands. | **Accept** |
| 223, answered | A decision of an ADR is quoted by its title only, so the gate does not see a change to its body under the same title. | **Accept** |

- **Item 223 accepts a limit of the gate,** which the matrix's README already lists under what the gate does not check. A reworded decision under an unchanged title has to be caught by whoever changes the ADR, who updates the decision's rows by hand.
- **Built:** the matrix and its gate, on `claude/dazzling-jackson-9eca1b`, not yet landed. Both states and the `owed` list are in it already; its README and TASKS item 35 will name these items.

## The first boot on a rented bare-metal machine (items 224 to 236): Accepted 2026-11-12

You asked whether bare metal is possible for testing, and answered "Yes, plan it" on 2026-11-12. A session of its own wrote the plan, `docs/research/2026-11-12-first-bare-metal-boot.md`. The coordinating session checked four of its statements against the code at `e6456dc` (identity maps for firmware-reserved memory are not built; the kernel prints no scope line; the boot proofs write only to the boot test's own disk; at most 64 processors are started), numbered its eleven decisions 224 to 234 in its order, and put them to you with three marked "Look". You answered **"Accept all 11"** the same day.

| # | Question | Answer |
|---|---|---|
| 224, answered | Provider: Latitude.sh, billed hourly: an `m4.metal.small` (EPYC 4244P, Zen 4, $0.41 an hour) for rentals A and B, and optionally one `f4.metal.large` (EPYC 9275F, Zen 5, $2.62 an hour) for rental C? | **Accept** |
| 225, answered | A budget cap of $10 (₹937) of machine time for all rentals together, with a stop to ask you before going past it; the $5 card hold outside it? | **Accept** |
| 226, answered | You sign up personally, and verify with a government ID through Stripe Identity if you are content to share it, otherwise with $100 of credit? (**Look**) | **Accept** |
| 227, answered | You do every dashboard action and hold every credential; Claude prepares the checklist and the image, reads the logs, and runs commands in rescue Linux only with an SSH key you add for that rental? | **Accept** |
| 228, answered | Rental A once you have signed up; rental B after identity maps land and pull request 16 merges; rental C only if B's serial console worked? | **Accept** |
| 229, answered | Does the deadline "identity maps must land before real hardware" (items 8 and 12) apply to rented servers too? (**Look**) | **Accept** |
| 230, answered | Accept by name T4's two windows, T5, T6, T8, T9 and T23, for bench boots on rented servers only? (**Look**) | **Accept** |
| 231, answered | Boot with the developer-mode loader and Secure Boot off, with no keys enrolled and no firmware variable written, and any firmware setting changed put back before deletion? | **Accept** |
| 232, answered | Commit the record and Origin's serial log with addresses and server IDs removed, and keep raw firmware tables and hardware dumps private, with their SHA-256 in the record? | **Accept** |
| 233, answered | Does the record claim bench evidence only, so that I2 stays Partly, E12 stays Open and no requirements row becomes done on it? | **Accept** |
| 234, answered | No self-hosted GitHub runner for now, since the repository is public? | **Accept** |

- **Nothing is rented and nothing is spent yet.** Signing up, the card and the identity check are yours (items 226 and 227). Rental A, on the provider's rescue Linux only, can start once you have signed up (item 228).
- **Rental B, Origin's first boot on a physical machine, waits** for identity maps for firmware-reserved memory (item 229 keeps items 8 and 12) and for pull request 16's merge.
- **Item 230 accepts six threats by name, for bench boots on rented servers only.** It is written into `THREAT_MODEL_IOMMU.md`'s list "before real hardware" and beside ADR-0009 H6. It does not reach a volunteer's machine, a release or any claim, and T14 is not accepted: identity maps close it.
- **Item 233:** a rented machine is bench equipment outside ADR-0009 A1's scope, so its record qualifies nothing.
- **Corrected after your answer: the prices.** Item 224 quoted the pricing page, whose "/hr" figure is the monthly price over 730 hours. Latitude's API, read by the bare-metal session with your key on 2026-11-12, gives hourly billing at $0.81 an hour for `m4.metal.small` in the US, Canada, Germany and the Netherlands, and $6.04 an hour for `f4.metal.large`, in stock only in Singapore and Tokyo. Rentals A and B fit item 225's $10 cap, at about $0.81 (at most $1.62) and $2.43. Rental C does not: its worst case passes the cap, so it runs only if you raise the cap or choose another machine. Items 224 and 225 stand as answered, and nothing was bought on the old figures.
- **Found, not changed:** ADR-0009 Amendment 1's table says Clearwater Forest was introduced in March 2026, and Intel's specification page for the Xeon 6990E+ gives a launch date of Q2'26. It is in the scope either way. Item 257 corrected it on 2026-11-12.

### Rentals through Latitude's API (item 235, changing item 227): Accepted 2026-11-12

After signing up with Latitude.sh, you told the bare-metal session to "try their API and make it automate from your side, money is also matter". That changes item 227, which had you doing every dashboard action. The session wrote a rental tool, `developer-tools/bench/latitude.py`. The coordinating session read it and found two faults that could cost money: a retried create could make a second server, and an unconfirmed deletion was recorded as done. Both were fixed (`70e596d`), with seven offline tests. The coordinating session put the change to you as item 235, and you answered **"Accept 235"**.

| # | Question | Answer |
|---|---|---|
| 235, answered | Change item 227: Claude runs the rentals through Latitude's API with `developer-tools/bench/latitude.py` once its money faults are fixed, and you sign up, create the API key and save it to the token file yourself, approve each rental's cost in chat before it is bought, and revoke the key when the rentals are done? | **Accept** |

- **Item 227 otherwise stands.** You hold every credential, and no Claude session creates the key, reads it off a page, or writes or prints it. The tool reads the token file only to send the key to `api.latitude.sh`.
- **Every purchase still needs your yes in chat, with the item and the total.** The tool's `--yes` follows that answer and never stands in for it. Item 225's $10 cap is enforced by the tool's own ledger.
- **What the tool cannot guard:** Latitude's API cannot set a deletion time, so the watchdog that deletes a server at its deadline runs on the Mac. Keep the Mac plugged in, lid open and online until `status` shows the server deleted.
- **Rental A done on 2026-11-12, $0.81 of the $10 cap.** You approved its cost in the bare-metal session ("Yes, rent it"). One `m4.metal.small` in Amsterdam ran for 9 minutes 11 seconds, was billed one hour, and its deletion was confirmed by the API. No Origin code ran. What the machine showed is in `docs/research/2026-11-12-rental-a-findings.md`. The serial console is COM2, and the firmware has no IVMD block. The raw dumps stay private (item 232).
- **Not in the repository yet.** Before rental A the tool had not been run against Latitude's API. It is on `claude/first-bare-metal-boot` (`11bb986` and `70e596d`), and it lands with a later batch, since it is code.

### How rental B starts Origin (item 236, changing item 231): Accepted 2026-11-12

Rental A showed that the rented server's management controller drops the serial console at every reset. Item 231's way of starting Origin, picking the disk once from the firmware's boot menu over that console, would have to be done blind. The bare-metal session proposed another way and put it to you in its own session, where you answered "Yes, replace the file". The coordinating session numbered it item 236 and put it to you, and you answered **"Accept 236"**.

| # | Question | Answer |
|---|---|---|
| 236, answered | Change item 231's boot method for rental B: put Origin's developer-mode loader over the file the firmware's first boot entry starts (on the rented board, `\EFI\ubuntu\shimx64.efi` on the first disk's EFI system partition), with the kernel at `\EFI\ORIGIN\KERNEL.ONBF` on the same partition, instead of picking the disk from the boot menu? | **Accept** |

- **The rest of item 231 stands:** no UEFI variable is written, no firmware setting is changed, and Secure Boot stays off.
- **Its cost:** the provider's system on that disk no longer starts. The server is deleted and its disks erased after each rental, and the provider's rescue mode, which starts from the network, can still recover it.
- **Rental B still waits** for identity maps (item 229), which its tool enforces. The tool (`b56a99b` on `claude/first-bare-metal-boot`) has not run, and the coordinating session reads it in full before it does.

## The fingerprint records a handle's width (item 237): Accepted 2026-11-12

Task S4-0 made a handle 64 bits (ADR-0005 E1, item 109), and `origin.kernel` and `user-abi` went to 0.7. OIDL §13's canonical description does not record how wide a handle is, so 0.7's fingerprint came out identical to 0.6's: the lock, which is there to show every ABI change, did not show this one. The session that built S4-0 proposed recording the width, the coordinating session numbered it item 237 and put it to you, and you answered **"Accept 237"**.

| # | Question | Answer |
|---|---|---|
| 237, answered | Does OIDL §13's canonical description record a handle's width, as format `oidl-fingerprint 2`, with every library's lock recorded again once while every library is at major 0? | **Accept** |

- **Why now:** at major 0 a changed fingerprint is reported and not refused (OIDL §9, ADR-0005 P12), so recording every lock again costs nothing. From major 1 it would be a break.
- **The alternative, not chosen:** leave the description as it is, since a handle's width can change only through a revision of OIDL §10, which is yours anyway.
- **Not built.** TASKS item 39 builds it: the generator's description, OIDL §13's text, and every lock recorded again. Until then §13 carries a dated note.

## Performance cost policy (ADR-0018, items 238 to 246): Accepted 2026-11-12, nothing built

You asked the architecture-review session on 2026-11-12: "Have you considered zero performance tradeoffs? if need to consider zero performance tradeoffs what do you do?" Nobody had. CLAUDE.md §2 puts correctness and security above performance, each ADR lists its trade-offs in prose, almost no cost is measured, and the budgets file of item 115 is not written. On your "Yes, write them" that session wrote nine decisions, each checked against CLAUDE.md 7.6 and ADR-0001 to ADR-0017. The coordinating session numbered them and put them to you once. You answered **"Accept all 9"** in the coordinating session the same day. They are V1 to V9 of `docs/adr/ADR-0018-performance-cost-policy.md`, in this order.

| # | Question | Answer |
|---|---|---|
| 238, answered | What "zero performance trade-offs" means for Origin: "no avoidable cost". Performance is given up only where (a) a P0 invariant, a rule of CLAUDE.md or an accepted ADR needs it, (b) no design we know gives the same result for less, and (c) the price is written in the cost ledger. Literal zero is not adopted, and §2's order does not change (V1). | **Accept** |
| 239, answered | Where the rule lives: a new ADR, "Performance cost policy", holds these decisions. CLAUDE.md is not changed now (V2). | **Accept** |
| 240, answered | Three ways to state a cost: every design decision from here on states its performance cost as one of "none, by construction", with the reason; "measured", with the figure and where it was measured; "unmeasured, owed", with the model it is estimated from (V3). | **Accept** |
| 241, answered | The order of choice: a design takes the first of these that holds its invariant, and its ADR says where it stopped and why: checked when the code is built; checked once, at load, at boot, or when a handle or a mapping is made; checked on each operation, in the cheapest measured form (V4). | **Accept** |
| 242, answered | One cost ledger, in the budgets file: `PERFORMANCE_BUDGETS.md` holds two tables: the budgets of item 115, one row a path; and the cost ledger, one row for each place the design pays on each operation or pays a price nobody has measured. A ledger row gives the path, what the cost buys (the ADR decision or the directive's section), the estimate and its model, the measured figure and its environment, the cheaper design considered, and what would let the cost be lifted. Its first fill is made by reading the Trade-offs of ADR-0001 to ADR-0017 and the threat models (V5). | **Accept** |
| 243, answered | The budgets file comes first: TASKS item 26 is built before any new figure is recorded: a hypothesis and its model for each path the boot already times, then the trends. No figure fails CI until declared hardware exists (V6). | **Accept** |
| 244, answered | How a protection's price is measured: as a pair. One commit is built twice, once as it is and once with one named edit that takes the protection out, booted in turn on the same machine in the same run. The price is the difference, given with its spread. A script applies the edit to an exported copy, the way boot-proof controls are planted (ADR-0013). No switch that turns a protection off enters the tree, and the second kernel is never signed or packaged. Where a protection cannot be taken out without a redesign, its price comes from a stated model and is marked so (V7). | **Accept** |
| 245, answered | A gate: `cargo xtask check` refuses an ADR or an amendment accepted after this one with no "Performance cost" row in one of item 240's forms; a "measured" or "owed" cost that names no ledger row; and a ledger row whose ADR decision does not exist, or whose figure has no environment (`emu`, `virt`, `hw`, the requirements matrix's words). ADR-0001 to ADR-0017 are not edited: the ledger lists each of them, with its rows or with "none" and the reason (V8). | **Accept** |
| 246, answered | What may be claimed: until a figure comes from declared hardware, the most a document says is "no known avoidable cost; unmeasured on a real processor". "Zero-cost", "zero overhead" and "zero performance trade-offs" are not said of Origin as a whole. One mechanism may be called "no cost by construction", with its reason. A rented server's figures are bench figures (item 233) (V9). | **Accept** |

- **What they change:** no word of CLAUDE.md and no accepted ADR. No ABI, format or interface moves.
- **Nothing is built.** The budgets file is TASKS item 26. The ledger's first fill and the gate of item 245 are a new task, which the coordinating session numbers.
- **Nothing is measured.** Every figure today is from QEMU's software processor or from CI's KVM guests, and neither offers IBPB (ADR-0011 L12). Item 246 is all that can be said until a declared processor runs.
- **Items 224 to 237 are not in this tree.** They are answered and recorded on other branches (pull request 20, and the record of item 237), and they join this sheet when those land. Item 233, which item 246 cites, is one of them.
- **No person has reviewed the ADR.**

## Milestone exit tests, and two launch dates in ADR-0009 (items 252 to 258): Accepted 2026-11-12

CLAUDE.md §82 forbids marking a milestone complete without its exit tests, and decision item 122 put them in `docs/ROADMAP.md`. No exit test existed, and `docs/STATUS.md` marked M0, M1 and M2 done. TASKS item 25's session proposed the tests below, each tied to rows of the requirements matrix and checked against what the tree shows; the coordinating session read them against §74, §82 and the final rules, changed M0-7 (no gate over task contracts) and added M6 to the milestones already started. Items 257 and 258 came from TASKS item 16, which read Intel's pages again for the CPU identification table. You answered "Accept all 7" in the coordinating session.

| # | Question | Answer |
|---|---|---|
| 252, answered | M0's exit tests, M0-1 to M0-7: `rust-toolchain.toml` as §8 has it and `verify.sh` passing on a clean tree, recorded; §10's three regression tests, the `wcslen` one inside the `no_std` loader; CI on every pull request and on `main` with exactly the pinned compiler, its commit checked, `--locked`, `verify.sh` and `cargo xtask check`; `check` passing; the release identity generated from `origin-version.toml`; `Cargo.lock` holding only the workspace and the approved Loom exception; the records kept, with the requirements matrix and the provenance table gated and task contracts written from the template and checked in review, with no gate over them? | **Accept** |
| 253, answered | M1's exit tests, M1-1 to M1-5: the loader reads, verifies, places, relocates and enters the kernel; the memory handoff, validated by the kernel; diagnostics on the UART the firmware names and on the framebuffer, with a fault report; a tampered image, a wrong key and a bad signature each refused; all of it on at least one physical machine in ADR-0009 Amendment 1's scope, with a record. Secure Boot integration and its first stage (S19-12, S19-15) and recovery boot (S19-13) go to M12? | **Accept** |
| 254, answered | M2's exit tests, M2-1 to M2-6: CPU initialization, with a processor below x86-64-v3 refused; the core generation named at boot (TASKS item 16); exceptions; the APIC timer and I/O APIC interrupts, edge and level; frames, the kernel's page tables, heap and page faults; all of it on at least one physical machine in scope, with each boot proof's negative controls. SMP, topology and shootdowns go to M3; user address spaces and speculation controls to M4? | **Accept** |
| 255, answered | A milestone whose scope is hardware is "built", never "done", on evidence from QEMU or KVM guests; "done" needs a hardware record under `docs/hardware/records/` (§74, final rule 30)? | **Accept** |
| 256, answered | The rule for later milestones: their exit tests are written in `docs/ROADMAP.md` before the milestone's first task contract, and now for the milestones already started (M3, M4, M5, M6 and M10), and come to you as one item for each milestone; each test names its entry point, its negative controls and its matrix rows; a row no sentence of §82 names goes to the milestone that names its subject, else M13; a milestone is done when every test passes in its scope and every row it names is `done`; the Server edition's work is placed by ADR-0014 S2 and ADR-0015 D9? | **Accept** |
| 257, answered | ADR-0009 Amendment 1, A1's table: Clearwater Forest from "introduced March 2026" to "Q2 2026, by Intel's product pages" (Intel's ark gives Q2'26 for all four Xeon 6900E+ processors)? | **Accept** |
| 258, answered | The same table: Wildcat Lake from in the scope "when they ship" to "April 2026" (launched 2026-04-16 as Core Series 3); Nova Lake stays "when it ships"? | **Accept** |

- **No milestone is done today.** Under items 252 to 255, M0 waits for TASKS item 22 (CI runs neither `verify.sh` nor `--locked`), and M1 and M2 wait for a physical machine in the scope. No such machine has run Origin: rental A was a Zen 4 bench machine running its provider's Linux, and rental B will be one too (items 228 and 233). `docs/STATUS.md`'s three cells change to "built; exit tests proposed in `docs/ROADMAP.md`" with TASKS item 25, as the coordinating session decided under §82.
- **Where they are written.** `docs/ROADMAP.md` holds the tests and lands with TASKS item 25 in the third batch. The matrix names items 252 to 256 on rows S82-01, S82-02, S82-03 and S82-19, and items 257 and 258 on row A09-A1. ADR-0009 Amendment 1 carries 257 and 258 in its table and a dated note under it.
- **Next under item 256:** the exit tests of M3, M4, M5, M6 and M10, one item each, from TASKS item 25's session. They are items 259 to 263, below.
- **Found, not changed:** Intel's ISE reference 319433-062 (June 2026) lists Diamond Rapids as family 13h, model 01h, so Amendment 1's evidence line "Diamond Rapids ... is not confirmed by Intel" is out of date. The rule never depended on it. TASKS item 16 records it as a dated note under the amendment.

### The exit tests of M3, M4, M5, M6 and M10 (items 259 to 263, under item 256): Accepted 2026-11-12

Item 256 asked for the exit tests of every milestone already started, one item each. TASKS item 25's session wrote them by its rule, each test with its matrix rows (`docs/ROADMAP.md` on its branch, `b67685d`), and listed six choices inside them with a recommendation each. The coordinating session read them against §24 to §82 and added one choice to item 262: networking in the runtime goes to M10. M7 is not included: nothing parses, builds or installs a `.oapp`, `.osi` or `.od` yet, so its tests are written before its first task contract. Under item 255, each milestone's last test is a run on a physical machine in the scope, with a record. You answered "Accept all 5".

| # | Question | Answer |
|---|---|---|
| 259, answered | M3's exit tests, M3-1 to M3-6: every processor the firmware lists started, with interprocessor interrupts, topology and processors taken offline and back; native 1:1 threads, preempted; §26's scheduling, quotas and safe fallback included; placement that accounts for topology (§27); TLB shootdowns, cross-core races and the Loom models; all of it on two physical machines in the scope, one with SMT and one with hybrid cores, with records? | **Accept** |
| 260, answered | M4's exit tests, M4-1 to M4-7: processes created through the kernel's calls, ended, supervised and cleaned up, a crash contained; each process's own address space, and shared memory through memory objects; typed, authenticated IPC with capability handles; system calls and their error codes, every value from ring 3 checked; ADR-0017's controls in effect on every processor before ring 3 runs anything but a boot proof; isolation mapped to the threat models' rows with negative controls; all of it on a physical machine in the scope? | **Accept** |
| 261, answered | M5's exit tests, M5-1 to M5-6: the driver framework's core; PCIe with VT-d and the AMD IOMMU; NVMe; the storage layers with asynchronous I/O; filesystem foundations only (an accepted on-disk format, its durability semantics, crash consistency, data and metadata integrity, atomic rename, versioned structures, power-loss recovery); all of it on a physical machine in the scope with an NVMe controller. The rest of §40 goes to M9; hot-plug and external-device authorization (S31-12, S32-12, S32-13) to M10? | **Accept** |
| 262, answered | M6's exit tests, M6-1 to M6-6: the userspace ABI written and versioned; the `.ose` loader, fuzzed; the Origin runtime; userspace built for its own target and never for Linux's APIs; the launch capability of ADR-0011 Amendment 1; all of it on a physical machine in the scope. Networking in the runtime (S44-08) goes to M10, with the network stack? | **Accept** |
| 263, answered | M10's exit tests, M10-1 to M10-7: USB through xHCI, descriptors fuzzed; USB4 and Thunderbolt with external-device authorization; networking, Wi-Fi and Bluetooth included; audio and peripherals; power, suspend and resume, with the clocks across sleep; every device standard through the SDK's three layers; all of it on physical machines in the scope, laptops among them? | **Accept** |

- **What they ask of hardware.** M3 needs two machines in the scope, and in practice both makers: Intel's recent client cores have no SMT, so the SMT machine is likely an AMD machine in the scope (a 2026 generation with Zen 5 or newer cores) and the hybrid one a 2026 Intel. None is owned, and none in the declared scope can be rented (ADR-0009 Amendment 1, A1; ADR-0011 L9). *(Corrected 2026-11-12: this line said "an AMD Zen 5 or newer" and "rentable within item 225's cap", which read as if a rented Zen 5 server would do.)*
- **What they ask of design.** M5's filesystem foundations need a successor to OriginFS v0, which never ships (items 1 to 4), with its own ADR. M10's Wi-Fi and Bluetooth can be done only if a chip has a legitimate source (ADR-0015 D6; rows S54-19, S54-20); otherwise they need a scope decision before M10 closes.
- **Where they are written.** `docs/ROADMAP.md` lands with TASKS item 25 in the third batch. The matrix names items 259 to 263 on rows S82-04 to S82-07 and S82-11, and item 262 also on S44-08.

### Corrections to the exit tests after a full read of the design documents (items 264 to 268): Accepted 2026-11-12

Items 252 to 263 were recommended from CLAUDE.md and the requirements matrix. You asked the coordinating session whether it had checked CLAUDE.md and its design and review documents end to end. It had not, and on your "Yes, read them" it read every accepted ADR, the four threat models and the specifications in full. The record is `docs/engineering/DESIGN_READ_2026-11-12.md`, with eleven findings (F0 to F11). Five needed your answer. You answered "Accept all 5".

| # | Question | Answer |
|---|---|---|
| 264, answered | Have TASKS item 25's session place every counted `build` row of the matrix in one milestone, or list it as belonging to none with the reason, and add a gate in `cargo xtask check` that reads `docs/ROADMAP.md`? Where that changes an accepted test, the change comes back to you as one item (F0: the roadmap names 809 of the matrix's 1,651 rows, and about 250 `build` rows of the directive are in no milestone) | **Accept** |
| 265, answered | Do M4-7 and M5-6 each ask for one Intel and one AMD machine in scope, as M3-6 does? Their rows are vendor-specific: VT-d and the AMD IOMMU (S32-07, S32-08), and ADR-0017's Z6, Z7 and Z12 (F3) | **Accept** |
| 266, answered | Does Secure Boot with Origin's own keys (S19-12, S19-15) move from M12 to M9, where the installer guides enrolment, with a test key until the ceremony? The release manifest's verification, the rollback floor and measured boot stay in M12 with ADR-0002's Amendment 2 (F9: `THREAT_MODEL_SYSTEM.md` asks for the Secure Boot keys before Origin can be installed, and ADR-0002 Amendment 1 has the installer guide enrolment) | **Accept** |
| 267, answered | Do M6's and M7's tests take `ONBF.md` §13's nine obligations by reference, each placed in M6 or M7 by its subject and none dropped (F11: §13 is headed "exit criteria for M6/M7", and M6-2 as accepted is weaker)? | **Accept** |
| 268, answered | Is M1-1's signature check met with the test trust store, on a build that says so at every boot, with Origin's own kernel key belonging to the ceremony before the first public release (F1)? | **Accept** |

- **Item 266 changes item 253,** which sent S19-12 and S19-15 to M12. Recovery boot (S19-13) stays there.
- **Item 255 stands, and what the session told you about it is corrected (F4).** It said M1 and M2 could be marked done after "rental C or another in-scope machine". Rental C is an EPYC 9275F, an EPYC 9005: Zen 5 from 2024, outside the scope by ADR-0009 Amendment 1, A1. No machine in the declared scope can be rented (ADR-0011 L9), and buying is off (ADR-0009's update of 2026-11-09). So no milestone from M1 on can be marked done until a volunteer's machine or a bought one exists. The alternative in TASKS item 25's proposal, done for a declared virtual scope now and hardware at M13, was not shown to you. It would first need that scope declared exactly: ADR-0014 S3 names "a KVM guest" and no more.
- **Not decisions: accepted rules the roadmap will state.** A first boot on a machine in scope needs the IOMMU model's list closed or accepted by you for that machine (ADR-0009 H6); item 230 covers rented bench servers only. E12 and I2 come before any claim about real hardware.
- **Recorded with these:** a dated note in ADR-0017's status line, which had not been brought up to item 213 (F7).
- **Performance cost of items 252 to 268** (ADR-0018 V3, accepted as items 238 to 246): none, by construction. They change documents, and item 264 adds one gate on the build host.
- **Where they are written.** `docs/ROADMAP.md` changes with TASKS item 42, given to TASKS item 25's session, and lands in the third batch. The matrix names item 264 on S82-19; 265 on S82-05 and S82-06; 266 on S19-12, S19-15, S82-10 and S82-13; 267 on S82-07 and S82-08; 268 on S82-02.

### The roadmap's further placements, and one row's scope (items 269 and 270): Accepted 2026-11-12

Item 264 asked for every counted `build` row of the matrix to have a milestone, and for any placement that adds to a milestone whose tests you had accepted to come back to you as one item. TASKS item 42 placed them (`docs/ROADMAP.md` at `5e80911`; the list is in that task's record). The coordinating session checked each proposed row's scope against the matrix, and the session that wrote the roadmap checked the two moves below against the ADRs. You answered "Accept both".

| # | Question | Answer |
|---|---|---|
| 269, answered | Accept the 328 rows placed beside the accepted tests of M0 to M6 and M10: M0 47, M1 26, M2 31, M3 25, M4 85, M5 35, M6 63, M10 16? With one change: nine rows proposed for M0 are scoped to hardware and move out, so that M0 stays provable on the build host. ADR-0013's eight (A13-R1 to A13-R7 and A13-Am1) go to M1, and A11-N13 goes to M6 | **Accept** |
| 270, answered | Does row A11-N13's scope in the matrix change from `both/hw` to `eng/host`? Every rule of ADR-0011 N13 is a gate in `cargo xtask check` on the build host, and its evidence ran there | **Accept** |

- **The accepted tests themselves are unchanged** by item 269, beyond what items 265 to 268 changed. The rows are placed beside them.
- **Three of the 328 came after the list was written.** ADR-0018 reached the batch through `main`, and the roadmap gate then asked for a place for A18-V2, A18-V5 and A18-V8 (its record, its cost ledger and its gate; `build`, scope `eng/host`). The session that keeps the batch put them in M0, and they were put to you with the rest.
- **What M0 now needs.** After the move it holds 37 of the added rows (the range that moved also held one row that only informs). 21 of them are not started, all scoped to the build host. Among them: §14's compiler and linker reviews, the overflow checks and integer features (TASKS item 22's second half), `CONTRIBUTING.md`, `SECURITY.md` and `CODEOWNERS` (TASKS item 12), and ADR-0018's ledger and gate.
- **Why ADR-0013's rows go to M1.** Their acceptance is "passes on each declared machine", so they need M1-5's machine, and the boot's verdict and refusal are M1's diagnostics.
- **Why item 270 is its own item.** It lowers the evidence one row needs before it counts as done, from a physical machine to the build host. The row still goes to M6 and not M0: one of its gates reads first-party manifests, which do not exist until the loader's work.
- **Not accepted by these:** the rows placed in M7 to M9 and M11 to M13, whose tests are not accepted yet.
- **Performance cost** (ADR-0018 V3): none, by construction. Both change documents.
- **Where they are written.** `docs/ROADMAP.md` and row A11-N13's scope change in the third batch, in two commits by the session that wrote the roadmap. The matrix names item 269 on S82-19 and item 270 on A11-N13.

### What task S4-2 rests on, and a comparison of images in CI (items 271 to 275): Accepted 2026-11-13

Task S4-2 (`origin.core` and the per-call pin) needed one thing more than the answers of 2026-11-12: ADR-0011 L5 asks for the psABI revision to be pinned before the first export. The coordinating session read the psABI on 2026-11-12 (`docs/research/2026-11-12-psabi-revision.md`). The reading gave items 271 to 273; item 274 is a question the independent review of task S4-1 left for S4-2; item 275 came from TASKS items 32 and 44, after the images were first seen to be the same bytes on the development Mac and in CI. You answered "Accept all 5".

| # | Question | Answer |
|---|---|---|
| 271, answered | Does CLAUDE.md §17 pin the psABI as version 1.0, the repository's `master` at commit `e1ce0983` (2025-03-12), read on 2026-11-12? Origin takes its data representation and function calling sequence, and not its process start-up stack, auxiliary vector or ILP32 (x32) chapter | **Accept** |
| 272, answered | Does ADR-0005 P6 state the flags at a thread's entry: the direction flag and the six status flags clear, interrupts on, and no other bit to be relied on? | **Accept** |
| 273, answered | Do `origin.core`'s generated stubs extend every parameter narrower than 64 bits to its whole register before the call: zero for unsigned, booleans, enums and bits, the sign for signed? | **Accept** |
| 274, answered | Are the runtime's three exit codes (a panic, a bad start record, an unwind) left unreserved in `origin.core` at major 0, as the runtime's own documented convention, with the reporting of a process's end decided with the crash record before `user-abi` 1.0? | **Accept** |
| 275, answered | Does CI compare the images across two machines on every run and fail if they differ, with a job on GitHub's standard macOS runner beside the Linux one and a job that compares the two sets of files? | **Accept** |

- **Why 273.** The psABI says the bits of an argument register beyond the parameter's type are unspecified, and that the receiving side extends. The kernel judges the whole register and answers `invalid_args` for a narrow value that "does not fit". A naked stub (ADR-0011 L6) passes the caller's register on unchanged. So a correct C call could be refused for what the caller's compiler left in the upper bits, and no accepted text covered it. The alternatives put were a 64-bit type for every C parameter, and a kernel that ignores the upper bits.
- **What 275 was told to give and not to give.** GitHub's macOS runner is Apple-processor, the same kind of host as the development Mac. The job makes the comparison of 2026-11-12, which was made once and by hand, run on every push: one Linux host against one macOS host. It adds no third kind of host, it needs a trial run first, and it is one more job that must be green before a merge. The alternatives put were two Linux runner images, which would not have noticed either difference that TASKS item 44 removed, and keeping the comparison by hand. The runner is one of GitHub's standard ones, free for this public repository, as decision item 32 set for CI (CLAUDE.md §80).
- **Where they are written.** Item 271: CLAUDE.md 7.7, §17, with 7.6 kept at `docs/engineering/CLAUDE-v7.6.md`. Item 272: ADR-0005 Amendment 9 (H1). Items 273 and 274: ADR-0011 Amendment 3 (L13, L14). The matrix: S17-17 quotes the new statement and names item 271; new rows A05-H1, A11-L13 and A11-L14 name items 272 to 274. Item 275 is listed in the matrix's README as owed: its job is TASKS item 46, and rows S60-03, S75-06 and S75-07 take it when that lands.
- **Performance cost of items 271 to 275** (ADR-0018 V3). Items 271, 272 and 274: none, by construction. Item 273: one instruction for each narrow parameter in each call; unmeasured, and owed with S4-2's figure for the cost of one call. Item 275: none at run time; one more CI job on each push.
- **What follows.** Task S4-2 has what it waited for. It is a privilege boundary and needs an independent review before it lands (CLAUDE.md §1). TASKS item 46 builds item 275's jobs, after a trial run on the macOS runner.
- **A criterion read, and no decision.** A signature's own bytes are never the same twice (`onbf-pack` signs with hedged ML-DSA, FIPS 204 §3.4). Where a row's criterion asks for "the same bytes", it is read as the unsigned image, which is everything a signature covers: ONBF-R9 already speaks of signed content. Each row takes that wording as its own criterion from the task that finishes it, as the matrix's README says.

### Two rows placed in M6, and one sentence of L13 corrected (items 276 and 277): Accepted 2026-11-13

Items 272 and 273 added two counted `build` rows to the matrix, A05-H1 and A11-L13, after item 269 had placed every such row. With the third batch (pull request 21) `main` gained the gate that asks each such row for a milestone (item 264), so the two needed a place. Item 277 came from building item 273: task S4-2's session, and the independent reviewer of its build half before it had seen that session's notes, each found that L13's sentence said "zero for an enum" where OIDL lets an enum sit over a signed integer. The coordinating session had drafted that sentence for item 273. You answered "Accept both".

| # | Question | Answer |
|---|---|---|
| 276, answered | Are rows A05-H1 (the flags at a thread's entry) and A11-L13 (the stubs extend narrow parameters) placed in milestone M6, beside the rows they extend? | **Accept** |
| 277, answered | Does L13 say that a stub extends an enum as its underlying integer, zero where that is unsigned and the sign where it is signed, in place of "zero for an enum"? | **Accept** |

- **Why 277.** OIDL §3 makes an `enum` "its integer", any of the eight, and §10's decoder accepts "a declared member". The kernel reads the register of an enum over a signed integer as signed. A negative member of an enum over `i8`, `i16` or `i32`, extended with zeros, would be answered `invalid_args`: a correct call refused.
- **What it changes that is built.** Nothing. No call of `origin.kernel` passes an enum in a register, and the generator on task S4-2's branch, which has not landed, already extends an enum as its underlying integer. Evidence for both: that branch's code and the bytes built from it, read by the review; nothing on `main`.
- **Item 276 changes no accepted test.** The rows are placed beside M6's tests (item 262), in no exit test, as item 269's rows are. The placement's text was written by task S4-2's session.
- **Performance cost** (ADR-0018 V3): none, by construction. Item 276 changes a document. Item 277 changes which of two one-instruction extensions an enum over a signed narrow integer gets, and no call has one.
- **Where they are written.** Item 276: `docs/ROADMAP.md`, M6; the matrix names it on S82-19, as it does item 269. Item 277: ADR-0011 Amendment 3, row L13, corrected in place with its date; the matrix names it on A11-L13. OIDL §10 gains L13's rule, in the corrected words, when task S4-2 lands.

### The compiler becomes Rust 1.100.0, and how that lands (items 278 and 279): 2026-11-13

On 2026-11-13 you wrote: "Updated Rust language to latest available version 1.100.0, use it". That is item 278: your instruction, not an answer to a question. The coordinating session checked the release the same day and put one question about how the change lands, item 279. You answered "Own pull request".

| # | Question | Answer |
|---|---|---|
| 278, answered | Which compiler does Origin build with? | **Rust 1.100.0 stable, in place of 1.99.1: "Updated Rust language to latest available version 1.100.0, use it" (your instruction, 2026-11-13)** |
| 279, answered | Does the upgrade land as a pull request of its own, as soon as CLAUDE.md §15's list passes, and not with the batch? It is an exception to item 70's 100 commits, for this change only | **Yes: "Own pull request"** |

- **What was checked on the day** (2026-11-13, 01:44 to 01:50 UTC). The official stable channel's manifest is dated 2026-11-12 and names rust `1.100.0 (0ef1bc386 2026-11-09)`; the release was published on 2026-11-12, so it was one day old. Its LLVM is 23.1.5, where 1.99.1 has 23.1.1. On the development Mac it was installed as the default `stable` toolchain with the host target only, and the repository's pin still selected 1.99.1 everywhere.
- **Nothing changes until the upgrade is qualified.** CLAUDE.md §1 and §15 send a compiler change through a dedicated task with a list: the release notes analysed, the ABI regression tests, the loader's tests, kernel boots, a review of `unsafe` code where the compiler's behaviour changed, the performance figures, a comparison of reproducible builds, and SDK compatibility. That task is TASKS item 52. Until it lands, `rust-toolchain.toml` names 1.99.1 and every session builds with it, so no evidence mixes two compilers. §15's "do not upgrade the compiler merely because a newer version exists" is met by your instruction; its list still applies.
- **CLAUDE.md 7.8 lands with the pin, in the same pull request.** The directive names the compiler in its title, in §8 to §15, in §17 and in its first final rule. Revision 7.8 is written by the coordinating session and reaches `main` together with `rust-toolchain.toml`, so the directive and the build never name different compilers.
- **What the release notes say that touches Origin,** as read that day and to be read again by the task: Cargo changed the layout of its build directory, which the build tool reads in three places; the Rust calling convention on x86 now uses SSE registers where they are always available, which changes ring-3 code and must not change `extern "C"`; enabling SSE or AVX on a soft-float target is now an error, and the kernel enables none; the never type is stable, and several lints are new or stricter. To verify again by §9: that the kernel's `harden-sls-ret` option is still accepted, with which warning, and ADR-0017 Amendment 1's counts for the toolchain's prebuilt libraries. Every image's bytes change.
- **If the new compiler breaks something that is relied on,** one of §10's three regressions or a behaviour an accepted ADR rests on, it comes back to you with the evidence. Staying on 1.99.1 until a point release is then one of the answers.
- **Why item 279.** A compiler change alters every image, so landing it alone keeps its evidence apart from other tasks' code, and it reaches `main` without waiting for a batch to fill. The pull request also carries TASKS item 46's CI job if that job's trial passes, so the runners themselves compare the two hosts' images under the new compiler.
- **Performance cost** (ADR-0018 V3). Item 278: unknown until measured. A new code generator changes every image; TASKS item 52 records the figures CI already takes, before and after, as trends that claim nothing (CLAUDE.md §53). Item 279: none.
- **Where they are written.** This sheet, and CLAUDE.md 7.8, which reaches `main` only together with the pin (TASKS item 52's pull request). The matrix names item 278 on rows H-04, H-05, S1-05, S8-01, S8-02, S12-01, S17-03 and F-01, and item 279 on H-04. *(Until revision 7.8 was written, the matrix's README listed item 278 as owed and item 279 as asking for nothing a row could carry.)*
- **What revision 7.8 changes in CLAUDE.md, every place.** The version is replaced where the directive states the pin or a present fact, and 7.7 is kept at `docs/engineering/CLAUDE-v7.7.md`:
  - the title ("Version 7.8 — Original-Source, Rust 1.100.0, ...") and the Revision line, which gains one sentence for 7.8 and the place where 7.7 is kept;
  - "**Mandatory compiler:** Rust 1.100.0 stable";
  - §1: "Follow the approved Rust 1.100.0 stable and Rust 2024 baseline";
  - the heading of PART III, "MANDATORY RUST 1.100.0 TOOLCHAIN";
  - §8: "Rust 1.100.0 is the mandatory production compiler." (it said "the mandatory initial production compiler"), "Require: Rust 1.100.0.", and `channel = "1.100.0"` in the example of `rust-toolchain.toml`;
  - §12, three places: "Stable Rust 1.100.0 is mandatory for the standard release build.", "If a feature cannot be implemented using stable 1.100.0:", "8. Preserve stable Rust 1.100.0 as the main compiler baseline.";
  - §14: "On stable Rust 1.100.0, `-Z cf-protection` (IBT/ENDBR64) and `-Z stack-protector` are unavailable", which the upgrade task checked again on 1.100.0 on 2026-11-13;
  - §17's baseline: "Rust 1.100.0.";
  - §82, M0: "Pin Rust 1.100.0, validate cross-compilation, ...";
  - §84: "2. Check Rust 1.100.0 availability.";
  - the first final rule: "1. Rust 1.100.0 stable is the mandatory compiler baseline." (it said "the initial mandatory compiler baseline").
- **Outside CLAUDE.md, one accepted specification states the pin.** The header of `docs/spec/ONBF.md` has a "Toolchain" line; it now names Rust 1.100.0 and says when it changed. No rule of the format changes. Three ADRs name Rust 1.99.1 in dated statements of what was checked, or not measured, on a past day (ADR-0017 Z10, ADR-0016, ADR-0011 Amendment 3); those stay, and ADR-0017 Amendment 2 carries what was seen under 1.100.0.
- **What 7.8 leaves as it was.** §10's heading and its first sentence still name Rust 1.99.1: they say which compiler fixed the three regressions the tests cover, which is a fact about that release, and the tests stay and are run under 1.100.0. §15 is unchanged: the named check of the kernel's hardening option is ADR-0017 Amendment 2's (item 280) and a heading of its own in `verify.sh`. No statement is added or removed, so the matrix keeps its rows and only their quotes change.

### The option behind the INT3 after every RET is to become an error in the compiler (items 280 and 281): Accepted 2026-11-13

The upgrade task's first build with the pin at Rust 1.100.0 passed `cargo xtask check` and the toolchain verification on the development Mac. One thing differed. The kernel gets its INT3 after every RET (ADR-0017 Z10; item 129) from `-C target-feature=+harden-sls-ret`, which stable Rust accepts with a warning. Under 1.100.0 the option still works and the image gate still passes, and the warning now adds that the option "will become a hard error in a future release", pointing to issue 162235 of `rust-lang/rust`. The coordinating session read that issue the same day and put two items. You answered "Accept both".

| # | Question | Answer |
|---|---|---|
| 280, answered | Does ADR-0017 record this as Amendment 2, with Z10 keeping the option for as long as the pinned compiler accepts it, every compiler upgrade checking it by name, and an upgrade to a release that refuses it stopping and coming back to you? | **Accept** |
| 281, answered | Does Origin tell the Rust project, on that issue, that it relies on `harden-sls-ret` on stable Rust, with the coordinating session drafting the text and you posting or approving it? | **Accept** |

- **What the issue says** (open, created 2026-09-03, last updated 2026-09-28). The error has no release named. Its intended replacement is two options that are both unstable, and the issue says the error will likely wait for both to be stable. It asks projects that cannot use nightly to say which features they rely on.
- **It does not stop the move to 1.100.0** (item 278). Nothing is weaker today, and 1.99.1 has the same option and would meet the same error in a later release.
- **Why keep the option.** ADR-0017 turned retpolines off over a warning of this kind, because a hardware control stands in their place. For a return there is none, so dropping the option would remove a protection.
- **Item 281 sends nothing by itself, and no session posts it.** After you answered, the coordinating session read the Rust project's moderation policy (`rust-lang/moderation-team`, `policies/spam.md`) and the issue's comments: machine-written and automated comments are treated as spam there, and a commenter on this issue was suspended under that policy. So the item is narrower than it was put: you post it yourself, in your own words, from a short list of facts the session gives you, and you state the compiler's version, which the maintainers ask for. If you would rather not post, nothing is lost but the request.
- **Performance cost** (ADR-0018 V3): none, by construction.
- **Where they are written.** ADR-0017 Amendment 2; the matrix's new row A17-Am2 names both items, and the roadmap's M4-5 already places every row of ADR-0017. The build's line about the warning is TASKS item 52's.

### A port's creator pays for its index (item 282): Accepted 2026-11-13

Task S4-3 (TASKS item 55), building the kernel-wide bound you accepted as item 209 (ADR-0005 G5), measured a port's index per binding, as G5's last sentence asks, and stopped on what it found: the index never shrinks while the port is open, so the slots that departed clients' bindings grew stay uncounted until the port closes, and across many long-lived ports that is not bounded by any figure the kernel holds. Closing it gives `object_wait_async` a new reason for `limit`, which changes what an accepted call does (F2, F7), so it was put to you. You answered "Accept 282".

| # | Question | Answer |
|---|---|---|
| 282, answered | Does a port's creator pay for its index, through a new kind in a process's limits, `bindings_per_port` (the most bindings armed at once on any one of its ports), with a bind past it refused with `limit`? | **Accept** |

- **The alternative put** was one kernel constant for every port (256 a port): simpler, but a default nobody chose, against item 210 (G6).
- **What it changes that is built.** Nothing yet. S4-3 lands with threat row K7 open on this point, naming this item. TASKS item 57 builds it, before S4-4's directory, the first long-lived service whose clients come and go.
- **Performance cost** (ADR-0018 V3): unmeasured and owed, as ADR-0005 Amendment 10 states it: one comparison at each bind, and a larger figure for each process given the kind. It names cost ledger row CL38, which is added when the ledger (TASKS item 54, in the batch) and this amendment are in one tree.
- **Where it is written.** ADR-0005 Amendment 10 (row I1). The matrix's new row A05-I1 names the item. The roadmap places that row in M4 beside G5, which it refines, in no exit test; the coordinating session placed it under item 264's rule, and you may reopen that.
