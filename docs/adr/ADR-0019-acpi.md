# ADR-0019: ACPI 6.6, everything it asks of the operating system on x86-64

| | |
|---|---|
| Status | **Accepted** by the owner on 2026-11-14, each item with the recommended answer ("Accept all seven", decision sheet items 301 to 307; CLAUDE.md §18). Nothing in it is built beyond the static tables the kernel already parses at boot (`framekernel/frame/src/acpi.rs`, `dmar.rs`, `ivrs.rs`). Tasks A and B of A6 are TASKS items 62 and 63 |
| Decides | What "implement ACPI 6.6 in full and completely", the owner's request of 2026-11-14, means for Origin; where the firmware's AML runs; how the interpreter is built and tested; the sources it may be written from; the order of work; and what may be claimed without a machine in the declared scope |
| Requires | CLAUDE.md P0, P1, P5, P8; §1 (roles 7, 8, 15, 54 and 64; "Discover and report capabilities truthfully", "Deny by default", "Qualify the declared environment"); §3, §4, §5, §13, §16, §17, §19, §24, §31, §49, §58, §67, §69, §72 to §74; rules 14, 26 and 30 |
| Performance cost | A1, A4 to A7: none, by construction. A2 and A3: unmeasured, owed, in cost ledger row CL41 |
| Code | None changed. Tasks A and B add to the tree (A6) |
| Evidence | ACPI 6.6, the owner's copy, whose outline was read on 2026-11-14 (1,202 pages, 21 chapters and three appendices); the kernel's ACPI code as it stands in the fourth batch (`land/s4-part2` at `e6fd12c`). Documents and code read; nothing measured |
| Builds on | ADR-0001 D2 (third-party drivers in user mode); ADR-0007 (DMAR, IVRS); ADR-0008 and its Amendment 1 (the MADT); ADR-0011 N1, N2, N6, N8 and Amendment 4; ADR-0013 (boot proofs); ADR-0014 S7 (Hardware health); ADR-0015 (devices through the SDK); ADR-0018 V3 |

## Context

On 2026-11-14 the owner asked: "Can you implement ACPI 6.6 in full and completely." CLAUDE.md's standards baseline names ACPI 6.6 (§17), §19 asks for ACPI discovery, and §1 gives role 8 "ACPI runtime evaluation". §67 asks for power management that rests on ACPI on every x86 laptop.

**What exists.** The kernel reads the RSDP and the XSDT, the FADT's century register and boot-architecture flags, the MADT, the SRAT and the SLIT, each checked as untrusted input and with no `unsafe`; and the DMAR and the IVRS, which Intel's and AMD's own specifications define. Nothing reads the DSDT or an SSDT. There is no AML interpreter, no namespace, no SCI, no ACPI shutdown and no ACPI reset, and nothing for power sources, thermal zones, the embedded controller, processor power states or the platform error interfaces.

**What ACPI 6.6 holds.** The hardware model (chapter 4) and the software programming model (chapter 5), with about 35 system description tables in §5.2, the namespace and the event model; device configuration (6); power and performance management (7); processor configuration and control (8); the devices ACPI defines (9); power sources and power meters (10); thermal management (11); the embedded controller (12); SMBus (13); the Platform Communications Channel (14); the system address map (15); waking and sleeping (16); NUMA (17); the platform error interfaces, APEI (18); the source language, ASL (19); the machine language, AML (20); the data tables' definition language (21); and appendices for device classes and video.

## Decisions

