# Research ledger: any processor count

| | |
|---|---|
| Date | 2026-11-13 |
| For | TASKS item 58; `docs/adr/ADR-0008-cpu-index.md` Amendment 1 (D1 to D8, decision items 283 to 290, accepted 2026-11-13); `origin_frame::percpu`, `cpumap`, `smp`, `stack`, `vm`; `origin_sync::shootdown` and `slots`; `origin_paging::layout`; the kernel crate's `sched`, `storage` and `irq`; `cargo xtask percpu-gate`, `boot-many` and `boot-listed` |
| Requires | CLAUDE.md §16 (research first), §24 (APIC/x2APIC, SMP start-up, per-CPU state, CPU topology), §28, §32, §74 |
| Method | Vendor manuals and standards only, read on 2026-11-13 at today's revisions. The processor manuals were piped from `curl` into `pypdf` in memory and nothing was saved. ACPI was read first from the UEFI Forum's own repository of 6.5's specification sources. It was then read from the 6.6 PDF the owner supplied that day, with macOS PDFKit in memory; the PDF is in no branch, and nothing of its text is copied here (CLAUDE.md §17). No operating system's source was read or searched (CLAUDE.md §3). QEMU was measured on the development Mac as the test machine. |
| Follows | `docs/research/2026-11-08-x2apic-above-255.md` (ADR-0008 C1 to C7), whose two gaps (the SDM PDF and ACPI 6.6) this ledger meets in part |

## 1. Sources

| ID | Document | Revision and date | How read | SHA-256 |
|---|---|---|---|---|
| I1 | Intel, *Intel 64 and IA-32 Architectures Software Developer's Manual*, Volume 1 (253665) | -093, served 2026-11-13 (Last-Modified 2026-09-24) | `cdrdv2.intel.com/v1/dl/getContent/671436`, in memory | `f6ffc430dfa2dada4f791b1ecb462810e59827befe9a9c23f9160941eea642f9` |
| I3 | Intel SDM, Volumes 3A to 3D (325384) | -093, as I1 | `…/671447`, in memory | `77919be320f35da2782771ef2cb65a868cf68e62c8f8279f2ca61199cb70d1f8` |
| A2 | AMD, *AMD64 Architecture Programmer's Manual*, Volume 2: System Programming (24593) | 3.46, October 2026 (AMD's portal: latest, updated 2026-10-28) | AMD's documentation portal, in memory | `e4130d5aa26227b6a116d75af9e0f6e5adb4c034ff0d49a3c0fe73c3f49794cf` |
| A3 | AMD64 APM Volume 3 (24594) | 3.39, October 2026 | as A2 | `b598514b19f204d973040d9867a7ffc48c949009a8b3dc86b20c9188e721ee45` |
| ACPI65 | UEFI Forum, ACPI Specification 6.5, the release's reStructuredText sources in `UEFI/ACPI-Specification-Release` (the Forum's read-only repository; its README says the official documents are those at uefi.org) | 6.5, commit `5dabe6f37fbe753d90292a2a419105be5d55a778`, the repository's newest | raw files over HTTPS, searched in memory; 6.3 and 6.4 of the same repository for corroboration | chapter 5 `c150004f19cb4c4b0c67de7378f9c9e847c14a95701c52ba207f494cd9f0258d` |
| ACPI66 | UEFI Forum, *Advanced Configuration and Power Interface (ACPI) Specification*, Release 6.6, 1,202 pages | 6.6, May 13, 2025 (its title page) | the PDF the owner supplied on 2026-11-13 (uefi.org asks for a human check no session passes), read with macOS PDFKit in memory; sections 5.2.12 to 5.2.12.13 and 5.2.12.19 (PDF pages 204 to 214 and 221 to 222, printed 133 to 143 and 150 to 151), 5.2.16 (PDF 236), 5.6.6 (PDF 407), 6.2.11 (PDF 470), 8.4 (PDF 594), 17.3.1 and 17.4.1 (PDF 913 to 914), and the revision history (PDF 45 to 53) | `8c7542dd4de974ae47bba71bb0336637fe1e3838daad7692370ab4cf218efd35` |
| Q1 | QEMU 11.1.3 (Homebrew), on the development Mac (Apple M1 Max, 10 cores, 64 GiB) | measured 2026-11-13 | `qemu-system-x86_64` run directly, paused, nothing booted | — |

