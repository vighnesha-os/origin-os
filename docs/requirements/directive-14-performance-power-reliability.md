# Requirements matrix: PART XIV, Performance, power and reliability

One row for each statement of CLAUDE.md in §66 to §69. The columns and their rules are in [README.md](README.md), and `cargo xtask requirements-gate` holds this file to them and to the directive.

| ID | Requirement | Kind | Scope | Owner | State | Implementation | Acceptance | Evidence | Env | Review | Limitations | Decided by |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| S66-01 | Optimize: Per-CPU queues. | build | both/hw | 33 | not-started | - | - | - | - | none | - | - |
| S66-02 | Optimize: Cache locality. | build | both/hw | 33 | not-started | - | - | - | - | none | - | - |
| S66-03 | Optimize: NUMA locality. | build | both/hw | 33 | not-started | - | - | - | - | none | - | - |
| S66-04 | Optimize: SMT-aware placement. | build | both/hw | 33 | not-started | - | - | - | - | none | - | - |
| S66-05 | Optimize: Hybrid-core scheduling. | build | both/hw | 33 | not-started | - | - | - | - | none | - | - |
| S66-06 | Optimize: Reduced lock contention. | build | both/hw | 33 | not-started | - | - | - | - | none | - | - |
| S66-07 | Optimize: IPC paths. | build | both/hw | 33 | not-started | - | - | - | - | none | - | - |
| S66-08 | Optimize: Wakeups. | build | both/hw | 33 | not-started | - | - | - | - | none | - | - |
| S66-09 | Optimize: Context switching. | build | both/hw | 33 | not-started | - | - | - | - | none | - | - |
| S66-10 | Measure scaling rather than assuming more threads always improve performance. | build | both/hw | 33 | not-started | - | - | - | - | none | - | - |
| S67-01 | Support: CPU idle states. | build | both/hw | 15 | not-started | - | - | - | - | none | - | - |
| S67-02 | Support: CPU performance policies. | build | both/hw | 15 | not-started | - | - | - | - | none | - | - |
| S67-03 | Support: Device runtime power. | build | both/hw | 15 | not-started | - | - | - | - | none | - | - |
| S67-04 | Support: Suspend/resume. | build | both/hw | 15 | not-started | - | - | - | - | none | - | - |
| S67-05 | Support: Modern low-power idle where supported. | build | both/hw | 15 | not-started | - | - | - | - | none | - | - |
| S67-06 | Support: Battery reporting. | build | both/hw | 15 | not-started | - | - | - | - | none | - | - |
| S67-07 | Support: Embedded controller integration where documented. | build | both/hw | 15 | not-started | - | - | - | - | none | - | - |
| S67-08 | Support: Thermal monitoring. | build | both/hw | 15 | not-started | - | - | - | - | none | - | - |
| S67-09 | Support: Fan/power policies where exposed. | build | both/hw | 15 | not-started | - | - | - | - | none | - | - |
| S67-10 | Support: Display sleep. | build | both/hw | 15 | not-started | - | - | - | - | none | - | - |
| S67-11 | Support: USB power. | build | both/hw | 15 | not-started | - | - | - | - | none | - | - |
| S67-12 | Support: Safe shutdown/restart. | build | both/hw | 15 | not-started | - | - | - | - | none | - | - |
| S67-13 | Physical hardware testing is mandatory for reliable laptop power management. | build | both/hw | 15 | not-started | - | - | - | - | none | - | - |
| S68-01 | Implement: Monotonic clock. | build | both/hw | 15 | built | framekernel/frame/src/monotonic.rs, framekernel/frame/src/clock.rs, framekernel/src/time.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:monotonic clock, proof:TSC consistency, proof:clock sources | virt | agent | Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S68-02 | Implement: Wall clock. | build | both/hw | 15 | partial | framekernel/src/rtc.rs, framekernel/src/time.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:clock sources | virt | none | UTC is read from the CMOS RTC. Nothing sets or synchronizes it. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S68-03 | Implement: High-resolution timers. | build | both/hw | 15 | partial | framekernel/src/timers.rs, framekernel/frame/src/timer.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:timer queue, proof:APIC timer | virt | agent | The queue serves the kernel. No program can set a timer. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S68-04 | Implement: Timer calibration. | build | both/hw | 15 | built | framekernel/frame/src/apic.rs, framekernel/frame/src/timer.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:APIC timer, proof:clock sources, xtask:boot-legacy-free | virt | none | Against the PIT, or the ACPI PM timer on a machine without one. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S68-05 | Implement: Cross-CPU time consistency. | build | both/hw | 15 | built | framekernel/frame/src/monotonic.rs, framekernel/frame/src/clock.rs, framekernel/src/time.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:monotonic clock, proof:TSC consistency, proof:clock sources | virt | agent | Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S68-06 | Implement: Time zones. | build | both/hw | 15 | not-started | - | - | - | - | none | - | - |
| S68-07 | Implement: Clock synchronization. | build | both/hw | 15 | not-started | - | - | - | - | none | - | - |
| S68-08 | Implement: Sleep/resume clock handling. | build | both/hw | 15 | not-started | - | - | - | - | none | - | - |
| S68-09 | Implement: Calendar APIs. | build | both/hw | 15 | not-started | - | - | - | - | none | - | - |
| S68-10 | Implement: Leap-second policy. | build | both/hw | 15 | not-started | - | - | - | - | none | - | - |
| S68-11 | Implement: Timer cancellation. | build | both/hw | 15 | not-started | - | - | - | - | none | - | - |
| S68-12 | Do not confuse wall-clock adjustments with monotonic elapsed time. | rule | both/hw | 15 | built | framekernel/frame/src/monotonic.rs, framekernel/frame/src/clock.rs, framekernel/src/time.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:monotonic clock, proof:TSC consistency, proof:clock sources | virt | agent | Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S69-01 | Build: Kernel panic diagnostics. | build | both/hw | 26 | built | framekernel/frame/entry/main.rs, framekernel/frame/src/unwind.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:frame-pointer backtrace | virt | agent | A panic prints a frame-pointer backtrace on the console and halts. Nothing is kept. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S69-02 | Build: Crash dumps. | build | both/hw | 26 | not-started | - | - | - | - | none | - | - |
| S69-03 | Build: Service supervision. | build | both/hw | 38 | not-started | - | - | - | - | none | - | - |
| S69-04 | Build: Driver recovery. | build | both/hw | 9 | not-started | - | - | - | - | none | - | - |
| S69-05 | Build: Watchdogs. | build | both/hw | 54 | not-started | - | - | - | - | none | - | - |
| S69-06 | Build: Structured logging. | build | both/hw | 26 | partial | framekernel/src/klog.rs, framekernel/src/console.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:concurrent console | virt | agent | A kernel log of whole lines on the console. It is not structured, and nothing keeps it. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S69-07 | Build: Persistent failure records. | build | both/hw | 26 | not-started | - | - | - | - | none | - | - |
| S69-08 | Build: Safe mode. | build | both/hw | 54 | not-started | - | - | - | - | none | - | - |
| S69-09 | Build: Boot recovery. | build | both/hw | 54 | not-started | - | - | - | - | none | - | - |
| S69-10 | Build: Storage recovery. | build | both/hw | 54 | not-started | - | - | - | - | none | - | - |
| S69-11 | Build: Long-duration stress testing. | build | both/hw | 24 | not-started | - | - | - | - | none | - | - |
