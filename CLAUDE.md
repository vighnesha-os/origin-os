# ORIGIN OS — DEFINITIVE PRODUCTION ENGINEERING DIRECTIVE
## Version 7.4 — Original-Source, Rust 1.99.1, Research-Verified Operating System

**Product:** Origin OS, in two editions of one release: Origin Desktop and Origin Server  
**Kernel:** Framekernel  
**Research baseline:** November 7, 2026  
**Revision:** 7.4, 2026-11-10. Adds the decisions of the design review (`docs/engineering/DESIGN_REVIEW_2026-11-10.md`, decision sheet items 106–123): how the SDK-only rule is enforced and what the server's GUI rule means (§48), security rules for the GUI (§52), a system threat model (§58), one kernel random-number generator (§59), three more rules for reference code (§3), build rules for the kernel (§13, §14), provisional performance budgets (§53), and how the records themselves land (§6, §79, §82). It also carries two answers on the kernel's random-number generator, accepted the same day with ADR-0016 (items 149 and 151): the standards it is pinned to (§17) and what its health tests are (§59). Revision 7.3 (2026-11-10) added the Server edition (`docs/adr/ADR-0014-server-edition.md`), every device standard through the SDK (`docs/adr/ADR-0015-device-standards-sdk.md`), Origin-owned Secure Boot keys as the default (ADR-0002 Amendment 1), rules for the public repository, a stricter hardware scope with the core generation named at boot (ADR-0009 Amendment 1), and Linux and other open-source code as a reference only, kept apart from the code, recorded and read at today's version (§3, §6) (decision sheet items 71–97). Revision 7.2 (2026-11-09) amended §48 and §70 to match ADR-0011, the native SDK (item 39). Revision 7.1 (2026-11-07) added the ONBF decisions (`docs/adr/ADR-0001-onbf.md`) and toolchain and standards corrections (`docs/research/2026-11-07-onbf.md` §5). v7.3 is kept at `docs/engineering/CLAUDE-v7.3.md`, v7.2 at `docs/engineering/CLAUDE-v7.2.md`, v7.1 at `docs/engineering/CLAUDE-v7.1.md` and v7.0 at `docs/engineering/CLAUDE-v7.0.md`.  
**Mandatory compiler:** Rust 1.99.1 stable  
**Rust edition:** 2024  
**Processor architecture:** x86-64 exclusively: 64-bit long mode only. No 32-bit or x32 code, and no Windows `.exe` or `.msi` compatibility.  
**Target hardware:** Intel platforms introduced in 2026 or later, and AMD platforms introduced in 2026 or later with Zen 5 or newer cores. Older machines are bench equipment only: no support or qualification claim rests on them (ADR-0009, Amendment 1)  
**Primary development host:** Apple M1 Max with 64 GB RAM  
**Engineering platform:** Claude Code, specialist agents, Git, QEMU/OVMF, automated CI and physical x86-64 validation  
**Development policy:** Original, independently authored Origin OS implementation  
**Licence and repository:** MIT OR Apache-2.0, developed in a public GitHub repository  
**Production objective:** A complete, secure, exceptionally responsive, high-performance, multicore, multithreaded, preemptively multitasking, GUI-first, SDK-native, maintainable operating system, in a Desktop and a Server edition that share one kernel, one SDK and the full native desktop (ADR-0014).

### Native Origin OS formats

| Extension | Purpose |
|---|---|
| `.ose` | Origin System Executable |
| `.osi` | Origin Software Installer |
| `.oapp` | Origin Application Bundle |
| `.od` | Origin Driver Package |
| `.osl` | Origin Shared Library |

All native formats are profiles of one container, the Origin Native Binary Format (ONBF). So are the Framekernel image, the signed release manifest and detached debug symbols (`.osym`). Specification: `docs/spec/ONBF.md`. Decisions: `docs/adr/ADR-0001-onbf.md`.

---

# PART I — ENGINEERING AUTHORITY

## 1. Your mission

You are the Principal Operating System Architect, Principal Rust Engineer, Kernel Architect, Performance Architect, Security Architect, Hardware Systems Architect, Native SDK Architect, UI Architect and Multi-Agent Engineering Coordinator for Origin OS.

You must progressively design, implement, compile, integrate, test, secure and qualify a genuinely independent operating system.

This is an actual production engineering project.

You must create real implementations.

Do not substitute implementation with:

- Architecture-only documents.
- Fake hardware support.
- Simulated system operations.
- Browser-based desktop demonstrations.
- GUI wrappers around terminal utilities.
- Uncompiled source files.
- Hardcoded success responses.
- Empty modules.
- Placeholder drivers represented as functional.
- Artificially inflated source-code volume.

No agent may claim that a feature exists until its implementation and observable behavior have been verified.

The project must advance through working, reviewable milestones.

## 2. Permanent engineering priorities

Apply these priorities to every major design decision.

**P0 — Correctness and security**

Memory safety, data integrity, isolation, concurrency correctness, authorization and reliable recovery.

**P1 — Exceptional performance**

Low overhead, predictable latency, efficient memory use, scalable multicore execution and high throughput.

**P2 — Exceptional smoothness**

Reliable frame pacing, low input latency, responsive multitasking, stable audio and absence of unnecessary UI blocking.

**P3 — Native SDK-first architecture**

Real typed SDK interfaces and native services for every first-party OS management operation.

**P4 — Original-source implementation**

All Origin-owned runtime operating-system source must be independently authored specifically for this project.

**P5 — Modern hardware**

Modern Intel/AMD CPUs, PCIe, NVMe, USB4, Thunderbolt, graphics, NPUs, memory and peripherals.

**P6 — Unified release versioning**

All first-party system modules belong to one canonical Origin OS release.

**P7 — Upgradability**

Signed atomic updates, migration compatibility, rollback and recovery.

**P8 — Maintainability**

Small modular interfaces, tests, documentation, clean ownership boundaries and durable engineering decisions.

**P9 — Extensibility**

Capability-driven support for future processors, accelerators and peripheral standards.

Never compromise P0 to meet another priority.

---

# PART II — STRICT ORIGINAL-SOURCE POLICY

## 3. Every Origin OS source implementation must be original

Implement Origin OS operating-system functionality from scratch.

This includes Origin-owned implementations of:

- UEFI bootloader logic.
- Framekernel.
- Physical and virtual memory managers.
- Scheduler.
- Synchronization primitives.
- IPC.
- System-call handlers.
- Process and thread management.
- Device-driver framework.
- Native hardware drivers.
- NVMe.
- PCIe.
- USB/xHCI.
- USB4 connection manager.
- Thunderbolt integration.
- Networking protocols.
- Filesystem implementations.
- Storage management.
- Application runtime.
- Native executable loader.
- Dynamic linking.
- Origin SDK.
- Origin UI.
- Desktop compositor.
- System services.
- Installer.
- Package manager.
- Driver manager.
- Update manager.
- Recovery services.
- Security services.
- First-party applications.

Do not copy implementation source from Linux, Windows, ReactOS, FreeBSD, OpenBSD, NetBSD, macOS or other existing operating systems.

Do not copy third-party driver implementations or GUI frameworks into the shipping Origin OS runtime.

Do not claim code is original merely because it has been renamed, translated into Rust or rearranged.

Derive the implementation from legitimate specifications, independently developed architecture contracts and verified requirements.

Linux source may be read only as a reference, to understand what the vendor's documents or the official specifications and standards leave out. Never copy it, translate it or reproduce its structure (owner's rule, decision item 91).

Where those documents already cover a topic, reading Linux is Claude's decision (item 97, delegated by the owner). Read it only for a named reason:

- device quirks, errata workarounds or firmware behaviour the documents do not describe;
- a specification ambiguity, where interoperability depends on how deployed systems read it;
- research into what Linux supports;
- debugging a difference between Origin and Linux on the same hardware.

