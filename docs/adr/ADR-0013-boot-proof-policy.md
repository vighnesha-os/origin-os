# ADR-0013: What a failed boot proof means on a real machine

| | |
|---|---|
| Status | **Proposed**, 2026-11-09. It decides what a machine does when the kernel's own checks fail, so it awaits the owner (CLAUDE.md §18). It is not an ABI or format decision (§84). Everything below is built and boot-tested under QEMU, on a local branch, except what Migration says is left for later. Nothing here has shipped. |
| Requires | CLAUDE.md P0, P8, §13 (explicit panic handling), §18, §19 (early diagnostics), §20 (boot-failure detection), §24–§26, §28–§31, §58, §59, §65, §68, §69 (safe mode, boot recovery), §74, rules 14, 16, 30, 34, 35, 40 |
| Code | `framekernel/src/bootproof.rs` (the policy table and the tally), `framekernel/src/main.rs` (`kernel_main`), `framekernel/src/sched.rs`, `framekernel/src/wait.rs`, `developer-tools/xtask/src/main.rs` |
| Evidence | `docs/research/2026-11-09-boot-proof-failures.md`; `docs/engineering/SOURCE_PROVENANCE.md`, "What a failed boot proof means" |
| Related | ADR-0003, ADR-0007 (the driver gate), ADR-0009 (hardware targets), ADR-0010 E5 (a console problem never stops the boot), ADR-0011, ADR-0012 C4 (which left this question open) |

## Context

Every boot proof prints a result. `kernel_main` stopped for five of them: relocations not applied, an invalid boot info, a CPU below x86-64-v3, no room for the frame bitmap, and SMEP/SMAP. It ignored the result of every other proof. A failed proof printed a line such as `: clock failed`, which `cargo xtask` turned into a failed boot, and on a real machine nothing acted on it: the kernel went on and ended with `M1 entry complete; halting`.

Two things made this worth deciding rather than patching.

**Ignoring results is not always harmless.** A kernel whose page-table proof failed carried on with no heap. Control C0 in Evidence shows what that looked like: twelve more lines that all read as healthy, then `panic: memory allocation of 40 bytes failed` on the first heap allocation, with nothing connecting the panic to its cause.

**Stopping on every failure would be worse.** Proofs have already failed on real processors with nothing wrong, and one of the five existing stops was among them. A 2.1 to 2.5 times imbalance between two workers failed the scheduler proof on a Xeon 8370C. A TSC step of 16 ticks in 261,083 handoffs failed the clock proof on an EPYC 9V45. A ring that never filled failed the NIC proof there too. On three Intel Xeons the SMEP proof failed in 9 of 10 boots because of a stale translation in the proof's own setup (since fixed by an INVLPG on the CI branch), and that proof already halts. The evidence is the CI boots through KVM on real processors, listed in the research ledger. Under every one of those a halting policy would have stopped a working machine.

So the policy has to say which proofs can stop a boot, and on what grounds.

## Decisions

### R1. A proof stops the boot in two cases

(a) **Nothing can run without what it covers.** The APIC timer is the example: with no tick there is no preemption, clock or timeout. Whether a correct kernel could fail the proof does not matter here, because stopping loses nothing the machine could have gone on to do. It can still be a machine the kernel should support (F3); the fix then is a better proof or a better calibration, not a looser rule.

(b) **It covers a P0 property (memory safety, data integrity, isolation, concurrency correctness, authorization) and it is structural:** a correct kernel on a correct machine cannot fail it through timing, load or being a different machine. It may wait on something with a budget of seconds (running out of budget means something that must make progress did not), but it may not compare two measured quantities, assume which devices a machine has, or demand a tight time bound.

Either case makes it **Refuse**: the kernel stops (R4, R5). P0 outranks availability, so a false positive costs a boot, and that cost is accepted. Rule (b)'s second half is what keeps false positives rare, and it is why a proof of a P0 property that can fail on a correct kernel is Report until the proof is fixed, not Refuse.

Where one result mixed the two kinds, it is split. Scheduler fairness (a factor-of-two bound on loop counts) is separate from preemption (both workers ran on their processor and were switched at least 15 times). Wait-queue timing is separate from wait-queue correctness.

### R2. A proof that covers one capability withdraws that capability

**Withdraw** means the boot goes on without that one thing, and the thing is withheld where it would be used, so the check cannot be forgotten. The gates all existed before this ADR: `DeviceDma::attach` refuses every driver when the quiesce or the IOMMU failed (ADR-0007), a processor that did not start is not used, and the loader's memory is not reclaimed unless every processor came up. Only the record of it is new.

