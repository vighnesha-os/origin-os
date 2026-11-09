# ADR-0008: Dense CPU indexes for per-CPU state; APIC IDs only as addresses

| | |
|---|---|
| Status | **Accepted** by the owner on 2026-11-08: the four questions below answered with the recommended option each. Kernel-internal (no ABI or format). It amends ADR-0003 K7 ("one run queue per CPU, indexed by x2APIC ID for now") and the per-CPU note at the end of ADR-0003. C7 (added the same day) completes the failure path of ADR-0003 K6 without changing K6's design. |
| Requires | CLAUDE.md §18 (scheduler, kernel architecture), §24 (APIC/x2APIC, per-CPU state, CPU topology), §27 (topology-aware scheduling) |
| Evidence | `docs/research/2026-11-08-x2apic-above-255.md` (how QEMU 11.1 presents an APIC ID above 255, what its MADT and SRAT list, what failed before); `cargo xtask boot-x2apic` and its negative controls in `docs/engineering/SOURCE_PROVENANCE.md` ("CPU indexes and processors above APIC ID 255"); for C7, the park drill on every boot and controls N1–N3 in the same file ("AP start-up: parking an AP that misses its handshake") |
| Related | ADR-0003 (K6 SMP start-up, K7 scheduler), ADR-0007 I8 (interrupt remapping with 32-bit x2APIC destinations) |

## Context

The kernel keeps per-CPU state in fixed arrays of 64 slots: the GS-based CPU-local areas, the run queues and thread tables, the TLB shootdown records, and the NVMe driver's per-CPU queues and wait queues. They were indexed by x2APIC ID, and the MADT and SRAT were read into 64-entry buffers.

x2APIC IDs are 32-bit and sparse. Firmware builds an ID from topology fields (package, die, module, core, thread), each rounded up to a power of two, so IDs above 255 and gaps appear on machines with only a few hundred processors, and on 2026+ many-core parts. QEMU 11.1 gives the second package of a `sockets=2,dies=3,modules=5,cores=5` machine the IDs 256–420. The MADT and SRAT list every *possible* processor, enabled or not (QEMU: all of `maxcpus`, up to 4096 on q35).

With interrupt remapping (ADR-0007 I8) an MSI can reach any 32-bit APIC ID, but the kernel could not run on such a processor. Booted on that QEMU machine, it failed with `ACPI processor discovery failed: invalid MADT` (150 structures for a 64-entry buffer) and `numa: invalid SRAT`. Even with bigger buffers, APIC ID 256 had no CPU-local area and no run queue: it would never schedule.

## Decisions

### C1. A dense CPU index, given at discovery
The boot processor is index 0, wherever and whether the MADT lists it. Every other processor the MADT marks **enabled** gets the next index, in table order. A processor listed twice (as type 0 and type 9 structures) gets one index; processors that are not enabled get none, so hot-plug is a later step. The rule lives in `framekernel::cpumap` (safe code, host-tested, including a randomized invariant test).
*Alternatives:* index = APIC ID with arrays sized to the largest ID (thousands of slots for a few processors, and the size is unknown until the MADT is read); a map from APIC ID to state (a lookup on every hot path); an index computed from the topology fields (compact only if firmware fills every field densely, which it does not have to). Table order is the one order the firmware defines.

### C2. Per-CPU arrays are indexed by `CpuIndex`
`cpu::CpuIndex` is a newtype, so an APIC ID cannot be used as an array index by mistake. The CPU-local area (GS) holds the index at offset 0 (`cpu::this_cpu`). The run queues and thread tables, `ThreadRef`, the shootdown records, and the NVMe per-CPU state use it; so does every `sched::spawn`.

### C3. The APIC ID is only an address
IPIs (INIT and start-up, reschedule, TLB shootdown) and MSI destinations (`iommu::msi_message`, so the IRTEs) use the APIC ID recorded for the index (`CpuIndex::apic_id`). The record is a lock-free table, written once per index before that processor starts, and read on the IPI paths. A shootdown target with no recorded ID is still waited for, so the shootdown times out and panics rather than skipping a processor that may hold a stale translation.

### C4. An application processor proves who it is
The boot processor hands each AP its index. The AP gets the CPU-local area of that index only if the APIC ID recorded for the index is its own (CPUID leaf 0Bh). Otherwise GS points at an area whose index names no slot, so the AP never schedules, and its start-up handshake is refused.

### C5. Discovery reads everything the firmware lists
The MADT and SRAT lists are sized from the table's length, on the heap. They are not bounded by the number of CPU indexes: a MADT may list thousands of possible processors with few enabled.

### C6. 64 indexes for now; the rest are reported and left in firmware
`MAX_CPUS` stays 64, in static arrays. An enabled processor beyond it, or one with the broadcast ID `0xFFFFFFFF`, gets no index. It is counted and reported, and stays in firmware. "Every enabled processor online" is then false, so loader and boot-services memory is not reclaimed: such a processor may still be running firmware code.
*Alternative:* allocate per-CPU state at discovery, sized to the enabled count. This is needed above 64 processors. It is a later revision of this ADR, together with hot-plug (MADT flag "online capable").