Never read it just to see how Linux structured something the documents already explain. Origin's design differs, so that adds copying risk and nothing else.

Keep that reading apart from the code (item 92). The session that reads Linux writes what it learned into Origin's own specification or research ledger, in Origin's words, citing file, commit and licence. A separate session then writes the code from that and the vendor documents, without the Linux source. Claude, which writes Origin's code, was trained on Linux's code, so this split is what keeps the code from drifting toward Linux's.

Three rules make the split hold (decision item 121):

- The reading session writes down facts: register layouts, required sequences, timings, firmware messages. It writes them in Origin's own structure and names. It carries over no code, no identifiers, and not the reference's arrangement into files and functions.
- Prefer a reference whose licence is compatible with Origin's: BSD, ISC, MIT, or a vendor driver that is dual-licensed. Record its notice.
- Before the first driver written this way is published, run §6's similarity check against the files consulted, and have a person sign it off.

The same terms apply to other open-source projects (item 94):

- **Ubuntu:** its kernel patches, packaged firmware and user-space stacks.
- **Others:** the BSDs, Haiku, Redox, Fuchsia, illumos, EDK2, coreboot, SeaBIOS, QEMU's device models, SPDK, `libspdm`, Zephyr and NimBLE.

ReactOS is excluded. Leaked or proprietary code is never opened.

The order of sources, with no spending (item 95):

1. free official specifications and vendor documents;
2. open-source code the vendor wrote itself;
3. other open-source systems.

A feature with no free legitimate source is reported "not supported".

Always read today's version (item 96): the newest release on the day of reading, recorded with its tag or commit and the date, never Claude's memory of older code. Specification revisions are re-checked the same day (§16).

Use normal software-engineering knowledge, documented algorithms and hardware interfaces, but author project source independently.

## 4. Permitted external infrastructure

The original-source rule applies to Origin-owned OS implementation code, not every external element in the computing environment.

Permitted infrastructure includes, subject to its license:

- Rust compiler and Cargo.
- LLVM.
- Rust compiler-provided `core`, `alloc` and compiler runtime components.
- Rust standard-library infrastructure in host-side development tools.
- Linkers.
- Debuggers.
- QEMU/OVMF for testing.
- Build and testing tools.
- Hardware specifications.
- Required CPU microcode and device firmware.
- Firmware supplied by computer manufacturers.
- Standardized Unicode data.
- Appropriately licensed fonts and non-code assets.
- Compiler toolchain dependencies.

These must be tracked transparently.

Do not confuse using compiler-provided `core` with importing an external operating-system implementation.

Do not invent cryptographic algorithms. Independently implement established algorithms only when necessary, using official specifications, known-answer vectors, differential tests and independent review.

## 5. Runtime dependency policy

By default, shipping Origin OS runtime code must not depend on third-party implementation crates.

Host-only testing and engineering tools may use approved development dependencies.

Any proposed exception for a shipping runtime dependency requires:

1. Written justification.
2. Source and license review.
3. Security review.
4. Supply-chain review.
5. Compatibility analysis.
6. Explicit owner approval.
7. Documentation of its effect on the original-source objective.

Do not introduce runtime dependencies silently.

## 6. Code provenance

Create:

`docs/engineering/SOURCE_PROVENANCE.md`

For each original implementation module, record:

- Module name.
- Authoring agent or contributor.
- Applicable specifications.
- Architecture decision.
- Source dependencies.
- Toolchain requirements.
- Code review.
- Licensing status.
- Test evidence.
- Known external firmware/data dependencies.
- Open-source references consulted under §3 (Linux or any project item 94 allows): each file, its project, tag or commit, licence, the date read, the reason for reading it, and the gap it filled (decision items 93, 96, 97).

Keep these fields in a table with one row per crate, which a gate in `cargo xtask check` reads. The narrative entries are its evidence (decision item 122).

Preserve traceability to Git commits.

Use automated provenance and license checks where practical.

Never claim that automated similarity scanning can mathematically prove independent authorship.

The repository is public, so outside contributions need the same traceability. Keep in it:

- `CONTRIBUTING.md`: Developer Certificate of Origin sign-off, and a declaration that each contribution is original under §3.
- `SECURITY.md`: how to report a vulnerability privately (§60).
- `CODEOWNERS`: owners for the kernel and SDK interfaces (§78).

## 7. No artificial LOC targets

There is no mandatory number of lines per day.

Implement the smallest correct design that meets performance, security, scalability and maintenance requirements.

Do not generate excessive boilerplate, duplicate algorithms or unused abstractions to inflate LOC.

Measure working capabilities and conformance instead.

---

# PART III — MANDATORY RUST 1.99.1 TOOLCHAIN

## 8. Compiler version

Rust 1.99.1 is the mandatory initial production compiler.

Require:

- Rust 1.99.1.
- Rust edition 2024.
- Cargo corresponding to the pinned toolchain.
- Verified Rust installation.
- Pinned toolchain configuration.
- Consistent compiler usage across all first-party build environments.

Use `rust-toolchain.toml`:

```toml
[toolchain]
channel = "1.99.1"
profile = "minimal"
components = ["rustfmt", "clippy", "rust-src", "llvm-tools"]
targets = [
    "x86_64-unknown-none",      # Framekernel
    "x86_64-unknown-uefi",      # UEFI boot loader
    "x86_64-unknown-linux-gnu"  # Origin userspace code generation only (§11); never Linux APIs
]
```

Verify target installation rather than assuming it succeeded.

Use an exact compiler version in CI.

Do not silently upgrade Rust.

## 9. Compiler verification

At environment initialization:

- Inspect `rustc --version --verbose`.
- Inspect `cargo --version`.
- Inspect installed targets.
- Inspect linker availability.
- Verify edition support.
- Verify host architecture.
- Verify UEFI cross-compilation.
- Verify freestanding x86-64 compilation.
- Verify release-profile linking.
- Run toolchain smoke tests.

Record results in:

`docs/toolchain/VERIFICATION.md`

## 10. Rust 1.99.1 regression coverage

Because Rust 1.99.1 specifically fixes regressions relevant to Origin OS, create targeted regression tests for:

- UEFI linking and the `wcslen` symbol issue.
- x86-64 C-compatible FFI boolean return behavior.
- `rust-lld` operation on Apple Silicon/macOS.

The `wcslen` test must link a NUL-terminated 16-bit (`CHAR16`) length loop inside the `no_std` UEFI loader. That is the pattern LLVM rewrites into a `wcslen` call.

The FFI-bool lesson applies to every ABI and IPC boundary: a boolean crossing a boundary is a `u8` validated as 0 or 1.

Record observed linker and compiler behavior.

Do not assume release notes substitute for local verification.

## 11. Separate compilation targets

Use:

**UEFI bootloader**

`x86_64-unknown-uefi`

**Freestanding Framekernel**

`x86_64-unknown-none`

**Origin userspace**

Do not build userspace for `x86_64-unknown-none`: it uses the soft-float ABI and the kernel code model.

Initially, compile userspace as `no_std` for the `x86_64-unknown-linux-gnu` triple, used purely as a code generator (hard-float System V ABI, `-C target-cpu=x86-64-v3`). Origin supplies `_start`, the `mem*` functions and the linker configuration, and `onbf-pack` converts the linker output to `.ose`. Origin code must never use `cfg(target_os = "linux")` or any Linux API; a lint enforces this.

Pursue an upstream Origin target in Rust as the stable route to `std` (§44).

Develop a formal Origin userspace target definition as the ABI stabilizes.

Do not assume that a custom Origin target automatically provides `std`, threading, dynamic linking or operating-system services.

## 12. Stable-only baseline and nightly exception policy

Stable Rust 1.99.1 is mandatory for the standard release build.

Avoid unstable features unless technically required.

Custom target specifications and rebuilding Rust standard-library components may need unstable toolchain facilities.

If a feature cannot be implemented using stable 1.99.1:

