# Research ledger: identity maps for firmware-reserved memory (VT-d RMRRs, AMD-Vi IVMDs)

| | |
|---|---|
| Date | 2026-11-12 |
| For | TASKS item 40: the identity maps ADR-0007 questions 4 and 8 require before real hardware (decision items 8 and 12, kept for rented servers by item 229); rows A07-I11 and A07-A9 of the requirements matrix; threat T14 |
| Requires | CLAUDE.md §16 (research first), §32 (Intel VT-d, AMD IOMMU), §58 (DMA protection), §74 (QEMU is not hardware) |
| Method | Primary sources at today's revision, read as text extracted in memory from the owner's copies, after checking that the vendors still serve the same files today (§1). QEMU's ACPI builder was read in its source, because QEMU is the device under test. **No operating system's source was read** (CLAUDE.md §3): the specifications answer every question below, so no reason under item 97 applies. The specifications' text is paraphrased and cited by section and table, never reproduced (their terms, §17). |

## 1. Sources

| ID | Source | Revision checked on 2026-11-12 |
|---|---|---|
| V1 | Intel, *Virtualization Technology for Directed I/O Architecture Specification*, Rev 5.20, order number D51397-019 | intel.com's page for content 919688 lists 5.2 as the latest (dated 2026-05-14; 5.1, 5.0, 4.1, 4.0 older). The PDF it serves (12,791,994 bytes) hashed in memory to SHA-256 `44179074934b5d28f49e0302805d69e74bd4e7ad323525ef127d6e99e0c0a2fa`, the same as the owner's copy that the VT-d ledger (`2026-11-08-vt-d-iommu.md`) was checked against. |
| A1 | AMD, *I/O Virtualization Technology (IOMMU) Specification*, publication 48882, Rev 3.11 | docs.amd.com's portal returns it with `isLatest` true and "last updated" 2026-04-08. The PDF it serves (7,705,637 bytes) hashed in memory to SHA-256 `f7c375a15db5ed63de760356867211063d164a2ed59f2d38613daec95894ce22`, the same as the owner's copy that the AMD-Vi ledger (`2026-11-08-amd-vi-iommu.md`) was checked against. |
| Q1 | QEMU 11.1.3, `hw/i386/acpi-build.c` (gitlab.com/qemu-project/qemu, tag `v11.1.3`); the host runs 11.1.3 | The order of the tables QEMU builds, and what its DMAR and IVRS hold. |
| R1 | `docs/research/2026-11-12-rental-a-findings.md` (on `docs/decisions-224-234`) | A real IVRS: revision 2, one IOMMU described as 10h and 11h, **no IVMD**. |

## 2. What VT-d requires (V1)

- **§3.16, reserved system memory.** Firmware may set aside memory that devices reach by DMA: graphics memory a UMA graphics controller uses, or buffers a USB controller uses under SMM for legacy keyboard emulation. When software turns DMA remapping on, it is expected to give the devices named an **identity mapping of those ranges, with read and write permission**, in second-stage translation. Software must also make sure the other addresses it gives those devices for OS memory **do not overlap** the reserved ranges. Platforms are told to keep such use small, because each range is a hole in the devices' DMA address space.
- **§8.4, the RMRR structure.** Type 1. The segment is at offset 6, the base at 8, the limit (the last byte, inclusive) at 16, and the device scopes from 24. The base is 4 KiB aligned, the size a whole number of 4 KiB pages, and the limit must be greater than the base. Every device a scope names must be under some DRHD's scope. **The firmware must report every RMRR range as reserved, or as EFI runtime memory, in the memory map** it gives the OS. RMRRs are optional; none means there are no such ranges. Ranges used only before boot (a boot disk's DMA) must not be reported.
- **§8.10, the footnote to the `_DSM` function table.** RMRRs are not reported for hot-plugged hardware, because only legacy devices (USB, integrated graphics) use them.
- **§3.15, the interrupt range.** Software must not map any address to `FEEx_xxxxh`. A DWORD write there without a PASID is an interrupt request and is never translated; a translation that results in that range is blocked.
- **§11.4.8.1.** Protected memory ranges are to be deprecated; software is advised not to overlap them with RMRRs. The kernel uses neither.
- Second-stage leaves carry read and write permission separately (§9.8, already implemented in `vtd::map`).

What the specification leaves to software: when the identity maps are made (§3.16 says "when system software enables DMA remapping", which presumes a domain exists for each named device at that time), and how the OS tells a range it can trust from one it cannot.