### C7. An AP that misses its handshake is parked before the trampoline data is rewritten
ADR-0003 K6 starts APs one at a time through one trampoline data page (stack, entry, start record), rewritten for each AP. An AP that misses its 200 ms handshake may be slow, not dead. If it carried on later, it would run on the next AP's stack and start record, so two processors would share one kernel stack and one set of tables. C4's refusal caught that only after the shared stack was in use. Shown live by control N1. So:
- **Each start-up is settled once.** A compare-and-swap on the start record settles it, whichever side comes first: the AP's answer or refusal, or the boot processor's deadline (`framekernel::apstart`, host-tested, including the race on real threads). An answer after the deadline does not join: the AP halts.
- **Every AP that is not online is parked before the page is rewritten.** The boot processor sends INIT (level assert) and waits the 10 ms the MP protocol gives INIT. INIT stops the processor wherever it is and leaves it in wait-for-SIPI (Intel SDM Vol. 3A, MP initialization). Only a start-up IPI runs it again, and those go only to the AP being started, after its own data is written.
- **A park drill proves this on every boot that has an AP.** Before the first start-up, the first AP is started into a loop that counts, then parked; its count must stand still. If it does not, no AP is started.

*Alternatives:*
- Per-AP trampoline data: each AP finds its own slot by its CPUID APIC ID, so no page is reused. But a timed-out AP still runs on later, after the trampoline's identity mapping is gone (ADR-0004 H1), and faults with no IDT, a triple fault. So it must be stopped anyway. Per-AP data belongs with parallel start-up (ADR-0003 K6, alternatives).
- Stop starting APs after the first timeout: safe, but every later processor is lost.
- Leave the AP and reuse the page: the hazard.

*Residual risk:* INIT is not taken at once in every state. It is blocked in VMX root operation, which the kernel never enters, and held while the processor is in SMM until it leaves (Intel SDM Vol. 3C). An SMI lasting more than 10 ms on a timed-out AP could still let it read the rewritten page. Per-AP data would close that, together with parallel start-up.

## Consequences

- Reports name a processor as `CPU {index} (APIC ID {id})` where the ID matters: AP start-up, MSI-X targets, interrupt delivery. Elsewhere they give `CPU {index}`, which equals the APIC ID only on dense machines.
- `cargo xtask boot-x2apic` boots a processor at APIC ID 256 of 150 possible: it comes online, runs threads, takes shootdowns and wake-up IPIs, and the NVMe and virtio-net interrupts reach it through IRTEs. `boot-matrix` (1, 2 and 4 CPUs) is unchanged.
- Nothing here is qualified on hardware (CLAUDE.md §74). CPU topology (package, die, module, core, thread) is now read per processor from CPUID leaves 1Fh, 0Bh or 80000026h, in records indexed by `CpuIndex` (C2), and checked against the MADT: `framekernel::topology`, research ledger `docs/research/2026-11-08-cpu-topology-leaf-1fh.md`. `boot-x2apic` boots a third processor, at APIC ID 412 (package 1, die 2, module 3, core 4), to show those fields apart.
- C7: each boot with an AP reports `SMP: park drill: CPU 1 (APIC ID n) … its count moved 0 in 1000 us: parked true`, and the boot tests require it. By construction the drill adds about 22 ms to the boot: two INIT waits of 10 ms, 1 ms watching the count, and the AP's trip through the trampoline (under a millisecond in QEMU TCG). An AP that misses its handshake is reported as `parked by INIT`, and the other APs still start.

## Trade-offs

- **An extra table lookup on IPI paths (C3)**, in exchange for compact arrays and one index type. Hot paths read the index from GS (`this_cpu`).
- **64 static slots (C6):** simple and free of allocation at boot, but the memory is reserved whether used or not, and a machine with more than 64 enabled processors leaves the rest in firmware.
- **Table order (C1)** is the one order firmware defines, but it carries no topology: adjacent indexes need not be adjacent cores. Placement must use the topology records.
- **INIT parking with a drill on every boot (C7):** about 22 ms of boot time to prove the mechanism works before anything depends on it.

## Migration consequences

None on disk and none at any ABI: `CpuIndex` is kernel-internal and is not exposed to user space.
- Indexes follow the firmware's table order, so they are not stable across boots or firmware updates. Nothing persistent may store one; logs and crash dumps should record the APIC ID as well (`CpuIndex::named`).
- Moving per-CPU state to allocation at discovery (C6) changes the storage only; `CpuIndex` and its call sites stay.
- Hot-plug extends the map; an index already given must not change.
- If the SDK ever exposes processors (affinity, §25), that needs its own stable identifier and ABI decision. `CpuIndex` should not be it.

## Questions for the owner (answered 2026-11-08: the recommended option for all four)

1. Accept dense indexes in MADT order with the boot processor as 0 (C1), and `CpuIndex` as the only per-CPU array index (C2)? **Yes.**
2. Keep 64 static per-CPU slots for now, and report and leave extra processors in firmware (C6)? Or move per-CPU state to discovery-time allocation now? **Keep 64 for now.** Per-CPU state moves to sizing at discovery with the scheduling work (TASKS item 4), before any machine with more than 64 logical CPUs enters the declared scope.
3. Should `boot-x2apic` join the default `boot-matrix` gate? Since the topology work it boots twice and adds about 40 s. **Yes:** `boot-matrix` now ends with it.
4. Accept C7: park an AP that misses its handshake with INIT, and run the park drill on every boot? Or also give each AP its own trampoline data now, ahead of parallel start-up? **Accept parking; per-AP data comes with parallel start-up.**