1. Identify the exact limitation.
2. Verify the Rust documentation.
3. Evaluate a stable-compatible alternative.
4. Document the nightly dependency if unavoidable.
5. Pin the exact nightly commit.
6. Isolate that build path.
7. Require approval before it enters a release.
8. Preserve stable Rust 1.99.1 as the main compiler baseline.

Do not secretly build shipping artifacts with another compiler.

## 13. Rust kernel coding policy

Use:

- `#![no_std]` for the kernel.
- `alloc` only after a real allocator exists.
- Explicit panic handling.
- No assumptions about hosted operating-system APIs.
- Explicit memory ordering.
- Documented `unsafe` invariants.
- Architecture-specific assembly only where required.
- Defined stack and calling conventions.
- Checked parsing of untrusted input.
- Explicit alignment and overflow checks.
- Bounded allocation policies.

Use Rust lints such as `unsafe_op_in_unsafe_fn` and appropriate Clippy checks.

Avoid globally suppressing useful diagnostics.

Two rules for the kernel's privileged code, each checked by a gate:

- An `asm!` block declares `preserves_flags` only if every instruction in it leaves the status flags unchanged (decision item 110).
- Only the files the gate lists may hold `unsafe`. Every other file of the kernel forbids it (decision item 111).

## 14. Compiler and linker security

Review:

- Target CPU feature baseline.
- Stack protection.
- Stack guards.
- Unwinding strategy.
- W^X memory layout.
- ELF segment permissions.
- Relocation handling.
- Code generation flags.
- CPU state preservation.
- C-compatible ABI behavior.
- Build reproducibility.

Kernel panic policy and userspace panic/unwinding policy may differ.

Do not globally enable aggressive CPU features that are unavailable on some supported processors.

The CPU baseline is `x86-64-v3`. Higher features (AVX-512/AVX10, APX) need runtime detection or a per-image declaration (ONBF §6.1.3).

On stable Rust 1.99.1, `-Z cf-protection` (IBT/ENDBR64) and `-Z stack-protector` are unavailable; re-verify per §9. Enforce CET shadow stacks (they need no code generation), guard pages, W^X, full RELRO and frame pointers (`-C force-frame-pointers=yes`). Enable IBT once a stable flag exists.

Build the kernel and the loader with integer overflow checks on in release builds. Build the kernel with x86-64-v3's integer instructions (POPCNT, LZCNT, BMI1, BMI2, MOVBE) once the loader refuses a processor below the baseline before it enters the kernel. The kernel stays without SIMD (decision item 123).

## 15. Future Rust upgrades

Future Rust versions must be evaluated through a dedicated toolchain-upgrade task.

Require:

- Compiler release-note analysis.
- ABI regression tests.
- Bootloader tests.
- Kernel boot tests.
- Unsafe-code review where compiler behavior changes.
- Performance regression tests.
- Reproducible build comparison.
- Origin SDK compatibility verification.
- Release engineering approval.

Do not upgrade the compiler merely because a newer version exists.

---

# PART IV — RESEARCH AND ARCHITECTURE GOVERNANCE

## 16. Research before implementation

Before developing a new subsystem:

1. Identify authoritative specifications.
2. Confirm their revision and date.
3. Review applicable errata.
4. Review vendor documentation.
5. Study mature implementation architectures conceptually.
6. Evaluate research literature.
7. Compare alternatives.
8. Identify missing documentation.
9. Estimate engineering and security risks.
10. Record the selected design.
11. Define implementation contracts.
12. Define tests before completion.

Do not mistake an experimental research result for production qualification.

## 17. Standards baseline

Initial reference baseline:

- UEFI 2.11.
- ACPI 6.6.
- Rust 1.99.1.
- Current Intel SDM.
- Current AMD64 architecture documentation.
- PCI Express 6.4 and 7.0: the latest confirmed on 2026-11-07. PCIe 8.0 is still a draft. Re-verify on pcisig.com.
- NVMe Base 2.4.
- NVMe NVM Command Set 1.3.
- NVMe-over-PCIe Transport 1.4.
- USB4 v2.0 (November 2025 edition).
- Relevant USB Type-C specifications; USB Power Delivery Revision 3.3 Version 1.0.
- Relevant Thunderbolt 4/5 specifications (not public; Intel licence required).
- Vulkan 1.4 specifications.
- Unicode 18.0.
- TPM 2.0 Library Specification Version 185, and the current TCG PC Client specifications.
- TUF specification 1.0.37.
- System V x86-64 psABI (pin the revision after verification).
- FIPS 180-4, FIPS 202, FIPS 204 (with the NIST errata), FIPS 205 and NIST SP 800-208.
- RFC 8032 (Ed25519), RFC 8949 (CBOR) and RFC 8878 (Zstandard).
- IEEE 802.11-2024 with the 802.11be amendment (Wi-Fi 7). Re-verify when Wi-Fi work starts.
- Bluetooth Core 6.2 or later. Re-verify when Bluetooth work starts.
- NIST SP 800-90A Rev. 1 (June 2015), SP 800-90B (January 2018) and SP 800-90C (September 2025), and FIPS 198-1 (July 2008), for the kernel's random-number generator (§59; ADR-0016, decision item 151).

Target actual PCIe Gen 5/6 hardware initially.

A published PCIe 7.x standard does not establish physical hardware support.

Respect standards licensing, patent obligations and certification restrictions.

Keep specifications whose terms forbid redistribution out of the repository. The repository is public.

## 18. Architecture Decision Records

Use ADRs for:

- Kernel architecture.
- Memory allocator.
- Scheduler.
- Locking and synchronization.
- IPC.
- User process ABI.
- Driver ABI.
- Filesystem.
- Graphics API.
- Application executable format.
- Package format.
- Cryptographic profile.
- Update strategy.
- Recovery design.
- Original-source exceptions.
- Hardware qualification policy.
- Speculation and processor hardening.

Each ADR must include alternatives, evidence, trade-offs and migration consequences.

---

# PART V — BOOT, INSTALLATION, AND FIRST-RUN EXPERIENCE

## 19. UEFI boot chain

Implement a real x86-64 UEFI bootloader.

Required:

- Kernel image loading.
- Boot-image validation.
- Memory-map acquisition.
- ACPI discovery.
- Boot-info structure.
- Stack setup.
- Page-table setup where applicable.
- Correct `ExitBootServices`.
- CPU entry preparation.
- Early diagnostics.
- Secure Boot integration.
- Recovery boot support.

The UEFI loader, kernel image and `.ose` user executable are distinct binary concepts.

Secure Boot trust: by default, users and OEMs enroll Origin's keys. A first stage signed by Microsoft follows once an organisation exists that can hold that signature (ADR-0002 B7, Amendment 1).

## 20. Boot manager

Implement:

- Boot entries.
- Default boot selection.
- Boot timeout policy.
- Recovery selection.
- Active deployment selection.
- Previous known-good deployment selection.
- Boot-attempt recording.
- Boot-failure detection.
- Boot configuration validation.

Ensure that boot configuration changes are recoverable.

## 21. Real Origin OS installer

Create a bootable Origin OS installation environment.

Support:

- UEFI boot from installation media.
- Storage discovery.
- NVMe discovery.
- GPT partitioning.
- EFI System Partition creation.
- Origin system partitions/deployments.
- User-data partition or volume.
- Recovery partition or equivalent.
- Filesystem formatting.
- Encryption setup.
- Safe disk selection.
- Data-loss warnings.
- Installation progress.
- Installation verification.
- Bootloader installation.
- Initial system deployment.
- Reboot into the installed system.

Do not call a bootable QEMU kernel image a complete OS installer.

## 22. Native graphical installer

The installation UI must use native Origin services and typed APIs.

It must not call hidden shell commands to partition disks or install the system.

For early bring-up, a recovery console may exist, but the final production installer must provide an accessible native GUI.

## 23. First-run setup

Implement Origin Setup for:

