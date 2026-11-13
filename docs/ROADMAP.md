# Roadmap: each milestone's exit tests

| | |
|---|---|
| Required by | CLAUDE.md §82: "Do not mark a milestone complete without its exit tests. Each milestone's exit tests are listed in `docs/ROADMAP.md` before it is marked done (decision item 122)." |
| State | **M0, M1 and M2's exit tests and the rule for later milestones are accepted.** The owner accepted them on 2026-11-12 as decision items 252 to 256 ("Accept all 7", which covered items 252 to 258): 252 for M0, 253 for M1, 254 for M2, 255 for what QEMU and guests under KVM can show, 256 for the rule. **M3, M4, M5, M6 and M10's tests are accepted too,** as decision items 259 to 263 ("Accept all 5", 2026-11-12). **Items 264 to 268 corrected the roadmap** ("Accept all 5", 2026-11-12): every counted `build` row is placed (264), two machines for M4 and M5 (265), Secure Boot at M9 (266), `ONBF.md` §13 in M6 and M7 (267), the test trust store for M1 (268). **Item 269 accepted the further rows of M0 to M6 and M10** ("Accept both", 2026-11-12): the rows each of those milestones adds beyond its tests, under "Further rows placed here", with ADR-0013's rows moved from M0 to M1 and ADR-0011 N13 from M0 to M6. The decision sheet records them all through the coordinating session's records pull request to `main`. **The tests of M7, M8, M9, M11, M12 and M13 are proposed,** written by item 256's rule, and are not accepted, except where an item above says otherwise. **The rows those six milestones add beyond their tests are proposed** with them, until the owner answers on them. |
| Milestones done | **None.** A milestone is done when every one of its accepted exit tests passes in its declared scope (items 255 and 256). M0, M1 and M2 are built. |
| Machines | **No machine in the declared scope can be rented** (ADR-0011 L9). ADR-0009 Amendment 1 A1 puts the EPYC 9005 generation outside the scope, and buying is off (ADR-0009's update of 2026-11-09). Until a machine in scope exists, through a volunteer or a purchase, no milestone from M1 on can be done. |
| Written | 2026-11-12, by TASKS item 25 at `land/s4` `5ea97ef`; corrected by TASKS item 42 from findings F0 to F11 of `docs/engineering/DESIGN_READ_2026-11-12.md`. |
| Rows | The IDs are rows of the requirements matrix (`docs/requirements/`). What a row's state means is in its `README.md`. A row's state is the matrix's; this page does not repeat it, so it cannot go stale here. |

Until 2026-11-12, `docs/STATUS.md` marked M0 "done", M1 "done (developer mode)" and M2 "done (baseline)", before any exit test existed. §82 forbids that, so at the coordinating session's request those three marks now say "built", with a pointer to the tests here. By the accepted tests, none of the three passes today (see each milestone's "Marked against the tests").

## How a row is placed, and the gate that reads this page

