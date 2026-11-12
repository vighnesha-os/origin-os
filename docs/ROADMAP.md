# Roadmap: each milestone's exit tests

| | |
|---|---|
| Required by | CLAUDE.md §82: "Do not mark a milestone complete without its exit tests. Each milestone's exit tests are listed in `docs/ROADMAP.md` before it is marked done (decision item 122)." |
| State | **M0, M1 and M2's exit tests and the rule for later milestones are accepted.** The owner accepted them on 2026-11-12 as decision items 252 to 256 ("Accept all 7", which covered items 252 to 258): 252 for M0, 253 for M1, 254 for M2, 255 for what QEMU and guests under KVM can show, 256 for the rule. The decision sheet records them through the coordinating session's records pull request to `main`. **The tests of M3 to M13 are proposed**, written by item 256's rule, and are not accepted. M3, M4, M5, M6 and M10's are with the owner as decision items 259 to 263. |
| Milestones done | **None.** A milestone is done when every one of its accepted exit tests passes in its declared scope (items 255 and 256). M0, M1 and M2 are built. |
| Written | 2026-11-12, by TASKS item 25, at `land/s4` `5ea97ef`. |
| Rows | The IDs are rows of the requirements matrix (`docs/requirements/`). What a row's state means is in its `README.md`. A row's state is the matrix's; this page does not repeat it, so it cannot go stale here. |

Until 2026-11-12, `docs/STATUS.md` marked M0 "done", M1 "done (developer mode)" and M2 "done (baseline)", before any exit test existed. §82 forbids that, so at the coordinating session's request those three marks now say "built", with a pointer to the tests here. By the accepted tests, none of the three passes today (see each milestone's "Marked against the tests").

## The rule for writing exit tests (accepted: decision items 255 and 256)

1. A milestone's exit tests are written here before its first task's contract (`docs/engineering/TASK_CONTRACT_TEMPLATE.md`), or now for milestones already started (M3, M4, M5, M6 and M10), and are put to the owner as one decision item before the milestone is marked done. M6 counts as started because TASKS item 41 (task S4-1, on its own branch) builds the native Rust user runtime.
2. Each test says what passes, through the entry point a supported caller uses (CLAUDE.md §1: "Test through actual entry points"), names its negative controls, and names the matrix rows it completes. A row's acceptance criterion becomes the test's text (`docs/requirements/README.md`, "Acceptance criteria").
3. A milestone covers the rows of its sentence in §82. A row of the directive that no sentence names goes to the milestone whose sentence names its subject, and otherwise to M13. A row of kind `rule` in the project's own scope (`eng`) binds from the day it was written and belongs to no milestone.
4. A milestone is done when every exit test passes in its declared scope and every row it names is `done`. Where the rows' scope is hardware, that needs a record of a physical machine in ADR-0009 Amendment 1's scope under `docs/hardware/records/` (§74, final rule 30). Under QEMU, or as a guest under KVM, a milestone is at most built.
5. The Server edition follows the same milestones (ADR-0014 S2; ADR-0015 D9). Each edition is qualified for its own declared scope (§76). Where its work falls is below, under "The Server edition".

## M0. Toolchain and engineering foundation

**Accepted: decision item 252.**

§82: "Pin Rust 1.99.1, validate cross-compilation, establish repository, tests, research governance, source provenance and CI." Scope: the project's own tools, on the development host (`eng/host`). Matrix row S82-01.