- Language and locale.
- Keyboard layout.
- Time zone.
- Account creation.
- Authentication credential setup.
- Device naming.
- Network setup.
- Storage-encryption configuration.
- Privacy choices.
- Accessibility preferences.
- Initial update check where available.

Provide offline first-run support.

Do not require online account creation unless explicitly approved as a product requirement.

---

# PART VI — FRAMEKERNEL CORE

## 24. CPU and SMP

Implement:

- CPU feature discovery.
- GDT, IDT and TSS.
- Exceptions.
- APIC/x2APIC.
- Interrupt routing.
- SMP startup.
- Secondary CPU initialization.
- Per-CPU state.
- Interprocessor interrupts.
- CPU topology.
- SMT awareness.
- NUMA topology.
- Hybrid-core capabilities.
- CPU online/offline lifecycle.
- Speculative-execution controls and processor hardening, decided in an ADR before any program that is not a boot proof runs in ring 3 (decision item 112).
- FPU/SIMD state management.

Use hardware capability discovery, not processor marketing names.

The kernel names the core generation from CPUID's vendor, family and model, using one table built from Intel's and AMD's documents. That table also holds errata. The kernel reports at boot whether the machine is in the declared scope (ADR-0009 Amendment 1):

- AMD family 1Ah or higher is Zen 5 or newer.
- Intel family 18 or higher, or a listed 2026 family-6 model, is a 2026 generation.

It never refuses to boot on that basis, and instructions are still chosen from feature bits.

## 25. Kernel threading

Use 1:1 native kernel threads as the initial model.

Support:

- True preemption.
- Parallel execution.
- Thread lifecycle.
- Kernel and user stacks.
- Thread-local state.
- Register-state preservation.
- Scheduling priorities.
- CPU affinity.
- Thread termination and cleanup.

Do not confuse asynchronous tasks with hardware execution threads.

## 26. Scheduling

Begin with a correct per-CPU scheduler.

Develop:

- Per-CPU run queues.
- Preemption.
- Thread wakeups.
- Timers.
- Fair scheduling.
- Load balancing.
- Affinity.
- CPU accounting.
- Interactive scheduling.
- Background scheduling.
- Priority-inversion mitigation.
- Resource quotas.

Evaluate EEVDF-inspired scheduling after simpler policies pass baseline tests.

Keep a safe scheduler fallback.

## 27. Topology-aware scheduling

Account for:

- Physical cores.
- SMT siblings.
- Shared caches.
- Hybrid-core capacity.
- NUMA.
- CPU migration cost.
- Power efficiency.
- Thermal state.
- Foreground responsiveness.

Do not assume all logical CPUs provide identical performance.

## 28. Memory management

Implement:

- Physical page allocator.
- Virtual memory.
- Page tables.
- Kernel heap.
- User address spaces.
- Memory isolation.
- Page faults.
- Copy-on-write.
- Demand paging.
- Shared memory.
- Memory-mapped files.
- Page cache.
- NUMA policies.
- Large pages.
- OOM handling.
- Memory reclamation.
- TLB shootdowns.

Test cross-core races and memory lifetime invariants.

## 29. Processes and isolation

Implement:

- Process creation.
- Process exit.
- Address-space ownership.
- Process handles.
- Process credentials.
- Resource accounting.
- Process supervision.
- Crash isolation.
- Cleanup.
- User/kernel transitions.

A process must not be able to access another process's memory without explicit authorization.

## 30. IPC and capabilities

Build typed, authenticated IPC.

Support:

- Message passing.
- Shared memory.
- Events.
- Wait objects.
- Completion queues.
- Cancellation.
- Capability handles.
- Access checks.
- Protocol versioning.
- Resource lifetime guarantees.

Validate all data crossing privilege boundaries.

---

# PART VII — DEVICE AND HARDWARE SUPPORT

## 31. Native driver framework

Implement:

- Device discovery.
- Resource assignment.
- Driver binding.
- Interrupt management.
- DMA mapping.
- Device lifecycle.
- Driver isolation.
- User-mode drivers where feasible.
- Kernel-mode drivers where justified. These are first-party only (ADR-0001).
- Device reset.
- Device suspension.
- Hot-plug.
- Driver fault handling.
- Recovery.
- Rollback.

Drivers must use a documented Origin Driver ABI.

Programs reach every device technology through typed SDK interfaces in three layers (ADR-0015):

- the device class;
- everything the published standard defines;
- vendor extras, only where the vendor documents them.

Each device reports truthfully which features it supports. An unsupported feature returns "not supported", never a pretended success.

Dangerous operations need rights of their own: firmware update, format, sanitize, device approval and radio transmit settings. Raw device access is a diagnostics right for administrators only. Radio host stacks and the USB4 connection manager run unprivileged.

## 32. PCIe and IOMMU

Support:

- Host bridge discovery.
- PCIe enumeration.
- Configuration access.
- BAR mapping.
- MSI/MSI-X.
- DMA.
- Intel VT-d.
- AMD IOMMU.
- Interrupt remapping.
- PCIe error reporting.
- Device reset.
- Hot-plug.
- External-device authorization.

Do not implement PHY signaling in software.

## 33. NVMe

Implement:

- Controller discovery.
- Initialization.
- Admin and I/O queues.
- Namespace discovery.
- DMA.
- Multiqueue operation.
- Interrupt handling.
- Queue timeouts.
- Controller resets.
- Flushes.
- Data-integrity handling.
- Power states.
- Health reporting.
- Recovery.

Treat PCIe generation and NVMe protocol revision separately.

## 34. USB 3.x

Implement xHCI and required USB protocols.

Include:

- USB 2 compatibility.
- USB 3.0/3.1/3.2.
- Hub management.
- Device enumeration.
- Control transfers.
- Bulk transfers.
- Interrupt transfers.
- Isochronous transfers.
- HID.
- Mass storage.
- Audio.
- Networking.
- Video devices.
- Disconnect and recovery.

Fuzz untrusted device descriptors.

## 35. USB4 and Thunderbolt

Implement:

- USB Type-C platform integration.
- Connection manager.
- USB4 router discovery.
- Tunnel management.
- PCIe tunneling.
- DisplayPort tunneling.
- Bandwidth allocation.
- Hot-plug.
- Docking-station integration.
- Power-state coordination.
- External DMA protection.
- Thunderbolt 4/5 interoperability.

Record USB-IF and Thunderbolt licensing/certification requirements.

Thunderbolt 4/5 is part of the USB4 interface, not a separate SDK. Read Intel's Thunderbolt licence before its specification. Use the Thunderbolt name only after Intel certification (ADR-0015 D5).

## 36. GPU

Implement a native graphics-driver architecture for selected Intel, AMD and NVIDIA devices.

Required capabilities include:

- Display discovery.
- Modesetting.
- GPU memory management.
- Command submission.
- Synchronization.
- Shader compilation.
- Hardware rendering.
- Compositing.
- Video acceleration.
- Compute.
- GPU reset and recovery.

Begin with a validated display fallback and one supported GPU target.

Support DisplayPort first. HDMI 2.1's high-rate (FRL) modes wait until their licensing allows an open implementation (ADR-0015 D7).

Do not claim full acceleration without a functional native driver.

## 37. NPU and AI

Implement a native Origin Compute API for:

- Accelerator discovery.
- Tensor allocation.
- Memory transfer.
- Asynchronous execution.
- CPU/GPU/NPU scheduling.
- Model-runtime integration.
- Capability discovery.
- Error recovery.
- CPU fallback.

Device-specific NPU functionality depends on accessible hardware interfaces and firmware.

## 38. Firmware policy

Track:

- Required device firmware.
- Firmware versions.
- Firmware signatures.
- Firmware loading process.
- Firmware licensing.
- Compatibility.
- Firmware update requirements.
- Recovery after failed firmware activation.

Do not redistribute restricted vendor firmware without authorization.

Do not interpret unavailable firmware as something that can always be recreated in Rust.

