# Threat model: address spaces that change while they run

| | |
|---|---|
| Required by | CLAUDE.md §58 ("Threat-model every privileged interface"), §28, §29, §73; ADR-0011 N14 step 3, task S3-5a |
| Covers | `framekernel/frame/src/userspace.rs`, `framekernel/frame/src/aliases.rs`, `framekernel/frame/src/tlb.rs`, `framekernel/frame/src/trap.rs` (the `Switch` token), `framekernel/frame/sync/src/shootdown.rs`, `framekernel/frame/sync/src/refcount.rs`, `framekernel/frame/src/vm.rs` (the kernel half), `framekernel/paging`; ADR-0004 H2 |
| Written | 2026-11-10, with the implementation. Updated the same day with the fixes of its independent review (below, "Review findings"). S3-7 combines it with the threat model of the system-call boundary. |
| Evidence | QEMU only. "Proven" means a boot proof with a negative control under QEMU 11.1 TCG. "Modelled" means a Loom model of the real code, with a committed negative control, within the bounds it prints. Nothing has run on hardware (§74). The paging rules are as the code cites them, from Intel SDM Vol. 3A §4.10 ("Caching Translation Information"), §4.10.4 and §4.10.5; those sections were not re-read for this document. |

## What is being protected

- What a frame holds, from anyone who is not its current owner. A frame moves between owners when one space lets it go and the allocator gives it to another.
- The kernel's own mappings: every address space must reach the same kernel.
- The kernel's progress: a process's map and unmap calls must not be able to stop it.

## Who the attackers are

| Actor | Can | Today |
|---|---|---|
| A hostile process | Touch its pages from every processor its threads run on, at any moment; hold a translation cached by not entering the kernel; call map, unmap and protect at any rate, in any order, from any of its threads | The main threat. No system call exposes these yet: task S3-5b adds them, and the boot proofs act for the process until then. |
| Two processes that share a frame | One can unmap while the other reads | The same, with the sharing call S3-5b adds. |
| A buggy first-party kernel caller of the framework | Pass a wrong range, or call with a lock held | In scope, as error rather than malice. The API is safe Rust and refuses what it can. |
| A processor that stops answering | Hold up every shootdown that is for it, by a hardware fault or a kernel bug that keeps interrupts off | In scope as a fault. No process can do this: ring 3 cannot mask interrupts. |

**Out of scope:** physical attacks, a kernel already compromised some other way, device DMA (the IOMMU threat model; memory-object frames stay out of IOMMU domains in step 3), and side channels, including speculative use of a stale translation in the window before a shootdown completes (ADR-0004 H5).

## Threats

Status: **Proven**, **Modelled**, **By design** (no proof), **Partly**, **Open**.

### A frame reused too early

| # | Threat | Defence | Status |
|---|---|---|---|
| T1 | A frame is reused while a processor still holds a translation to it, so its next owner's data is read through the old mapping | The page table entry goes first, then a shootdown completes, and only then is the frame released (`Shootdown::retire`). Every access through a cached translation is ordered before the release | **Proven** and **Modelled**: boot proof `unmap_before_reuse` (a frame reused as soon as the unmap returns, holding a secret the old reader never reads); `deferred_free_comes_after_the_last_use_of_a_stale_translation`. The controls that reorder release and shootdown, and that drop the initiator's Acquire on the answers (`initiator_reads_answers_relaxed`, review finding 7), are Loom's: a boot cannot observe either |
| T2 | A frame shared by two spaces is freed while one of them still maps it | Each mapping holds a reference, given up after its own shootdown; the frame goes with the last reference (`RefCount`) | **Proven** and **Modelled**: boot proof `shared_between_spaces`; `deferred_free_of_a_frame_shared_by_two_spaces`, with three controls on the count |
| T3 | A processor that does not answer is left holding a translation, and the frame is reused anyway | `retire` releases nothing on a timeout and leaks the frame for good. The caller gets `ChangeError::Stalled` | **By design**; host tests only (no boot proof can stall a processor on purpose) |
| T4 | A frame is unmapped from a space that is being torn down while a processor is still in it | A space's memory is freed only when no processor has it loaded (`tlb::root_loaded`); otherwise it is leaked | **Proven** by the existing proofs of when a space goes |
| T5 | The kernel copies into a page as it is unmapped, and ends up writing into the frame's next owner | The copy runs through this space's own page tables and recovers from a fault at the instruction; the frame is not released until this processor has answered the shootdown | **By design** |
| T28 | A reference count wraps round to zero, so the object or a shared frame is freed under its holders (review housekeeping) | Past `isize::MAX` references `RefCount::retain` reports `TooMany` and `ObjectRef::clone` panics, which halts and never unwinds, as `Arc` aborts | **By design**; a host test drives the count to the limit |

