# Research ledger: AMD IOMMU (AMD-Vi) DMA and interrupt remapping

| | |
|---|---|
| Date | 2026-11-08 |
| For | `framekernel::ivrs` (the ACPI IVRS parser), `framekernel::amdvi` (AMD-Vi structure, command, event and register encodings), the kernel's `iommu::amd` driver, and `docs/adr/ADR-0007-iommu.md` (**Proposed**, AMD-Vi section) |
| Requires | CLAUDE.md §16 (research first), §31 (DMA mapping, interrupt management), §32 (AMD IOMMU, interrupt remapping), §58 (DMA protection), §72 (testing), §74 (QEMU is not hardware) |
| Method | Primary source first: the AMD specification, read in full for the chapters used (§1 below). QEMU's device model was read in its source and driven directly, because it is the device under test (not an OS implementation). No operating system's IOMMU driver was read or copied (CLAUDE.md §3). Local experiments on the M1 Max are in §3. The VT-d ledger, `2026-11-08-vt-d-iommu.md`, is the model for this one. |

## 1. Sources

| ID | Source | What it establishes |
|---|---|---|
| A1 | AMD, *AMD I/O Virtualization Technology (IOMMU) Specification*, publication **48882, Revision 3.11, April 2026** (cover: "48882-PUB—Rev 3.11—Apr 2026"; 313 pages). Rev 3.10 (February 2025) is the one before; the revision history lists 3.11's changes as SEV-TIO, CXL memory attributes, vIOMMU guest commands and editorial fixes, none in the structures used here. | The authority for everything below: §2.2.2 (device table entry), §2.2.3 (host page tables), §2.2.5 (interrupt remapping tables, 32- and 128-bit IRTEs, x2APIC), §2.3 (start-up), §2.4 (commands), §2.5 (event log and events), §3.1–§3.4 (PCI function, capability block, MMIO registers), §4.1 and §4.5 (caching; software rules), §5 (IVRS). |
| A2 | QEMU 11.1.0 source (tag `v11.1.0`, commit 84f07211cc5b), `hw/i386/amd_iommu.c` and `.h`, `hw/i386/acpi-build.c` (`build_amd_iommu`, `insert_ivhd`), `hw/i386/x86-iommu.c`, `hw/virtio/virtio-bus.c`, `hw/virtio/virtio.c`. These files are byte-identical at v11.1.0–v11.1.3 (md5 compared at each tag); the host runs 11.1.3. | What the emulated `amd-iommu` implements (§2). |
| A3 | QEMU upstream: commit 4adfb431c0c3 (2026-08-14, master only, not in any 11.1.x) "Avoid undefined behavior in amdvi_setevent_bits()"; open issues #3365 (IntCtl 01b pass-through broken), #3366 (translation switched only by INVALIDATE_DEVTAB_ENTRY), #3618 (large-PTE undefined behaviour). | Known defects of the device under test (§2.5). |
| A4 | virtio 1.1 §6, `VIRTIO_F_ACCESS_PLATFORM` (bit 33) | As for VT-d: without it a virtio device uses physical addresses and bypasses the IOMMU. |
| A5 | Linux documentation patches (lkml, December 2025 and July 2026) naming the specification's current location (`docs.amd.com/v/u/en-US/48882_3.11_IOMMU_PUB`) | Only to locate the document; no code was read. |

**How A1 was obtained.** `docs.amd.com` answers 401 to automated readers and the older `amd.com` PDF address is 404. The PDF was read from the kib.kiev.ua mirror of AMD's documents (`x86docs/AMD/IOMMU/48882-3.11.pdf`, 7,705,637 bytes, SHA-256 `f7c375a15db5ed63de760356867211063d164a2ed59f2d38613daec95894ce22`), with text extracted by macOS PDFKit. **Checked against AMD's own copy on 2026-11-08: identical.** AMD's Technical Information Portal lists it as `48882_3.11_IOMMU_PUB.pdf`, Revision 3.11, released 2026-04-08, and the file it serves has the same size and SHA-256. It was hashed in the browser's memory and not saved.

**Licence.** The specification's agreement allows reviewing it to design products that interface with AMD products and forbids duplicating it. The repository therefore cites it by section and table and implements from it; it reproduces none of its text or figures.

**Errata and inconsistencies found in Rev 3.11** (no separate errata document exists): the revision history calls Control register bit 57 `CXLMemAttrEn` while the register table still marks it reserved; Table 42 gives event code 1001b to both INVALID_PPR_REQUEST and EVENT_COUNTER_ZERO. Neither touches what this kernel uses.

## 2. What QEMU 11.1's `amd-iommu` emulates

### 2.1 Options and placement (A2; §3 experiment 1)