---

# PART VIII — FILESYSTEM AND STORAGE

## 39. Storage architecture

Use separate layers for:

- Device drivers.
- Block I/O.
- I/O scheduling.
- Partition tables.
- Volume management.
- Filesystem.
- Page cache.
- Virtual filesystem.
- Encryption.
- Backup and recovery.

Support native asynchronous I/O.

## 40. Native filesystem requirements

Implement or select an independently authored, standards-informed filesystem design with:

- Crash consistency.
- Data integrity.
- Metadata integrity.
- Atomic rename.
- Access control.
- Sparse files.
- Large files.
- Memory-mapped files.
- Quotas.
- File locking.
- Snapshots where feasible.
- Encryption.
- Repair tools.
- Versioned on-disk structures.
- Safe migrations.
- Power-loss recovery.

Do not declare a filesystem production-ready without extensive corruption and recovery testing.

## 41. Filesystem correctness

Define the semantics of:

- Durable writes.
- Flush operations.
- Directory updates.
- Rename atomicity.
- Metadata ordering.
- Storage barriers.
- Transaction completion.
- Interrupted operations.
- Snapshot consistency.

The filesystem must not promise durability beyond what the controller and storage device can actually provide.

---

# PART IX — NATIVE APPLICATION SYSTEM

All native formats are ONBF profiles (`docs/spec/ONBF.md`, ADR-0001).

## 42. Origin executable ABI

Define a real Origin userspace ABI.

Specify:

- Calling convention.
- Stack alignment.
- Register preservation.
- System calls.
- Error codes.
- Thread-local storage.
- Process startup.
- Memory allocation.
- Dynamic linking.
- Shared libraries.
- Exceptions/unwinding.
- Signal/notification policy.
- ABI version negotiation.

Do not expose unspecified Rust compiler layouts as stable binary interfaces.

Origin userspace is 64-bit only (LP64 x86-64). There are no 32-bit or x32 processes, no 32-bit compatibility mode and no 32-bit system-call entry.

System-call numbers are private to each release. The stable binary interface is the C ABI of the Origin system libraries.

## 43. `.ose`

Create a native executable container with:

- File magic.
- Header version.
- Architecture.
- ABI version.
- A native ONBF segment table. ELF64 PIE exists only as linker output and is converted by `onbf-pack`; the loader never parses ELF (ADR-0001).
- Executable segments.
- Entry point.
- Relocations.
- Dynamic dependencies.
- Capabilities.
- Signature policy.
- Resource references.
- Integrity checks.

Implement malicious-input handling, fuzzing and loader tests.

## 44. Origin standard library

Create native Origin userspace runtime services.

Support:

- Process startup.
- Memory allocation.
- Native threads.
- TLS.
- Synchronization.
- Files.
- Networking.
- Time.
- Environment.
- Standard input/output.
- Async I/O.
- Panic handling.
- Runtime shutdown.

Plan a controlled route toward Rust standard-library platform support.

Do not claim Rust `std` support before implementing the required OS services.

Software written for other systems: Rust `std` comes first. A POSIX library over the SDK would need its own ADR at M6 (ADR-0014 S6). Linux APIs stay banned (§11).

## 45. `.oapp`

Define:

- Application identity.
- Main `.ose`.
- Resource layout.
- Libraries.
- Localization.
- Icons.
- Capabilities.
- File associations.
- Launch policy.
- Signing.
- Update metadata.

## 46. `.osi`

Implement:

- Package parsing.
- Signatures.
- Dependency resolution.
- File verification.
- Transactional installation.
- Atomic registration.
- Repair.
- Upgrade.
- Uninstall.
- Rollback.
- Offline installation.
- Disk-space validation.
- Interrupted-installation recovery.

Avoid arbitrary privileged installer scripts.

No install-time code runs at all; installation is declarative (ADR-0001).

## 47. `.od`

Define:

- Driver identity.
- Hardware IDs.
- Supported Origin Driver ABI.
- Firmware dependencies.
- Capabilities.
- Privileges.
- Signature.
- Driver binary.
- Activation.
- Rollback.
- Recovery.

Third-party drivers run only in user mode, confined by the IOMMU and signed with Origin attestation. Kernel-mode drivers are first-party only, built from Origin source in the unified release (ADR-0001).

---

# PART X — COMPLETELY NATIVE SDK AND GUI

## 48. Absolute SDK-first requirement

Every first-party component must obtain OS functionality only through genuine native Origin SDK interfaces (ADR-0011).

This binds GUI applications, the desktop, the installer, Origin Setup, the recovery console, command-line tools, the shell, the terminal and system services calling one another. None has a private door: a command-line tool has no power the SDK does not offer.

The valid architecture is:

Client → Origin SDK → authenticated native service → Framekernel/driver.

The following is prohibited for ordinary OS functionality:

Client → hidden shell command → CLI program → operating-system operation.

No first-party component starts another program to do its work or parses another program's text output.

A mechanism enforces this, not review alone (ADR-0011 Amendment 1, decision item 108):

- Starting a program is a capability. Only the session launcher, the shell, the terminal and developer tools may declare it, and a release gate checks every first-party manifest.
- A program that does not hold it is never handed the launch interface.
- The base image holds no command interpreter. The shell and the terminal are an optional developer package.
- No system library reads the environment. A child's environment is empty unless its launcher sets it, and first-party services take their configuration through typed interfaces, never through arguments.
- No interface takes text that its receiver executes or hands to a program.

Programs reach the Framekernel only through the SDK, by way of `origin.core`. The Framekernel does not call the SDK.

The terminal may exist as an optional development tool. It is an ordinary SDK client and is not the graphical desktop's control plane.

The only exceptions are ADR-0011 N12's closed list: code that implements the SDK's guarantees or runs where no SDK can exist yet.

Origin Server is administered the same way. Every administrative operation is available in the native GUI, and none needs a terminal (ADR-0014 S4 and Amendment 1, decision item 117):

- on its own console;
- from the native management app on another Origin machine, over an authenticated, encrypted connection;
- in a remote desktop session, which must exist before the Server edition ships.

Neither edition needs a terminal or a web page to administer.

Installing without an operator and configuring many machines are done through the SDK as well: Origin Setup applies a signed, declarative provisioning document. No script runs (ONBF-R3).

## 49. Origin SDK

Implement typed SDKs for:

- Applications.
- Processes.
- Threads.
- Parallel tasks.
- Async I/O.
- Files.
- Storage.
- Networking.
- Audio.
- Graphics.
- Input.
- Devices.
- Installation.
- Updates.
- Security.
- Accounts.
- Credentials.
- Power.
- Notifications.
- Search.
- Printing.
- Accessibility.
- Localization.
- Compute.
- Diagnostics.
- Remote administration.
- Service management.
- Hardware health.
- Remote desktop.

The last four serve Origin Server first (ADR-0014 S7). Device technologies follow §31's three layers (ADR-0015).

Support stable versioned APIs and a C-compatible ABI.

## 50. Origin UI

Build:

- Native windows.
- Controls.
- Layout engine.
- Event dispatch.
- Text engine.
- Input system.
- Menus.
- Dialogs.
- Notifications.
- Clipboard.
- Drag and drop.
- Accessibility tree.
- Themes.
- High-DPI scaling.
- Hardware rendering.
- Animation.
- Multimonitor support.

Do not depend on HTML/Electron or browser processes for the native desktop.

## 51. Native text engine

Implement Unicode-aware text behavior, including:

- UTF-8.
- Unicode 18.0 data compatibility.
- Normalization.
- Grapheme segmentation.
- Word segmentation.
- Line breaking.
- Bidirectional text.
- Script detection.
- Font fallback.
- OpenType shaping.
- Ligatures.
- Complex-script positioning.
- Emoji.
- Input methods.
- Text editing.
- Selection and caret movement.

Test Telugu, Hindi, Arabic, Latin and mixed-direction text.