There is no system-wide degraded mode. A kernel-level "safe mode" has nothing to run without user space. §69's safe mode belongs to the services layer, which does not exist yet.

### R3. Everything else is reported and the boot goes on

**Report** means nothing relies on the result yet, or the proof can fail on a correct kernel. The proof prints its line as before, and the verdict (R6) names it.

A proof moves from Report to Withdraw or Refuse when something comes to depend on what it proves. Entropy is the plain example: nothing consumes it yet, and whatever first does must withhold keys when the proof failed (§59).

### R4. When the kernel stops

Immediately, for the proofs everything else stands on: page tables, frame allocator, heap, the APIC timer, and each processor's control state. There is nothing useful to run on top of them.

At the end of the thread proofs, for the proofs inside `sched::prove`. They run to the end and are judged together. Everything that runs after a failure is the kernel's own fixed proof programs, not outside code, and the operator gets every result instead of the first. The device proofs that follow do not run.

### R5. What stopping means

The kernel prints `origin-kernel: boot refused: <proofs> failed; the kernel stops rather than run without it (ADR-0013)` on every console ("them" for more than one proof) and halts the boot processor. The application processors keep running what they were, which is their idle loop unless a proof timed out; nothing new starts on them.

It does not reset. Nothing yet records a boot attempt (§20), so a reset would only erase the one diagnostic on the screen. When the boot manager exists, a refusal becomes how the kernel tells it this deployment failed, and it can fall back to the previous one (§64, §65).

### R6. Every boot ends with a verdict

Before the halt line the kernel prints what its proofs found, on every console, because a real machine's operator has no `xtask` and the individual lines scroll away:

- `origin-kernel: boot verdict: 45 of 45 proofs passed`
- `origin-kernel: boot verdict: degraded: 42 of 45 proofs passed; failed: IOMMU bring-up (no device drivers), NVMe (no storage), i8042`
- `origin-kernel: boot verdict: degraded: 33 of 33 proofs passed; 12 did not run`

It claims only what was recorded. The virtio-net proof is the one proof it does not cover (see Migration).

### R7. The table is the policy

`framekernel::bootproof` holds one table of every proof, its name and its consequence, generated so a new proof cannot be added without saying what its failure means. A host test pins the Refuse and Withdraw sets, so changing one fails until this ADR changes with it.

### R8. What did not change

`cargo xtask` still fails a boot on every `: X failed` line, Report-class ones included: the test is stricter than the policy on purpose. The proofs' own lines are unchanged. The new failure marker is `origin-kernel: boot refused`.

## The inventory

"Correct kernel can fail it?" asks whether a real machine could fail the proof with nothing wrong. "Seen" cites a failure on real processors under KVM (research ledger §3).

**Already stopped the boot, unchanged:**

| Check | Failure means | Decision |
|---|---|---|
| Relocations applied | Loader or image broken; even formatting would crash | Refuse (existing) |
| Boot info valid | Nothing the loader said can be trusted | Refuse (existing) |
| x86-64-v3 and AVX state | The kernel's own code would fault (§14) | Refuse (existing) |
| Room for the bitmap and page tables | No memory to manage | Refuse (existing) |
| SMEP and SMAP (`prove_user_isolation`) | The kernel is not protected from user pages | Refuse (existing). **Seen** failing falsely on Xeons (fixed on the CI branch, not yet on `main`) |

**Refuse, newly enforced (R1):**

| Proof | Failure means on a real machine | Correct kernel can fail it? | Today's dependents |
|---|---|---|---|
| Page tables (`init_paging`) | No NX, tables not built, kernel left at a low alias, or a W^X or guard probe did not fault | No | Everything |
| Frame allocator (`prove_frames`) | RAM does not hold data, or the allocator's bookkeeping is wrong | No | Heap |
| Heap (`prove_heap`) | Contents wrong or a leak | No | Everything after it |
| APIC timer (`timer::init`) | No x2APIC, PIT calibration failed, or the tick rate is wrong: no preemption, clock or timeouts | **Can**: it needs a working 8254, which a machine may gate (F3). Refused under R1(a): nothing can run without a tick | Everything |
| Per-processor control state (`smp::start`) | A processor allows ring 3 RDPMC or FSGSBASE, leaves SYSENTER or a debug breakpoint armed, or will not raise #MC or #MF | No: register reads | Ring 3 |
| Preemption (`sched::prove`, split) | A processor's two workers did not both run there, or switched fewer than 15 times, or a thread stack's guard did not fault | Only with the tick already broken: passes at any load seen | Everything |
| SIMD state (`prove_simd`) | One thread saw another's registers | No: a corruption count | Every thread |
| Ring-3 isolation (`user::prove`) | A user program read kernel memory or ran a privileged instruction, or two processes shared an address space | No | Ring 3 |
| TLB shootdown (`tlb::prove`) | A processor kept a stale translation after an unmap | No | Every unmap |
| Wait queues (`wait::prove`, split) | Items lost or reordered, or a thread never woke | No | IPC, drivers |
| IPC and handles (`ipc_proof::prove`) | A handle or quota check let a hostile call through. This one result also covers a leak check | No | Ring 3 |