- Options: `dma-remap` (default **off**), `dma-translation` (default on), `intremap` (auto: on under TCG), `xtsup` (default off), `pci-id`, `device-iotlb` (referenced nowhere in the AMD code: a no-op).
- The IOMMU is a PCI function, `AMDVI-PCI`, 1022:1419, class 08:06:00 (System Peripheral, IOMMU), with no BARs and its capability block at 40h (the Enable bit of its base register reads 0). Its registers are 16 KiB at **0xFED80000**.
- Unpinned, it takes the first free bus-0 slot when the `amd-iommu` option is processed. In this repository's machine that is 00:02.0, which would move the NVMe and the NIC. `-device AMDVI-PCI,id=…,addr=05.0 -device amd-iommu,pci-id=…` pins it.
- Nothing needs KVM or `kernel-irqchip=split` under TCG.

### 2.2 The IVRS for q35 (A2; captured byte for byte, §3 experiment 3)

- Revision **1**, OEM "BOCHS ", IVinfo **0x2801**: EFRSup, 40-bit physical addresses.
- **Two IVHDs for the one IOMMU**: type 10h (flags 0xD1: HtTunEn, IotlbSup, PrefSup, PPRSup; feature reporting 0x45) and type 11h (flags 0x11; attributes 0; EFR image 0x29D7 with `xtsup=on`, 0x29D3 without; EFR2 image 0). Both name DeviceID 0x0028 (the pinned 00:05.0), capability offset 40h, base 0xFED80000, segment 0.
- Device entries, the same in both: one **Select** (02h) per PCI function, host bridge and IOMMU included, then one **special device** (48h) for the I/O APIC: handle 0, DeviceID **0x00A0**, variety I/O APIC, DTE setting 0. No HPET entry, no IVMD, no IVHD 40h. ("All" is emitted only when no device is found.)
- Coherent (flag bit 5) is not set, so the kernel leaves Control.Coherent clear and flushes device table lines it writes.

### 2.3 Registers and features (A2)

- **EFR** 0x29D3: PreFSup, PPRSup, GTSup, IASup, GASup, HESup, HATS 10b (6 levels), GATS 10b; `xtsup=on` adds XTSup (0x29D7). EFR2 (01A0h) is not implemented and reads 0; so are the exclusion registers and the hardware-error and counter registers.
- **Capability header**: IotlbSup, HtTunnel, **NpCache** (entries marked not present may be cached), EFRSup.
- **Control bits acted on**: IOMMUEn, EventLogEn, EventIntEn, CmdBufEn, GAEn (128-bit IRTEs), XTEn (with xtsup and GAEn), IntCapXTEn. ComWaitIntEn, Coherent, HtTunEn, PassPW, Isoc and the PPR bits are ignored.
- **Status**: bits 0–2, 5, 6 are write-1-to-clear; CmdBufRun and EventLogRun mirror their enables. **EventOverflow and EventLogInt are set only with EventIntEn**: with it clear (this kernel), a full log drops events without telling software.
- Commands run **synchronously** inside the tail write.

### 2.4 Behaviour that decides the proofs (A2; §3)

