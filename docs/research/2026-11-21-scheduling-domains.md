# Research ledger: scheduling domains, shared caches and capacity sources (TASKS item 97)

| | |
|---|---|
| Date | 2026-11-21 |
| For | TASKS item 97 (plan P6): `origin_frame::topology` (shared caches), `origin_frame::domains`, `origin_frame::capacity`, `origin_frame::acpi::pptt_caches`, `framekernel::numa::node_order`, `prove_domains`, the SMT and two-package machines in `cargo xtask` |
| Requires | CLAUDE.md §3, §16, §17, §24, §27, §74; ADR-0020 S8, S9, S23 |
| Method | The M3 research ledger (`2026-11-20-m3-scheduling.md`, §1, §2.2 to §2.8) already reads these documents. Each section the code rests on was read again on 2026-11-21 at the same revision: Intel's and AMD's manuals by `curl` into `pypdf` in memory, with nothing saved and each SHA-256 compared with the ledger's; ACPI 6.6 from the owner's local PDF with macOS PDFKit (SHA-256 checked). Facts are paraphrased with section and page. No operating system's source and no QEMU source was read (§3): QEMU's behaviour was measured through its own QMP interface with guests paused (`-S`), never from its code. No network was used between 00:00 and 00:30 UTC (a rented-server run) |

## 1. Sources

| ID | Document | Revision | Read | SHA-256 |
|---|---|---|---|---|
| I1 | Intel SDM Vol. 1 (253665) | -093US, September 2026 | `cdrdv2.intel.com/v1/dl/getContent/671436` (redirects to `253665-093-sdm-vol-1.pdf`), in memory | `f6ffc430dfa2dada4f791b1ecb462810e59827befe9a9c23f9160941eea642f9`, as the M3 ledger's |
| I3 | Intel SDM Vols. 3A to 3D (325384) | -093US | `…/671447`, in memory | `77919be320f35da2782771ef2cb65a868cf68e62c8f8279f2ca61199cb70d1f8`, as the M3 ledger's |
| A3 | AMD APM Vol. 3 (24594) | 3.39, October 2026 | AMD portal `BqEoC2FadIAgbrUdEzDJIg`, in memory | `b598514b19f204d973040d9867a7ffc48c949009a8b3dc86b20c9188e721ee45`, as the M3 ledger's |
| A2 | AMD APM Vol. 2 (24593) | 3.46, October 2026 | AMD portal `LNBl5evreogi0r25VIFF2g`, in memory | `e4130d5aa26227b6a116d75af9e0f6e5adb4c034ff0d49a3c0fe73c3f49794cf`, as the M3 ledger's |
| ACPI66 | ACPI Specification 6.6 (UEFI Forum, May 13, 2025) | 6.6 | the owner's local PDF, PDFKit; never in the repository (§17) | `8c7542dd4de974ae47bba71bb0336637fe1e3838daad7692370ab4cf218efd35` (checked) |
| Q | QEMU 11.1.3 (Homebrew build on the development Mac) | 11.1.3 | QMP `query-cpu-model-expansion` and `query-cpus-fast`, and `-machine q35,help` / `-device max-x86_64-cpu,help`, guests paused | — |

## 2. What each source says, as the code uses it