Apart from the SMEP proof's stale translation above (Intel, fixed on the CI branch), none of the Refuse proofs has failed on a real processor in the runs read: 59 complete boots on AMD EPYC processors through KVM (research ledger §3). AMD only, and virtual machines, so this is the strongest evidence the project has rather than proof.

**Withdraw (R2; the gates predate this ADR):**

| Proof | Failure means | Withheld |
|---|---|---|
| Bus-master quiesce (`pcie::quiesce`, `pcie::check_quiet`) | A device may still be mastering the bus (T4) | Every driver (`DeviceDma`) |
| IOMMU bring-up (`iommu::init`) | Units not running, or their tables unreadable. No DMAR or IVRS at all passes (ADR-0007, sheet item 9) | Every driver |
| Application processors (`smp::start`) | One or more did not start | Those processors, and the reclaim of loader memory (`online.complete`) |
| PCIe enumeration (`pcie::init`) | The bus could not be listed | Every driver |
| NVMe (`storage::init`) | The controller did not come up. On a disk that is not the boot test's, only the read-only proofs run | Storage |

**Report (R3):**

| Proof | Failure means on a real machine | Correct kernel can fail it? |
|---|---|---|
| Backtrace (`prove_backtrace`) | Panic reports lose frames | No, but nothing rests on it |
| Bus transactions settled (`pcie::settle`) | A device has a request outstanding after 100 ms; its reads return to it alone (T4) | Yes |
| NMI stack (`prove_nmi_ist`) | Either the NMI was not delivered or it ran on the wrong stack | **Yes**: the proof sends itself the NMI with a shorthand Intel's table marks invalid (F1) |
| Clock sources (`time::init`) | The RTC, HPET or PM timer disagrees with the TSC, or the wall clock is unreadable. Nothing uses HPET or the PM timer, and the monotonic clock is published either way | Yes: firmware values, PIT gating |
| I/O APIC routing (`irq::prove`) | The PIT's IRQ0 did not arrive through the I/O APIC | **Can**: a gated 8254 has no IRQ0 (F3) |
| CPU topology, NUMA affinities, NUMA distances | Firmware and CPUID disagree. The scheduler uses none of it yet | Yes: firmware tables |
| Hardware entropy (`prove_entropy`) | RDSEED/RDRAND dead or stuck | Yes. Nothing consumes it yet (R3) |
| Scheduler fairness | Two workers' loop counts differ by over 2× | **Seen**: 2.1–2.5× on a Xeon 8370C (shared cores) |
| Scheduler weights | Shares off under load | **Seen** failing under load (1 boot in 30, all cores loaded) |
| Thread lifecycle, refused spawns, thread reaping | Accounting or a leak; a yield or sleep out of bounds | Yes: timing bounds. A leak is a bug, not a safety failure |
| TSC consistency (`prove_consistent`) | Counters disagree. The clock does not rest on this (ADR-0012 C2, C4) | **Seen**: 16 ticks on an EPYC 9V45 |
| Concurrent console | A processor's lines did not all print | Yes |
| Wait timing | Round-trip or wake-up latency out of bounds | Yes |
| IOMMU proofs through NVMe (`storage::prove_iommu`) | Translation or blocking did not behave. Skipped on a disk that is not the boot test's | **Yes**: "no NVMe controller" counts as a failure when the IOMMU translates (F4) |
| Remapped I/O APIC (`irq::prove_remapped`) | Interrupt remapping of the PIT's pin | **Seen**: KVM's split irqchip fails both IRTE steps every boot. And the 8254 again (F3) |
| Bus mastering after the drivers (`pcie::prove_bus_mastering`) | A function has the wrong bus-master state | **Yes**: a platform with no IOMMU always fails (F5) |
| VFS, initramfs, rasterizer, net, TCP, `/sys` | A pure library gave a wrong answer: a real sign of bad memory or a miscompile, since the same code passes on the host. But nothing uses them yet | No, but nothing rests on them |
| i8042 | No PS/2 controller answered | **Yes**: a legacy-free machine has none (F2) |
| Timer queue (`prove_timers`) | Late or out-of-order timers | Yes: repeated on stalls, still load-sensitive |

