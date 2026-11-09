# Research ledger: what a boot proof's failure means on a real machine

| | |
|---|---|
| Date | 2026-11-09 |
| For | `framekernel/src/bootproof.rs`, `kernel_main`, and `docs/adr/ADR-0013-boot-proof-policy.md` (**Proposed**) |
| Requires | CLAUDE.md §16 (research first), §19, §20, §58, §69, §74 and rule 30 (QEMU is not hardware) |
| Method | Every proof's code was read to see what its failure can mean. Then the question was asked of the only real-processor evidence the project has: its CI logs through KVM, on GitHub's runners. Vendor documents and public kernel history were consulted where a proof depends on a platform fact. Other kernels were studied through published descriptions, for concepts only; no operating-system source was opened or copied (CLAUDE.md §3). |

## 1. Why this was needed

ADR-0012 C4 recorded that `kernel_main` ignored every proof's result, and left the question open. Whether a proof failure should stop a real machine depends on whether the proof can fail on a machine with nothing wrong. Only evidence from real processors can answer that, so the policy rests on it rather than on how the proofs behave under QEMU's software CPU, where they were developed and where they all pass.

## 2. Sources

| ID | Source | What it establishes |
|---|---|---|
| S1 | Intel SDM Vol. 3A, Table 10-3 (valid combinations for the Pentium 4 and Xeon local xAPIC Interrupt Command Register) and Table 10-4 (P6 family), read on a mirror of an older edition: `xem.github.io/minix86/manual/intel-x86-and-64-manual-vol3/`, page 383 | The Self destination shorthand with Lowest Priority, NMI, INIT, SMI or Start-Up delivery is marked **Invalid**. Used by F1. |
| S2 | Intel SDM Vol. 3A, "ICR Operation in x2APIC Mode", read on the same mirror, page 406 | The x2APIC ICR has the same shorthand and delivery-mode encodings; ICR bits 19:18 = 01b select Self. The page does not repeat the table of valid combinations. |
| L1 | Linux commit c8c40767, "x86/timer: Skip PIT initialization on modern chipsets", with its LKML discussion (2019), read for the facts, not the code | Skylake and Apollo Lake chipsets have an ITSSPRC register that can gate the 8254: its registers still program, but IRQ0 never fires. Boards ship with this on by default. Linux stopped needing the PIT where the TSC and APIC timer rates are enumerated. Used by F3. |
| Q1 | QEMU's patch series adding the 8042 bit to the FADT (`patchew.org/QEMU/20220304154032.2071585-3-ani@anisinha.ca`, 2022), on the ACPI specification's IA-PC Boot Architecture Flags | IAPC_BOOT_ARCH bit 1 reports whether an 8042 or equivalent exists, since ACPI 2.0. Used by F2. |
| C1 | Commits `9224288` and `b1fd62c` on `claude/interesting-carson-ae7c83` | On Intel Xeon 8370C, 8573C and 6973P-C runners under nested KVM, the SMEP proof's fetch faulted with error 0x10 rather than 0x11 in 9 of 10 boots; an INVLPG cured it. |
| C2 | Commit `9d93c48` on the same branch | The scheduler's factor-of-two fairness bound failed on a Xeon 8370C (2.1 to 2.5 times, both workers running, 51 switches); the virtio-net TX ring never filled on an EPYC 9V45. |
| C3 | Commit `26264f1` on the same branch | One TSC step of 16 ticks in 261,083 handoffs on an EPYC 9V45, in 1 of 13 boots. |
| C4 | The CI logs themselves, re-read for this ledger: see §3 | How often each failure marker appeared on real processors. |
| K1 | The kernel's own source, every proof named in the ADR's inventory | What a proof's failure can actually come from, and what depends on it. |

**Gaps.**

- S1 and S2 are an **older edition on a third-party mirror**. Intel's current Volume 3A (the copies at `cdrdv2-public.intel.com`) could not be read: the PDF exceeds what the research tool extracts. So whether the current edition's x2APIC section keeps the same rows is not established, and this is F1's weak point. The fix F1 proposes (send the NMI to the processor's own APIC ID, no shorthand) is valid under either reading.
- AMD's APM Vol. 2 on the same ICR combinations was not found in readable form. F1 is not established for AMD.
- The chipset documents for the ADR-0009 machines (Panther Lake, Ryzen AI 400) were not read, so whether those platforms gate the 8254 is unknown. F3 is a risk, not a finding about them. L1 reports that a gated 8254's registers still program and IRQ0 never fires; whether channel 2's OUT2, which the kernel's calibration polls, behaves the same way when gated was not established.
- No physical machine exists yet (§74). "Real processors" below always means through KVM, with QEMU's devices and firmware.

