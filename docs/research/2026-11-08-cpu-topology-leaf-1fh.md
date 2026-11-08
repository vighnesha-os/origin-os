# Research ledger: CPU topology beyond leaf 0Bh (dies, modules, core types)

| | |
|---|---|
| Date | 2026-11-08 |
| For | `framekernel::topology` (the pure decoder), `cpu::record_topology` and `cpu::topology_of` (per-CPU records indexed by `CpuIndex`, ADR-0008), `prove_topology` in `framekernel/src/main.rs`, `cargo xtask boot-x2apic` |
| Requires | CLAUDE.md §16 (research first), §24 (CPU topology, SMT awareness, hybrid-core capabilities), §27 (topology-aware scheduling), §72 (boot testing), §74 (hardware qualification) |
| Method | Primary sources first: Intel's SDM and topology paper, AMD's APM. QEMU's CPUID code was read in its source (QEMU is the test machine, not an operating system), then QEMU's behaviour was measured by running the kernel on each CPU model and printing the raw leaves. No operating-system code was read or copied (CLAUDE.md §3). A search returned links into an operating-system tree; they were not opened. |

## 1. Sources

| ID | Source | What it establishes |
|---|---|---|
| T1 | Intel, *Intel 64 and IA-32 Architectures SDM*, **Volume 1**, order 253665-**093**US, September 2026, Chapter 21 "Processor Identification and Feature Determination": CPUID.0BH (p. 21-39), CPUID.1AH (p. 21-66), CPUID.1FH (p. 21-71). Volume 2A's CPUID entry (253666-093US, p. 3-203) now only points there. | The bit-level definition of leaves 0Bh, 1Ah and 1Fh, used below. |
| T2 | Intel, *Intel 64 Architecture Processor Topology Enumeration*, 337015-002, Rev 2.0, April 2023 | Leaf 1Fh supersedes 0Bh; which leaf to choose; the masks and shifts; **unknown domains must be absorbed into the known domain directly below them** (§1.3.1–1.3.3, Tables 1-4 to 1-13). |
| A1 | AMD, *AMD64 Architecture Programmer's Manual*, Volume 3, publication 24594, **Rev 3.39**, October 2026: Appendix E, E.3.8 (Fn0000_000B), E.4.16 (Fn8000_001E), E.4.24 (Fn8000_0026), and Fn8000_0008_ECX. Read from the kib.kiev.ua mirror of AMD's manuals, in memory, SHA-256 `b598514b19f204d973040d9867a7ffc48c949009a8b3dc86b20c9188e721ee45`. **Identical to AMD's own copy**, checked on AMD's Technical Information Portal on 2026-11-08 (`24594_3.39_APM_Vol3.pdf`, 16,226,519 bytes, released 2026-10-28). | The AMD leaves below. |
| Q1 | QEMU 11.1.3 source (tag `v11.1.3`), `target/i386/cpu.c` (`cpu_x86_cpuid`, `encode_topo_cpuid1f`, `encode_topo_cpuid8000001e`, `x86_cpu_expand_features`), `target/i386/cpu.h` (`x86_has_cpuid_0x1f`), `include/hw/i386/topology.h` | What QEMU's model implements, read to the line (§3). |
| Q2 | QEMU 11.1.3, measured on the M1 Max (TCG): the kernel run with a temporary dump of the raw leaves on the boot processor, on each CPU model below | What the guest actually sees (§3). |
| Q3 | QEMU development list, patch "i386: Add support for CPUID 0x80000026 and Bus Lock Detect" (November 2025; not merged, absent from `v11.1.3`) | Agrees with AMD's reading of the mask widths (§2.3). |

**Gaps:**
- Core-type code tables for AMD are in each processor's PPR; none was read, because the kernel records codes without interpreting them (§4).
- Intel's capacity interfaces (CPUID.06H Hardware Feedback Interface) were not studied. §27's hybrid-core *capacity* is a later step: the leaves here only name core types.
- ACPI PPTT, the firmware's own topology description, is not used: QEMU's x86 machines do not build it (no `pptt` in `hw/i386/acpi-build.c` or `acpi-common.c`).

