# ADR-0012: The monotonic clock never goes backwards, whatever the processors' TSCs say

| | |
|---|---|
| Status | **Proposed**, 2026-11-09. It changes the kernel clock's contract and awaits the owner (CLAUDE.md §18). The code is built and boot-tested under QEMU only, on a local branch. Nothing here has shipped. |
| Requires | CLAUDE.md §68 (monotonic clock, cross-CPU time consistency), §13 (explicit memory ordering), §66 and rule 21 (measure), §73 (Loom), §74 and rule 30 (QEMU is not hardware) |
| Code | `framekernel/sync/src/floor.rs` (`origin_sync::floor::MonotonicFloor`), `framekernel/src/time.rs`, `framekernel/sync/src/models.rs`, `developer-tools/xtask/src/{main,loom}.rs` |
| Evidence | `docs/research/2026-11-09-tsc-consistency.md`; `docs/engineering/SOURCE_PROVENANCE.md`, "The monotonic clock's floor" |
| Related | ADR-0003 (the APIC timer), ADR-0008 (CPU indexes), ADR-0009 (hardware targets) |

## Context

The monotonic clock was the TSC minus its value at kernel entry, read on whichever processor called it. On 2026-11-09 CI booted the kernel on real AMD EPYC processors through KVM, and on an EPYC 9V45 the boot proof `time::prove_consistent` saw one reading step back by 16 ticks in 261,083 handoffs between processors, in 1 of 13 kernel boots. Those runners are virtual machines, so the step may be the hypervisor's, but the kernel had no answer for it on any machine: a caller reading the clock on one processor and then on another could be given a smaller time.

The research ledger found what the architecture does and does not promise. Intel and AMD each promise that one processor's counter only goes forward. Neither promises how two processors' counters compare. An invariant TSC is a promise about one counter's rate, and says nothing about two counters' values. A plain RDTSC is not ordered with the instructions around it. And a virtual machine reads whatever its hypervisor computes. So a monotonic clock across processors cannot be obtained by reading the hardware; the kernel has to make one.

## Decisions

### C1. The contract

`time::monotonic_nanos` never returns less than it has returned before, on any processor, for calls whose order the caller has established (a lock, a release/acquire pair, a wake-up, a thread's own sequence). This holds whatever the processors' TSCs do. Two calls may return the same value. It holds from the moment `time::init` publishes the rate.

It is a promise of order, not of rate. Rate is C3; time standing still is C7.

### C2. The floor: one shared high-water mark, on every read, always

Every reading passes through `origin_sync::floor::MonotonicFloor`, one atomic variable holding the latest time anyone has been given. A reading below it comes back as the floor; a reading above it raises the floor by compare-exchange and is returned.

- **Unconditional.** It is not switched off by a passing boot proof, an invariant TSC or running on bare metal. The finding is a machine that passed the proof in 12 boots of 13, so a passing proof of finite length cannot promise the rest of a boot.
- **No fence on the reading.** A plain RDTSC is used. The floor makes the ordering of the counter read irrelevant to the guarantee: whatever the read returned, the value handed out is one the floor has held. So the LFENCE or RDTSCP that keeping the counter ordered would need is not paid.
- **In nanoseconds, after the rate conversion.** The promise is about what callers are given, and the floor survives any future refinement of the rate.
- **`Relaxed` orderings**, proven sufficient by Loom (Evidence). The floor carries nothing but itself; the order between two calls is the caller's. On x86-64 the generated code is the same for every ordering.
- **Lock-free and safe in any context**, including interrupt and NMI handlers. The floor sits alone in a 128-byte-aligned block so that no other variable on the clock's path shares its cache lines.

### C3. A TSC that is not invariant

The kernel keeps using the TSC, reports it at boot, and promises order but not rate.

Without the invariant bit a counter may change rate with P-states or stop in deep C-states, so the clock's time may run fast, slow or stop, while C1 still holds. The boot line says this in words. The supported hardware (CLAUDE.md, ADR-0009) has an invariant TSC, and where the bit is false today (QEMU's TCG, a VM that hides it, an older CPU) the machine is outside the supported set. Falling back to a platform timer for such a machine, or refusing to boot on it, is not done: QEMU's TCG and KVM without `invtsc` both lack the bit and are how the kernel is tested, and a platform timer would cost about a microsecond per read to serve machines that are not supported. If a supported machine ever lacks the bit, the answer is a platform-timer clock source, decided then. Only the boot processor's CPUID is consulted for the report.