- **`dma-remap` gates every device's DMA translation**, emulated devices included: with the default `off`, NVMe and virtio DMA is never translated or checked. With `on`, **a device is put under translation only by an INVALIDATE_DEVTAB_ENTRY for its DeviceID** (not by IOMMUEn, not by INVALIDATE_IOMMU_ALL; issue #3366).
- Outcomes by DTE state, once switched: V=0 passes through; **V=1, TV=0 blocks**, silently; Mode 7 blocks silently; **Mode 0 passes through without checking IR and IW** (the specification says IR and IW decide); Modes 1–6 walk the tables. Permissions are in effect checked at the leaf only.
- **IOTLB**: successful translations are cached by (IOVA page, DeviceID) and survive a cleared PTE — and an INVALIDATE_DEVTAB_ENTRY — until INVALIDATE_IOMMU_PAGES (which flushes the whole domain, ignoring address and S), INVALIDATE_IOMMU_ALL or a reset. **A missing page invalidation is observable**, so the unmap path is testable, as under VT-d.
- **No IRTE cache**: QEMU reads each IRTE from memory per interrupt, so a missing INVALIDATE_INTERRUPT_TABLE is not observable (the VT-d interrupt-entry-cache blind spot again).
- **Interrupt remapping**: the IRTE index is MSI data bits 10:0; IntTabLen is not enforced; IV=0 passes a message unchanged; remapping is active as soon as the device-table base is programmed, even with IOMMUEn clear. Fixed and arbitrated messages go through the IRTE (legacy 32-bit format, or 128-bit with GAEn, the destination's high byte used with XTEn); IntCtl 01b is broken (issue #3365); the I/O APIC is DeviceID 0x00A0, its redirection entry's bits 10:0 being the index.
- **Illegal commands** are logged but processing never stops (the specification halts it).

### 2.5 Event log deviations (A2, A3; §3)

Measured on the host's QEMU 11.1.3, every IO_PAGE_FAULT QEMU writes differs from §2.5.3:

- **Address 0.** The encoder stores the address through a 64-bit-wide mask built with a shift by 64, undefined in C and empty in this build. Fixed only on master (4adfb431c0c3).
- **I set on every page fault**, DMA ones included, with the access type in GN (read) and NX (write); PR, RW and PE never set.
- **DomainID 0**, and the DeviceID holds the devfn only (no bus number; bus 0 hides this).
- One fault is logged per bounced 4-byte access: a blocked 4 KiB device write produced 1,025 page faults and filled the 256-entry log.
- **Interrupt-remapping failures are never logged** (only `failed to remap interrupt from devid 0x…`, once, on QEMU's stderr).

### 2.6 virtio (A2, A4)

`iommu_platform=on` is needed for translation (plus `disable-legacy=on` on the root bus, as here), the driver must accept `VIRTIO_F_ACCESS_PLATFORM`, and `dma-remap=on` is still required.

## 3. Local experiments (M1 Max, QEMU 11.1.3, TCG)

1. **Placement** (QEMU monitor, `info pci`, no guest): with `-device amd-iommu` alone the IOMMU's function is 00:02.0, NVMe 00:03.0, virtio-net 00:04.0; with it pinned at 05.0 through `pci-id`, NVMe and virtio-net keep 00:02.0 and 00:03.0.
2. **`dma-remap`**: the first boot (`intremap=on,xtsup=on`) brought the IOMMU up (IVRS parsed, device table set, INVALIDATE_IOMMU_ALL and completion waits done) but the NVMe controller's first admin command timed out. A trace (`-trace 'amdvi_*'`) shows not one translation for 00:02.0: its DMA bypassed the IOMMU and went to the unbacked IOVAs. With `dma-remap=on` every proof passes; the trace shows the translations (e.g. `00:02.0 gpa 0x40005000 hpa 0x51000`), the 128-bit IRTE reads (`hi 0x40 lo 0x101`: vector 40h, destination 1) and the blocked interrupt (`RemapEn is disabled`).
3. **The IVRS bytes**: read by the kernel from the firmware and printed once (a temporary debug line, removed); they are the `QEMU_IVRS` test vector in `framekernel/src/ivrs.rs`. (The UEFI shell bundled with QEMU has no `acpiview`.)
4. **Event decoding**: with `dma-remap=on` the blocked-DMA proof at first found no fault: printing the events showed code 2, DeviceID 0x0010, flags I+NX, address 0 (§2.5). QEMU's source then confirmed both defects.
5. Baseline kernel boot under VT-d: 15.2 s (2 CPUs); under AMD-Vi: 15.0–15.5 s.

A second agent drove the device model directly (`-accel qtest`, `iommu-testdev` and `edu` as DMA masters) to confirm §2.3–§2.5 independently; its notes were checked against the boot results above.

## 4. Design decisions (detail in ADR-0007, AMD-Vi section, Proposed)

- **A1. One IOMMU per register base, from its richest IVHD** (40h, then 11h, then 10h, as §4.5 recommends). A feature is used only if the EFR register and the IVHD's image both report it.
- **A2. Default deny.** The device table reaches the highest DeviceID the IVHD names (rounded up to a page; a DeviceID beyond the table is denied by the IOMMU itself). Every entry starts **V=1, TV=0** — every DMA aborted and logged, none passed untranslated — with **IV=1, IntCtl 00b** (fixed and arbitrated interrupts aborted) and the IVRS DTE setting's pass bits and SysMgt. V=1/TV=0 rather than Mode 0 with IR=IW=0: both block by the specification, but QEMU passes Mode 0 through (§2.4). Every entry is invalidated after enabling (INVALIDATE_IOMMU_ALL as well where IASup): what the specification asks without IASup, and what puts every device under its entry in QEMU.
- **A3. One domain per requester DeviceID** (the IVRS alias where one is named), domain IDs from 1, 4-level host tables (Mode 4; every IOMMU's HATS allows at least 4), IOVAs from [1 GiB, 2 GiB) as under VT-d (ADR-0007 I4). Directory entries name the next level and allow read and write, so the leaf decides.
- **A4. Unmap invalidates and waits**: the PTE is cleared, then INVALIDATE_IOMMU_PAGES for the page and a COMPLETION_WAIT whose store the kernel polls. A map invalidates only where the capability block reports NpCache (QEMU does).
- **A5. Commands only through the command buffer** (AMD-Vi has no register-based invalidation): one page, 256 entries, every batch closed by a COMPLETION_WAIT with a store and the f bit, bounded by 100 ms.
- **A6. Coherency.** Page-table and interrupt-table walks are snooped (DTE.SD clear; §2.2.5); device-table reads follow Control.Coherent, set as the IVHD recommends, and device-table lines are flushed when it is clear.
- **A7. Interrupt remapping per device.** On first use a device gets its own 256-entry interrupt remapping table (IV, IntCtl 10b, IntTabLen 8). Its MSI-X entries carry `0xFEE0_0000` and data = the index, below 256 so the message type stays fixed; the vector and destination exist only in the IRTE. With GASup and XTSup the IOMMU runs GAEn and XTEn: 128-bit IRTEs with 32-bit x2APIC destinations, GuestMode 0. Without them, 32-bit IRTEs and 8-bit destinations, and APIC IDs above 255 are refused. The table hangs off the device's own DTE, so no other device can use it: the source check VT-d does in each IRTE is structural here.
- **A8. Faults are polled from the event log** (event interrupts off) and decoded by the specification. An I/O page fault counts as an interrupt only if I is set **and** its address is in the interrupt range (an interrupt request is a write there); this changes nothing on hardware and reads QEMU's mislabelled DMA faults (§2.5) correctly.
- **A9. Refused rather than half-done**: an IVRS failing any check, a segment other than 0, an IOMMU the firmware left enabled, and attaching a device an IVMD names (unity maps and exclusion ranges are not implemented).
- **A10. One set of entry points.** `iommu::{attach, map, unmap, translate, msi_message, route, reserve_blocked_interrupt, release_interrupt, take_faults, domains, source_id}` dispatch to VT-d or AMD-Vi; `dma::DeviceDma` and the NVMe and virtio-net drivers are unchanged. Faults reach the proofs as one neutral `iommu::Fault` (page not mapped, page denied, interrupt blocked, other) with each vendor's own record attached.

## 5. Tests defined before completion (CLAUDE.md §16.12)

- **IVRS parser (host):** QEMU's exact table; a two-IOMMU platform in two segments with every IVHD type (one IOMMU described twice, as 10h and 40h), every device entry type (select, range, alias select and range, extended select and range, special I/O APIC and HPET, ACPI HID with string and empty UIDs, reserved 4- and 8-byte types) and all three IVMD types; resolution by rank (named, range, all) with aliases and segments; every refusal at its offset; a mutation soak over three tables (CLAUDE.md §72).
- **AMD-Vi encodings (host):** capability, EFR and EFR2 decoding; the base registers; DTE round-trips for both forms, with every refusal and every entry the kernel never writes rejected; the host-table walker (table sharing, permissions, double maps, skipped levels, exhaustion) and a 20,000-step model check against a map; both IRTE formats with x2APIC destinations; messages and the index the IOMMU derives; every command's encoding; event decoding including QEMU's I-on-DMA form, and 20,000 random entries.
- **Boot (QEMU, `ORIGIN_QEMU_IOMMU=amd`, `-smp` 1/2/4):** the IVRS and the IOMMU's registers reported; NVMe and virtio-net pass every existing proof through their own domains; a DMA through an IOVA lands in the mapped physical page; after an unmap the same DMA is blocked (the page untouched) and an I/O page fault is recorded for the controller's DeviceID; every MSI-X entry is an index whose IRTE holds the vector and x2APIC destination, and APIC ID 300 is accepted; an interrupt through an IRTE with RemapEn clear is not delivered (and QEMU reports it on its stderr). Each with a negative control that fails the boot; the VT-d boot must pass unchanged.

## 6. Gaps and risks

- **QEMU is not hardware (§74).** Under QEMU 11.1 the event address, the interrupt-fault event and the IRTE cache cannot be shown (§2.4, §2.5); the kernel follows the specification on each, and the boot test pins QEMU's current behaviour so a change is noticed. A hardware run must require the IOVA in the fault record and the interrupt fault event.
- The specification was read from a mirror (§1); its hash has since been checked against AMD's copy and is identical.
- Not implemented: IVMD unity maps and exclusion ranges (such devices are refused), segments other than 0, taking over an IOMMU the firmware left enabled (pre-boot DMA protection, IVinfo bit 1), guest translation, guest virtual APIC (posted interrupts), ATS and device IOTLBs, PPR, the event-log interrupt, 2048-entry interrupt tables, device-table segmentation, and hot-plug. The register window mapped is the 16 KiB every register used lies in; with PCSup the window is 512 KiB.
- **The I/O APIC is blocked under AMD-Vi.** Its DTE (DeviceID 0x00A0 from the IVRS) aborts fixed interrupts. The kernel uses the I/O APIC only before the IOMMU comes up; routing an I/O APIC line afterwards needs an IRTE in that DeviceID's table (the redirection entry's vector field then holds the index). Under QEMU, a level-triggered line also needs an explicit I/O APIC EOI, because QEMU drops the trigger mode.