**Not in the verdict yet:** the virtio-net proof (`virtionet::prove` returns nothing; see Migration). On a real machine it does nothing unless the NIC is virtio.

## Follow-ups this inventory found

Each of these is a proof that cannot yet tell a fault from an absent device or a machine difference. None changes the decisions above.

- **F1. `prove_nmi_ist` sends a self-NMI** through the Self shorthand (`ICR` bits 19:18 = 01b with delivery mode NMI). Intel's table of valid combinations marks that invalid. Send it to the processor's own APIC ID with no shorthand, and then the proof can be Refuse.
- **F2. `i8042::init` fails on a machine with no controller.** Consult the FADT's IAPC_BOOT_ARCH bit 1 and call it absent.
- **F3. Nothing calibrates or proves interrupts without the 8254.** On a platform that gates it (Intel PCH clock gating; Linux has skipped PIT setup on such platforms since 2019), IRQ0 never fires, so `irq::prove` and `irq::prove_remapped` fail, and `timer::init`'s calibration through channel 2 may fail too (the ledger says what is and is not known). Before the first physical boot the kernel needs a calibration source that does not depend on it (CPUID 15h and the HPET both exist here).
- **F4. `storage::prove_iommu` fails when the IOMMU translates and no NVMe controller is present.** Nothing was disproved in that case.
- **F5. `pcie::prove_bus_mastering` requires translation,** so it fails on a platform with no IOMMU, a case the owner accepted for internal devices (ADR-0007).
- **F6. The fairness proof counts loop iterations.** The weights proof already measures time. Fairness could too.
- **F7. Inside `smp::start`:** the interrupt-stack guards and the trampoline's identity pages are printed but not visible to `kernel_main`. They belong to this table after the framework branch moves the file.

## Evidence

**Controls (2026-11-09, QEMU 11.1.3 TCG, 2 processors, Intel VT-d):** temporary edits on the final tree, each booted once with `boot_failures` emptied so the boot runs past a failing line. The serial log is what was checked.

| # | Edit | Result |
|---|---|---|
| C0 | `paging` forced false, **on the code before this change** | Twelve healthy-looking lines, then `panic: memory allocation of 40 bytes failed`. Nothing connects the panic to the cause. |
| C1 | `paging` forced false | `boot refused: kernel page tables failed ...`, the last line (39 lines in the log). No later proof runs. |
| C2 | `timer::init` result forced to `None` | `boot refused: APIC timer failed ...`, last line. |
| C3 | `user::prove` (a real failure: the proof expects the wrong fault) | The group runs on (IPC lines and all), then `boot refused: ring-3 isolation failed ...`, last line. PCIe, NVMe and the rest did not run. |
| C4 | `tlb::prove` recorded as failed | The group runs on, wait queues and IPC included, then `boot refused: TLB shootdown failed ...`, last line. |
| C5 | Scheduler weights recorded as failed | The boot goes on to the end: `boot verdict: degraded: 44 of 45 proofs passed; failed: scheduler weights`, then the halt line. |
| C6 | Fairness forced off, with both workers running and switching | `: scheduler failed` printed on each processor as before, preemption not refused; the boot goes on and ends `boot verdict: degraded: 44 of 45 proofs passed; failed: scheduler fairness`. |
| C7 | `iommu::init` returns failure (the real driver gate, not the tally) | Drivers refused where they are used (`NVMe: an IOMMU is present but did not come up, so no driver may bind`, the same for the NIC); `boot verdict: degraded: 42 of 45 proofs passed; failed: IOMMU bring-up (no device drivers), NVMe (no storage), bus mastering after the drivers`. |

C7's third entry is the cascade R1 expects: F5's proof cannot pass without translation.

**Real processors:** research ledger §3 (CI through KVM).

**Host and gates (the final tree):** `framekernel::bootproof`'s unit tests pin the table and the exact text of both lines. `cargo xtask check` (979 host tests, 0 failed; 26 Loom models, 35 controls), `boot-test` (11 of 11) and `boot-matrix` (both IOMMUs at 1, 2 and 4 processors, the sparse machine, the bridge machine, the console machines) pass. Every one of the 17 full boots in `boot-matrix` ended `boot verdict: 45 of 45 proofs passed`.

## Alternatives