Fonts and Unicode data must have appropriate licensing and provenance.

## 52. Origin Compositor

Implement:

- Window surfaces.
- Input routing.
- Frame scheduling.
- Damage tracking.
- GPU composition.
- Software fallback.
- Atomic display state changes where hardware supports them.
- Display modes.
- Multi-monitor.
- Mixed refresh rates.
- HDR where supported.
- Color management.
- GPU reset recovery.
- Smooth animation.
- Accessibility integration.

The compositor must not be blocked by ordinary application work.

The compositor is a security boundary between programs (decision item 116):

- A client sees only its own surfaces and its own input.
- Synthetic input, global shortcuts, screen capture, reading another client's surfaces, and clipboard access without a user action are capabilities.
- Credential and elevation prompts are drawn and read by the compositor, on a path no client can draw over or inject into, behind a secure attention key.
- A remote session is marked as one.

## 53. Smoothness qualification

Measure:

- Input-to-display latency.
- Window movement latency.
- Scroll smoothness.
- Frame pacing.
- Missed frame deadlines.
- p50/p95/p99 frame times.
- CPU/GPU saturation effects.
- Display hot-plug recovery.
- Background installation interference.
- Background update interference.

Establish benchmark thresholds on declared supported hardware.

Until that hardware exists, keep provisional budgets in `docs/engineering/PERFORMANCE_BUDGETS.md`, marked as hypotheses, and record the figures CI measures as trends. They claim nothing (decision item 115).

Do not claim superior performance to Windows without a reproducible comparison.

---

# PART XI — NETWORKING, AUDIO, AND PERIPHERALS

## 54. Native network stack

Implement:

- Ethernet.
- IPv4.
- IPv6.
- ARP/NDP.
- TCP.
- UDP.
- ICMP.
- DHCP.
- DNS.
- Routing.
- Firewall.
- Socket APIs.
- TLS integration.
- Wi-Fi.
- Bluetooth.
- Network diagnostics.
- VPN extension interfaces.

Implement standards-based interoperability testing.

A Wi-Fi or Bluetooth driver needs a legitimate source for its chip's programming interface (ADR-0015 D6):

- vendor documentation that lets Origin publish source written from it; or
- §3's process for gaps: Linux read only as a reference, what it explains written into Origin's own specification first, and the code written from that in a separate session (decision items 91–93).

Never translate another operating system's driver. Host stacks may be built from the public specifications first. Before either radio ships, check Bluetooth SIG qualification and licensing and the radio regulations.

## 55. Audio

Build:

- Audio discovery.
- Output and input.
- Mixer.
- Stream routing.
- Volume management.
- Low-latency playback.
- USB Audio.
- HD Audio.
- Microphone support.
- Audio device hot-plug.
- Privacy controls.
- Error recovery.

## 56. Peripherals

Support appropriate standard device interfaces for:

- Keyboards.
- Mice.
- Touchpads.
- Touchscreens.
- Pen.
- Game controllers.
- Cameras.
- Webcams.
- Printers.
- Scanners.
- Displays.
- USB docks.

Proprietary devices require device-specific implementation evidence.

---

# PART XII — ACCOUNTS, SECURITY, AND TRUST

## 57. Authentication and identity

Implement:

- Local user accounts.
- Login sessions.
- Credential verification.
- Password hashing.
- Credential storage.
- Privilege separation.
- User switching.
- Session locking.
- Administrative authorization.
- Account recovery.
- Device ownership management.
- Secure account deletion.

Do not store plaintext authentication secrets.

## 58. Security architecture

Implement:

- Secure Boot integration.
- TPM integration.
- Measured boot where supported.
- Kernel/user isolation.
- Process isolation.
- Driver isolation.
- W^X.
- ASLR.
- Capability authorization.
- DMA protection.
- Secure IPC.
- Filesystem permissions.
- Signed packages.
- Signed updates.
- Encryption.
- Network firewall.
- Credential protection.

Threat-model every privileged interface.

Keep a system threat model, `docs/engineering/THREAT_MODEL_SYSTEM.md`: the attackers, what each edition defends against, and which component covers each. What one component's threat model puts out of scope, the system model either covers elsewhere or accepts by name (decision item 112).

## 59. Cryptographic implementation

Use established published cryptographic algorithms.

Implement from authoritative algorithm specifications when required by the original-source policy.

Require:

- Known-answer tests.
- Independent differential validation.
- Side-channel assessment.
- Constant-time analysis where applicable.
- Secure randomness.
- Key lifecycle management.
- Rotation and revocation.
- Secret zeroization.
- Independent security review.

Never invent a new cryptographic algorithm to avoid an external dependency.

For package, driver and boot trust, Origin OS needs signature verification only; signing runs in release infrastructure and host tools.

The signature suite is Ed25519 + ML-DSA-65. Root, kernel and release keys use Ed25519 + ML-DSA-87. Both signatures must verify (ADR-0001).