| Test | Passes when | Rows |
|---|---|---|
| M0-1 | `rust-toolchain.toml` is §8's, and `developer-tools/toolchain/verify.sh` passes on a clean tree, its output recorded in `docs/toolchain/VERIFICATION.md` | S8-01 to S8-09, S8-11, S9-01 to S9-11, S84-02, S84-03, S84-05 |
| M0-2 | §10's three regression tests pass, the `wcslen` test inside the `no_std` UEFI loader, run by `verify.sh` | S10-01 to S10-04, S10-06 |
| M0-3 | CI runs on every pull request and on `main` with exactly the pinned compiler, its commit checked, `--locked`, `verify.sh` and `cargo xtask check` | S8-07, S8-10 |
| M0-4 | `cargo xtask check` passes: each artifact built for its own target, the release profile linked, the freestanding libraries checked on both targets | S11-01, S11-02, S12-01, S84-06, S84-11 |
| M0-5 | The release identity is generated from `origin-version.toml`, and a lint refuses a hand-written crate version | S62-01, S62-16, S62-17, S84-07 |
| M0-6 | `Cargo.lock` holds only the workspace's packages, and the one host-only exception (Loom) is approved and pinned | S5-01, S5-10 |
| M0-7 | The records are kept: the requirements matrix and the provenance table, each read by a gate in `cargo xtask check`; a §78 contract for each task, following `docs/engineering/TASK_CONTRACT_TEMPLATE.md` and checked in review; the ADRs, the research ledgers and the decision sheet | S6-01 to S6-13, S78-01 to S78-11, S79-06 to S79-09, S84-08 |

**Marked against the tests, 2026-11-12: built, not done.** M0-2 and M0-3 fail until TASKS item 22 lands.