### C4. TSCs that disagree across processors

`time::prove_consistent` stays exactly as strict as it was and keeps its boot line: it is how the kernel finds out whether a machine's raw counters agree, and CI's tolerance of a tiny step under KVM remains in `xtask` only (`kvm_forgives`, PR vighnesha-os/origin-os#4). It decides nothing about the clock: the floor does not depend on it passing.

`sched::prove` returns the proof's result and `kernel_main` ignores that, as it ignores every proof's. Nothing in the clock needs to act on the result now, since the floor is unconditional. A real machine failing `prove_consistent` still prints `: clock failed`, and the boot still continues. What a real boot should do about any failed proof is a wider question and is outside this ADR.

A new boot proof, `time::prove_monotonic`, proves the contract rather than the counters: every processor reads the clock in turn, and no value may be below the one before it. It carries its own negative control in every boot (Evidence).

### C5. IA32_TSC_ADJUST: not used yet

The kernel neither reads nor writes IA32_TSC_ADJUST in this change.

- It is an Intel-documented MSR. The AMD manuals read for this work list CPUID Fn0000_0007_EBX bits 2:1 as reserved and do not name the MSR (the ledger's gaps say which revisions).
- QEMU's TCG does not offer it, and asking for it draws a warning and leaves it off. Code that wrote it could not have run anywhere Origin is tested today.
- C1 does not need it, because the floor holds whatever the counters do.
- A wrong write moves a processor's counter. The SDM's own advice is to write the same value on every logical processor, but firmware may already have used this MSR to align packages on a multi-socket machine, where equalizing would break counters that were right.

When a physical machine shows a constant offset, `prove_consistent` will say so, and a later task can repair through this MSR on the processors that have it, beginning within one package, on a machine that exhibits the problem so the repair can be proven. Until then a processor behind the others pays only C7's stand-still.

### C6. Who may read the TSC directly

`cpu::rdtsc` stays for spans that begin and end in one kernel thread, for bounded spin-waits and cost measurements. The audit in the ledger (§6) found no caller outside this proof that compares raw TSC values across processors. Anything that compares times across processors must use `monotonic_nanos`. When thread migration is built, those spans must be re-examined.

### C7. What is not promised

- **Time can stand still.** A processor whose counter is behind reads the floor until its own counter catches up, so it sees no time pass for as long as the difference. That is bounded by the disagreement: a few ticks on the machines the project targets, and the length of the offset on a machine with a constant one (which `prove_consistent` then reports).
- **One processor far ahead stops the others.** Its reading raises the floor, and every other processor reads the floor until it catches up. A wild value (a counter written to something huge, or to `u64::MAX` by the conversion's saturation) stops the clock for the rest of the boot. Such a counter present at boot fails `prove_consistent` and its `: clock failed` line. One arising later is not caught: the kernel has no runtime watchdog comparing the TSC with the HPET or PM timer, and building one is a separate task.
- **Suspend and resume.** A processor's counter can restart at zero across S3. With the floor in place that would stop the clock until the counter caught up, possibly for the whole uptime so far. §67's resume path must re-anchor the clock before anything reads it.
- **Hot-added processors, and processors with a counter this ADR's boot proof never saw.** Not built yet.
- **A clock for user mode.** No system call or shared page exposes time today. User code cannot be allowed to write this floor, so a user-visible fast clock needs its own decision: a system call, or a read-only page with its own argument for ordering.
- **Raw `rdtsc`.** The promise is `monotonic_nanos`'s only.

### C8. What it costs

Measured numbers are in Evidence. In short: a read costs one extra shared-variable access alone, and scales poorly when many processors read at once, because every reader writes the same cache line. Today no caller is hot: every use is a timeout, a deadline or a boot proof.

If a path that reads the clock very often appears, it should get its own per-processor clock with a narrower promise (same processor only), decided then, with measurements. It should not weaken this one.

## Alternatives

| Alternative | For | Against |
|---|---|---|
| Keep trusting the TSC | No cost | The finding; no vendor promises agreement between processors |
| Gate the floor on the boot proof (floor only if the proof fails) | Cheap on a machine that passes | A proof of finite length cannot speak for the rest of a boot. The EPYC 9V45 passed 12 boots in 13. Also the worst of both: the cost arrives only on the machines where a read is already suspect, and only after it has already been wrong once |
| Floor only under a hypervisor (CPUID.1:ECX[31]) | Free on bare metal | The architecture promises no agreement on bare metal either; firmware can write the counters; and there would be no way to test the unfloored path here |
| HPET or ACPI PM timer as the clock | One global counter, monotonic by construction | About a microsecond a read (Microsoft's figure); HPET may be absent; the PM timer is 24 or 32 bits and wraps; not a clean monotonic source without shared state either |
| Commit-wait or an uncertainty window (Ordo; the idea in Spanner's TrueTime) | No shared write | Needs a bound on the skew that holds for the whole boot. No vendor gives one, and the finding is exactly a failure of a bound measured once. Costs the bound on every read |
| Coarsen the floor (publish once per quantum) | Far fewer writes when many processors read in a tight loop | Returns coarser time; below the quantum's read rate it saves nothing; when many processors cross a boundary at once it stampedes the variable. Would be sound if the tight loops ever matter |
| Per-processor offsets learned from the floor | Removes C7's stand-still on a constant offset, in software | Learned offsets ratchet upward with every race and every bit of noise, and cannot be told from a stall without bracketing each read; per-processor state on the read path. Deferred until a machine needs it |
| Fence each RDTSC (LFENCE or RDTSCP) | Orders the reading | Unneeded once the floor exists: the guarantee no longer depends on the read's position |
| Repair through IA32_TSC_ADJUST at boot | Fixes a constant offset at the source | C5 |

## Evidence

To be read with its limits (next section). Numbers are from QEMU 11.1.3 TCG on an Apple M1 Max, except where marked.

- `time::prove_monotonic` on every boot of `cargo xtask boot-test` and `boot-matrix`, with its in-boot control. The line reads, for example: `monotonic across 2 processors: 5841 readings, 5563 handoffs between processors, 0 backwards, 0 held at the floor; control, counters skewed by a second per processor: 2563 backwards without a floor, 0 through one (2720 held)`.
- The in-boot negative control: the same detector, on each processor's TSC offset by one second per processor index with no floor, must step back; through a floor of its own it must not. The skew never touches the real clock. Failing either way fails the boot.
- Scratch negative controls (temporary edits, each failing the boot as expected, in `SOURCE_PROVENANCE.md`). The clearest pair: with every TSC reading on a processor offset by 5000 ticks per processor index, `prove_consistent` reported `6184 backwards (worst 5000 ticks)` while `prove_monotonic` reported `0 backwards, 1970 held at the floor`; with the floor removed from the real clock and the same skew, `prove_monotonic` reported 1704 backwards and failed the boot.
- Loom models of the real `MonotonicFloor` (`floor_handoff_between_two_processors`, `floor_handoff_chain`, `floor_racing_readers_end_at_the_largest_reading`; 158, 2668 and 513 executions, within their printed bounds), each with negative controls in `xtask`: no floor, a floor that never rises, a non-atomic raise, and the caller's handoff weakened to `Relaxed` to show the models see the difference.
- Host tests in `floor.rs`, a stress test on counters that disagree with the same check with and without the floor.

### Cost, measured (CLAUDE.md §66)

The cost is printed on every boot (`clock: reading it costs …`), so CI on real processors will report it when it runs. The local measurements, as medians over the boots of `cargo xtask boot-matrix` on 2026-11-09, on a host other sessions were also loading. Ticks are nanoseconds, roughly, under QEMU TCG's 1 GHz TSC.

| Processors | Boots | Read alone | RDTSC alone | The same read without the floor | All processors reading at once |
|---|---|---|---|---|---|
| 1 | 2 | 76 | 33 | 37 | 76 |
| 2 | 11 | 84 | 32 | 36 | 261 |
| 3 | 2 | 81 | 33 | 38 | 630 |
| 4 | 2 | 82 | 33 | 37 | 1043 |

Single boots in that table varied widely: the "alone" column ranged from 75 to 320, and "all reading at once" at 2 processors from 170 to 461. TCG translates each instruction, so a `lock cmpxchg` and a load cost differently from silicon, and the host was busy.

Every reader writing the floor on every read, native on the host (Apple M1 Max, `cargo test -p origin-sync --release floor_cost -- --ignored --nocapture`, a measure of cache-line contention on real silicon but not on x86-64): 2 ns with one reader, 17 ns with two, 90 ns with four, 301 ns with eight, 582 ns with ten.

These are not numbers for x86-64 hardware. The shape is the point: alone the floor costs one more variable access, and it does not scale when everything reads the clock at once.

## Trade-offs

A shared cache line is written on every read, so the clock is not scalable under heavy concurrent reading; C8 says what to do if that ever matters. In exchange, the contract holds on every machine, including ones the project has never seen, without any further argument about hardware. A processor that is behind sees time stand still rather than go back (C7).

## Migration consequences

`monotonic_nanos`'s signature and meaning for existing callers are unchanged except that it can no longer step back. Nothing persistent depends on it. The Framekernel framework branch moves the monotonic clock into `origin_frame::monotonic`, so a merge must keep the floor on that path; the hand-over note says how.

## Threat model (§58)

| Input | What could be wrong with it | What happens |
|---|---|---|
| A processor's TSC | Any value: firmware or a hypervisor can write or offset it, and a malicious hypervisor can feed anything | Order holds by construction (C1). A wild high value stops the clock, an availability problem (C7); nothing is read or written out of bounds, and the only arithmetic is a saturating subtraction and a 128-bit product, already covered by the clock's host tests. |

The floor is kernel-only memory. No user code reads or writes it. The clock's value is not used to decide anything about security today.

## What this does not prove

- **Physical hardware (§74, rule 30).** Everything above is QEMU TCG, and the host-native measurement is on arm64. TCG's processors share one clock, so the floor's work under a real disagreement is shown only through the skew the proof injects (and, once pushed, by CI under KVM on real AMD processors, whose logs will print the same lines).
- **The cause of the original step.** The step was seen once, on a runner that is a virtual machine under nested KVM; whether a hypervisor or the processors caused it is not known, and this design does not depend on knowing.
- **`IA32_TSC_ADJUST` behaviour** on any machine (C5).
- **Rate** on any machine lacking an invariant TSC (C3).

## Questions for the owner

1. Accept the contract (C1) and an unconditional floor (C2), at the cost shown in C8? **Recommended: yes**. Nothing else makes the promise on every machine, and no caller is hot today.
2. Accept that a machine without an invariant TSC keeps order but not rate (C3), rather than failing or moving to a platform timer? **Recommended: yes.** Supported hardware has the bit; a machine without it is outside the supported set, and it is how the kernel is tested.
3. Accept leaving IA32_TSC_ADJUST unused until a physical machine shows a constant offset (C5)? **Recommended: yes.** It is not needed for the contract, and code for it could not have run anywhere yet.