| # | Item | Decision |
|---|---|---|
| A1 | 301 | **"ACPI 6.6 in full" is everything the specification asks of the operating system on x86-64.** Every table and every object that a platform in Origin's declared scope can present is read, checked and acted on as the specification says, and whatever is not built answers "not supported" (CLAUDE.md §1). Out of scope by CLAUDE.md's header, which allows x86-64 alone: the content for Arm, RISC-V and LoongArch processors, among it the GTDT, the RHCT and its node structures, and the MADT's structures for those processors' interrupt controllers. Also out: compilers for ASL (chapter 19) and for the data tables' definition language (chapter 21), which are firmware authors' tools; an operating system reads AML and tables and writes neither. A disassembler for diagnostics may come later as a developer tool. Tables that other specifications define, such as the DMAR, the IVRS, the MCFG, the HPET table and the TPM2 table, are not ACPI 6.6's and get tasks of their own. |
| A2 | 302 | **The firmware's AML runs in a user-mode ACPI service, not in the kernel.** The kernel grants the service only the I/O ports, memory-mapped registers and PCI configuration space that the firmware's operation regions name, and never memory that the kernel or a program owns: a region over such memory is refused, and the refusal is reported. Other services reach ACPI only through SDK interfaces (Power, Devices, Hardware health; ADR-0011 N1, N2). The kernel keeps parsing the static tables it needs at boot, and resets the machine itself through the FADT's reset register. |
| A3 | 303 | **The AML interpreter is built first as a library.** It is `no_std` with `alloc` and `#![forbid(unsafe_code)]`, tested on the host and fuzzed. It reaches hardware, time, mutual exclusion and notifications only through an interface its host provides, and holds hard limits on each method's loop iterations, time, call depth and memory, so that no definition block can hang or exhaust its host. Until the service of A2 can exist, which needs the Driver SDK (ADR-0001 D2), a kernel boot proof runs QEMU's own DSDT and SSDTs through the library with every operation-region access refused, and checks named results. |
| A4 | 304 | **Sources, in this order.** ACPI 6.6 itself. Then Microsoft's published ACPI and `_OSI` documentation, for what deployed firmware expects of an operating system. Then ACPICA, Intel's reference implementation, only under CLAUDE.md §3: for a named gap, in a session that writes no Origin code, which writes the facts it found into Origin's research ledger in Origin's own words and structure, under ACPICA's BSD licence option, recorded with the file, the release and the date it was read. Nothing is copied or translated. The specification's own text stays out of the repository and is paraphrased (§17). |
| A5 | 305 | **Test definition blocks come from an original AML encoder,** written in Rust for the interpreter's tests, together with QEMU's own tables read at boot and fuzzing. No third-party ASL compiler is installed. |
| A6 | 306 | **The order of work.** A: every x86 table of §5.2 parsed and checked, starting now. B: the interpreter library of A3, starting now beside A. C: the event model (the SCI, the PM1 and GPE register blocks, the fixed events and the power button), ACPI shutdown and the reset, once the Driver SDK exists. D: the namespace's devices: identification, resources (`_CRS`), PCI interrupt routing (`_PRT`) and `_OSC`, with M5. E: processor power and performance (`_CST`, `_LPI`, `_CPC`), thermal zones, power sources, the embedded controller, SMBus, the lid and buttons, the ambient light sensor, the video extensions, and S3 and S4, with M10. F: the HMAT and the PPTT, into the scheduler's topology. G: APEI (the HEST and its error sources, the BERT, the ERST, the EINJ), for the Hardware health domain and the Server edition (ADR-0014 S7). H: the Platform Communications Channel and the other firmware channels. Each area that decides an interface gets an ADR, or an amendment to this one, before it is built. |
| A7 | 307 | **What may be claimed.** QEMU can show what it emulates: its tables, its AML, the power button, ACPI shutdown, the reset, S3, and PCI and processor hot-plug, each to be checked when its task starts. It emulates no embedded controller, battery, thermal zone, lid, ambient light sensor, backlight, processor power states or x86 error sources. Those are built from the specification, tested on the host with test definition blocks, and recorded as not verified on hardware. No ACPI row is done until a machine in the declared scope runs it (CLAUDE.md §74, rule 30), and none exists while buying is off. |

## Alternatives

| Alternative | For | Against |
|---|---|---|
| The interpreter in the kernel, as on Linux and Windows | Direct access to every region; no IPC on an event | A large interpreter of bytecode the firmware vendor wrote, in ring 0. Against ADR-0011 N6's small privileged framework and ADR-0001 D2's place for code Origin did not write |
| Porting ACPICA | Mature, and the reference that firmware is tested against | Third-party code in the shipping runtime (§3, §5) |
| The static tables alone, no AML | Small, and done today | No ACPI shutdown, power button, power source, thermal zone or device configuration on a real machine. §67 cannot be met |
| ACPICA's `iasl` compiler for the tests | A real compiler, and later a second opinion for differential tests | A third-party tool to download and keep, for inputs an original encoder can write |
| The service first, the library after | One step | The service waits for the Driver SDK, and the interpreter could not be tested until then |

