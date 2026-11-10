# Open owner decisions

| | |
|---|---|
| As of | 2026-11-09, on `main`, after the pre-merge review. Everything that was on a side branch is merged. |
| Checked against | CLAUDE.md v7.1, item by item. That check changed the first version of this sheet. |
| Evidence | Everything below is proven under QEMU only. Nothing has run on physical hardware (CLAUDE.md §74). |
| Answered | 2026-11-08: items 1–29. 2026-11-09: items 30–70. |
| Still open | No item. Buying machines is off for now (ADR-0009's update of 2026-11-09). |

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

- Tasks collect on one landing branch per batch, as a draft pull request. CI runs on every push to it, so each task is tested, including the real-CPU boots, as it arrives. The first batch is ADR-0011 N14 step 3, pull request 13.
- Commits are small and self-contained: each one builds and passes its tests.
- Merge commits from `main` don't count toward the 100, and nothing is padded to reach it (CLAUDE.md §7).
- Merging still takes the owner's word.

**Later:** the Microsoft signature for the first-stage loader (ADR-0002 B7), once that stage exists; USB-IF and Thunderbolt licensing (§35), when that work starts.

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