## 3. What AMD-Vi requires (A1)

- **§5.2.2 and §5.2.2.2, the IVMD block.** Optional. An IVMD **pertains to the IVHD immediately before it**. It may name a device by the DeviceIDs the IVHD supplies, including alias, special and ACPI HID devices' DeviceIDs. System software is expected to use the IVMDs when it programs the IOMMU. When one definition applies to some devices but not all, firmware repeats it for each discontiguous device or range.
- **Tables 111 and 112.** 32 bytes. Type 20h applies to every peripheral (its DeviceID and segment are reserved); 21h names one DeviceID; 22h a DeviceID range, with the last DeviceID inclusive at offset 6. Segment at 8 (21h, 22h), start (a system physical address) at 16, length at 24. **Software may round the length up to 4 KiB**; the specification states no alignment for the start.
- **Table 113, the flags.** Bit 0 Unity: the device addresses must equal the physical addresses. Bit 1 IR and bit 2 IW: read and write permission. Bit 3 ExclusionRange: the range belongs to the IOMMU's exclusion range, and **IR, IW and Unity are then ignored**. Bits 7:4 reserved. **IR = IW = 0 means the range must not be mapped into the device's address space, and Unity must then be 0**; a 20h with IR = IW = 0 keeps the range out of every device's address space.
- **MMIO 0020h and 0028h, the exclusion range.** One base and limit per IOMMU. Accesses to it are **neither translated nor checked** for a device whose DTE has EX set, or for every device when the register's Allow bit is set. A translation request there returns read, write and the physical address. **When the system enables SEV-SNP, the registers are locked and repurposed as the completion-store range**, and ExEn and Allow must be clear before and after. DTE bit 103 (EX) chooses per device (§2.2.2.1); §2.1.4.4 covers remote IOTLBs.
- **Table 3, special addresses.** With HTRangeIgnore clear, `FD_0000_0000h`–`FF_FFFF_FFFFh` is decoded by DTE controls, interrupt tables and page tables by sub-range (interrupt and EOI, legacy PIC acknowledge, system management, I/O space, configuration). And the `FEEx_xxxxh` range carries MSIs (§2.2.5).
- Host page-table leaves carry IR and IW separately (Table 17; already implemented in `amdvi::map`).

What the specification leaves to software: when the maps are made, what to do with a non-unity range that allows access ("any translation may be used"), and whether to honour an exclusion range with the registers or otherwise.

## 4. What QEMU 11.1.3 gives (Q1) and the test fixture

- `build_dmar_q35` writes one DRHD and, with `device-iotlb=on`, one ATSR. **No RMRR.** `build_amd_iommu` writes IVHDs only. **No IVMD.** QEMU's `amd-iommu` does not implement the exclusion registers (AMD-Vi ledger §2).
- `acpi_build` appends the tables given with `-acpitable` **after** every table it builds, the DMAR and IVRS included. `firmware::acpi_table` takes the first valid table of a signature. So a DMAR or IVRS given with `-acpitable` is never the one the kernel reads, and a plain QEMU command line cannot carry reserved regions to the kernel.
- So the proof needs a firmware-stage step: a test-only UEFI application that runs before Origin's loader. It replaces QEMU's DMAR or IVRS with a copy that carries the regions (in pages the memory map then reports as reserved), and starts the unchanged loader. The OS then meets the table exactly where firmware puts it: the XSDT, in ACPI memory, with its ranges reserved in the memory map. The proposal to the coordinating session names the alternative (the same step as a test-only loader feature).
- The 512 MiB QEMU machine has no RAM in [1 GiB, 2 GiB), so no region there can be both reserved and real memory: the IOVA window's skipping of identity-mapped pages is host-tested only.

## 5. Questions put to the coordinating session (2026-11-12)

The ADR leaves these open. Each was sent with a recommendation (notes folder, `2026-11-12-reserved-memory-maps-proposals.md`). The coordinating session numbered them decision items 247 to 251 (R n is item 246 + n) and put them to the owner on 2026-11-12; not answered when this was written. **The owner accepted all five as recommended on 2026-11-13** (ADR-0007 Amendment 1; matrix rows A07-R1 to A07-R5).