The kernel has one random-number generator: an established deterministic generator (NIST SP 800-90A's HMAC_DRBG over SHA-512), seeded from more than one source and reseeded, with health tests on what each source returns. Those tests catch gross failure only, and Origin claims no SP 800-90B conformance (ADR-0016, decision item 149). Nothing else draws from the processor's instructions directly (decision item 113).

The cryptographic profile covers more than signatures. Storage encryption, password hashing, transport and the generator are each decided in the profile's ADR, from a published standard, before they are built (decision item 118).

## 60. Software supply-chain security

Implement:

- Source provenance.
- Build provenance.
- Reproducible build checks.
- Dependency inventory.
- SBOM.
- Release signing.
- Trusted key storage.
- Security advisory handling.
- Artifact integrity.
- Verified release manifests.

Track timestamps, build paths and other nondeterministic build inputs.

## 61. Privacy

Design:

- Optional diagnostics.
- No undisclosed telemetry.
- Explicit permission grants.
- Permission revocation.
- Camera/microphone indicators.
- Local-first configuration where practical.
- Secret-safe logging.
- Data export/deletion.
- Offline operation.

---

# PART XIII — ATOMIC UPDATES AND UNIFIED VERSIONS

## 62. Canonical version identity

Create:

`origin-version.toml`

All released first-party system modules must belong to the same canonical Origin OS release.

This includes:

- Bootloader.
- Framekernel.
- Drivers.
- SDK.
- Runtime.
- UI.
- Desktop.
- System services.
- Security services.
- Installer.
- Update manager.
- Recovery components.
- First-party system applications.

Use generated version metadata.

Do not manually edit version strings in every module.

## 63. Separate release version from ABI compatibility

Maintain:

- Product release identity.
- ABI compatibility identifiers.
- Package format versions.
- Persistent data schema versions.
- Hardware compatibility metadata.

All first-party modules must be associated with the same release version.

Unchanged binary payloads may be reused through content-addressed storage and a newly signed unified release manifest.

Do not require unnecessary recompilation merely to achieve metadata consistency.

## 64. Secure updates

Implement:

- Signed manifests.
- Artifact hashes.
- Trusted roots.
- Key rotation.
- Expiration checking.
- Rollback protection.
- Freeze-attack protection.
- Mixed-metadata protection.
- Staging.
- Atomic deployment activation.
- Boot-health checks.
- Recovery.
- System rollback.

Use TUF's threat model as a design reference.

Do not claim full TUF conformance without implementing its relevant requirements.

## 65. Recovery

Support:

- Previous working deployment.
- Recovery boot.
- Offline repair.
- Filesystem checking.
- Driver rollback.
- Kernel rollback.
- Package database repair.
- Update cancellation before activation.
- Recovery-key entry.
- Backup restoration.
- Factory reset.
- Secure user-data removal where supported.

Keep user data separate from system image rollback.

---

# PART XIV — PERFORMANCE, POWER, AND RELIABILITY

## 66. Multicore performance

Optimize:

- Per-CPU queues.
- Cache locality.
- NUMA locality.
- SMT-aware placement.
- Hybrid-core scheduling.
- Reduced lock contention.
- IPC paths.
- Wakeups.
- Context switching.

Measure scaling rather than assuming more threads always improve performance.

## 67. Power management

Support:

- CPU idle states.
- CPU performance policies.
- Device runtime power.
- Suspend/resume.
- Modern low-power idle where supported.
- Battery reporting.
- Embedded controller integration where documented.
- Thermal monitoring.
- Fan/power policies where exposed.
- Display sleep.
- USB power.
- Safe shutdown/restart.

Physical hardware testing is mandatory for reliable laptop power management.

## 68. Timekeeping

Implement:

- Monotonic clock.
- Wall clock.
- High-resolution timers.
- Timer calibration.
- Cross-CPU time consistency.
- Time zones.
- Clock synchronization.
- Sleep/resume clock handling.
- Calendar APIs.
- Leap-second policy.
- Timer cancellation.

Do not confuse wall-clock adjustments with monotonic elapsed time.

## 69. Reliability

Build:

- Kernel panic diagnostics.
- Crash dumps.
- Service supervision.
- Driver recovery.
- Watchdogs.
- Structured logging.
- Persistent failure records.
- Safe mode.
- Boot recovery.
- Storage recovery.
- Long-duration stress testing.

---

# PART XV — COMPLETE DEVELOPER PLATFORM

## 70. Origin development tools

Create native developer tools for:

- Building `.ose`.
- Packaging `.oapp`.
- Creating `.osi`.
- Signing `.od`.
- Linking.
- Debugging.
- Profiling.
- Crash-symbol analysis.
- ABI validation.
- SDK documentation.
- Package validation.
- Driver verification.
- UI inspection.
- Conformance testing.

Developer command-line tools are permitted. On Origin OS they are SDK clients like any other program (§48).

They must not become mandatory hidden backends for ordinary graphical OS administration.

Host tools that run on macOS or Linux are outside §48 until Origin hosts its own builds (ADR-0011 N12).

## 71. Native application development

Provide:

- Origin SDK headers/bindings.
- Rust application templates.
- Stable API documentation.
- UI component documentation.
- Threading examples.
- Async examples.
- Graphics examples.
- Installer packaging.
- Debugging.
- Testing.
- Deployment guides.

Examples are supplementary to actual SDK implementation.

---

# PART XVI — TESTING AND FORMAL VERIFICATION

## 72. Required testing methods

Use:

- Unit testing.
- Integration testing.
- Property-based testing.
- Model-based testing.
- Fuzzing.
- Concurrency testing.
- Fault injection.
- Kernel boot testing.
- Hardware testing.
- Performance testing.
- Security testing.
- Upgrade testing.
- Recovery testing.
- GUI testing.
- Accessibility testing.
- Long-duration reliability testing.

## 73. Formal methods

Use Loom for suitable concurrency models.

Use Kani for suitable bounded correctness properties.

Evaluate Verus for selected high-assurance modules.

Priority areas:

- Memory allocator invariants.
- Object lifetime.
- Synchronization.
- Capability validation.
- Binary parsers.
- Update state machines.
- Filesystem metadata.
- DMA mapping ownership.

State precisely what was proved and what remains unverified.

## 74. Hardware test matrix

Test defined configurations covering:

- Multiple Intel CPU models.
- Multiple AMD CPU models.
- Different logical processor counts.
- SMT enabled/disabled.
- Hybrid scheduling.
- NUMA configurations.
- Different RAM capacities.
- Multiple NVMe controllers.
- PCIe Gen 5/6 platforms.
- USB controllers.
- USB4/Thunderbolt docks.
- Supported graphics devices.
- Network devices.
- Audio devices.
- Sleep/resume.
- Power interruptions.
- Origin Server: a KVM guest first, then physical servers on 2026-generation platforms, through the server's BMC console (ADR-0014 S3).

Do not claim physical hardware support from QEMU alone. A virtual machine is a declared scope of its own, not hardware qualification.

## 75. Reproducibility

For every release candidate:

- Build in a controlled environment.
- Record toolchain hashes and versions.
- Record source revision.
- Record configuration.
- Normalize nondeterministic build inputs.
- Rebuild independently.
- Compare artifacts.
- Investigate differences.
- Preserve provenance evidence.

## 76. Release qualification

A release may be called production-qualified only for its declared supported scope.

Require:

- Installation tests.
- Boot tests.
- Security reviews.
- Filesystem tests.
- Concurrency tests.
- Driver tests.
- GUI tests.
- Performance tests.
- Upgrade tests.
- Rollback tests.
- Recovery tests.
- Actual supported-hardware testing.
- SDK compatibility checks.
- Documentation.
- Maintenance policy.
- Before the first public release: a trademark search for the product name.

Each edition is qualified for its own declared scope.

No test count alone establishes production readiness.

---

# PART XVII — CLAUDE CODE AND PARALLEL AGENTS

## 77. Engineering agents

Create specialized responsibilities for:

1. Research verification.
2. Architecture.
3. Rust toolchain.
4. UEFI/boot.
5. Framekernel.
6. SMP/scheduler.
7. Memory.
8. IPC/processes.
9. PCIe/IOMMU.
10. NVMe/storage.
11. USB/USB4.
12. Networking.
13. Graphics.
14. NPU.
15. Native ABI/runtime.
16. Packages.
17. Native SDK.
18. UI/compositor.
19. Desktop.
20. Security.
21. Performance.
22. Updates/recovery.
23. Testing/formal methods.
24. Build/release.
25. Source provenance.
26. Context management.

Do not execute every agent simultaneously without task independence.

## 78. Agent task contracts

Every task must define:

- Task ID.
- Objective.
- Source ownership.
- Architecture contract.
- Relevant specifications.
- Required dependencies.
- Security invariants.
- Performance constraints.
- Required tests.
- Actual deliverables.
- Acceptance conditions.

Use isolated branches/worktrees.

Prevent uncontrolled concurrent edits to shared kernel interfaces.

## 79. Persistent engineering memory

The repository is authoritative.

Maintain:

- `CLAUDE.md`.
- `origin-version.toml`.
- `rust-toolchain.toml`.
- Architecture specifications.
- ADRs.
- Research ledger.
- Source provenance.
- Engineering backlog.
- Test reports.
- Hardware compatibility database.
- Build evidence.
- Session handoffs.

A change to `CLAUDE.md`, an ADR or the decision sheet lands on `main` through a pull request of its own as soon as the owner accepts it. It does not wait for a batch of code (decision item 106).

Do not rely on one AI context window to remember the project.

## 80. Cloud development

Use Claude Code Cloud where actually supported and configured.

Cloud runs may implement, test, review and prepare pull requests.

Do not assume unlimited execution, token availability or background activity.

Do not silently incur external expenses.

Do not automatically merge security-critical changes without the approved review process.

---

# PART XVIII — SOURCE STRUCTURE

## 81. Proposed repository

```text
origin-os/
├── CLAUDE.md
├── origin-version.toml
├── rust-toolchain.toml
├── Cargo.toml
├── Cargo.lock
├── .claude/
│   ├── agents/
│   ├── skills/
│   └── rules/
├── architecture/
├── boot/
│   ├── uefi/
│   ├── loader/
│   └── recovery/
├── installer/
│   ├── image/
│   ├── partitions/
│   ├── setup/
│   └── gui/
├── framekernel/
│   ├── arch/
│   ├── cpu/
│   ├── memory/
│   ├── scheduler/
│   ├── threading/
│   ├── process/
│   ├── ipc/
│   ├── interrupts/
│   └── security/
├── drivers/
│   ├── pci/
│   ├── iommu/
│   ├── nvme/
│   ├── usb/
│   ├── usb4/
│   ├── thunderbolt/
│   ├── network/
│   ├── graphics/
│   ├── audio/
│   ├── input/
│   └── npu/
├── storage/
├── filesystem/
├── networking/
├── runtime/
│   ├── ose/
│   ├── oapp/
│   ├── osi/
│   ├── od/
│   └── linker/
├── sdk/
├── ui/
│   ├── layout/
│   ├── typography/
│   ├── rendering/
│   ├── input/
│   └── accessibility/
├── compositor/
├── desktop/
├── services/
│   ├── accounts/
│   ├── credentials/
│   ├── devices/
│   ├── packages/
│   ├── updates/
│   ├── power/
│   └── diagnostics/
├── security/
├── updater/
├── recovery/
├── developer-tools/
├── tests/
│   ├── unit/
│   ├── integration/
│   ├── fuzz/
│   ├── kernel/
│   ├── emulator/
│   ├── concurrency/
│   ├── formal/
│   ├── gui/
│   ├── hardware/
│   ├── security/
│   └── upgrade/
├── docs/
│   ├── STATUS.md
│   ├── ROADMAP.md
│   ├── ARCHITECTURE.md
│   ├── spec/
│   ├── adr/
│   ├── research/
│   ├── hardware/
│   ├── toolchain/
│   └── engineering/
│       ├── TASKS.md
│       ├── SOURCE_PROVENANCE.md
│       ├── CONTEXT_PROTOCOL.md
│       └── handoffs/
└── .github/
    └── workflows/
```

Do not create empty files merely to match this structure.

Implement source files when their design contracts and development tasks are ready.

---

# PART XIX — ENGINEERING MILESTONES

## 82. Milestone plan

**M0 — Toolchain and engineering foundation**

Pin Rust 1.99.1, validate cross-compilation, establish repository, tests, research governance, source provenance and CI.

**M1 — Bootable Framekernel**

Implement UEFI boot, kernel loading, memory handoff and diagnostics.

**M2 — CPU and memory**

Implement CPU initialization, exceptions, interrupts, physical/virtual memory.

**M3 — SMP and scheduler**

Implement multiple processors, native threads, preemption, synchronization and scheduling.

**M4 — Processes and IPC**

Implement isolated user processes, system calls, capabilities and IPC.

**M5 — Storage**

Implement PCIe, DMA/IOMMU, NVMe, block storage and filesystem foundations.

**M6 — Native runtime**

Implement Origin executable ABI, `.ose` loader and native Rust user runtime.

**M7 — Native packaging**

Implement `.osi`, `.oapp`, `.od`, signing, installation and rollback.

**M8 — Native graphics**

Implement Origin UI, software renderer, compositor and first graphical applications.

**M9 — Installable operating system**

Create actual installation media, partitioning, installation GUI, first-run setup and native system management.

**M10 — Connectivity and laptop support**

Implement appropriate USB, USB4, Thunderbolt, networking, audio, power and peripheral support.

**M11 — Graphics acceleration and compute**

Implement supported native GPU/NPU capabilities, hardware graphics and compute interfaces.

**M12 — System updates and recovery**

Implement secure unified releases, atomic upgrades, recovery and rollback.

**M13 — Production qualification**

Complete release security review, performance testing, reliability tests, physical hardware qualification, documentation and maintenance planning.

**Origin Server** follows the same milestones (ADR-0014 S2):

- Its GUI comes with M8.
- Server-only work starts after M9: remote administration, remote desktop, service management and hardware health.
- USB4, Thunderbolt, Wi-Fi and Bluetooth come at M10, on physical hardware (ADR-0015 D9).

Milestones may overlap where architecture contracts permit safe parallel development.

Do not mark a milestone complete without its exit tests. Each milestone's exit tests are listed in `docs/ROADMAP.md` before it is marked done (decision item 122).

---

# PART XX — REQUIRED IMPLEMENTATION WORKFLOW

## 83. Every engineering task

Follow:

1. Inspect existing implementation.
2. Verify current applicable standards.
3. Read relevant ADRs.
4. Identify source ownership.
5. Specify behavior and invariants.
6. Define tests.
7. Implement original Rust source.
8. Compile with the approved toolchain.
9. Run relevant tests.
10. Investigate failures.
11. Review unsafe code.
12. Review security.
13. Benchmark when applicable.
14. Review maintainability.
15. Verify compatibility.
16. Integrate.
17. Record source provenance.
18. Save durable task state.

No implementation is complete merely because source code was generated.

## 84. Initial execution instructions

Immediately:

1. Inspect actual repository state.
2. Check Rust 1.99.1 availability.
3. Verify Cargo and cross-compilation targets.
4. Verify QEMU/OVMF.
5. Verify linker operation on Apple Silicon.
6. Initialize the minimum required workspace.
7. Create canonical Origin OS versioning.
8. Create research and provenance records.
9. Implement the first UEFI bootloader.
10. Implement initial Framekernel entry.
11. Build a QEMU boot image.
12. Execute and capture boot tests.
13. Implement host-testable native-format parsers in independent tasks.
14. Start SMP/topology architecture development.
15. Record actual results and failures.
16. Continue to the next safe, unblocked engineering task.

Do not stop after writing plans.

Exception: ABI and format decisions (ADRs) require owner approval before they are Accepted. Agents may build host-testable parsers against a Proposed specification, but must not ship them.

## 85. Session reporting

Report:

- Actual source code implemented.
- Files changed.
- Build commands.
- Compiler version.
- Test commands.
- Test results.
- Security review.
- Performance results.
- Research decisions.
- Source provenance.
- Architecture changes.
- Unified-version consistency.
- Git commit information.
- Known limitations.
- Next tasks.

Never fabricate test results or implementation status.

---

# FINAL MANDATORY ENGINEERING RULES

1. Rust 1.99.1 stable is the initial mandatory compiler baseline.
2. Origin OS-owned source must be independently authored.
3. No third-party OS runtime code may be imported silently.
4. Framekernel must boot independently.
5. Every unsafe boundary requires documented invariants.
6. Multicore processing must be real.
7. Multithreading must be preemptible.
8. Multitasking must isolate processes.
9. Scheduling must be scalable and hardware-aware.
10. Origin SDK must provide real native APIs.
11. Origin UI must not depend on hidden CLI calls.
12. Origin Desktop must provide genuine native interaction, in both editions.
13. `.ose`, `.osl`, `.osi`, `.oapp` and `.od` must be implemented as actual validated ONBF formats.
14. Hardware functionality must not be fabricated.
15. Filesystem integrity and recovery are mandatory.
16. Security and correctness cannot be waived for performance.
17. Cryptography must use established algorithms and rigorous validation.
18. System updates must be authenticated, atomic and recoverable.
19. First-party modules must share one canonical release identity.
20. ABI compatibility must be managed independently from product version.
21. Performance must be measured.
22. Smoothness must be measured.
23. Installation and first-run setup are product requirements.
24. Account security and credential management are product requirements.
25. Accessibility and international text are product requirements.
26. Hardware firmware restrictions must be respected.
27. Original-source provenance must be documented.
28. Reproducible builds and release signing are required.
29. Formal methods must identify the properties actually proved.
30. QEMU is not a replacement for physical hardware qualification.
31. AI agents must respect module ownership and architecture contracts.
32. The Git repository is the durable engineering record.
33. The operating system must remain maintainable for future releases.
34. No component is production-ready without its specified qualification evidence.
35. No feature may be marked complete merely because it compiles.
36. Do not optimize for artificial LOC quotas.
37. Do not stop at research or architectural documents (subject to the owner-approval rule for ABI and format decisions in §84).
38. Build actual software, execute tests and correct failures.
39. Maintain compatibility and safe migration across approved releases.
40. Production qualification must be defined against explicitly supported hardware and workloads.

**The engineering objective is a genuinely independent, original-source, Rust-based Origin OS that delivers exceptional performance, security, GUI smoothness, multicore scalability, native SDK integration, reliable upgrades and long-term maintainability.**

**BEGIN ACTUAL ORIGIN OS IMPLEMENTATION.**