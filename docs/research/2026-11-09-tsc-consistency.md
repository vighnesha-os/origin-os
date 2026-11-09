# Research ledger: what the TSC promises across processors

| | |
|---|---|
| Date | 2026-11-09 |
| For | `origin_frame::monotonic`, `framekernel/src/time.rs`, `origin_sync::floor`, and `docs/adr/ADR-0012-monotonic-clock.md` (Accepted 2026-11-09) |
| Requires | CLAUDE.md §16 (research first), §68 (monotonic clock, cross-CPU time consistency), §66 (measure scaling), §74 and rule 30 (QEMU is not hardware) |
| Method | Primary sources first: the current Intel SDM and AMD's manuals. Other kernels and hypervisors were studied through their published documentation, for concepts only; no operating-system source was opened or copied (CLAUDE.md §3). QEMU was queried and booted as the test machine. |

## 1. Why this was needed

A CI run on an AMD EPYC 9V45 GitHub runner, under nested KVM (Actions run 42555218264), saw `time::prove_consistent` report one reading stepping back by 16 ticks in 261,083 handoffs between processors, in 1 of 13 kernel boots. At the time `time::monotonic_nanos` returned the TSC minus an entry value, unclamped, so a caller reading it on one processor and then on another could see time go back.

## 2. Sources

