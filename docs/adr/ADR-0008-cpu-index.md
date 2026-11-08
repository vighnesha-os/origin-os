# ADR-0008: Dense CPU indexes for per-CPU state; APIC IDs only as addresses

| | |
|---|---|
| Status | **Proposed** (2026-11-08). The owner accepts ADRs (CLAUDE.md §18, §84). This is kernel-internal (no ABI or format), so it is implemented and boot-proven while Proposed. It amends ADR-0003 K7 ("one run queue per CPU, indexed by x2APIC ID for now") and the per-CPU note at the end of ADR-0003. |
| Requires | CLAUDE.md §18 (scheduler, kernel architecture), §24 (APIC/x2APIC, per-CPU state, CPU topology), §27 (topology-aware scheduling) |
| Evidence | `docs/research/2026-11-08-x2apic-above-255.md` (how QEMU 11.1 presents an APIC ID above 255, what its MADT and SRAT list, what failed before); `cargo xtask boot-x2apic` and its negative controls in `docs/engineering/SOURCE_PROVENANCE.md` ("CPU indexes and processors above APIC ID 255") |
| Related | ADR-0003 (K6 SMP start-up, K7 scheduler), ADR-0007 I8 (interrupt remapping with 32-bit x2APIC destinations) |

## Context

The kernel keeps per-CPU state in fixed arrays of 64 slots: the GS-based CPU-local areas, the run queues and thread tables, the TLB shootdown records, and the NVMe driver's per-CPU queues and wait queues. They were indexed by x2APIC ID, and the MADT and SRAT were read into 64-entry buffers.

x2APIC IDs are 32-bit and sparse. Firmware builds an ID from topology fields (package, die, module, core, thread), each rounded up to a power of two, so IDs above 255 and gaps appear on machines with only a few hundred processors, and on 2026+ many-core parts. QEMU 11.1 gives the second package of a `sockets=2,dies=3,modules=5,cores=5` machine the IDs 256–420. The MADT and SRAT list every *possible* processor, enabled or not (QEMU: all of `maxcpus`, up to 4096 on q35).

With interrupt remapping (ADR-0007 I8) an MSI can reach any 32-bit APIC ID, but the kernel could not run on such a processor. Booted on that QEMU machine, it failed with `ACPI processor discovery failed: invalid MADT` (150 structures for a 64-entry buffer) and `numa: invalid SRAT`. Even with bigger buffers, APIC ID 256 had no CPU-local area and no run queue: it would never schedule.

## Proposed decisions

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

## Consequences

- Reports name a processor as `CPU {index} (APIC ID {id})` where the ID matters: AP start-up, MSI-X targets, interrupt delivery. Elsewhere they give `CPU {index}`, which equals the APIC ID only on dense machines.
- `cargo xtask boot-x2apic` boots a processor at APIC ID 256 of 150 possible: it comes online, runs threads, takes shootdowns and wake-up IPIs, and the NVMe and virtio-net interrupts reach it through IRTEs. `boot-matrix` (1, 2 and 4 CPUs) is unchanged.
- Nothing here is qualified on hardware (CLAUDE.md §74). The topology proof still reads CPUID leaf 0Bh only, and leaf 1Fh (dies, modules) is a later step for §27.

## Questions for the owner

1. Accept dense indexes in MADT order with the boot processor as 0 (C1), and `CpuIndex` as the only per-CPU array index (C2)?
2. Keep 64 static per-CPU slots for now, and report and leave extra processors in firmware (C6)? Or move per-CPU state to discovery-time allocation now?
3. Should `boot-x2apic` join the default `boot-matrix` gate? It adds about 18 s.