## Evidence

The specification's outline (ACPI 6.6, the owner's copy, read with the PDF's own bookmarks on 2026-11-14). The kernel's ACPI code in `land/s4-part2` at `e6fd12c`: `acpi.rs`, `firmware.rs`, `dmar.rs`, `ivrs.rs` and `cpumap.rs`. What QEMU emulates is from the coordinating session's knowledge of QEMU and was not checked against QEMU 11.1.3 for this ADR; A7 asks each task to check it. Nothing was measured.

## Trade-offs

- **A2** puts an IPC round trip in front of every ACPI request and every SCI. Both are rare beside the work they serve, except an embedded controller's queries on a laptop, whose cost E measures.
- **A3** delays ACPI shutdown and the power button until the Driver SDK exists, in exchange for an interpreter tested from its first day on firmware bytecode.
- **A5** is more work than installing a compiler, and gives tests that need nothing outside the repository.
- **A7** leaves most of ACPI "built" and none of it "done" until a machine in scope exists.

## Migration consequences

None: no ABI, file format or accepted text changes. The static-table parsers that exist stay where they are, and task A decides whether new ones join them or a crate of their own.

## Threat model (§58)

| # | Threat | Defence |
|---|---|---|
| T1 | A definition block with a bug, or one written to attack, makes the interpreter loop, recurse or allocate without end | A3's limits on each method; the interpreter runs in a service (A2), which the kernel can stop |
| T2 | An operation region names memory the kernel or a program owns, to read or write it | The kernel refuses it (A2) |
| T3 | A malformed table: a wrong length, a bad checksum, an offset past its end | Checked parsing that never panics, as the existing parsers do (A6's task A) |
| T4 | A compromised ACPI service | It holds only the regions the firmware names, never the kernel's memory, and DMA stays behind the IOMMU (ADR-0007). It can still misprogram those devices, power the machine off among them, which is accepted: the firmware gives it that power on any operating system |
| T5 | Firmware that behaves differently for each operating-system name it is told (`_OSI`) | The answer Origin gives is a policy, decided with task B from A4's sources and recorded |
| T6 | Code the platform runs below the operating system (System Management Mode) | Out of scope: nothing an operating system does can see or stop it |

## Performance cost (ADR-0018 V3)

**Performance cost.** A1: none, by construction. A2: unmeasured, owed: cost ledger row CL41, an IPC round trip for each ACPI request and the delivery of each SCI to a user-mode service. A3: unmeasured, owed: CL41 again, the time each method takes to interpret, which the boot proof reports once it runs. A4 to A7: none, by construction.

## What this does not decide

The service's SDK interfaces; the `_OSI` policy; the Driver SDK; the path into and out of S3 and S4; the interpreter's internal design; the names of the boot proofs. Each comes with its task (A6), and each that decides an interface comes to the owner first.

## Questions for the owner (all answered 2026-11-14)

1. "ACPI 6.6 in full" is everything it asks of the operating system on x86-64, with the content for other processors and the ASL and table-language compilers out of scope (A1)? **Yes (item 301).**
2. The firmware's AML runs in a user-mode service, with the kernel granting only the regions the firmware names (A2)? **Yes (item 302).**
3. The interpreter first as a library, with a read-only boot proof on QEMU's tables until the service exists (A3)? **Yes (item 303).**
4. The specification, then Microsoft's documentation, then ACPICA under §3's split (A4)? **Yes (item 304).**
5. An original AML encoder for test definition blocks, rather than a third-party compiler (A5)? **Yes (item 305).**
6. The order of work A to H (A6)? **Yes (item 306).**
7. What may be claimed without a machine in scope (A7)? **Yes (item 307).**