### Stale translations

| # | Threat | Defence | Status |
|---|---|---|---|
| T6 | A thread keeps using a page after the unmap returned | The shootdown reaches every processor with the space loaded and waits for each | **Proven**: boot proof, on another processor |
| T7 | A processor entering the space as the unmap runs is missed, and caches the old translation | `switch_space` announces the new root, then fences, then loads CR3; `shoot` changes the tables, then fences, then reads who is in the space. One or the other must see the change | **Modelled**: `shootdown_and_a_processor_entering_the_space`, with a control removing the fence on each side |
| T8 | A processor leaving the space is passed over while it still holds translations | A processor counts as holding both roots until its CR3 write is done, and the two are read in the order that cannot miss it | **Modelled**: `shootdown_and_a_processor_leaving_the_space`, with three controls; the third reads the two in the other order (`holds_reads_leaving_first`, review finding 7) |
| T9 | Write or execute access outlives a `protect` | Every change of permissions shoots down, and waits | **Proven**: boot proof `protect_stops_a_writer` |
| T10 | A permission added by `protect` is not yet seen, the access faults, and the fault ends the thread (Intel SDM Vol. 3A §4.10.4.3) | `protect` shoots down for additions too | **Partly**: boot proof passes, but QEMU's TLB cannot show the spurious fault this prevents: **hardware qualification must check it** |
| T11 | A processor is passed over because it was not online yet | The two sides fence, as for T7 | **Modelled**: `shootdown_and_a_processor_coming_online` |
| T24 | The record of a processor's space disagrees with its CR3, so a shootdown passes it over, or a space it runs on is freed: an interrupt between the record and the CR3 write lets the scheduler switch the processor itself (review finding 1) | `switch_space` writes the record and CR3 with interrupts off, and reads the processor and the root it leaves inside | **Modelled**: `a_switch_and_the_scheduler_on_one_processor`, whose interrupt flag is a lock, with the control `a_switch_that_can_be_interrupted` |
| T25 | A space activated outside the scheduler is undone when the scheduler next resumes the thread, so a copy checked against one space runs through another's tables, or a system call returns to ring 3 on them (review finding 2) | `UserSpace::activate` and `vm::activate_kernel_space` take a `trap::Switch`, which only the trap entry makes, for the scheduler's hooks. A proof reads through another space with `UserSpace::probe_read`: a visit with interrupts off, recorded in `visiting` so that a shootdown for that space waits for it, and ended by loading the old CR3 again | **By design** for the token, which the compiler checks; **Modelled** for the visit: `shootdown_and_a_processor_visiting_the_space`, with the control `a_visit_that_is_not_recorded` |
| T26 | A stalled shootdown is forgotten: a retry finds nothing left to change, sends nothing and reports success, while the processor that never answered keeps the old, perhaps writable, translation (review finding 3) | `Confirmation` marks the space. The next `unmap` or `protect` first flushes every processor in it (`Request::NonGlobal`), and only a completed flush clears the mark; the W^X counts of the stalled change stay taken until then | **By design**; host tests (no boot can stall a processor) |

### The kernel half