## 2. What the specifications say

### 2.1 Leaves 1Fh and 0Bh (T1, T2)

- Which leaf: Intel prefers 1Fh. Check that the maximum basic leaf is at least 1Fh and that EBX of sub-leaf 0 is non-zero, then fall back to 0Bh in the same way. Leaf 0Bh will only ever describe three domains (logical processor, core, package).
- Sub-leaves run from the logical-processor domain at sub-leaf 0 upward, and end at the first with EBX\[15:0\] = 0. Domain order is fixed by the architecture, but the type codes are not ordered and software must not rely on them: a new domain between core and module would get a code above 5.
- Each sub-leaf gives, in EAX\[4:0\], the shift to reach the next wider domain, and in ECX\[15:8\] the domain's type (1 logical processor, 2 core, 3 module, 4 tile, 5 die, 6 die group). The package is implied above the last sub-leaf. EDX is the processor's x2APIC ID and does not vary between sub-leaves.
- EBX\[15:0\] is a count for display and diagnostics only: software must not enumerate topology from it. Its only other role is ending the list.
- On a processor without SMT, the logical-processor shift may be 0 or 1. So on a hybrid part the shift below the core field may differ between processors.
- A domain type software does not know must be absorbed into the known domain directly below it. Ignoring it would create phantom packages (T2's Example B).

### 2.2 Leaf 1Ah and the hybrid flag (T1)

- CPUID.07H:EDX\[15\] marks a hybrid part. Leaf 1Ah is valid only when the maximum basic leaf is at least 1Ah and EAX is non-zero; it exists on every logical processor of a hybrid part, and may exist elsewhere.
- EAX\[31:24\] is a core type, EAX\[23:0\] a native model ID. The core type's numeric value is not a ranking and implies nothing else about the processor. **So the kernel records it as a code and derives nothing from it.** Capacity has to come from elsewhere.

### 2.3 Leaf 80000026h (A1)

- Sub-leaves run from ECX = 0 until ECX\[15:8\] (LevelType) is 0. Level types: 1 core, 2 complex, 3 die, 4 socket. Not every level need be listed.
- EAX\[4:0\] (MaskWidth) is the shift that reaches **this level's own unique ID**. That is different from 1Fh, where the shift reaches the next level, so the same numbers must be read differently. Thus core's width is the thread field; socket's is the bits below the package. Q3's encoder agrees: its core level reports the SMT field width.
- EAX\[30\] HeterogeneousCores and EAX\[31\] AsymmetricTopology, at each level. EAX\[29\] says a power-efficiency ranking is available; it and EBX\[31:16\] are valid only at level type 1. EBX\[31:28\] is a core type, EBX\[27:24\] a native model ID, EBX\[23:16\] the ranking, EBX\[15:0\] a logical-processor count.
- Individual logical processors may report different values. AMD says to use the leaf on each one.
- It does not depend on the TopologyExtensions feature bit, which gates 8000001Eh.

### 2.4 Leaf 8000001Eh, and where it matters (A1)

It gives the extended APIC ID, the compute-unit ID (with threads per compute unit) and the node ID (with nodes per processor), and its function number is reserved unless CPUID.80000001h:ECX\[TopologyExtensions\] is set. Fn8000_0008_ECX\[ApicIdSize\] gives the width of the package-local ID.

It matters only for AMD processors with neither 80000026h nor 0Bh, which are below §2's 2026+ target. **Decided: not decoded.** Such a processor falls back to the flat layout (each processor its own package). If older AMD parts are ever qualified, this leaf with ApicIdSize is the fallback to add.

## 3. What QEMU 11.1 exposes (Q1, Q2)

Measured on the sparse machine `-smp 1,maxcpus=150,sockets=2,dies=3,modules=5,cores=5,threads=1` (`boot-x2apic`) and plain `-smp 2`:

| `-cpu` | Machine | Maximum basic leaf / extended leaf | Vendor | Leaf 0Bh (shift, count, type) | Leaf 1Fh |
|---|---|---|---|---|---|
| `max` | sparse | 0Dh / 80000021h | AuthenticAMD | (0, 1, 1), (8, 0x4B, 2), end | **out of range: returns leaf 0Dh's data** (XSAVE) |
| `max,vendor=GenuineIntel` | sparse | **1Fh** / 80000021h | GenuineIntel | as above | **(0, 1, 1), (3, 5, 2), (6, 0x19, 3), (8, 0x4B, 5), end** |
| `max,min-level=0x1f` | sparse | 1Fh / 80000021h | AuthenticAMD | as above | same as above: QEMU's encoder does not check the vendor |
| `max,xlevel=0x80000026` | sparse | 0Dh / 80000026h | AuthenticAMD | as above | out of range |
| `max,vendor=GenuineIntel` | `-smp 2` | 0Dh / 80000021h | GenuineIntel | (0, 1, 1), (1, 2, 2), end | out of range (QEMU only raises the level for dies/modules) |
| `max` | `-smp 2` | 0Dh / 80000021h | AuthenticAMD | as above | out of range |
| `EPYC-Genoa,+x2apic` | `-smp 2` | 0Dh / 80000022h | AuthenticAMD | as above | out of range |

So, to the questions asked:
- **Does TCG fill leaf 1Fh on the `boot-x2apic` machine?** Its encoder does whenever dies or modules exist (`x86_has_cpuid_0x1f`). But QEMU raises the CPUID maximum to 1Fh only for an Intel CPU model (`x86_cpu_expand_features`), so under the default `-cpu max`, whose TCG vendor is AMD, the leaf cannot be reached. `-cpu max,vendor=GenuineIntel` reaches it, and that is what `boot-x2apic` uses.
- **Does it fill 80000026h for an AMD CPU model?** No, for no model: QEMU 11.1.3 has no such leaf. With `xlevel=0x80000026` it returns zeros (level type 0). Anywhere else it is above the maximum.
- **A read above the maximum does not return zeros.** `cpu_x86_cpuid` clamps to the maximum *basic* leaf, as Intel documents. On `max,vendor=GenuineIntel`, 80000026h returns 1Fh's list, with a valid-looking level type 1 at sub-leaf 0 that reads as *core* in AMD's encoding. **Every leaf is read only below its maximum.** A host test reproduces exactly these results.
- **Leaf 8000001Eh:** `max` and `EPYC-Genoa` both leave TopologyExtensions clear under TCG, so the leaf is architecturally reserved there; its encoder ignores that.
- **Hybrid core types are not modelled.** QEMU 11.1.3 has no name for CPUID.07H:EDX\[15\] (bit 15 cannot be set by any `-cpu` option), and leaf 1Ah falls to its "reserved values: zero" default: the leaf reads zero, invalid, wherever it is in range.

## 4. Design

**A pure decoder, driven by a CPUID reader.** `CpuTopology::detect` takes `cpuid(leaf, sub)` as a closure. The kernel passes `cpu::cpuid_subleaf`; host tests pass a modelled processor, including the out-of-range behaviour above. `forbid(unsafe_code)`.

**One layout type for three encodings.** `Layout` records, for each domain from thread to package, how many low bits lie below its ID field. Leaves 1Fh and 0Bh give upper bounds, so a field runs from the previous bound to its own. Leaf 80000026h gives lower bounds, so a field runs from its own start to the next listed start. The two are decoded apart and normalized. AMD's complex is reported as the module domain: a group of cores below a die.

**Which leaf:** 80000026h where the processor has it, then 1Fh, then 0Bh, then none. Each vendor recommends its own; here each is checked by capability (its maximum and its validity rule), not by vendor name. A malformed leaf (shifts that decrease, domains out of order, no terminator, a varying x2APIC ID, a level missing from 80000026h) is skipped for the next one and recorded, so the boot proof fails, while the kernel keeps working with what it has.

**Per CPU.** Each processor reads its own leaves on itself before it reports online, and stores the decoded record in a table indexed by `CpuIndex`, under a spin lock. A hybrid part's leaves differ between processors (T1, A1).

**Checks against the MADT.** `topology::agree` takes each online processor's MADT-listed APIC ID and its own record, and counts: no record; an x2APIC ID from CPUID that is not the MADT's; a layout that differs from the first processor's (above the thread field, with the thread field allowed to differ only on a hybrid part); an undecodable leaf; a hybrid processor with no core type; a duplicate placement. The boot processor's layout is then applied to every processor the MADT lists, enabled or not, and the package, die, module, core and thread counts are printed. `cargo xtask` computes the expected numbers from `-smp` alone and compares.

*Alternatives considered:*
- Decode only on the boot processor and apply its layout to all: simple, but wrong for a hybrid part, and the APs' own IDs go unchecked.
- A vendor switch (Intel uses 1Fh, AMD uses 80000026h): fragile against hypervisors and a capability check is all it needs (§24).
- Use EBX\[15:0\] to count processors: forbidden by Intel for enumeration.
- Check the MADT by re-deriving what the layout implies about it only: the APIC-ID split is injective, so distinct IDs always give distinct tuples and that check alone would prove nothing. Hence the counts, compared against the machine xtask built, and the cross-checks above.

## 5. What QEMU cannot show (CLAUDE.md §74)

Nothing here is qualified on hardware. Under QEMU 11.1.3 these stay unproven at boot and are host-tested only, from the specifications:
- Leaf 80000026h, including its normalized fields, core types and the efficiency ranking.
- Leaf 1Ah and any hybrid processor: no QEMU model sets the hybrid flag or leaf 1Ah. **The kernel reports `hybrid false` under QEMU; that says nothing about hybrid processors.** A hybrid machine must be qualified on real hardware, including whether the thread field really differs between core types.
- Mixed SMT widths, tiles and die groups: QEMU has neither (its encoder asserts on other levels).
- Hybrid-core *capacity* (§27) is not here at all.
- Leaf 8000001Eh's node IDs.

## 6. Tests defined before completion (CLAUDE.md §16.12)

- **Host (`framekernel::topology`, 19 tests):** QEMU's exact leaf 1Fh and 0Bh values and the full census; leaves above the maximum not believed (the XSAVE reply, and 80000026h reading 1Fh's list); every domain including tile and die group; unknown domains absorbed (Intel's Examples B, C and D); shift-0 core levels; every malformed-list case skipped and reported with a fallback; 80000026h's own-ID convention, hybrid flag, core type and ranking, and fallback without a core or socket level; leaf 1Ah's validity rules; `agree`'s every disagreement; mixed thread widths on a hybrid only; and a randomized test over 3000 layouts through all three encodings, against a brute-force census.
- **Boot, `cargo xtask boot-matrix` (Intel and AMD IOMMUs at 1, 2, 4 CPUs):** the default `-cpu max` reaches only leaf 0Bh; each CPU's line, the MADT census and the agreement must match what `-smp` implies.
- **Boot, `cargo xtask boot-x2apic`:** run twice on the sparse machine, with a third processor at APIC ID 412 (package 1, die 2, module 3, core 4), so a die, module and core mix-up shows. With `-cpu max,vendor=GenuineIntel`, leaf 1Fh must give `2 packages, 6 dies, 30 modules, 150 cores`. With `-cpu max`, leaf 0Bh must give `2 packages, 2 dies, 2 modules, 150 cores`, with APIC ID 412 reading core 156.
- **Negative controls (each must fail the boot, then be restored):** see `docs/engineering/SOURCE_PROVENANCE.md`, "CPU topology from CPUID leaves 1Fh, 0Bh and 80000026h".
