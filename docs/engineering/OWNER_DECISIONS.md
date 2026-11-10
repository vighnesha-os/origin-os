# Open owner decisions

| | |
|---|---|
| As of | 2026-11-10 for items 71–123. Items up to 70: 2026-11-09, after the pre-merge review. |
| Checked against | Items 1–70: CLAUDE.md v7.1, item by item. That check changed the first version of this sheet. Items 71–97: v7.2, and the 7.3 revision they produced. Items 98–105: as their sections say. Items 106–123: v7.3 and every accepted ADR, in the design review that put them, and the 7.4 revision they produced. |
| Evidence | Everything below is proven under QEMU only. Nothing has run on physical hardware (CLAUDE.md §74). |
| Answered | 2026-11-08: items 1–29. 2026-11-09: items 30–70. 2026-11-10: items 71–123 (item 97 delegated to Claude). |
| Still open | No item. Buying machines is off for now (ADR-0009's update of 2026-11-09). Items 77 and 79 leave you two things to do, and items 106, 121 and 122 three more (see their sections). |

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

Step 3 adds system calls and structures, so these are ABI and format decisions (CLAUDE.md §84). The owner accepted all thirteen with the recommended answer on 2026-11-09 ("Accept all 13"). They are ADR-0005 Amendment 4, C1–C13; the scoping is `docs/research/2026-11-09-n14-step3-scope.md`. C1–C3 are built (task S3-2, 2026-11-10: a message's writer and handle records), C4–C6 (task S3-3, 2026-11-10: the start record and its launch path), and C11 and C12 (task S3-4, 2026-11-10: `not_found` and the service directory, `origin.directory` 0.1); the rest are not.

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

- Tasks collect on one landing branch per batch, as a draft pull request. CI runs on every push to it, so each task is tested, including the real-CPU boots, as it arrives. The first batch is ADR-0011 N14 step 3, pull request 13.
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

Task S3-5b (memory objects) needs five interface decisions that C6–C8 (items 61–63) leave open. They are ABI decisions, so they are the owner's (CLAUDE.md §84). The session building S3-5b proposed them, with the options it rejected, and the coordinating session checked them against CLAUDE.md, ADR-0004 and ADR-0005 before asking. The owner answered **"Accept all"** on 2026-11-10. They are ADR-0005 Amendment 5, D1–D5. Items 71–97 are in use on the unmerged `docs/editions-device-sdk` branch.

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

**Two later answers changed this revision's wording, on the same day.** You accepted the six questions of ADR-0016, the kernel's random-number generator, as items 146 to 151. They are recorded with that ADR, which is on the generator's branch until it lands. Two of them are in draft 7.4:

- item 149: §59 says the health tests are on what each source returns, catch gross failure only, and claim no SP 800-90B conformance;
- item 151: §17 names SP 800-90A Rev. 1, SP 800-90B (January 2018), SP 800-90C (September 2025) and FIPS 198-1.

**Yours to do:**

- Read revision 7.4's wording and say whether the records pull request may merge (item 106). CLAUDE.md is your text, and merging takes your word.
- Confirm ADR-0009's text, which you delegated and have not reviewed, and ADR-0007's two additions marked "for the owner's review" (item 122).
- Name the person who signs off the first driver written from reference reading, when one exists (item 121).

## The kernel's random-number generator (ADR-0016): Accepted 2026-11-10, the mechanism built, nothing in the kernel

Decision item 113 accepted one generator in outline. [ADR-0016](../adr/ADR-0016-kernel-random-numbers.md) filled the outline in and ended with seven questions, each with a recommended answer checked against CLAUDE.md 7.4, the accepted ADRs and NIST's SP 800-90A, 90B and 90C. The seventh, a download, is item 145 above. You answered **"Accept all"** to the other six on 2026-11-10, in the session that wrote the ADR. The coordinating session put the same six to you the same day, in shorter words, and records the same answer (its commit `44de977`).

Three of them change something you had already accepted. The ADR said so before you answered:

- item 147 changes ADR-0002 B4: boot info goes to 0.6;
- items 147 and 148 change ADR-0004 H3: the loader's slides come from a generator of its own, as two requests, and the firmware's generator is never a source by itself;
- item 146 carries out ADR-0013 Amendment 1, which you accepted with item 113: one row of its table moves when the generator lands.

| # | Question | Answer | Where it is recorded, and its state |
|---|---|---|---|
| 146, answered | The design (R1 to R9, R11 to R13): one HMAC_DRBG over SHA-512; a root generator and one for each processor, in the shape of SP 800-90C's tree of generators with two named differences; health tests on the instructions' outputs; no generator and no randomness without a healthy seed; overwriting in three layers; a new crate for the policy? | **Accept** | ADR-0016. The mechanism is built and tested on the host. Everything else is task 18b |
| 147, answered | Boot info 0.6 (§84): one appended field, `boot_seed`, the physical address of 64 secret bytes inside the boot-info region or 0, which the kernel overwrites once it has taken them? | **Accept** | ADR-0016 R10; ADR-0002 B4. Not built: `origin-version.toml` stays at 0.5 until 18b |
| 148, answered | What may seed, and what happens when the sources fail? RDRAND with no RDSEED: the generator runs, the boot says so, and requests for prediction resistance are refused. The firmware's seed with neither instruction: no generator. Every instruction failing after boot: the root stops and the processors' generators go on with the seeds they have. | **Accept, as stated.** The stricter alternative to the third part, stopping all randomness, was offered and not chosen | ADR-0016 R3, R6, R8; ADR-0004 H3's note. Not built |
| 149, answered | What "health tests on the raw source" (CLAUDE.md §59) can mean: tests on the instructions' outputs, because the noise behind them cannot be reached; gross failure only; no SP 800-90B conformance claimed? | **Accept** | ADR-0016 R4. Not built. Draft 7.4's §59 says so since `44de977` on `docs/design-review-2026-11-10` |
| 150, answered | Keep `unsafe` out of `origin-crypto`: best-effort overwriting there, and the guarantee in the framework, by volatile writes over the state and an overwrite of the stack after every request? | **Accept** | ADR-0016 R9. The best-effort part is built; the framework's part is 18b |
| 151, answered | CLAUDE.md §17: pin SP 800-90A to Rev. 1 (June 2015) and SP 800-90B to January 2018, and add SP 800-90C (September 2025) and FIPS 198-1 (July 2008)? | **Accept** | ADR-0016 R1. CLAUDE.md is not changed on this branch. Draft 7.4 on `docs/design-review-2026-11-10` names them in §17 since `44de977`; it reaches `main` on your word (item 106) |

**Yours to do:** nothing new. The wording of §17 and §59 is in draft 7.4, which you read with its pull request (item 106).

The numbers start at 146 because 142 to 144 are in use on `land/s3-part2` and 145 is the download above. The header of this sheet still counts items to 123: it is brought up to date when the branches that hold items 124 to 151 meet.