| Alternative | For | Against |
|---|---|---|
| Keep ignoring results | Simplest | Runs on with broken isolation the moment anything real runs. The kernel's last line says nothing failed |
| Halt on every failure | One rule | Stops working machines. The proofs have already done this on real Xeons and EPYCs |
| A kernel-wide "degraded" flag instead of per-capability withdrawal | One bit to check | Nothing would know what it may still do. The gates exist already and are per capability |
| A global safe mode | Standard name, §69 | Needs a service layer to be safe in. Belongs there, later |
| Refuse only on memory safety, leaving isolation proofs as Report | Fewer stops | P0 includes isolation and authorization. A kernel that cannot show them must not run outside code |
| Report the proofs but run a boot-manager reset on a refusal | Self-healing | No boot manager records attempts yet; a reset loop would erase the screen, the only diagnostic |
| A kernel command line or firmware switch to continue past a refusal, for first physical boots | Sees further on a bad machine | A switch that can be left on in a product. The verdict already shows everything up to the refusal; a one-line local edit does the rest for a bring-up session (question 4) |

## Trade-offs

A refused boot is a machine that does not start, and the same code may be working perfectly (a Xeon with a stale translation in a proof's own setup is the proof of that). R1(b)'s structural test is the defence. It leaves some P0 properties covered by a proof that cannot refuse, such as the NMI stack, until the proof is fixed. The alternative is worse: a boot that halts because a manual's table said "invalid".

The Refuse list costs a stricter boot on real machines than the proofs had before, but no stricter than they had already been in QEMU, where every one of these has always been a failed boot.

## Migration consequences

- `sched::prove` and `wait::prove` take a `&mut Tally` and return nothing. The other proofs that returned nothing now return `bool`.
- **The framework branch (`claude/silly-bose-b3bc0a`)** moves `kernel_main` to `kernel.rs`, turns `framekernel::` into `crate::`, and moves `prove_frames` into `origin_frame::early`, which must return its result (`start_heap` is the natural place). `smp.rs`, `iommu.rs` and `virtionet.rs` were left alone so that it could merge. The table is one new file. Each call site is a one-line edit, and the hand-over note lists them.
- **ADR-0012** adds `prove_monotonic` to `sched::prove`: it needs a row in the table.
- **The CI branch (PR vighnesha-os/origin-os#4)** forgives four lines under KVM: none of them is a Refuse-class failure, and the boot test only requires the verdict line to exist, so a forgiven proof failing does not fail the boot. The SMEP fix on that branch matters more now: without it, a Refuse-class proof falsely stops Intel machines.
- The nesting in `kernel_main` (`if paging`, `if let Some(tsc_hz)`, `heap_ok`) is now mostly unreachable. It stays to keep this change small; it can be flattened when the framework branch lands.
- The virtio-net driver needs its proof to return a result, then a row in the table; that file is being rewritten by the framework branch, so it waits for that.

## Threat model (§58)

| Who | What they can do | What happens |
|---|---|---|
| Firmware or a device forcing a Refuse-class proof to fail | Stop the boot | Availability only, and they could already stop a boot by hanging it. Nothing is read or written out of bounds |
| Firmware or a device forcing a Withdraw or Report proof to fail | Lose a capability or a line | Fails closed: the capability is withheld |
| Anything making a proof pass wrongly | Not addressed: this ADR decides what results mean, not whether they are right | The proofs' own soundness is each proof's concern |
| The verdict and refusal lines | Fixed text and proof names only; no addresses, keys or user data (§61) | Safe to show |

## What this does not prove

- **Physical hardware (§74, rule 30).** Everything is QEMU. CI through KVM runs real processors with QEMU's devices and firmware, and that is the strongest evidence here; it is not qualification.
- **Hangs.** A proof that never returns is still a hang. There is no watchdog (§69).
- **Persistent records.** A refusal is not recorded anywhere that survives the halt (§20, §69).
- **That the Refuse proofs detect what they name** beyond each proof's own controls.

## Questions for the owner

1. Accept R1's two cases (nothing can run without it; or a P0 property with a structural proof), and R2 and R3 as the rest of the policy? **Recommended: yes.** It is the smallest policy that does not stop working machines.
2. Accept the Refuse list as above? **Recommended: yes.** The most debatable entry is the APIC timer, which refuses on a machine with a gated 8254 where the kernel could not run anyway (F3).
3. Accept that a refusal halts rather than resets until a boot manager records boot attempts (R5)? **Recommended: yes.**
4. No "continue past a refusal" switch for first physical boots? **Recommended: yes, no switch.** A one-line local edit does it for a bring-up session, and a switch can be left on.
5. Which proofs should a production boot still run? They were written for development and cost several seconds of deliberate waiting on every boot (the 2 s weights proof alone, the 1 to 3.2 s RTC second). **Recommended: decide before M9** and keep them all until then; the table above is the list to prune.