| # | Threat | Defence | Status |
|---|---|---|---|
| T12 | A kernel mapping made after a process was created is missing from that process, so the kernel faults while on its tables | Every entry 256–511 of the kernel's PML4 has a table before anything can copy it (ADR-0004 H2); the entries are recorded once and never change | **Proven** under QEMU TCG: boot proof `prove_kernel_half`, with an in-boot control (a space lacking the slot faults) and a control that restores the old behaviour. Review finding 4: on a real processor (CI, AMD EPYC under KVM) the in-boot control did not fault, because the probe page was global and its translation survived the CR3 load. The probe page is now not global, each read drops its translation first, and the proof checks the G bit, which a TCG boot can see (control C24). That the control now faults on a real processor is for CI's next KVM run to show |
| T29 | A safe caller builds a space that lacks part of the kernel half, and the kernel runs on it (review housekeeping) | `UserSpace::without_probe_slot` is crate-private, used only inside `KernelProbe::read_without_its_slot`, which visits the space for one read and drops it before it returns | **By design** |
| T13 | The kernel half differs between spaces | Every space copies the recorded 256 entries; the proof compares each space with them, ignoring only the bit the processor sets | **Proven** |
| T14 | A kernel page is unmapped and a stale copy lingers | Unchanged: `Scope::Kernel` reaches every online processor | **Proven** by the TLB proof, unchanged |

### A hostile loop, and contention

| # | Threat | Defence | Status |
|---|---|---|---|
| T15 | A process maps and unmaps in a loop to interrupt other processors | Only processors with the process's own space loaded are interrupted, so its threads are what it can disturb; the proof counts 0 interrupts on a processor outside the space, and in the model `shootdown_for_a_space_asks_only_the_processors_in_it` interrupting such a processor fails the model at once (control `a_space_request_asks_everyone`, review finding 7). Each call reports its cost (`Change`), and `processors_loaded` prices the next one | **Partly**: charged nowhere yet. **S3-5b must charge or limit it** |
| T16 | Every shootdown in the kernel goes through one turn, so a hostile process's requests delay other processes' and the kernel's | The turn is held only for the round trip, and a waiter serves the request in progress | **Open**: a process can slow others while it runs. S3-5b's limit bounds it; per-initiator requests would remove it |
| T17 | A request whose range is huge holds the kernel | A request is walked by what is mapped, not by its size, in chunks of 128 pages with interrupts on between them | **By design** |
| T18 | Mapped-page bookkeeping is slow with interrupts off | A chunk's work is bounded, and the sorted records' largest memmove is bounded by `MAX_TABLES` (about 14,800 pages, under 500 KiB) | **Partly**: S3-5b raises the limits at its own cost; measure before it does |
| T19 | Two initiators, or a waiter on a space's editor lock, hold up each other's shootdown forever | The space's short lock is never held across a shootdown. Whoever waits for the turn or for the editor lock serves shootdown requests while waiting | **Modelled** for two initiators (`shootdown_two_initiators_serve_each_other`; its control now fails with "a processor the shootdown asked never answered", review finding 7); the editor's waiting loop by design, and its unbounded wait (review finding 6) is unchanged |
| T20 | A kernel caller holds a lock another processor spins on with interrupts off, and shoots down | Documented rule on `unmap` and `protect`. A violation times out as `Stalled`, which leaks; it cannot corrupt memory | **Partly**: the rule cannot be checked by the compiler |

### What a mapping allows

| # | Threat | Defence | Status |
|---|---|---|---|
| T21 | A page is writable and executable | `Access` has no such variant, and both `map` and `protect` take their bits from one function | **By design**, host tests |
| T27 | A frame shared between mappings is writable through one and executable through another, in one space or two, so it is written as data and run as code (review finding 5) | `Aliases` counts each shared frame's writable and executable mappings in one word. A mapping is counted before its entry is written and given back only after the shootdown that removes the access completes; `map_shared` and `protect` refuse a change that would give the frame both (`ChangeError::WriteAndExecute`), and `protect` checks its whole range before it changes anything. A direct step from writable to executable is refused too: it goes through read-only | **Proven**: boot proof `shared_frames_keep_w_xor_x`, with the control C25 (a count that refuses nothing fails the boot); host tests, including racing takers |
| T22 | A process maps something of the kernel's | The API maps only frames the kernel gave it, into the user half; `user_range_end` refuses anything else | **By design** |
| T23 | The kernel reads a user frame as a Rust object while ring 3 changes it | Frames shared with ring 3 are reached by volatile copies only (`write_frame`, `read_u64`, `SharedFrame`) | **By design** |