- **Leaf 04H** (I1 Table 21-14, PDF p. 459). Sub-leaves until type 0; EAX[4:0] type (1 data, 2 instruction, 3 unified, 4 to 31 reserved); EAX[7:5] level from 1; EAX[25:14] plus one is the most addressable IDs of the cache's sharers, and the nearest power of two not below it is how many initial APIC IDs are set aside for them. EAX[31:26] may be saturated where leaf 0BH or 1FH exists. Every field but the line size is in the Logical Processor domain. → `Caches::decode`, `SharedCache::shift`.
- **Fn8000_001D** (A3 §E.4.15, PDF pp. 704 and 705). Reserved without Fn8000_0001_ECX[22] TopologyExtensions; sub-leaves until type 0; ShareId is the APIC ID shifted right by log2 of NumSharingCache + 1, rounded up to a power of two; EAX[7:5] levels 1 to 3, the rest reserved; **the sub-leaf's index need not be the cache's level.** → `Caches::detect`, the reserved-level rule.
- **Fn8000_0026** (A3 §E.4.24, PDF pp. 716 and 717): core type EBX[31:28], native model EBX[27:24], efficiency ranking EBX[23:16] (with EAX[29]), a fixed ranking of efficiency among the cores of one core type (APM Vol. 3 §E.4.24, paraphrased). Already decoded by `topology.rs`; → `capacity::Ranking`.
- **The HFI and Thread Director tables** (I3 §17.6.1 and §17.6.2, Tables 17-4 to 17-9, PDF pp. 553 to 558). The HFI: a 16-byte header (an 8-byte timestamp; performance flags at byte 8 and efficiency flags at byte 9, bit 0 changed, bit 1 force or inject idle) and an 8-byte row per logical processor (byte 0 performance, byte 1 efficiency, 0 to 255; 0 "do not schedule here", efficiency 255 "consolidate here"). Thread Director generalises it: a header of 8 bytes plus CP × CL change bytes rounded up to 8, row i at 8 + (i + 1) × (CP × CL rounded up to 8), class cl's capability cp at row + CP × cl + cp, CP the bits enumerated in CPUID.06H:EDX[7:0], CL in ECX[15:8], the row index in EDX[31:16]. → `HfiShape`, `hfi_flags`, `hfi_row`.
- **IA32_HWP_CAPABILITIES** (I3 §17.4.3, PDF p. 536): Highest_Performance bits 7:0. → `hwp_highest`.
- **CPPC_CAPABILITY_1** (A2 §17.6.4.1, PDF p. 769): HighestPerf bits 31:24. → `amd_cppc_highest`.
- **The PPTT** (ACPI66 §5.2.31, PDF pp. 337 to 343). Processor hierarchy nodes (flags: physical package, processor ID valid, thread, leaf, identical; a parent; the MADT's ACPI processor ID, which a leaf must give; private resources). A cache listed as a node's private resource is private to that node, so shared by the processors under it. One structure may be listed by several nodes "for compactness", as separate identical caches, unless the implementation needs each instance named; a valid Cache ID makes the structure a unique cache in the system. Levels follow the processor architecture's and may not be skipped; a cache in one level's chain must not point to a structure another level's node lists; instruction and data chains converge at the unified level. Table values flagged valid take precedence over the processor's own discovery **for the cache's properties**; cache structures "complement or replace" the processor's discovery of sharing (§5.2.31.2). A processor the MADT marks disabled, and its resources, are disabled. → `pptt_caches`, `PpttRule`, the dated note in ADR-0020 S8 (ruling Q1).
- **`_CPC`** (ACPI66 §8.4.6.1, Table 8.23, PDF pp. 640 to 644): element 0 NumEntries (25 in revision 4), element 1 Revision (4), element 2 Highest Performance, an Integer or a Buffer holding one Register(); a Notify 0x85 makes the OS re-read Highest only where it is a register, and `_CPC` is never evaluated again. Revisions 2 and 3's entry counts are in no document read today. → `cpc_highest`, `cpc_rereads_highest`.
- **The Generic Register Descriptor** (ACPI66 §6.4.3.7, Table 6.53, PDF pp. 525 and 526): 0x82, length 12, address space, bit width, bit offset, access size, an 8-byte address. → `generic_register`.

## 3. What QEMU 11.1.3 shows (measured)

- **The software processor's `max`** is `AuthenticAMD`, `topoext` off, basic leaves to 0Dh, extended to 8000_0021h. Asked for `topoext=on`, TCG refuses it ("TCG doesn't support requested feature: CPUID[eax=80000001h].ECX.topoext"); `threads=2` with the AMD vendor draws a warning that the family "doesn't support hyperthreading". With `vendor=GenuineIntel` neither appears. A recent kernel boot on the Mac printed `cache: 0 caches`: an AMD guest's leaf 04H is zero. So under QEMU's software processor only an Intel guest reports which processors share a cache.
- **`-machine smp-cache`** exists in 11.1.3 and was accepted with L1 data, L1 instruction and L2 per core and L3 per socket on every machine shape `boot-matrix` uses (1, 2, 4, 65 processors; the x2APIC, sparse, many and discovery machines; the two new ones), with `max` and with `max,vendor=GenuineIntel`.
- **Default NUMA placement** with two `-numa node` options and no `cpus=`: each processor in node (package index mod 2) (`query-cpus-fast`): all of a one-socket machine in node 0, one node a package on two-socket machines, cold-plugged processors included.
- **The MADT's ACPI processor IDs** on both new machines: CPU index = ACPI processor ID, 0 to 7 (a temporary line in `prove_domains`, booted once with `boot-domains` and put back, 01:24 UTC; ruling Q6). The planted PPTT names its leaves so.
- **AMD reserves leaf 04H:** APM Vol. 3 rev. 3.39 §E.3.3 (PDF p. 669), "Functions 2h–4h—Reserved" (checked for the review's R97-12): leaf 04H is read only on an Intel processor.

## 4. What QEMU cannot show here

Hybrid parts, HFI, `_CPC`, HWP and CPPC (ledger §5 of the M3 ledger): host tests only. A cache-cluster level distinct from the core and package levels: the machines S23 names have no modules, so with `smp-cache` the L2 is a core's; the host tests build one (a hybrid part's efficient-core modules). Fn8000_001D at boot: only under KVM on an AMD host with TopologyExtensions.