## 2. What the processors' manuals say

**The x2APIC ID and its addresses.**
- The x2APIC ID is 32 bits wide, and CPUID leaf 0Bh or 1Fh returns all of it in EDX in either APIC mode (I3 §13.12.5.1, §13.12.8.1; I1 chapter 21, leaves 0Bh and 1Fh).
- In x2APIC mode the ICR is one 64-bit MSR (830h). Its destination is bits 63:32. Destination FFFF_FFFFh is a broadcast in both physical and logical mode (I3 §13.12.9).
- AMD's manual says the same: the ID width, the ICR's destination, FFFF_FFFFh and the logical layout (A2 §16.12 to §16.14).
- Logical mode packs a 16-bit cluster and a 16-bit bitmap, derived from the ID, so it can address up to 2^20 − 16 processors (I3 §13.12.10; A2 §16.14). The kernel uses physical mode only.

**Device interrupts.** A compatibility-format MSI address carries an 8-bit destination, in bits 19:12 (I3 §13.11.1). Intel says 32-bit IDs cannot be expressed in that format, and that VT-d interrupt remapping is how device interrupts reach processors in x2APIC mode (I3 §13.12.6, §13.12.7). AMD's manual has no text on it.

**Firmware hand-off.** Firmware may hand over in xAPIC mode only if every processor it enables has an ID below 255; any above must be left disabled (I3 §13.12.8.1). The kernel switches to x2APIC itself and refuses to start an AP without it (ADR-0003 K6).

**Starting processors.**
- Intel's sequence is a broadcast INIT, a 10 ms wait, a start-up IPI, a 200 µs wait, a second start-up IPI, and a timer of at least 100 ms (I3 §11.4.4.1, Table 11-1). There are two relaxations: after power-up or RESET a single start-up IPI is enough; and software that knows how many processors to expect may stop waiting once they have all arrived.
- A start-up IPI is not retried by the hardware: software must notice a lost one and send it again (I3 §13.6.1). The kernel already sends a second (ADR-0003 K6).
- No text in the 1,602 pages of I3 allows shorter waits, or none, on newer processors.
- AMD gives no waits at all and refers to implementation-specific documents (A2 §16.5, §14.1.4).

**No maximum.**
- Neither vendor states a maximum number of processors.
- CPUID.1:EBX[23:16] is 8 bits wide and can saturate; Intel says not to use it where leaf 0Bh or 1Fh exists (I1 chapter 21).
- The counts in leaf 0Bh and 1Fh EBX[15:0] are for display only and must not be used to enumerate (I1).
- AMD deprecates the legacy count in favour of Fn8000_0008_ECX (A3 §E.5).
- What bounds the count is the ID space: 32 bits, less the broadcast ID.

## 3. What ACPI says (from 6.5, then 6.6)

The facts below were first read from 6.5's sources. Each was then read in 6.6 (§3a).

- **MADT revision:** 6 in ACPI 6.5, 5 in 6.3 and 6.4 (ACPI65 §5.2.12).
- **Local APIC Flags, shared by type 0 and type 9** (ACPI65 §5.2.12.2, §5.2.12.12):
  - Enabled (bit 0) set: the processor is usable now.
  - Enabled clear, Online Capable (bit 1) set: the hardware can enable the processor while the OS runs.
  - Both clear: OSPM must ignore the entry.
  - Enabled set: bit 1 is reserved and must be zero.
  - The revision history dates bit 1 to ACPI 6.3, ECR 1948, when the MADT was at revision 5. That below revision 5 the bit is reserved, and not read, is this ledger's inference: the pre-6.3 text was not read.