| Test | On the tree | Evidence |
|---|---|---|
| M0-1 | Not shown: `verify.sh` was last recorded on 2026-11-07, from a tree with five uncommitted paths | `docs/toolchain/VERIFICATION.md`; design review finding 18 |
| M0-2 | Fails: the tests are run by hand, and are not part of `cargo xtask check` or of CI | rows S10-01 to S10-04, "Limitations" |
| M0-3 | Fails: CI installs the pinned toolchain, and runs neither `verify.sh` nor `--locked` | `.github/workflows/ci.yml`; TASKS item 22 |
| M0-4 | Passes | `cargo xtask check`, run on every merge into the batch |
| M0-5 | Passes | `cargo xtask check` (the §62 lint) |
| M0-6 | Passes | `cargo xtask check` (`dependency_check`) |
| M0-7 | Passes: both gates are in `cargo xtask check` (the provenance gate since TASKS item 25), and the template exists. Three pieces of work done since item 122 have no contract table (S3-5b, H2-stacks, TASKS item 39; the template's comparison), which review, not a gate, is to catch | `developer-tools/xtask/src/requirements.rs`, `developer-tools/xtask/src/provenance.rs`, `docs/engineering/TASK_CONTRACT_TEMPLATE.md` |

## M1. Bootable Framekernel

**Accepted: decision item 253.**

§82: "Implement UEFI boot, kernel loading, memory handoff and diagnostics." Scope: both editions, on hardware (`both/hw`). Matrix row S82-02.

| Test | Passes when | Rows |
|---|---|---|
| M1-1 | The loader, a UEFI application built reproducibly, reads the KRN image, verifies it (its content digest, each section's digest or Merkle root, and its signature outside developer mode), places and relocates it, prepares the stack, the page tables and the processor's state, exits boot services correctly and enters the kernel | S19-01 to S19-03, S19-07 to S19-10, S84-09 |
| M1-2 | The memory handoff: the firmware's memory map, its ACPI root and its framebuffer reach the kernel in boot info, which the kernel validates and refuses when malformed | S19-04 to S19-06 |
| M1-3 | Diagnostics: the loader's and the kernel's lines reach the serial port the firmware names (its SPCR) and the framebuffer; a kernel fault prints a report with a backtrace | S19-11, S84-10 |
| M1-4 | Negative controls: a tampered image, a wrong key and a bad signature are each refused with their own message | S19-03 |
| M1-5 | M1-1 to M1-4 pass on at least one physical machine in ADR-0009 Amendment 1's scope, with a record under `docs/hardware/records/` | the rows above |

Given to a later milestone by rule 3: Secure Boot integration and its first stage (S19-12, S19-15) and recovery boot (S19-13), to M12. S19-14 is a rule that binds now.

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

**Marked against the tests, 2026-11-12: built, not done.** M2-1, M2-3 and M2-5 pass under QEMU and as guests under KVM (`docs/STATUS.md`, "By milestone"; boot proofs `kernel page tables`, `frame allocator`, `per-processor control state`), with S28-07 partial. M2-2 fails: not built (TASKS item 16). M2-4 is partial: no live level-triggered pin (TASKS item 2). M2-6 fails: no physical machine.

## M3. SMP and scheduler

**Proposed, by the rule of item 256, and with the owner as decision item 259.**

§82: "Implement multiple processors, native threads, preemption, synchronization and scheduling." Scope: both editions, on hardware. Matrix row S82-04.

| Test | Passes when | Rows |
|---|---|---|
| M3-1 | Every processor the firmware lists is started and has its own state; interprocessor interrupts; topology (cores, SMT siblings, caches, NUMA, hybrid capacity); processors taken offline and back | S24-06 to S24-14 |
| M3-2 | Native 1:1 kernel threads, preempted, with their stacks, register state, priorities and affinity, and cleaned up when they end | S25-01 to S25-11 |
| M3-3 | Scheduling: per-processor run queues, wake-ups, timers, fair shares, load balancing, accounting, interactive and background classes, priority-inversion mitigation, quotas and a safe fallback | S26-01 to S26-15 |
| M3-4 | Placement that accounts for SMT siblings, shared caches, hybrid capacity, NUMA, migration cost, power, heat and the foreground | S27-01 to S27-10 |
| M3-5 | Synchronization across processors: TLB shootdowns, cross-core races and lifetimes tested, and Loom models of the locks and wait queues with their negative controls | S28-17, S28-18, S73-01, S73-06 |
| M3-6 | M3-1 to M3-5 on physical machines in scope, one with SMT and one with hybrid cores, with records | S74-03 to S74-05 |

## M4. Processes and IPC

**Proposed, by the rule of item 256, and with the owner as decision item 260.**

§82: "Implement isolated user processes, system calls, capabilities and IPC." Scope: both editions, on hardware. Matrix row S82-05.

| Test | Passes when | Rows |
|---|---|---|
| M4-1 | Processes are created through the kernel's calls, end, are supervised and cleaned up; a crash is contained | S29-01 to S29-11 |
| M4-2 | Each process has its own address space; no process reaches another's memory without a handle that grants it; shared memory through memory objects | S28-05, S28-06, S28-10 |
| M4-3 | Typed, authenticated IPC with capability handles, access checks, cancellation, versioning and lifetimes | S30-01 to S30-12 |
| M4-4 | System calls and their error codes as the user ABI defines them, every pointer and value from ring 3 checked | S42-05, S42-06, S42-17 |
| M4-5 | The speculation controls and processor hardening ADR-0017 decides are in effect on every processor before ring 3 runs anything but a boot proof | S24-15, rows A17-* |
| M4-6 | Kernel and process isolation, capability authorization and secure IPC, each mapped to the threat models' rows with negative controls | S58-04, S58-05, S58-09, S58-11, S58-18, S58-19 |
| M4-7 | M4-1 to M4-6 on a physical machine in scope, with a record | the rows above |

## M5. Storage

**Proposed, by the rule of item 256, and with the owner as decision item 261.**

§82: "Implement PCIe, DMA/IOMMU, NVMe, block storage and filesystem foundations." Scope: both editions, on hardware. Matrix row S82-06.

| Test | Passes when | Rows |
|---|---|---|
| M5-1 | The driver framework's core: discovery, resources, binding, interrupts, DMA mappings, lifecycle, isolation, reset, faults and recovery, through a documented Driver ABI | S31-01 to S31-10, S31-13, S31-14, S31-16 |
| M5-2 | PCIe: host bridges, enumeration, configuration access, BARs, MSI and MSI-X, DMA through VT-d and the AMD IOMMU with interrupt remapping, error reporting and reset | S32-01 to S32-11, S32-14, S58-10 |
| M5-3 | NVMe: controllers, queues, namespaces, interrupts, timeouts, resets, flushes, integrity, power states and health | S33-01 to S33-15 |
| M5-4 | The storage layers: block I/O, I/O scheduling, partitions, volumes, the page cache and the virtual filesystem, with asynchronous I/O | S39-01 to S39-11 |
| M5-5 | Filesystem foundations: an accepted on-disk format, its durability semantics defined and tested, crash consistency, data and metadata integrity, atomic rename, versioned structures and power-loss recovery | S40-01 to S40-04, S40-14, S40-16, S41-01 to S41-10 |
| M5-6 | M5-1 to M5-5 on a physical machine in scope with an NVMe controller, with a record | S74-08 |

Given elsewhere by rule 3: hot-plug and external-device authorization (S31-12, S32-12, S32-13) to M10; the rest of §40 (access control, sparse, large and mapped files, quotas, locking, snapshots, encryption, repair, migrations) to M9.

## M6. Native runtime

**Proposed, by the rule of item 256, and with the owner as decision item 262.**

§82: "Implement Origin executable ABI, `.ose` loader and native Rust user runtime." Scope: both editions, on hardware. Matrix row S82-07.

| Test | Passes when | Rows |
|---|---|---|
| M6-1 | The userspace ABI is written and versioned: calling convention, stack, registers, TLS, start-up, allocation, dynamic linking, shared libraries, unwinding, notifications, version negotiation, 64-bit only | S42-01 to S42-04, S42-07 to S42-16 |
| M6-2 | The `.ose` loader verifies and loads an ONBF executable, refuses malformed and hostile images, and is fuzzed | S43-01 to S43-14 |
| M6-3 | The Origin runtime: start-up, allocation, threads, TLS, synchronization, files, time, standard I/O, async I/O, panics and shutdown | S44-01 to S44-17 |
| M6-4 | Userspace is built for its own target and never for Linux's APIs | S11-03 to S11-07 |
| M6-5 | Starting a program is a capability, with the manifest gate, no interpreter in the base image and an empty environment by default (ADR-0011 Amendment 1) | S48-05 to S48-10 |
| M6-6 | M6-1 to M6-5 on a physical machine in scope, with a record | the rows above |

## M7. Native packaging

**Proposed, by the rule of item 256.**

§82: "Implement `.osi`, `.oapp`, `.od`, signing, installation and rollback." Scope: both editions, on hardware. Matrix row S82-08.

| Test | Passes when | Rows |
|---|---|---|
| M7-1 | `.oapp`: identity, the main executable, resources, libraries, localization, capabilities, launch policy, signing and update metadata | S45-01 to S45-11 |
| M7-2 | `.osi`: parsed, signature-checked, resolved, installed in one transaction with no install-time code, repaired, upgraded, removed and rolled back, including after an interruption | S46-01 to S46-15 |
| M7-3 | `.od`: identity, hardware IDs, Driver ABI, firmware, privileges, signature, activation, rollback; third-party drivers in user mode only | S47-01 to S47-12, S31-15 |
| M7-4 | Signed packages verified with both signatures of the accepted suite | S58-13, S59-01 to S59-16 |
| M7-5 | M7-1 to M7-4 on a physical machine in scope, with a record | the rows above |

Not started. The tree holds groundwork only: the ONBF container parser knows each profile's allowed sections, and signing and verification exist for the kernel's image (`onbf-format`, `onbf-sign`, `onbf-verify`, `onbf-pack`, which writes KRN only). Nothing parses a `.oapp`, `.osi` or `.od` manifest, builds one or installs one.

## M8. Native graphics

**Proposed, by the rule of item 256.**

§82: "Implement Origin UI, software renderer, compositor and first graphical applications." Scope: both editions, on hardware. Matrix row S82-09.

| Test | Passes when | Rows |
|---|---|---|
| M8-1 | Origin UI: windows, controls, layout, events, menus, dialogs, notifications, clipboard, drag and drop, the accessibility tree, themes, scaling and animation | S50-01 to S50-18 |
| M8-2 | The text engine: Unicode 18.0 data, normalization, segmentation, line breaking, bidirectional text, shaping, fallback, emoji, input methods and editing, tested in Telugu, Hindi, Arabic, Latin and mixed direction | S51-01 to S51-18 |
| M8-3 | The compositor, with its software fallback, as a security boundary between programs | S52-01 to S52-20 |
| M8-4 | Smoothness measured against the provisional budgets, reported as figures with no claim | S53-01 to S53-13 |
| M8-5 | The first graphical applications, each an SDK client with no private door | S48-01 to S48-04 |
| M8-6 | M8-1 to M8-5 on a physical machine in scope, with a record | S74-12 |

## M9. Installable operating system

**Proposed, by the rule of item 256.**

§82: "Create actual installation media, partitioning, installation GUI, first-run setup and native system management." Scope: both editions, on hardware. Matrix row S82-10.

| Test | Passes when | Rows |
|---|---|---|
| M9-1 | The installer, from media to a booted installed system: storage discovery, GPT, the system partition, deployments, user data, recovery, formatting, encryption, safe disk choice, progress and verification | S21-01 to S21-19 |
| M9-2 | The installer is a native GUI over typed APIs, with no hidden command | S22-01 to S22-03 |
| M9-3 | Origin Setup, offline as well: language, keyboard, time zone, account, credentials, device name, network, encryption, privacy and accessibility | S23-01 to S23-13 |
| M9-4 | Accounts and sessions: local accounts, credentials stored hashed, locking, switching, administrative authorization, recovery and deletion | S57-01 to S57-13 |
| M9-5 | The filesystem's remaining features for an installed system | S40-05 to S40-13, S40-15, S40-17 |
| M9-6 | Every setting and management task in the native GUI; no terminal needed | S48-11 to S48-13, S48-17, S48-18 |
| M9-7 | M9-1 to M9-6 on a physical machine in scope, with a record | the rows above |

## M10. Connectivity and laptop support

**Proposed, by the rule of item 256, and with the owner as decision item 263.**

§82: "Implement appropriate USB, USB4, Thunderbolt, networking, audio, power and peripheral support." Scope: both editions, on hardware. Matrix row S82-11. USB4, Thunderbolt, Wi-Fi and Bluetooth are shown on physical hardware only (ADR-0015 D9; S82-17).

| Test | Passes when | Rows |
|---|---|---|
| M10-1 | USB through xHCI: hubs, enumeration, every transfer type, HID, mass storage, audio, networking and video, with descriptors fuzzed | S34-01 to S34-16 |
| M10-2 | USB4 and Thunderbolt: the connection manager, tunnels, bandwidth, docks, hot-plug and DMA protection, with external-device authorization | S35-01 to S35-14, S31-12, S32-12, S32-13 |
| M10-3 | Networking: Ethernet, IPv4 and IPv6, TCP, UDP, DHCP, DNS, routing, the firewall, sockets, TLS, Wi-Fi and Bluetooth | S54-01 to S54-21 |
| M10-4 | Audio and peripherals | S55-01 to S55-12, S56-01 to S56-13 |
| M10-5 | Power, suspend and resume, with the clocks across sleep | S67-01 to S67-13, S68-08, S31-11 |
| M10-6 | Every device standard through the SDK's three layers, each feature reported truthfully, dangerous operations behind rights of their own | S31-17 to S31-21, rows A15-* |
| M10-7 | M10-1 to M10-6 on physical machines in scope, laptops among them, with records | S74-10, S74-11, S74-13 to S74-16 |

## M11. Graphics acceleration and compute

**Proposed, by the rule of item 256.**

§82: "Implement supported native GPU/NPU capabilities, hardware graphics and compute interfaces." Scope: both editions, on hardware. Matrix row S82-12.

| Test | Passes when | Rows |
|---|---|---|
| M11-1 | A native driver for one supported GPU: modesetting, memory, command submission, synchronization, shaders, composition, video, compute, reset and recovery, with the display fallback kept | S36-01 to S36-15 |
| M11-2 | The Origin Compute API over CPU, GPU and NPU, with discovery, tensors, asynchronous execution, recovery and a CPU fallback | S37-01 to S37-10 |
| M11-3 | M11-1 and M11-2 on a physical machine in scope, with a record | S74-12 |

## M12. System updates and recovery

**Proposed, by the rule of item 256.**

§82: "Implement secure unified releases, atomic upgrades, recovery and rollback." Scope: both editions, on hardware. Matrix row S82-13.

| Test | Passes when | Rows |
|---|---|---|
| M12-1 | The boot manager: entries, the active and the last known-good deployment, attempts recorded and failures detected | S20-01 to S20-10 |
| M12-2 | One release identity for every first-party module, kept apart from ABI and format versions | S62-02 to S62-15, S63-01 to S63-08 |
| M12-3 | Signed updates with TUF's threat model as reference: rollback, freeze and mixed-metadata protection, staging, atomic activation and boot-health checks | S64-01 to S64-15, S58-14 |
| M12-4 | Recovery: the previous deployment, recovery boot, offline repair, driver and kernel rollback, factory reset | S65-01 to S65-13, S19-13 |
| M12-5 | Secure Boot with Origin's own keys enrolled by default, and the TPM and measured boot where supported | S19-12, S19-15, S58-01 to S58-03 |
| M12-6 | M12-1 to M12-5 on a physical machine in scope, with power interrupted during an update, with a record | S74-16 |

## M13. Production qualification

**Proposed, by the rule of item 256.**

§82: "Complete release security review, performance testing, reliability tests, physical hardware qualification, documentation and maintenance planning." Scope: each edition, on its declared hardware. Matrix row S82-14.

| Test | Passes when | Rows |
|---|---|---|
| M13-1 | The hardware test matrix, run on the declared configurations, with records | S74-01 to S74-18 |
| M13-2 | Reproducible release candidates, rebuilt independently and compared | S75-01 to S75-09 |
| M13-3 | Release qualification: every test §76 lists, and a trademark search before the first public release | S76-01 to S76-19 |
| M13-4 | Performance, smoothness and multicore scaling measured on declared hardware against accepted budgets | S53-01 to S53-13, S66-01 to S66-10 |
| M13-5 | Reliability: crash dumps, supervision, watchdogs, persistent failure records, safe mode and long-duration stress | S69-01 to S69-11 |
| M13-6 | Supply chain, privacy and security review: SBOM, signing, advisories, the privacy defaults, and an independent review of every sensitive boundary | S60-01 to S60-11, S61-01 to S61-09 |

## The Server edition

Placed by point 5 of item 256's rule, from ADR-0014 S2 and ADR-0015 D9. The rows are the matrix's.

| Where | What | Rows |
|---|---|---|
| M0 to M7 | The same kernel, runtime and packages as the desktop: nothing of its own | A14-S1 |
| M8 | Its GUI, which is M8's UI and compositor | S82-15, A14-S2 |
| After M9 | Remote administration, remote desktop, service management and hardware health, and their SDK areas; remote administration needs M10's networking and new cryptography | S82-16, A14-S4, A14-S5, A14-S7, A14-A1 to A14-A3, S48-14 to S48-16 |
| M10 | USB4, Thunderbolt, Wi-Fi and Bluetooth, on physical hardware only | S82-17, A15-D9 |
| M13 | Qualified for its own scope: a KVM guest first, then physical servers in scope through their BMC console | A14-S3, S74-17 |

Software written for other systems (A14-S6) waits for Rust `std` and, if ever, a POSIX layer under an ADR of its own at M6.

## What no gate checks here

No gate reads this page. Its row IDs were checked by hand against the matrix when it was written; a row renumbered or deleted later is not caught. The tests themselves are judged only when the owner answers.