- A row is placed in a milestone when a table in that milestone's section names it in a column headed "Rows". A row in the table of "Rows in no milestone" belongs to none, for the reason given beside it.
- A cell of a "Rows" column holds row IDs, ranges ("S8-01 to S8-09", taken in the matrix's order within one file), and whole prefixes ("A17-*", every row of ADR-0017), separated by commas. Other words in it, such as "the rows above", name nothing.
- `cargo xtask roadmap-gate`, part of `cargo xtask check`, reads this page and the matrix (`developer-tools/xtask/src/roadmap.rs`). It fails on a row ID the matrix does not hold, anywhere in a "Rows" column; on a counted `build` row (not `info`, not `removed`) placed in no milestone and not listed in "Rows in no milestone"; and on a row placed in two milestones, or in a milestone and in that list. A row may appear in several tests of one milestone. It fails closed when it reads no matrix row or no placement.
- Rows of kind `rule` need no place (rule 3 below). Some are placed, where a test checks them.
- The gate does not judge whether a placement is right, or whether a test's text matches its rows.

## The rule for writing exit tests (accepted: decision items 255 and 256)

1. A milestone's exit tests are written here before its first task's contract (`docs/engineering/TASK_CONTRACT_TEMPLATE.md`), or now for milestones already started (M3, M4, M5, M6 and M10), and are put to the owner as one decision item before the milestone is marked done. M6 counts as started because TASKS item 41 (task S4-1, on its own branch) builds the native Rust user runtime.
2. Each test says what passes, through the entry point a supported caller uses (CLAUDE.md §1: "Test through actual entry points"), names its negative controls, and names the matrix rows it completes. A row's acceptance criterion becomes the test's text (`docs/requirements/README.md`, "Acceptance criteria").
3. A milestone covers the rows of its sentence in §82. A row of the directive that no sentence names goes to the milestone whose sentence names its subject, and otherwise to M13. A row of kind `rule` in the project's own scope (`eng`) binds from the day it was written and belongs to no milestone.
4. A milestone is done when every exit test passes in its declared scope and every row it names is `done`. Where the rows' scope is hardware, that needs a record of a physical machine in ADR-0009 Amendment 1's scope under `docs/hardware/records/` (§74, final rule 30). Under QEMU, or as a guest under KVM, a milestone is at most built.
5. The Server edition follows the same milestones (ADR-0014 S2; ADR-0015 D9). Each edition is qualified for its own declared scope (§76). Where its work falls is below, under "The Server edition".

## Before a first boot on a machine in scope

These are accepted rules, not new decisions (finding F10). Each hardware test below (M1-5, M2-6, M3-6, M4-7, M5-6, M6-6, M7-5, M8-6, M9-7, M10-7, M11-3, M12-6, M13-1) needs them before its first boot on a machine in the declared scope:

- **ADR-0009 H6 and `THREAT_MODEL_IOMMU.md`:** T4's two windows, T5, T6, T8, T9 and T23 are closed, or accepted by the owner by name for that machine, and T14 is closed. Decision item 230 (on `main`) accepts them "for bench boots on rented servers only", and the acceptance "does not reach a volunteer's machine".
- **`THREAT_MODEL_SYSCALLS.md`:** E12 (the nested NMI) and I2 (ADR-0017's controls read back on a processor in scope) before any claim about real hardware.
- **`THREAT_MODEL_ADDRESS_SPACES.md`:** T10, which only hardware shows.
- **`THREAT_MODEL_SYSTEM.md`,** "Any claim about real hardware": the IOMMU model's list, and ADR-0017's proofs on a processor in scope.

## M0. Toolchain and engineering foundation

**Accepted: decision item 252.**

§82: "Pin Rust 1.100.0, validate cross-compilation, establish repository, tests, research governance, source provenance and CI." Scope: the project's own tools, on the development host (`eng/host`). Matrix row S82-01.

| Test | Passes when | Rows |
|---|---|---|
| M0-1 | `rust-toolchain.toml` is §8's, and `developer-tools/toolchain/verify.sh` passes on a clean tree, its output recorded in `docs/toolchain/VERIFICATION.md` | S8-01 to S8-09, S8-11, S9-01 to S9-11, S84-02, S84-03, S84-05 |
| M0-2 | §10's three regression tests pass, the `wcslen` test inside the `no_std` UEFI loader, run by `verify.sh` | S10-01 to S10-04, S10-06 |
| M0-3 | CI runs on every pull request and on `main` with exactly the pinned compiler, its commit checked, `--locked`, `verify.sh` and `cargo xtask check` | S8-07, S8-10 |
| M0-4 | `cargo xtask check` passes: each artifact built for its own target, the release profile linked, the freestanding libraries checked on both targets | S11-01, S11-02, S12-01, S84-06, S84-11 |
| M0-5 | The release identity is generated from `origin-version.toml`, and a lint refuses a hand-written crate version | S62-01, S62-16, S62-17, S84-07 |
| M0-6 | `Cargo.lock` holds only the workspace's packages, and the one host-only exception (Loom) is approved and pinned | S5-01, S5-10 |
| M0-7 | The records are kept: the requirements matrix and the provenance table, each read by a gate in `cargo xtask check`; a §78 contract for each task, following `docs/engineering/TASK_CONTRACT_TEMPLATE.md` and checked in review; the ADRs, the research ledgers and the decision sheet | S6-01 to S6-13, S78-01 to S78-11, S79-06 to S79-09, S84-08 |

**Further rows placed here: accepted (item 269).** They are in no exit test; the owner accepted them as part of M0's scope.

| Rows | Subject |
|---|---|
| S82-01 | The milestone's own sentence |
| S1-03 | Coverage measured against the versioned matrix |
| S6-15, S6-17 to S6-19 | Automated provenance and licence checks; `CONTRIBUTING.md`, `SECURITY.md` and `CODEOWNERS` (TASKS item 12) |
| S13-10, S14-01 to S14-11, S14-16 | The compiler and linker reviews, overflow checks and the integer features (TASKS item 22) |
| S18-15, S18-16 | ADRs for original-source exceptions and for hardware qualification |
| S72-01, S72-02, S72-04, S72-06, S72-08 | Unit, integration, model-based, concurrency and kernel boot testing |
| S79-02 to S79-05, S79-13 | `CLAUDE.md`, `origin-version.toml`, `rust-toolchain.toml`, the specifications and the session handoffs kept |
| S84-01, S84-04, S84-12 | The repository inspected, QEMU and OVMF verified, boot tests run and captured |
| A18-V2, A18-V5, A18-V8 | The performance cost policy's record, its cost ledger and the gate on later ADRs' costs (ADR-0018; placed when landing, after ADR-0018 reached the batch) |

**Marked against the tests, 2026-11-12: built, not done.** M0-3 is not shown until CI has run the workflow as TASKS item 22a changed it, and that needs a push. M0-1 and M0-2 pass on the development host since 22a.

| Test | On the tree | Evidence |
|---|---|---|
| M0-1 | Passes on the development host: `verify.sh` recorded on 2026-11-12 at `b17dc8d`, from a tree with nothing uncommitted (PASS 20, FAIL 0). `rust-toolchain.toml` has §8's settings; one comment says "CLAUDE.md §11" where §8's block says "§11" | `docs/toolchain/VERIFICATION.md`; `SOURCE_PROVENANCE.md`, "TASKS item 22a" |
| M0-2 | Passes on the development host: `verify.sh` runs all three, the `wcslen` test in the loader's own library and link, and a planted fault fails each. CI's workflow runs it; no run exists yet | `docs/toolchain/VERIFICATION.md`; `SOURCE_PROVENANCE.md`, "TASKS item 22a" |
| M0-3 | Built, not shown: the workflow runs `verify.sh`, gives every cargo command `--locked`, and every job fails unless the compiler reports the pinned commit. No run of it exists | `.github/workflows/ci.yml`, `.github/ci/install-rust.sh`; TASKS item 22a |
| M0-4 | Passes | `cargo xtask check`, run on every merge into the batch |
| M0-5 | Passes | `cargo xtask check` (the §62 lint) |
| M0-6 | Passes | `cargo xtask check` (`dependency_check`) |
| M0-7 | Passes: both gates are in `cargo xtask check` (the provenance gate since TASKS item 25), and the template exists. Three pieces of work done since item 122 have no contract table (S3-5b, H2-stacks, TASKS item 39; the template's comparison), which review, not a gate, is to catch | `developer-tools/xtask/src/requirements.rs`, `developer-tools/xtask/src/provenance.rs`, `docs/engineering/TASK_CONTRACT_TEMPLATE.md` |

## M1. Bootable Framekernel

**Accepted: decision item 253; M1-1's key by item 268.**

§82: "Implement UEFI boot, kernel loading, memory handoff and diagnostics." Scope: both editions, on hardware (`both/hw`). Matrix row S82-02.

| Test | Passes when | Rows |
|---|---|---|
| M1-1 | The loader, a UEFI application built reproducibly, reads the KRN image, verifies it (its content digest, each section's digest or Merkle root, and its signature outside developer mode), places and relocates it, prepares the stack, the page tables and the processor's state, exits boot services correctly and enters the kernel. The signature check is met with the test trust store (`boot/uefi/trust/`), on a build that says so at every boot (ADR-0002 B8; `ONBF-errata.md` V4; item 268) | S19-01 to S19-03, S19-07 to S19-10, S84-09 |
| M1-2 | The memory handoff: the firmware's memory map, its ACPI root and its framebuffer reach the kernel in boot info, which the kernel validates and refuses when malformed | S19-04 to S19-06 |
| M1-3 | Diagnostics: the loader's and the kernel's lines reach the serial port the firmware names (its SPCR) and the framebuffer; a kernel fault prints a report with a backtrace | S19-11, S84-10 |
| M1-4 | Negative controls: a tampered image, a wrong key and a bad signature are each refused with their own message | S19-03 |
| M1-5 | M1-1 to M1-4 pass on at least one physical machine in ADR-0009 Amendment 1's scope, with a record under `docs/hardware/records/` | the rows above |

Origin's own kernel key belongs to the signing ceremony (§60, TASKS item 6) before the first public release (item 268), and so to M13. Given to a later milestone: Secure Boot with Origin's own keys (S19-12, S19-15) to M9 by item 266; recovery boot (S19-13) to M12 by rule 3. S19-14 is a rule that binds now. `ONBF.md` §12 has the loader verify SYS as well as KRN, and §8.1 has it check the rollback floor and measure into the TPM: those wait for ADR-0002's Amendment 2 (TASKS item 28) and are M12's (finding F9).

M1-5 needs the prerequisites in "Before a first boot on a machine in scope".

**Further rows placed here: accepted (item 269).**

| Rows | Subject |
|---|---|
| S82-02 | The milestone's own sentence |
| A02-B1 to A02-B6, A02-B8 | ADR-0002's boot decisions: where the kernel comes from, the order of verification, placement, boot info, leaving the firmware, entering the kernel, developer mode |
| A01-def5 | Merkle-SHA256-4K, which the loader verifies |
| A04-H3, A04-H4 | KASLR; the loader builds the first page tables |
| A09-H6 | Before the first boot (ADR-0009) |
| A10-E1 to A10-Am2 | The early console (ADR-0010 and its amendments) |
| A16-R10, A16-Am1 | The loader's generator, its seed for the kernel, and the stack region's value |
| S13-01, S13-03, S13-08, S13-09 | `no_std`, explicit panic handling, defined stack and calling conventions, checked parsing of what the firmware hands over |
| A13-R1 to A13-Am1 | What a failed boot proof means (ADR-0013): the test policy every boot runs under, on each declared machine (moved from M0 by item 269) |

**Marked against the tests, 2026-11-12: built, not done.** M1-1 to M1-4 pass under QEMU's software processor and as guests under KVM in CI (`cargo xtask boot-test`, `boot-matrix`; `docs/STATUS.md`), signed with a test key only. M1-5 fails: no Origin code has run on a physical machine. The one rented machine so far ran Ubuntu, was a Zen 4 bench machine outside the declared scope, and qualifies nothing (`main`, `docs/research/2026-11-12-rental-a-findings.md`).

## M2. CPU and memory

**Accepted: decision item 254.**

§82: "Implement CPU initialization, exceptions, interrupts, physical/virtual memory." Scope: both editions, on hardware. Matrix row S82-03.

| Test | Passes when | Rows |
|---|---|---|
| M2-1 | CPU initialization: features discovered from CPUID, a processor below x86-64-v3 refused with what it lacks, the kernel's own GDT, TSS and IDT, FPU and SIMD state saved and restored | S24-01, S24-02, S24-16, S24-17 |
| M2-2 | The core generation named at boot from one table, which never refuses to boot (ADR-0009 Amendment 1) | S24-18 to S24-20 |
| M2-3 | Exceptions: every vector dispatched, a double fault on its own stack, a kernel fault reported with a backtrace, a breakpoint resumed | S24-03 |
| M2-4 | Interrupts: the local APIC or x2APIC timer calibrated; a device interrupt routed through the I/O APIC, edge-triggered and level-triggered | S24-04, S24-05 |
| M2-5 | Physical and virtual memory: the frame allocator, the kernel's own page tables (W^X, an NX direct map, nothing in the lower half, SMEP and SMAP, guard pages), the kernel heap, page faults reported | S28-01 to S28-04, S28-07 |
| M2-6 | M2-1 to M2-5 pass on at least one physical machine in scope, with a record, and each boot proof's negative controls fail the boot | the rows above |

Given elsewhere by rule 3: SMP, topology and TLB shootdowns (S24-06 to S24-14, S28-17, S28-18) to M3; user address spaces (S28-05, S28-06) and speculation controls (S24-15, which ADR-0017 needs before ring 3 runs anything but a boot proof) to M4.

M2-6's negative controls are shown under QEMU: ADR-0013's controls are temporary edits booted there. The proofs themselves pass on the machine (finding F5). M2-6 needs the prerequisites in "Before a first boot on a machine in scope".

**Further rows placed here: accepted (item 269).**

| Rows | Subject |
|---|---|
| S82-03 | The milestone's own sentence |
| A03-K1, A03-K3 to A03-K5 | ADR-0003: capability-typed hardware access, the frame allocator, the heap, x2APIC and the calibrated timer |
| A04-H1, A04-H2, A04-A1 | ADR-0004: 4-level paging with the lower half for user space, the kernel half's layout, the heap in the direct map |
| A09-A2 | The core generation named at boot, which M2-2 tests |
| A11-N6 | The Framekernel's one privileged crate |
| A12-C1 to A12-C8 | The monotonic clock (ADR-0012) |
| S13-02, S13-11 | `alloc` only after an allocator exists; bounded allocation |
| S14-14 | The `x86-64-v3` baseline, with higher features detected |
| S18-01, S18-02 | ADRs for the kernel's architecture and its allocator |
| S28-14 | Large pages |
| S58-07 | W^X |
| S68-01, S68-02, S68-04, S68-05 | The monotonic clock, the wall clock, timer calibration, time consistent across processors |
| S73-02, S73-04 | Kani, first on the allocator's invariants |

**Marked against the tests, 2026-11-12: built, not done.** M2-1, M2-3 and M2-5 pass under QEMU and as guests under KVM (`docs/STATUS.md`, "By milestone"; boot proofs `kernel page tables`, `frame allocator`, `per-processor control state`), with S28-07 partial. M2-2 fails: not built (TASKS item 16). M2-4 is partial: no live level-triggered pin (TASKS item 2). M2-6 fails: no physical machine.

## M3. SMP and scheduler

**Accepted: decision item 259.** **State, 2026-11-12: partial.** In short: processors, threads, preemption, wait queues and fair shares are built; there are no scheduling classes, no load balancing and no topology-aware placement, and nothing has run on a physical machine (matrix row S82-04).

§82: "Implement multiple processors, native threads, preemption, synchronization and scheduling." Scope: both editions, on hardware. Matrix row S82-04.

| Test | Passes when | Rows |
|---|---|---|
| M3-1 | Every processor the firmware lists is started and has its own state; interprocessor interrupts; topology (cores, SMT siblings, caches, NUMA, hybrid capacity); processors taken offline and back | S24-06 to S24-14 |
| M3-2 | Native 1:1 kernel threads, preempted, with their stacks, register state, priorities and affinity, and cleaned up when they end | S25-01 to S25-11 |
| M3-3 | Scheduling: per-processor run queues, wake-ups, timers, fair shares, load balancing, accounting, interactive and background classes, priority-inversion mitigation, quotas and a safe fallback | S26-01 to S26-15 |
| M3-4 | Placement that accounts for SMT siblings, shared caches, hybrid capacity, NUMA, migration cost, power, heat and the foreground | S27-01 to S27-10 |
| M3-5 | Synchronization across processors: TLB shootdowns, cross-core races and lifetimes tested, and Loom models of the locks and wait queues with their negative controls | S28-17, S28-18, S73-01, S73-06 |
| M3-6 | M3-1 to M3-5 on physical machines in scope, one with SMT and one with hybrid cores, with records | S74-03 to S74-05 |

M3-6 needs the prerequisites in "Before a first boot on a machine in scope".

**Further rows placed here: accepted (item 269).**

| Rows | Subject |
|---|---|
| S82-04 | The milestone's own sentence |
| A03-K6, A03-K7 | ADR-0003: SMP start-up and the first scheduler |
| A04-H5, A04-A2 to A04-A6 | TLB efficiency and PCIDs; the kernel stacks' region and its random base |
| A08-C1 to A08-C7 | The CPU index (ADR-0008) |
| A08-D1 to A08-D8 | Any processor count (ADR-0008 Amendment 1, items 283 to 290; placed by the coordinating session under item 264's rule, in no exit test) |
| A09-H4, A09-H5 | The first machines, one hybrid and one with SMT, as M3-6 asks; what to check before buying |
| S13-05 | Explicit memory ordering |
| S18-03, S18-04 | ADRs for the scheduler and for locking (TASKS item 27) |
| S28-13 | NUMA policies |
| S68-03, S68-11 | High-resolution timers and their cancellation |
| S84-14 | SMP and topology work started |

## M4. Processes and IPC

**Accepted: decision item 260; M4-7's two machines by item 265.** **State, 2026-11-12: partial.** In short: isolated processes, system calls, handles, channels, the start record, the directory, memory objects, events and ports are built under QEMU; no call creates a process or a thread, there is no Loader Service, and ADR-0017's controls are partly built (rows S24-15 and S82-05).

§82: "Implement isolated user processes, system calls, capabilities and IPC." Scope: both editions, on hardware. Matrix row S82-05.

| Test | Passes when | Rows |
|---|---|---|
| M4-1 | Processes are created through the kernel's calls, end, are supervised and cleaned up; a crash is contained | S29-01 to S29-11 |
| M4-2 | Each process has its own address space; no process reaches another's memory without a handle that grants it; shared memory through memory objects | S28-05, S28-06, S28-10 |
| M4-3 | Typed, authenticated IPC with capability handles, access checks, cancellation, versioning and lifetimes | S30-01 to S30-12 |
| M4-4 | System calls and their error codes as the user ABI defines them, every pointer and value from ring 3 checked | S42-05, S42-06, S42-17 |
| M4-5 | The speculation controls and processor hardening ADR-0017 decides are in effect on every processor before ring 3 runs anything but a boot proof | S24-15, rows A17-* |
| M4-6 | Kernel and process isolation, capability authorization and secure IPC, each mapped to the threat models' rows with negative controls | S58-04, S58-05, S58-09, S58-11, S58-18, S58-19 |
| M4-7 | M4-1 to M4-6 on two physical machines in scope, one Intel and one AMD, with records: ADR-0017's Z6, Z7 and Z12 are vendor-specific (item 265) | the rows above |

M4-1 rests on ADR-0005 G3 and G4, drafts to be decided again with step 5 and with the Loader Service (ADR-0005 Amendment 8). So M4 cannot close before that part of M6's work (finding F2). M4-7 needs the prerequisites in "Before a first boot on a machine in scope".

**Further rows placed here: accepted (item 269).**

| Rows | Subject |
|---|---|
| S82-05 | The milestone's own sentence |
| A03-K8 | ADR-0003: the first user mode |
| A05-P1 to A05-A6, A05-B1, A05-B3, A05-C1 to A05-E1, A05-F1 to A05-F18 | ADR-0005's process ABI and IPC, as built and amended through events and ports and 64-bit handles |
| A05-G3 to A05-G5, A05-G8 | Where thread and process creation start, which F2 above names; creators pay the kernel-wide bound; a provider accepts connections |
| A11-N9 | The SDK is not a security boundary: the kernel checks everything |
| A16-R1 to A16-R9, A16-R11 to A16-R13 | The kernel's random-number generator (ADR-0016), which every random placement in a process draws from |
| S18-05, S18-06, S18-17 | ADRs for IPC, the user process ABI, and speculation and processor hardening |
| S28-08, S28-09, S28-15 | Copy-on-write, demand paging, out-of-memory handling |
| S73-03, S73-05, S73-07 | Verus evaluated; object lifetimes and capability validation as proof targets |

**One more row placed here, beside G5, which it refines (item 282, 2026-11-13).** Placed by the coordinating session under item 264's rule; it is in no exit test and changes none.

| Rows | Subject |
|---|---|
| A05-I1 | A port's creator pays for its index (ADR-0005 Amendment 10) |

## M5. Storage

**Accepted: decision item 261; M5-6's two machines by item 265.** **State, 2026-11-12: partial.** In short: PCIe, both IOMMUs, NVMe and a block cache are built; no filesystem is mounted, and nothing has run on a physical machine (matrix row S82-06).

§82: "Implement PCIe, DMA/IOMMU, NVMe, block storage and filesystem foundations." Scope: both editions, on hardware. Matrix row S82-06.

| Test | Passes when | Rows |
|---|---|---|
| M5-1 | The driver framework's core: discovery, resources, binding, interrupts, DMA mappings, lifecycle, isolation, reset, faults and recovery, through a documented Driver ABI | S31-01 to S31-10, S31-13, S31-14, S31-16 |
| M5-2 | PCIe: host bridges, enumeration, configuration access, BARs, MSI and MSI-X, DMA through VT-d and the AMD IOMMU with interrupt remapping, error reporting and reset | S32-01 to S32-11, S32-14, S58-10 |
| M5-3 | NVMe: controllers, queues, namespaces, interrupts, timeouts, resets, flushes, integrity, power states and health | S33-01 to S33-15 |
| M5-4 | The storage layers: block I/O, I/O scheduling, partitions, volumes, the page cache and the virtual filesystem, with asynchronous I/O | S39-01 to S39-11 |
| M5-5 | Filesystem foundations: an accepted on-disk format, its durability semantics defined and tested, crash consistency, data and metadata integrity, atomic rename, versioned structures and power-loss recovery | S40-01 to S40-04, S40-14, S40-16, S41-01 to S41-10 |
| M5-6 | M5-1 to M5-5 on two physical machines in scope with an NVMe controller, one Intel and one AMD, with records: VT-d and the AMD IOMMU are separate rows (S32-07, S32-08; item 265) | S74-08 |

Given elsewhere by rule 3: hot-plug and external-device authorization (S31-12, S32-12, S32-13) to M10; the rest of §40 (access control, sparse, large and mapped files, quotas, locking, snapshots, encryption, repair, migrations) to M9. M5-6 needs the prerequisites in "Before a first boot on a machine in scope".

**Further rows placed here: accepted (item 269).**

| Rows | Subject |
|---|---|
| S82-06 | The milestone's own sentence |
| A06-F1 to A06-F5 | OriginFS v0, the read-only bring-up format that never ships (ADR-0006) |
| A07-I1 to A07-A9 | The IOMMU under VT-d and AMD-Vi (ADR-0007) |
| S18-07, S18-08 | ADRs for the Driver ABI and the filesystem |
| S28-11, S28-12, S28-16 | Memory-mapped files, the page cache, reclamation |
| S58-06 | Driver isolation |
| S73-10, S73-11 | Filesystem metadata and DMA mapping ownership as proof targets |

**Five more rows placed here, beside the IOMMU rows they refine (items 247 to 251, 2026-11-13).** Placed by the coordinating session under item 264's rule; they are in no exit test and change none.

| Rows | Subject |
|---|---|
| A07-R1 to A07-R5 | Identity maps for firmware-reserved memory (ADR-0007 Amendment 1) |

## M6. Native runtime

**Accepted: decision item 262; `ONBF.md` §13 and §8.2 by item 267.** **State, 2026-11-12: not started on this tree.** In short: TASKS item 41 (task S4-1) builds the native Rust user runtime on its own branch; nothing of M6 is on this tree yet (matrix row S82-07).

§82: "Implement Origin executable ABI, `.ose` loader and native Rust user runtime." Scope: both editions, on hardware. Matrix row S82-07.

| Test | Passes when | Rows |
|---|---|---|
| M6-1 | The userspace ABI is written and versioned: calling convention, stack, registers, TLS, start-up, allocation, dynamic linking, shared libraries, unwinding, notifications, version negotiation, 64-bit only | S42-01 to S42-04, S42-07 to S42-16 |
| M6-2 | The `.ose` loader verifies and loads an ONBF executable, refuses malformed and hostile images, and is fuzzed | S43-01 to S43-14 |
| M6-3 | The Origin runtime: start-up, allocation, threads, TLS, synchronization, files, time, standard I/O, async I/O, panics and shutdown | S44-01 to S44-07, S44-09 to S44-17 |
| M6-4 | Userspace is built for its own target and never for Linux's APIs | S11-03 to S11-07 |
| M6-5 | Starting a program is a capability, with the manifest gate, no interpreter in the base image and an empty environment by default (ADR-0011 Amendment 1) | S48-05 to S48-10 |
| M6-6 | M6-1 to M6-5 on a physical machine in scope, with a record | the rows above |
| M6-7 | `ONBF.md` §13's obligations whose subject is the parser, verification and launch, taken by reference (item 267): 1, known-answer tests; 2, differential and round-trip tests; 3, structure-aware fuzzing of the header, section table, OCBOR-1 and signature block; 4, the malicious corpus, but for its decompression bomb and certificate cases; 5, Kani on `onbf-format`; 6, a second parser written independently by a different agent; 7, under QEMU, W^X enforced and executing from an unregistered root denied; 9, verification throughput and added cold-launch latency within 5%. With §8.2's sealed objects, Code Integrity Table and mapping rules: RX only from sealed objects whose root is in the table | `ONBF.md` §8.2, §13 |

Given elsewhere by item 262: networking in the runtime (S44-08) to M10, with the network stack, because a sockets API cannot be tested through an actual entry point before a network service exists. M6-6 needs the prerequisites in "Before a first boot on a machine in scope".

**Further rows placed here: accepted (item 269).**

| Rows | Subject |
|---|---|
| S82-07 | The milestone's own sentence |
| H-14, H-18, H-19 | `.ose` and `.osl`, and every native format a profile of ONBF |
| A01-D1, A01-def1, A01-def7 to A01-def10 | ONBF: one container with native segments; `.osl`; the `x86-64-v3` baseline and shadow stack; frame pointers and `panic=abort`; eager binding and full RELRO; private system calls with `origin.core` the only stable ABI |
| A05-B2, A05-E2, A05-G1, A05-G7, A05-G11 | A system call accepted only from its own stub; arguments and environment for the program alone; a launcher that fills a directory; the Diagnostics service's boot report |
| A11-N3 to A11-N5, A11-N7, A11-N8, A11-N10 | The SDK's interfaces, its three promises, the kernel below it, one definition generated everywhere at three costs, versions |
| A11-M1 to A11-M5 | The launch capability, the release gate over manifests, no command interpreter, an empty environment (ADR-0011 Amendment 1) |
| A11-L1 to A11-L11 | Step 4: `origin.core`, its stubs, the launch path, the diagnostics interface (ADR-0011 Amendment 2) |
| A14-S6 | Software written for other systems: Rust `std` first, and a POSIX layer only under an ADR of its own here |
| S14-15 | Shadow stacks, guard pages, W^X, full RELRO and frame pointers in every image |
| S18-10 | An ADR for the executable format |
| S49-02 to S49-06, S49-31 | The SDK for processes, threads, parallel tasks, async I/O and files; stable versioned APIs with a C ABI |
| S58-08 | ASLR |
| S68-09 | Calendar APIs |
| S70-01, S70-05, S70-06, S70-08 to S70-10 | Tools to build `.ose`, link, debug, analyse crash symbols, validate the ABI, document the SDK |
| S71-01 to S71-03, S71-05, S71-06, S71-09, S71-10 | SDK bindings, templates, documentation, threading and async examples, debugging and testing |
| S73-08 | Binary parsers as proof targets |
| S84-13 | Host-testable native-format parsers |
| A11-N13 | The SDK's rules enforced by gates, one of which reads first-party manifests (moved from M0 by item 269) |

**Two more rows placed here: accepted (item 276).** Decision items 272 and 273 (2026-11-13) added them to the matrix after item 269 was answered. They stand beside the rows they extend: A05-H1 states what a thread finds at its entry, as A05-B2's neighbours do for a call, and A11-L13 is one more rule for the stubs of A11-L6. Task S4-2 (TASKS item 47) placed them when its branch first held both the roadmap and the two rows, so that `cargo xtask roadmap-gate` passes; the owner accepted the placement on 2026-11-13. Like item 269's rows, they are in no exit test.

| Rows | Subject |
|---|---|
| A05-H1 | At a thread's entry the direction flag and the six status flags are clear and interrupts are on (ADR-0005 Amendment 9) |
| A11-L13 | Each generated stub extends every parameter narrower than 64 bits to its whole register (ADR-0011 Amendment 3) |

## M7. Native packaging

**Proposed, by the rule of item 256; `ONBF.md` §13 by item 267.** **State, 2026-11-12: not started; groundwork only, below.**

§82: "Implement `.osi`, `.oapp`, `.od`, signing, installation and rollback." Scope: both editions, on hardware. Matrix row S82-08.

| Test | Passes when | Rows |
|---|---|---|
| M7-1 | `.oapp`: identity, the main executable, resources, libraries, localization, capabilities, launch policy, signing and update metadata | S45-01 to S45-11 |
| M7-2 | `.osi`: parsed, signature-checked, resolved, installed in one transaction with no install-time code, repaired, upgraded, removed and rolled back, including after an interruption | S46-01 to S46-15 |
| M7-3 | `.od`: identity, hardware IDs, Driver ABI, firmware, privileges, signature, activation, rollback; third-party drivers in user mode only | S47-01 to S47-12, S31-15 |
| M7-4 | Signed packages verified with both signatures of the accepted suite | S58-13, S59-01 to S59-16 |
| M7-5 | M7-1 to M7-4 and M7-6 on a physical machine in scope, with a record | the rows above |
| M7-6 | `ONBF.md` §13's obligations whose subject is packages and installation, taken by reference (item 267): 3, structure-aware fuzzing of IMAGE_FS; 4, the malicious corpus's decompression bomb and its expired or out-of-namespace certificates; 7, under QEMU, a tampered byte on disk giving an integrity fault and a repair, and the CPUID-mismatch dialog; 8, a power cut injected at every install, upgrade and uninstall step, with the database consistent and no blob leaked; 9, install throughput and the frame-pacing cost of a background install. With §8.2's rules for the Package Service: the Code Integrity Table extended only by it, after signature verification, and repair on an integrity fault | `ONBF.md` §8.2, §13 |

Not started. The tree holds groundwork only: the ONBF container parser knows each profile's allowed sections, and signing and verification exist for the kernel's image (`onbf-format`, `onbf-sign`, `onbf-verify`, `onbf-pack`, which writes KRN only). Nothing parses a `.oapp`, `.osi` or `.od` manifest, builds one or installs one. `THREAT_MODEL_SYSTEM.md`: before a program that is not first-party runs in ring 3, ADR-0017's controls are read back as written on a processor in the declared scope (row I2 of the system-call model). M7-5 needs the prerequisites in "Before a first boot on a machine in scope".

**Further rows placed here: proposed (item 264).**

| Rows | Subject |
|---|---|
| S82-08 | The milestone's own sentence |
| H-15 to H-17 | `.osi`, `.oapp` and `.od` |
| A01-D2 to A01-D4, A01-def3, A01-def6, A01-def12 | ONBF: third-party drivers in user mode only; hybrid signatures; open publishing in tiers; manifests compiled from TOML; zstd for transport; "Try" without installing |
| S18-11 | An ADR for the package format |
| S49-13 | The SDK for installation |
| S70-02 to S70-04, S70-11, S70-12 | Tools to package `.oapp`, create `.osi`, sign `.od`, validate packages, verify drivers |
| S71-08, S71-11 | Installer packaging and deployment guides |

## M8. Native graphics

**Proposed, by the rule of item 256.** **State, 2026-11-12: not started; groundwork only: a software rasterizer, an in-memory file tree and text codecs (matrix row S82-09).**

§82: "Implement Origin UI, software renderer, compositor and first graphical applications." Scope: both editions, on hardware. Matrix row S82-09.

| Test | Passes when | Rows |
|---|---|---|
| M8-1 | Origin UI: windows, controls, layout, events, menus, dialogs, notifications, clipboard, drag and drop, the accessibility tree, themes, scaling and animation | S50-01 to S50-18 |
| M8-2 | The text engine: Unicode 18.0 data, normalization, segmentation, line breaking, bidirectional text, shaping, fallback, emoji, input methods and editing, tested in Telugu, Hindi, Arabic, Latin and mixed direction | S51-01 to S51-18 |
| M8-3 | The compositor, with its software fallback, as a security boundary between programs, with a threat model for §52's four rules written before it is designed (`THREAT_MODEL_SYSTEM.md`) | S52-01 to S52-20 |
| M8-4 | Smoothness measured against the provisional budgets, reported as figures with no claim | S53-01 to S53-10 |
| M8-5 | The first graphical applications, each an SDK client with no private door | S48-01 to S48-04 |
| M8-6 | M8-1 to M8-5 on a physical machine in scope, with a record | the rows above |

These tests need a keyboard and a pointer: ADR-0014 S3 has the GUI take input from a USB keyboard and tablet through xHCI, and ADR-0015 D9 puts xHCI first. This roadmap places USB and HID in M10 (M10-1), so M8 depends on that part of M10's work, or USB moves earlier. It is to be settled when M8's tests are put to the owner (finding F6). M8-6 needs the prerequisites in "Before a first boot on a machine in scope".

**Further rows placed here: proposed (item 264).**

| Rows | Subject |
|---|---|
| S82-09, S82-15, A14-S2 | The milestone's own sentence; the Server edition's GUI, which is M8's (ADR-0014 S2) |
| A11-M6 | A GUI test reads the kernel's record of process creation |
| S18-09 | An ADR for the graphics API |
| S49-01, S49-10, S49-11, S49-19, S49-22 | The SDK for applications, graphics, input, notifications and accessibility |
| S70-13 | UI inspection |
| S71-04, S71-07 | UI component documentation and graphics examples |
| S72-14, S72-15 | GUI and accessibility testing |

## M9. Installable operating system

**Proposed, by the rule of item 256; M9-8 accepted by item 266.** **State, 2026-11-12: not started.**

§82: "Create actual installation media, partitioning, installation GUI, first-run setup and native system management." Scope: both editions, on hardware. Matrix row S82-10.

| Test | Passes when | Rows |
|---|---|---|
| M9-1 | The installer, from media to a booted installed system: storage discovery, GPT, the system partition, deployments, user data, recovery, formatting, encryption, safe disk choice, progress and verification. Storage encryption designed and built, with AC2 decided again with it, before Origin can be installed (`THREAT_MODEL_SYSTEM.md`, EX1) | S21-01 to S21-19 |
| M9-2 | The installer is a native GUI over typed APIs, with no hidden command | S22-01 to S22-03 |
| M9-3 | Origin Setup, offline as well: language, keyboard, time zone, account, credentials, device name, network, encryption, privacy and accessibility | S23-01 to S23-13 |
| M9-4 | Accounts and sessions: local accounts, credentials stored hashed, locking, switching, administrative authorization, recovery and deletion, before Origin can be installed (`THREAT_MODEL_SYSTEM.md`) | S57-01 to S57-13 |
| M9-5 | The filesystem's remaining features for an installed system | S40-05 to S40-13, S40-15, S40-17 |
| M9-6 | Every setting and management task in the native GUI; no terminal needed | S48-11 to S48-13, S48-17, S48-18 |
| M9-7 | M9-1 to M9-6 and M9-8 on a physical machine in scope, with a record | the rows above |
| M9-8 | Secure Boot with Origin's own keys: the installer guides their enrolment, and the installed system boots with Secure Boot on, with a test key until the signing ceremony (ADR-0002 B7 and Amendment 1; `THREAT_MODEL_SYSTEM.md`, "the Secure Boot keys"; item 266) | S19-12, S19-15, S58-01, A02-B7, A02-Am1 |

The installer needs a USB keyboard on a real machine, and USB and HID are placed in M10 (finding F6, as under M8). M9-8 is accepted for its rows only: the rest of M9's tests are proposed. The release manifest's verification, the rollback floor and measured boot stay in M12, with ADR-0002's Amendment 2 (TASKS item 28). M9-7 needs the prerequisites in "Before a first boot on a machine in scope".

**Further rows placed here: proposed (item 264).**

| Rows | Subject |
|---|---|
| S82-10 | The milestone's own sentence |
| A09-A3 | The installer's answer on the hardware scope, decided at M9 |
| S18-12 | The rest of the cryptographic profile: storage encryption, password hashing (TASKS item 27) |
| S49-07, S49-15 to S49-17, S49-20, S49-23 | The SDK for storage, security, accounts, credentials, search and localization |
| S58-12, S58-15, S58-17 | Filesystem permissions, encryption, credential protection |
| S68-06 | Time zones |
| S70-16 | Developer tools never a hidden backend of the GUI's administration |

## M10. Connectivity and laptop support

**Accepted: decision item 263.** **State, 2026-11-12: partial.** In short: packet, TCP, DHCP and DNS code proven in memory, a virtio-net driver and PS/2 decoding; no USB, audio, power or radio (matrix row S82-11).

§82: "Implement appropriate USB, USB4, Thunderbolt, networking, audio, power and peripheral support." Scope: both editions, on hardware. Matrix row S82-11. USB4, Thunderbolt, Wi-Fi and Bluetooth are shown on physical hardware only (ADR-0015 D9; S82-17).

| Test | Passes when | Rows |
|---|---|---|
| M10-1 | USB through xHCI: hubs, enumeration, every transfer type, HID, mass storage, audio, networking and video, with descriptors fuzzed | S34-01 to S34-16 |
| M10-2 | USB4 and Thunderbolt: the connection manager, tunnels, bandwidth, docks, hot-plug and DMA protection, with external-device authorization | S35-01 to S35-14, S31-12, S32-12, S32-13 |
| M10-3 | Networking: Ethernet, IPv4 and IPv6, TCP, UDP, DHCP, DNS, routing, the firewall, sockets, TLS, Wi-Fi and Bluetooth; and networking in the Origin runtime, through the network service | S54-01 to S54-21, S44-08 |
| M10-4 | Audio and peripherals | S55-01 to S55-12, S56-01 to S56-13 |
| M10-5 | Power, suspend and resume, with the clocks across sleep | S67-01 to S67-13, S68-08, S31-11 |
| M10-6 | Every device standard through the SDK's three layers, each feature reported truthfully, dangerous operations behind rights of their own | S31-17 to S31-21, rows A15-* |
| M10-7 | M10-1 to M10-6 on physical machines in scope, laptops among them, with records | S74-10, S74-11, S74-13 to S74-16 |

`THREAT_MODEL_SYSTEM.md`: before anything but a boot proof uses the network, where the stack runs is decided (TASKS item 23), with a model of a hostile peer. M10-7 needs the prerequisites in "Before a first boot on a machine in scope".

**Further rows placed here: accepted (item 269).**

| Rows | Subject |
|---|---|
| S82-11, S82-17 | The milestone's own sentence; USB4, Thunderbolt, Wi-Fi and Bluetooth on physical hardware (ADR-0015 D9) |
| S38-01 to S38-06 | Device firmware: what is required, its versions, signatures, loading, licensing and compatibility |
| S49-08, S49-09, S49-12, S49-18, S49-21 | The SDK for networking, audio, devices, power and printing |
| S58-16 | The network firewall |
| S68-07, S68-10 | Clock synchronization and the leap-second policy |

## M11. Graphics acceleration and compute

**Proposed, by the rule of item 256.** **State, 2026-11-12: not started.**

§82: "Implement supported native GPU/NPU capabilities, hardware graphics and compute interfaces." Scope: both editions, on hardware. Matrix row S82-12.

| Test | Passes when | Rows |
|---|---|---|
| M11-1 | A native driver for one supported GPU: modesetting, memory, command submission, synchronization, shaders, composition, video, compute, reset and recovery, with the display fallback kept | S36-01 to S36-15 |
| M11-2 | The Origin Compute API over CPU, GPU and NPU, with discovery, tensors, asynchronous execution, recovery and a CPU fallback | S37-01 to S37-10 |
| M11-3 | M11-1 and M11-2 on a physical machine in scope, with a record | S74-12 |

M11-3 needs the prerequisites in "Before a first boot on a machine in scope".

**Further rows placed here: proposed (item 264).**

| Rows | Subject |
|---|---|
| S82-12 | The milestone's own sentence |
| S49-24 | The SDK for compute |

## M12. System updates and recovery

**Proposed, by the rule of item 256.** **State, 2026-11-12: not started.**

§82: "Implement secure unified releases, atomic upgrades, recovery and rollback." Scope: both editions, on hardware. Matrix row S82-13.

| Test | Passes when | Rows |
|---|---|---|
| M12-1 | The boot manager: entries, the active and the last known-good deployment, attempts recorded and failures detected | S20-01 to S20-10 |
| M12-2 | One release identity for every first-party module, kept apart from ABI and format versions, for both editions | S62-02 to S62-15, S63-01 to S63-08, A14-S1 |
| M12-3 | Signed updates with TUF's threat model as reference: rollback, freeze and mixed-metadata protection, staging, atomic activation and boot-health checks. The update path's threat model before the first public release (`THREAT_MODEL_SYSTEM.md`) | S64-01 to S64-15, S58-14 |
| M12-4 | Recovery: the previous deployment, recovery boot, offline repair, driver and kernel rollback, factory reset | S65-01 to S65-13, S19-13 |
| M12-5 | The release manifest verified by the loader with the SYS profile, the rollback floor, and the TPM and measured boot where supported (ADR-0002 Amendment 2, TASKS item 28; `ONBF.md` §8.1, §12) | S58-02, S58-03 |
| M12-6 | M12-1 to M12-5 on a physical machine in scope, with power interrupted during an update, with a record | the rows above |

Secure Boot with Origin's own keys moved from M12-5 to M9-8 (item 266). M12-6 needs the prerequisites in "Before a first boot on a machine in scope".

**Further rows placed here: proposed (item 264).**

| Rows | Subject |
|---|---|
| S82-13 | The milestone's own sentence |
| A01-def2, A01-def11 | The KRN and SYS profiles; no downgrade but to the version kept locally |
| S18-13, S18-14 | ADRs for the update strategy and the recovery design |
| S38-07, S38-08 | Firmware updates, and recovery after a failed activation |
| S49-14 | The SDK for updates |
| S72-12, S72-13 | Upgrade and recovery testing |
| S73-09 | Update state machines as proof targets |

## M13. Production qualification

**Proposed, by the rule of item 256.** **State, 2026-11-12: not started.**

§82: "Complete release security review, performance testing, reliability tests, physical hardware qualification, documentation and maintenance planning." Scope: each edition, on its declared hardware. Matrix row S82-14.

| Test | Passes when | Rows |
|---|---|---|
| M13-1 | The hardware test matrix, run on the declared configurations, with records; the Server edition as a KVM guest first, then on physical servers in scope through their BMC console | S74-01, S74-02, S74-06, S74-07, S74-09, S74-17, S74-18, A14-S3 |
| M13-2 | Reproducible release candidates, rebuilt independently and compared | S75-01 to S75-09 |
| M13-3 | Release qualification: every test §76 lists, a trademark search, and the key ceremony (§60) with Origin's own root and kernel keys, before the first public release (`THREAT_MODEL_SYSTEM.md`; item 268) | S76-01 to S76-19 |
| M13-4 | Performance, smoothness and multicore scaling measured on declared hardware against accepted budgets | S53-11 to S53-13, S66-01 to S66-10 |
| M13-5 | Reliability: crash dumps, supervision, watchdogs, persistent failure records, safe mode and long-duration stress | S69-01 to S69-11 |
| M13-6 | Supply chain, privacy and security review: SBOM, signing, advisories, the privacy defaults, and an independent review of every sensitive boundary | S60-01 to S60-11, S61-01 to S61-09 |
| M13-7 | The Server edition's own work, before it ships: remote administration, remote desktop, service management and hardware health, with their SDK areas, administered through the native GUI alone; the remote administration and remote desktop ADRs, each with a threat model, and the audit record AC7 rests on (`THREAT_MODEL_SYSTEM.md`; ADR-0014 S2, S4, S5, S7 and Amendment 1) | S82-16, A14-S4, A14-S5, A14-S7, A14-A1 to A14-A3, S48-14 to S48-16, S49-26 to S49-29 |

M13-1 needs the prerequisites in "Before a first boot on a machine in scope". Remote administration needs M10's networking and new cryptography (ADR-0014 S2).

**Further rows placed here: proposed (item 264).**

| Rows | Subject |
|---|---|
| S82-14 | The milestone's own sentence |
| H-13, S1-101, F-41 | The product's objective, judged whole at qualification |
| A09-H7 | Later machines, for §74's matrix |
| A11-L12 | N8's figure on the declared hardware before any performance claim |
| S49-25 | The SDK for diagnostics |
| S70-07, S70-14 | Profiling and conformance-testing tools |
| S72-03, S72-05, S72-07, S72-09 to S72-11, S72-16 | Property-based testing, fuzzing, fault injection, hardware, performance and security testing, long-duration reliability |
| S79-10 to S79-12 | Test reports, the hardware compatibility database, build evidence |

## The Server edition

Placed by point 5 of item 256's rule, from ADR-0014 S2 and ADR-0015 D9. Each row is placed in the milestone the third column names; this table repeats them, and the gate counts them there, not here.

| Where | What | Rows |
|---|---|---|
| M0 to M7 | The same kernel, runtime and packages as the desktop: nothing of its own | |
| M8 | Its GUI, which is M8's UI and compositor | S82-15, A14-S2 |
| M10 | USB4, Thunderbolt, Wi-Fi and Bluetooth, on physical hardware only | S82-17, A15-D9 |
| M12 | Two editions of one release | A14-S1 |
| M13 | Remote administration, remote desktop, service management and hardware health, and their SDK areas (M13-7); qualified for its own scope, a KVM guest first, then physical servers in scope through their BMC console (M13-1) | S82-16, A14-S3 to A14-S5, A14-S7, A14-A1 to A14-A3, S48-14 to S48-16, S49-26 to S49-29, S74-17 |

Software written for other systems (A14-S6) waits for Rust `std` and, if ever, a POSIX layer under an ADR of its own at M6.

## Rows in no milestone

| Rows | Why |
|---|---|
| S1-08 to S1-74 | §1's 67 roles. A role's responsibilities run through every milestone; its work is placed through the rows of the sections it owns |
| A11-N14 | ADR-0011's order of work: a sequence, not a deliverable. Its steps are placed through their own rows (N3 to N13, M1 to M6, L1 to L12) |

## What the gate does not check here

- Whether a placement is right, or whether a test's text says what its rows ask. Placements are Claude's judgement until the owner answers; the tests are judged when the owner accepts them.
- Rows of kind `rule`: they need no place.
- The prose outside the "Rows" columns, the notes and the threat models' identifiers.