## 3. What real processors showed

From the logs of eight Actions runs (ids below), the full log for the trial runs and failing-job logs for the pull-request runs, re-read on 2026-11-09. Lines are matched on the kernel's own failure text.

| Run | Jobs read | Complete kernel boots | Processors named |
|---|---|---|---|
| 42547128049 | `kvm` (full) | 1 | AMD EPYC 7763 |
| 42547942738 | `kvm` (full) | 16 | AMD EPYC 7763 |
| 42561010289 | `kvm` (full) | 16 | AMD EPYC 9V74 |
| 42555218264 | `boots on a real CPU` (failing job) | 11 | EPYC 9V45, per commit `26264f1` |
| 42561135158 | `boots on a real CPU` (failing job) | 15 | not named in the log excerpt |
| 42550240456, 42560054450, 42561135174 | no kernel boots in what was read | 0 | |

Fifty-nine complete boots. Across them, the only failure lines on real processors:

| Failure text | Count | Cause, where known |
|---|---|---|
| `I/O APIC failed` on the IRTE lines | 77 | KVM's split irqchip keeps the old route when an IRTE goes away; QEMU cannot show these two steps under KVM (C1's branch forgives them) |
| `clock failed` (TSC consistent) | 1 | C3 |
| `virtio-net failed` (ring never filled) | 1 | C2 |

Not one appearance, in those 59 boots, of: `user isolation failed`, `process isolation failed`, `scheduler failed`, `TLB failed`, `ipc failed`, `wait queues failed`, `heap` failures, `page protection violated`, `CPU baseline failed`, `NMI stack failed`, `NVMe failed`, `IOMMU failed`, `PCIe failed`, `topology failed`, `numa failed`, `entropy failed`, `timers failed`, `i8042 failed`.

The Intel findings (C1, C2's fairness) are from runs not in this table; they are recorded in those commits, and C1's cure is what makes that proof pass there. So this table is about AMD processors, and the Intel evidence is the commit history's.

Two proofs that passed in all 59 boots are still Report in the ADR because they can fail on a machine that is merely different (the NMI stack, F1; the i8042, F2), which CI's QEMU firmware and devices cannot reveal.

## 4. What a machine with no 8254 and no 8042 showed

QEMU can leave those devices out (`-machine pit=off,i8042=off`). OVMF boots such a machine, so it became a real test for F2 and F3 rather than a forced failure.

- Before the fix, the kernel read `0xFF` from the absent port 61h. OUT2 read high from the first poll of every window, the calibration "succeeded" on nonsense (`APIC timer calibrated against the PIT: 0 kHz ... TSC 0 MHz (32 disturbed windows discarded)`), and `timer::init` then failed on its own check.
- After the fix, a window whose OUT2 was never seen low is not used; with no 8254 every window is discarded, and the fallback runs: the ACPI PM timer. The result on QEMU TCG: APIC timer 62525 kHz, TSC 1000062 kHz, against one RTC second of 999937 us on that TSC (+/- 1 us), 0.006 % from 1 s. A read of the PM timer is a port read and so takes time of its own, which is why the calibration windows are bracketed the same way as the PIT's.

## 5. What the policy took from this

- The proofs that have failed on real processors with nothing wrong are exactly the ones that compare measured quantities or depend on platform behaviour: fairness (a ratio of loop counts), the TSC (agreement between counters), the TX ring (a race with QEMU's device thread), and the IRTE steps (a limit of QEMU under KVM). That is the shape of rule R1(b).
- A halting proof that failed on real Intel processors in 9 of 10 boots is the strongest argument for keeping the Refuse list short and structural.
- Nothing here shows that a Refuse-class proof **cannot** fail falsely. The most that can be said: none has, in the boots read, and their failure modes are register or fault checks rather than measurements.