- **Which structure:** the x2APIC model reports every processor with type 9. A compatibility note keeps type 0 for IDs below 255 on legacy OSes and requires type 9 for 255 and above (ACPI65 §5.2.12.12). Type 0's ID is one byte.
- **Order:** OSPM should start processors in MADT order; firmware should list the boot processor first, and the first thread of every core before any second (ACPI65 §5.2.12.1).
- **Duplicates:** nothing found about one processor listed as both type 0 and type 9. The kernel indexes such a processor once (ADR-0008 C1).
- **Hot-add:**
  - A processor that is not present is either left out of the MADT or listed with Enabled clear (ACPI65 §5.2.12.2, §5.2.12.12).
  - Hot-added processors are announced through the namespace: Notify (Bus Check, Device Check), `_STA`, and `_MAT` on an ACPI0007 device, which may return type 0, 9 and 0Ah entries (§5.2.12, §6.2.10, §6.3).
  - The SRAT section covers processors added after boot (§5.2.16).
  - **The boot MADT therefore does not bound every processor that can ever exist.** One may first appear through the namespace, with no MADT entry at boot. This is a reading of several clauses, not one statement.
- **No maximum:** the only text is that an OS may limit how many processors it supports (§5.2.12.1).

## 3a. ACPI 6.6 against 6.5

| Fact the decisions rest on | ACPI 6.5 | ACPI 6.6 | Same or changed |
|---|---|---|---|
| The MADT's revision | 6 | 7 (§5.2.12, Table 5.19, PDF 205) | **Changed**, and it changes no row. D2 reads Online Capable from revision 5 on, and 7 is past it. The 6.6 revision history lists no MADT change for x86: its MADT entries are RISC-V structures (M2348, M2381), Arm GIC flags and the GICC parking protocol (M2422, M2425), and a code-first change to the Multiprocessor Wakeup structure (M2366) (PDF 45 to 46) |
| Enabled and Online Capable, shared by type 0 and type 9 | as §3 | the same: Enabled means usable now; Enabled clear with Online Capable set means it can be enabled while the OS runs; both clear means ignore it; with Enabled set, Online Capable is reserved and zero; bits 31:2 reserved (§5.2.12.2, Table 5.23, PDF 207; type 9 takes the same flags, Table 5.34, PDF 214) | Same |
| Online Capable dates from ACPI 6.3, ECR 1948 | in the revision history | the same entry (PDF 53) | Same |
| Type 9 for IDs of 255 and above; type 0's ID is one byte | as §3 | the same compatibility note, and Table 5.22's one-byte APIC ID (§5.2.12.12, PDF 213; §5.2.12.2, PDF 207) | Same |
| Start processors in MADT order, the boot processor listed first, each core's first thread first | as §3 | the same guidelines (§5.2.12.1, PDF 206 to 207) | Same |
| A processor not present is left out of the MADT or listed disabled; the MADT does not change during a boot | as §3 | the same, for type 0 and type 9 (PDF 207, 213) | Same |
| Hot-add through the namespace | `_MAT`, Notify, `_STA`, the SRAT's dynamically added processors | the same: `_MAT` is §6.2.11 now (PDF 470, numbered 6.2.10 in 6.5's sources), and processes types 0, 9 and 0Ah under a processor device (Table 5.21, PDF 205 to 206); Device Check and Bus Check (§5.6.6, PDF 407); processor devices as ACPI0007 (§8.4, PDF 594); a dynamically added processor missing from the SRAT needs `_PXM` (§5.2.16, PDF 236); online hot plug (§17.3.1, §17.4.1, PDF 913 to 914) | Same |
| No maximum processor count | only "OSPM may limit" (§5.2.12.1) | the same sentence and no maximum (PDF 206; searched) | Same |
| The Multiprocessor Wakeup structure (type 10h) | present | present, with a mailbox that may be used again (§5.2.12.19, PDF 221 to 222) | Changed in detail (M2366); not used: Origin starts processors with INIT and start-up IPIs (D8) |

**Nothing in 6.6 changes a row of ADR-0008 Amendment 1, and nothing built rests on a 6.5 fact that 6.6 changed.** The condition "ACPI 6.6 not read" is met.