## Review findings (2026-11-10)

An independent review of S3-5a by one AI reviewer (`docs/engineering/REVIEW_S3-5A_2026-11-10.md`; no human has reviewed this code). The fixes are on `claude/s3-5a-review-fixes`, one commit per change.

| Finding | Fix | Rows | Commits |
|---|---|---|---|
| 1 (high): the record could disagree with CR3 | The record and the CR3 write with interrupts off, the processor and the root it leaves read inside | T24 | `de67a7c` |
| 2: an activation outside the scheduler is undone on resume | `trap::Switch` for activation; a recorded visit for the proofs | T25 | `e00402b` |
| 3: a stalled `protect` is forgotten | `Confirmation`, cleared only by a completed flush of the space | T26 | `f8eb5d5` |
| 4: the kernel-half control does not fault on a real processor | The probe page not global, its translation dropped before each read, its G bit checked | T12 | `40c8299` |
| 5: W^X per mapping only | `Aliases`, across every mapping of a shared frame | T27 | `7187c2f` |
| 6: `edit()` waits without bound | Not changed: the review's resolution covers findings 1–5 and 7. The rule on callers stays documented | T19, T20 | none |
| 7: gaps in the models and controls | An interrupted switch modelled (finding 1); a visit modelled (finding 2); controls for `holds()`'s read order and the initiator's Acquire; the two controls that counted Loom's branch limit as a catch now fail with their property's message (a bystander that fails the model when interrupted; an initiator that gives up after `PATIENCE` polls) | T1, T8, T15, T19, T24, T25 | `de67a7c`, `e00402b`, `20d0259`, `53b7b32`, `67948b5` |
| Housekeeping: a public constructor of a space without the probe slot | Crate-private, used only within one read | T29 | `51c6823` |
| Housekeeping: no overflow guard on `RefCount::retain` | `TooMany` past `isize::MAX`; `ObjectRef::clone` panics, which halts | T28 | `090d069` |
| Housekeeping: ADR-0004 H2's table and the code disagree on where the heap and stacks live | Decided by the owner on 2026-11-10 (ADR-0004 Amendment 1, items 98–100): the heap stays in the direct map, as the ADR now says; the stacks move to their region, which is not built yet; slot 480 is recorded | none | the amendment |

## What must be closed before real hardware and real programs

1. **T15 and T16**, before S3-5b lets a program call these: a hostile loop must be charged or limited.
2. **T10**: a spurious fault after `protect` is the one behaviour QEMU cannot show.
3. **T3**: a stalled processor leaks memory silently but for the caller's error; nothing records it (§69).
4. **PCIDs (ADR-0004 H5):** the targeting in `Scope::Space` rests on a CR3 load dropping a space's translations. With PCIDs it does not, and the shootdown must then also reach processors that have left the space, or use a generation per space.
5. **Device DMA to shared frames:** before a memory-object frame enters an IOMMU domain, it needs pinning and an IOTLB invalidation in the same place as the shootdown (the research ledger's note on `iommu::map`).

## What this does not prove

- Anything on physical hardware (§74, rule 30). Finding 4 was found on a real processor under KVM in CI, and its fix has not yet run there.
- Anything past each Loom model's printed bounds, or what Loom's memory model leaves out (`models.rs`, "What is not proved").
- That the stand-in machine in the Loom models behaves like a processor's page walker: it is a boolean and a relaxed load.
- The frame contents' confidentiality against side channels.