| | Question | Recommended |
|---|---|---|
| R1 | When the maps are made | When a driver attaches the device: its own domain holds the identity maps before its entry turns present. Not at bring-up for unattached devices: I12 clears their bus mastering, so it would gain nothing now, and it would change I2 |
| R2 | The IOVA window | Each domain's [1 GiB, 2 GiB) skips the pages identity-mapped in that domain; I4 gains "except within a firmware-reserved range" |
| R3 | What a range must be | Every page Reserved, RuntimeCode or RuntimeData in the memory map (VT-d §8.4); outside the interrupt range (and, under AMD-Vi, Table 3's range); clear of the platform's register ranges; within the domain's address width. Otherwise not mapped, its devices refused at attach as today, and reported; the boot goes on |
| R4 | IVMD flags | Unity with IR/IW: identity with exactly those permissions. IR/IW without Unity: identity as well. IR = IW = 0: `map` refuses a frame inside it for the devices it covers. ExclusionRange: identity, read and write, in each named device's domain; the exclusion registers are never programmed |
| R5 | IVMD placement (TASKS 11) | 20h: every device of the IOMMU before it. 21h and 22h: by their own segment and DeviceID (or the alias the device's requests carry), on whichever IOMMU translates it |

Not put as a question: a range longer than 4 GiB is refused, as no legacy usage the specification names needs one and each 4 KiB page costs a page-table entry per device (4 GiB is 1,048,576 entries, 2,048 table pages); the round-down of an IVMD's start and the round-up of its length to 4 KiB.

**After the independent review of 2026-11-13** (notes folder, `t40-review.md`; answered in `docs/engineering/REVIEW_T40_2026-11-13.md`), with the coordinating session's rulings:

- **A writable range over a table the kernel holds is refused** (M1). The kernel lends ACPI tables as `&'static` references (`vm::firmware_bytes`). A device allowed to write one could change it under the reference. VT-d §8.4 does not forbid firmware to keep its DMAR in reserved memory and name an RMRR over it, so R3's memory-map rule alone does not cover the case. This is threat T26's invariant, not a new decision.
- **A device's ranges together are bounded as one range is**, at 4 GiB (1,048,576 pages), so ranges side by side cannot add up past the bound one range is refused for (L4).
- **Overlapping ranges of one device get the union of their permissions** (I3, the coordinating session's ruling on the review's question 5): each range grants exactly its own access on its own pages, as R4 reads, so the union grants nothing no range granted.
- **Unity with IR = IW = 0 is a never-map range** (I2, the review's question 4): R4's "IR and IW both clear" decides it, whatever Unity says. Refusing the whole IVRS for it, as the first version did, is stricter than what was accepted.
- **A never-map type-20h IVMD and other IOMMUs** (L6): 48882 §5.2.2.2 says a 20h's flags apply "to all devices in a system". That changes R5, so it is with the owner as decision item 291. Until the answer, R5 stands as accepted.

## 6. Tests defined before completion (§16.12)

- **Host:** RMRR and IVMD ranges to identity plans (every flag combination, rounding, the refusals of R3, each at its boundary); a domain's window skipping identity pages and never handing one out; `unmap` refusing an identity page; `map` refusing a frame in a never-map range; R5's placement over a two-IOMMU IVRS; the fixture's table rewrite (lengths, checksums, the appended structures parsed back by `dmar::parse` and `ivrs::parse`).
- **Boot, under each IOMMU, with the fixture's tables:** the NVMe controller, named by a reserved range, writes a disk block into the range at its physical address and reads it back out; the NIC's domain holds no such page; a planted range that fails R3 is refused by name. Under AMD-Vi also: a read-only range takes the controller's read and blocks its write, with the fault recorded; a 20h range and an exclusion range are identity-mapped in every attached domain.
- **Negative controls,** each a temporary edit booted once: the maps not made; the maps made one page off; R3's memory-map check skipped; (AMD) a read-only range mapped read and write.
- **Added after the review (2026-11-13):** host tests of the refusal over a lent table, of lending and granting in both orders, of an RMRR whose end overflows, of a device's bound, of `unmap` and `map`'s refusals, of the walk that finds a physical page at any IOVA, of a BAR inside an identity range, and of the test fill. At boot, a writable range over the table copy itself (in reserved memory) is refused by name, and a writable reserved page without the test fill is never written. Two more controls: the proof's write leg forced onto that page, and `firmware_bytes` recording nothing it lends.

## 7. What this cannot show

- Anything about a physical machine (§74). Rental A's board has no IVMD, so even rental B would not exercise the unity-map code there.
- An integrated GPU or USB controller using its range: QEMU emulates neither with an RMRR, and I12 clears bus mastering before any of them could use it.
- A range inside the IOVA window, on the 512 MiB machine.
- SEV-SNP's effect on the exclusion registers, which are never used.