| ID | Source | What it establishes |
|---|---|---|
| I1 | Intel SDM Vol. 3B, order 253669-093US, September 2026, §20.17 "Time-Stamp Counter" | The counter is 64-bit, set to 0 by RESET, and counts while halted. RDTSC returns a monotonically increasing value on the processor that executes it. RDTSC is neither serializing nor ordered with other instructions. |
| I2 | Same, §20.17.1 "Invariant TSC" | CPUID.80000007H:EDX[8] reports a counter running at a constant rate in all ACPI P-, C- and T-states, and the OS may use it for wall-clock services instead of the ACPI or HPET timers. |
| I3 | Same, §20.17.2 | RDTSCP reads the counter and IA32_TSC_AUX atomically, so software can detect a migration between two reads and, the manual says, adjust for per-CPU differences in TSC values in a NUMA system. That is, the manual itself allows that such differences exist. |
| I4 | Same, §20.17.3 "Time-Stamp Counter Adjustment" | IA32_TSC_ADJUST (MSR 3BH, CPUID.07H.00H:EBX[1]) is per logical processor and 0 at RESET. Writing the TSC moves it by the same amount, and writing it moves the TSC by the same amount. The manual says writing each processor's TSC so that all agree may be difficult, and that writing the same value to IA32_TSC_ADJUST on each logical processor is the simpler way to adjust. |
| I5 | Same, §20.17.4 and its footnote | The invariant TSC is the Always Running Timer scaled, plus an offset K that a privileged agent can adjust; IA32_TSC_ADJUST and the VMCS TSC offset are the named interfaces. |
| I6 | Intel SDM Vol. 3A, order 253668-093, September 2026, Table 12-1 | The TSC and IA32_TSC_ADJUST are unchanged by INIT. So starting an application processor does not reset its counter. |
| I7 | Intel SDM Vol. 2B, order 253667-093, September 2026, RDTSC and RDTSCP | RDTSC is not ordered with the instructions around it; LFENCE before it orders it after earlier instructions. RDTSCP waits for earlier instructions and loads, not earlier stores, and later instructions may start before it. |
| A1 | AMD64 APM Vol. 2, publication 24593 rev. 3.43, June 2025, §13.2.4 "Time-Stamp Counter" | Cleared to 0 by reset; each read returns a larger value than the previous read (of that counter). RDTSC can execute out of order. CPUID Fn8000_0007_EDX[TscInvariant] reports a constant increment rate. Nothing about how two processors' counters compare. |
| A2 | Same, §15 (SVM): `TSC_OFFSET` in the VMCB control area; §15.30.5 TSC Ratio MSR | A guest's TSC is the host's scaled by a per-guest ratio plus a per-guest offset, which the hypervisor can change. What a virtual machine reads is what its hypervisor computes. |
| A3 | AMD64 APM Vol. 3, publication 24594 rev. 3.34, October 2022, RDTSC, RDTSCP | INIT does not modify the TSC. RDTSC is not serializing. RDTSCP forces all older instructions to retire before reading. |
| A4 | Same, CPUID Fn0000_0007_EBX_x0 and Fn8000_0007_EDX | Bits 2:1 of Fn0000_0007_EBX are listed as **reserved** (Intel's bit 1 is TSC_ADJUST). TscInvariant is bit 8 of Fn8000_0007_EDX, and the manual calls a counter with that bit suitable as a source of time. |
| K1 | Linux kernel documentation, `virt/kvm/x86/timekeeping` (docs.kernel.org), read for concepts only | TSCs of different CPUs may differ; sockets can drift; a hypervisor adds offsets and scaling; the document warns guests to guard against backwards readings across CPUs. |
| K2 | Linux kernel documentation, `virt/kvm/x86/msr` (docs.kernel.org), read for concepts only | KVM's paravirtual clock has a flag saying the host guarantees monotonic readings across vCPUs. So there is a state in which it does not, and the guest must handle that state. |
| M1 | Microsoft, "Acquiring high-resolution time stamps" (learn.microsoft.com, updated 2025-12-13) | Windows checks at boot whether the TSC can back the clock and synchronizes the processors' counters; on machines where it cannot, it uses HPET or the ACPI PM timer. Platform timers cost about 0.8 to 1.0 µs per read. Windows promises the counter does not go backward. |
| O1 | Kashyap et al., "Ordo", EuroSys 2018 (paper and slides, gts3.org) | A global ordering primitive over invariant clocks, with an uncertainty window set to the maximum offset measured between CPUs after boot. Comparisons within the window are unordered. |
| Q1 | QEMU 11.1.3 on the M1 Max, measured through QMP `query-cpu-model-expansion` under TCG | `tsc-adjust` is false for `max`, `EPYC-Turin`, `EPYC-Genoa`, `EPYC-Milan`, `EPYC-Rome`, `GraniteRapids` and `SierraForest`; so is `invtsc`. |

**Gaps:**
- AMD's newest revisions (APM Vol. 2 rev. 3.44 to 3.46, Vol. 3 rev. 3.35 to 3.39) are over the research tool's 10 MB limit, so the APM statements above are from rev. 3.43 and rev. 3.34. AMD's PPRs were not read. So whether current AMD processors implement IA32_TSC_ADJUST is **not settled here**: the published APMs read list no such MSR.
- The Intel SDM statements come from the current volumes (093), text extracted from the PDFs of a mirror; the mirror was not compared with Intel's copies.
- No physical machine was available; §5 is QEMU and the development host only.

## 3. What the architecture actually promises

Read together, I1 to I7 and A1 to A4 give three facts. The design rests on them.

1. **One processor's counter only goes forward.** Both vendors say so (I1, A1).
2. **Nothing is promised about two processors' counters.** Neither manual says that they agree, or by how much they can differ, and I3 and I5 acknowledge per-CPU differences and privileged offsets. An invariant TSC is a promise about one counter's rate, not about two counters' values.
3. **A read is not ordered with the code around it** (I1, I7, A1, A3). A plain RDTSC can execute before earlier loads complete, so even perfectly synchronized counters do not, on their own, give a monotonic reading across a lock handoff.

So a monotonic clock across processors cannot be obtained from the hardware by reading it. It has to be built.

## 4. How others deal with this (concepts only)

| System | Approach, as documented | What Origin takes from it |
|---|---|---|
| Windows (M1) | Boot-time check, then TSC or a platform timer; the check is what makes the promise | A platform timer always works but costs a microsecond per read, and needs an argument for when the TSC is good enough. A finite check is the argument, and Origin's finding is a machine where a finite check passed and the machine still stepped later. |
| KVM paravirtual clock (K1, K2) | A per-boot flag says whether readings across vCPUs are monotonic | The state with the flag clear exists and has to be handled by the guest; the guest cannot assume a hypervisor's counters agree. |
| Ordo (O1) | Measure the largest offset, treat anything within it as unordered | Needs a bound that holds for the whole boot. Neither vendor gives one, and the CI finding is a case where a measured clean boot was not a bound. |

## 5. What was measured here

All of it on the M1 Max development host (QEMU 11.1.3 TCG, Rust 1.99.1), so none of it is a claim about x86-64 silicon (§74, rule 30). Numbers are in `docs/adr/ADR-0012-monotonic-clock.md` and `docs/engineering/SOURCE_PROVENANCE.md`.

- `tsc-adjust` under TCG: not offered for any model (Q1). Asking for it with `-cpu max,tsc-adjust=on` on a q35 machine draws QEMU's warning that TCG does not support the requested feature, and the property reads back false. So code writing IA32_TSC_ADJUST cannot be exercised here at all.
- `invtsc` under TCG: not offered, so every boot here runs through the "not invariant" report. KVM could not be tried here (the host is arm64). The `invariant TSC false` in the CI boot that found the step says only that the virtual CPU QEMU presented did not report the bit; it says nothing about the physical EPYC processor.

## 6. Who reads the TSC today

Audit of every `rdtsc` and `monotonic_nanos` caller in `framekernel/src` on `411b183`. Re-checked after the merge onto the framework crate (`land/top` at `bf13d90`): the same call sites moved into `framekernel/frame/src` and `framekernel/src/kernel.rs`, and the console's new spans each measure one call on one processor:

| Call | Used for | Across processors? |
|---|---|---|
| `monotonic_nanos`: `iommu::poll`, `storage` (`wait_ready`, `run`, `kill`, command timeouts), `virtionet` (send, DHCP, TFTP), `sched::block_until`, `wait.rs` timed waits, `Scheduler::monotonic_nanos`, `ipc_proof`, `main.rs` (`prove_timers`, the IOMMU translating line), `wall_nanos` | Timeouts, deadlines, elapsed time of a thread | A deadline can be set on one processor and tested on another through `sched::block_until` and the wait queues, so these need the contract. `wait.rs` even subtracts two readings unchecked. |
| `rdtsc` spans: `console::draw`, `ipc_proof`, `irq` (three waits), `pcie` (`quiesce`, `settle`), `sched` (context-switch cost, `yield_until_away`, three boot waits, `weighted_worker`), `smp` (`delay`, `within`, `start_one`), `storage`'s measured rates, `timer` calibration, `tlb` (shootdown wait), `user`, `virtionet` (ping, settle), `wait` (ping-pong, timed-wait proof) | Bounded spins and cost measurements | No: each span starts and ends in one kernel thread, and kernel threads do not migrate today (no migration or affinity code exists in `sched` or `runqueue`). |
| `time::take_turns`'s `UNTIL` and `LATEST` | The boot proof's own stopping time, compared on every processor | Yes, and only there: a run ends a few ticks earlier or later on a processor that disagrees, which does not affect its comparison of readings. |

So nothing outside the proof itself compares raw TSC values across processors today. The rule from here on is in ADR-0012 C6.

## 7. The option that was not available: IA32_TSC_ADJUST

Intel documents a way to move a logical processor's counter (I4) that would repair a constant offset between processors, and the Linux, Windows and Fuchsia designs I recall use it or their own equivalent; that recollection is why it was examined, and it is not evidence. Reasons it is not used yet are recorded in ADR-0012 C5. Two facts from this ledger matter most: it is not in AMD's published manuals as read (A4), and QEMU's TCG does not offer it (Q1), so code using it could not have run anywhere Origin is tested today.