## 4. What QEMU does on this Mac (Q1)

- `-machine q35` takes at most 4,096 processors: "The max CPUs supported by machine 'pc-q35-11.1' is 4096".
- Creating a machine paused under TCG, with nothing booted: 65 processors at once; 288 in 2 s; 1,024 in 16 s. With 4,096 present, creation had not finished after 5 minutes and the process held over 30 GB, so it was stopped. 4,096 present processors cannot be booted on this Mac. The largest machine `boot-matrix` boots is the sparse 288 (§5).
- As recorded on 2026-11-08 (the earlier ledger, Q2), QEMU lists every possible processor in the MADT, with flags 1 if present and 0 if not, and never sets Online Capable. So no QEMU machine exercises D2's reserved indexes: the host tests do.
- QEMU's NVMe controller allocates 64 I/O queue pairs (`max_ioqpairs`) and has 65 MSI-X vectors (`msix_qsize`) by default. With the driver's two pairs and two vectors, 62 processors get a queue of their own and the rest none (D6).

## 5. Design (ADR-0008 Amendment 1, as built by TASKS item 58)

- **D1:**
  - Each table is a `PerCpu<T>`: the boot processor's slot in the kernel's image, and one heap slice for indexes 1 and up, published once through `SetOnce` and never freed. It needs no `unsafe`.
  - 27 tables, 14 in the framework and 13 in the kernel crate, are sized in each crate's `size_per_cpu` by `smp::start`, between reading the MADT and the first INIT. `cpu::count()` is then fixed once.
  - `Shootdown` takes its storage through `Records`: a `Record` on its own cache line for each processor, and the initiator's marks.
- **D2:** `CpuMap` gives the boot processor 0, then the enabled processors in table order, then the online-capable ones. Online Capable is read from MADT revision 5 on (`acpi::MADT_ONLINE_CAPABLE_REVISION`), and only with Enabled clear.
- **D3:**
  - The kernel stacks' region keeps its 256 MiB floor and grows to 32 slots of 128 KiB per processor, inside a 1 TiB reserve. Its base is drawn so the whole reserve fits: `STACK_PLACEMENTS` = 31 × 2^19, 2^23.95.
  - The page tables past the floor come from a second run added to the kernel's table pool at discovery (`TablePool::extend`, `vm::grow_stack_region`).
  - The bounds are the 32-bit ID, memory (each table sized as far as the heap allows; an AP whose stacks cannot be had is never sent INIT), and the region (room for 262,144). The "CPU indexes" line reports them.
- **D5:** `MAX_CPUS` is gone. `cargo xtask percpu-gate`, part of `check`, refuses it, refuses a static array sized by a processor count, and refuses a `PerCpu` static that its crate's `size_per_cpu` leaves out.
- **D6:** NVMe asks for 2 + n pairs and takes what the controller allocates. Processors get queues while pairs and vectors last.
- **D7:** `boot-matrix` gains 3 and 65 under each IOMMU, `boot-many` (288, sparse, TCG only) and `boot-listed` (2 of 4,096). They are held out of CI until measured and accepted.
- **D8:** APs still start one at a time. The boot prints the total.

## 6. Gaps

1. **ACPI 6.6: met on 2026-11-13.** The owner supplied the PDF, and §3a compares it with 6.5: the same in every fact the decisions rest on.
2. Whether an ACPI version newer than 6.6, or an erratum, exists today is not checked: uefi.org's specification page still asks for a human check.
3. The section numbers in §3 are 6.5's, computed from its sources' headings. Those in §3a are read from 6.6's rendered pages.
4. The pre-6.3 MADT text was not read. The rule that bit 1 is reserved below revision 5 is inferred from the revision history.
5. AMD's per-family documents (PPRs) and its IOMMU specification were not read for start-up waits or for remapping to IDs above 255.
6. **A hot-added processor with no MADT entry at boot** has no reserved index (§3, hot-add). The hot-plug task has to decide whether such a processor waits for a reboot or whether tables grow while processors run (D2's option B, not chosen). This is recorded for that task; it changes nothing built here.
