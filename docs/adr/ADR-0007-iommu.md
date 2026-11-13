# ADR-0007: DMA isolation and interrupt remapping with the IOMMU (Intel VT-d and AMD-Vi)

| | |
|---|---|
| Status | **Accepted** by the owner on 2026-11-08: questions 1–5 and 9 first, then questions 6, 7 and 8 (AMD-Vi) once AMD's manuals were confirmed byte-identical to the copies that were read. Question 10 (I12, bus mastering cleared at enumeration) on 2026-11-09. This is a kernel-internal security mechanism, not an ABI or format decision; the parts that will reach the Driver ABI (how a user-mode driver asks for DMA memory, §31) are decided later, against this. **Amendment 1 (below): Accepted** by the owner on 2026-11-13 (items 247 to 251): identity maps for firmware-reserved memory, the shape questions 4 and 8 asked for before real hardware; built by TASKS item 40, not landed. |
| Requires | CLAUDE.md §31 (DMA mapping, interrupt management, driver isolation), §32 (Intel VT-d, AMD IOMMU, interrupt remapping), §58 (DMA protection) |
| Evidence | `docs/research/2026-11-08-vt-d-iommu.md` (VT-d Rev 5.20, D51397-019, 2026-05-14; what QEMU 11.1's `intel-iommu` emulates); `docs/research/2026-11-08-amd-vi-iommu.md` (AMD IOMMU specification 48882 Rev 3.11, April 2026; what QEMU 11.1's `amd-iommu` emulates and where it departs from the specification); the boot proofs and negative controls in `docs/engineering/SOURCE_PROVENANCE.md` ("Intel VT-d DMA and interrupt remapping"; "I/O APIC interrupts through VT-d interrupt remapping"; "AMD-Vi DMA and interrupt remapping"; "Bus mastering cleared at enumeration"); `docs/research/2026-11-08-bus-master-quiesce.md` for I12 |
| Threat model | `docs/engineering/THREAT_MODEL_IOMMU.md` (CLAUDE.md §58) |
| Related | ADR-0001 D2 (first-party kernel drivers), ADR-0004 (address-space layout: the MMIO window holds the units' registers) |

## Context

Until now every bus-mastering device could read and write all of physical memory: a buggy or malicious device, or a driver that hands a device a wrong address, could overwrite the kernel. MSI-X messages could reach only APIC IDs 0–255 (compatibility format), so `pci::msi_address` refused any higher ID, and 2026+ platforms with many cores have x2APIC IDs above 255. Both need the IOMMU: DMA remapping for the first, interrupt remapping for the second. CLAUDE.md §32 asks for both vendors' IOMMUs. QEMU emulates Intel VT-d and AMD-Vi, so both can be proven in the boot test before hardware exists (CLAUDE.md §74 still applies: nothing here is qualified on hardware). The decisions I1–I11 were written for VT-d in vendor-neutral terms; A1–A10 below say how AMD-Vi meets them.

## Decisions

### I1. Legacy-mode translation first; scalable mode later
Root table → per-bus context tables → second-level tables (4 levels, 48-bit, where the unit supports it; else 3 levels, 39-bit), translation type 0. It is the smallest correct design and QEMU's default.
*Alternative:* scalable mode (PASID tables, first-level tables, nested translation) — needed later for shared virtual addressing and PASID-granular isolation of user-mode drivers; a later revision of this ADR. Rev 5.20 still defines legacy mode (`RTADDR_REG.TTM` 00b), with no deprecation notice. It depends on second-stage translation: a unit of major version 6 or higher that reports `ECAP.SSTS` clear also reports `SAGAW` 0, and the kernel refuses such a unit. Hardware qualification must confirm this on the declared machines.

### I2. Default deny
Translation is turned on before any driver binds, with no context entry present: until a driver attaches its device, the device's DMA faults (reason 2). A device with no driver never gets a context entry.
*Alternative:* pass-through for unclaimed devices (as some systems do for compatibility) — rejected: it leaves exactly the devices nobody watches unconfined.

### I3. One domain per requester ID
Each attached PCI function gets its own domain (domain ID from 1, never 0), second-level tables and IOVA space. A device reaches only the pages its own driver mapped for it.
*Alternatives:* one shared domain for all devices (simpler, but any device could reach any other's buffers); per-driver domains (equivalent while every driver binds one function).

### I4. IOVAs are not physical addresses
Every domain allocates bus addresses from **[1 GiB, 2 GiB)**: below every PC platform's 32-bit MMIO hole (so not routed peer-to-peer before the IOMMU), clear of the interrupt range `0xFEEx_xxxx`, reachable by 32-bit DMA engines, and — in the 512 MiB QEMU test machine — not backed by RAM, so a DMA that bypassed translation would land nowhere (the boot proof shows the data arriving in a different physical page). Pages are mapped one 4 KiB page at a time, read/write, and only the driver's DMA pages are mapped.
*Alternatives:* identity mapping (IOVA = physical; simpler, but a translation failure would be invisible and the device learns physical addresses); allocating anywhere in the 48-bit space (must then avoid MMIO windows known only from ACPI `_CRS`).

### I5. Unmapping invalidates, drains and waits
An unmap clears the leaf, then queues a page-selective IOTLB invalidation (domain-selective without `CAP.PSI`) with drain-reads and drain-writes where `CAP` offers them, and a wait descriptor; the unmap returns only once the unit wrote the wait's status. Only then may the page be reused.

### I6. Queued invalidation only
All invalidation goes through the invalidation queue (it is required for interrupt remapping anyway). A unit without queued invalidation is reported and left alone.

### I7. Coherency rules from the capability registers
With `ECAP.C` clear, every written root, context, paging, IRTE and queue line is flushed (`CLFLUSH`, then `MFENCE`) before the hardware can need it; with `CAP.RWBF`, a write-buffer flush follows changes; with `CAP.CM`, not-present-to-present changes are invalidated too (context: device-selective with domain 0; IOTLB: domain or page). Entries are written high half first and the present bit last.

### I8. Interrupt remapping with 32-bit x2APIC destinations and source checks
One interrupt remapping table per unit (256 entries to start), `EIME` set when the unit has `ECAP.EIM`. Every IRTE is fixed delivery, physical destination, and **verifies its source's ID** (`SVT` 1, `SQ` 0). An MSI's IRTE is edge-triggered. An MSI-X entry carries only a remappable-format handle (`SHV` set, sub-handle and data 0): the vector and destination exist only in the IRTE. `CFI` stays clear, so compatibility-format interrupts are blocked (QEMU does not enforce this; hardware does). With interrupt remapping on and EIM, `iommu::msi_message` accepts any 32-bit APIC ID — the APIC-ID-255 refusal is lifted. Without interrupt remapping the compatibility format and its refusal stay.

**I/O APIC pins** (added 2026-11-08) are routed the same way, since with `CFI` clear hardware drops their compatibility-format interrupts:
- The redirection entry is written in remappable format (VT-d chapter 5): the IRTE index, the format bit, delivery-mode bits 000, and the pin's polarity, trigger mode and mask.
- The IRTE (`iommu::route_ioapic_pin`) lives on the unit whose DMAR device scope lists that I/O APIC by its MADT ID, and checks the source ID that scope names. An I/O APIC no unit lists explicitly is refused; INCLUDE_PCI_ALL does not cover one.
- The IRTE has the pin's trigger mode. For a level-triggered pin, the entry's vector equals the IRTE's, so the EOI broadcast for the delivered vector clears the pin's remote IRR.

QEMU 11.1's I/O APIC sends its messages without a requester ID, so under QEMU the unit records their faults with source ID 0xffff and cannot check the source ID; the boot proof measures this and says so (`SOURCE_PROVENANCE.md`).

### I9. Faults are polled and recorded
Fault events stay masked; the fault-recording registers are read and cleared by the kernel (`iommu::take_faults`), decoded by reason. A fault-event interrupt that feeds the kernel log (§69) is a later step.

### I10. virtio devices
A virtio device that offers `VIRTIO_F_ACCESS_PLATFORM` sits behind the IOMMU: the driver accepts the feature (QEMU refuses `FEATURES_OK` otherwise) and uses IOVAs. One that does not offer it uses physical addresses (virtio 1.1 §6) and is given them only where the IOMMU is not translating; behind a translating IOMMU it is refused (I12).

### I11. What is refused rather than half-done
A DMAR that fails any check (`framekernel::dmar::parse`) leaves the IOMMU off and fails the boot proof loudly; the firmware's `DMA_REMAP_OPT_OUT` and `X2APIC_OPT_OUT` requests are reported and not honoured (Rev 5.20 §8.1; decision-sheet item 25); a unit the firmware left enabled is not taken over; a device named by a reserved memory region (RMRR) is refused at attach (identity-mapping RMRRs is not implemented yet), so its DMA stays blocked rather than unchecked; if one unit fails to come up, the units already running are turned off again so drivers are not left blocked with nobody to attach them.

### I12. Bus mastering is cleared at enumeration and set again only after a device is attached (added 2026-11-08; threat T4)

Translation is only on once the IOMMU is up, and firmware hands over with devices still able to master the bus (OVMF: every function, research ledger `docs/research/2026-11-08-bus-master-quiesce.md` §4). So:

- `pcie::quiesce` clears **Bus Master Enable on every function of every MCFG window, bridges included**, as soon as the kernel's page tables and heap exist, before any other subsystem runs, and reads each one back. The read-back is also what pushes out writes the function had already posted. Transactions Pending is then waited for once (at most 100 ms for all functions together). A bridge is cleared before the functions behind it.
- A driver turns bus mastering on again **only through `dma::DeviceDma::enable_bus_master`**, after `DeviceDma::attach` and, for a controller, after resetting it (the NVMe driver), which refuses when the IOMMU translates and the device has no domain of its own, and refuses if the bit is already on. It opens every bridge above the function, nearest the root first, then the function. `pcie::enable` no longer exists; nothing else can set the bit.
- `iommu::init` runs **before `smp::start`** instead of after enumeration, right after the compatibility-format I/O APIC proof (which interrupt remapping would block on hardware).
- A virtio device without `VIRTIO_F_ACCESS_PLATFORM` behind a translating IOMMU is refused (this tightens I10): the kernel cannot confine it.
- **The IOMMU's own PCI function is not touched** (AMD-Vi; the IVRS names it, never a class code, which any device can claim): a domain cannot confine the hardware that enforces domains, and AMD's specification (48882 Rev 3.11) does not mention its Bus Master Enable, so whether its table walks depend on it could not be determined. Hardware qualification must check this.

*Alternatives:* clearing only bridges (as a Linux patch did at ExitBootServices): an integrated endpoint has no bridge above it, so its own bit is all there is, and a device that ignores its own bit is stopped by its bridge, so both are cleared. Clearing only endpoints: a hostile endpoint ignores its bit; the bridge above it does not. Clearing in the loader before ExitBootServices: UEFI drivers are still running on those devices; rejected (ledger §1 E1, L2), and it would not shrink the hostile window much. Leaving firmware's bus masters running until a driver attaches: that is T4. Waiting per function for Transactions Pending: a device could then multiply the delay; one shared deadline cannot be stretched.
*Evidence:* ledger §2 (requests in flight; the specification could not be read), §4 (what OVMF leaves), §7 (why the IOMMU is not earlier still), the boot proofs and controls C1–C10.
*Trade-offs:* A device whose firmware keeps it running across the hand-off (SMM-driven USB legacy, a firmware framebuffer's scan-out) loses bus mastering; no driver depends on those yet, and hardware qualification must decide what to except (ledger §9). A function that ignores the write is only reported.

## Decisions for AMD-Vi

The same policy, through AMD-Vi's structures (AMD IOMMU specification 48882 Rev 3.11; research ledger §4). Drivers do not change: `dma::DeviceDma`, `iommu::msi_message` and the other `iommu` entry points dispatch to the AMD-Vi driver (`iommu::amd`) when the firmware provides an IVRS instead of a DMAR.

### A1. Discovery from the IVRS, each IOMMU once
The IVRS is checked whole (`framekernel::ivrs::parse`); a platform may describe one IOMMU in several IVHD types, and the richest one is used (40h, then 11h, then 10h; specification §4.5). A feature is used only if the Extended Feature Register and the IVHD's image of it both report it.
*Alternative:* trusting the IVHD image alone, as §5 allows — rejected for the features the kernel turns on (x2APIC mode, INVALIDATE_IOMMU_ALL): a firmware that over-reports would make the kernel program what the hardware lacks.

### A2. Default deny (I2), as device table entries that block
The device table reaches the highest DeviceID the IVHD names, rounded up to a page; a DeviceID beyond it is denied by the IOMMU itself. Every entry starts **V=1, TV=0**: every DMA aborted and logged, none passed untranslated; and **IV=1, IntCtl 00b**: fixed and arbitrated interrupts aborted. The firmware's DTE settings from the IVRS (the INIT, ExtInt, NMI, LINT0 and LINT1 pass bits and SysMgt) are kept. After enabling, every entry is invalidated (and INVALIDATE_IOMMU_ALL issued where supported).
*Alternatives:* V=0 — rejected: it passes DMA through untranslated (specification Table 8); Mode 0 with IR=IW=0 — blocks by the specification but QEMU 11.1 passes it through, so it could not be proven; a 64K-entry table always — wasteful when the IVRS names a few buses.

### A3. One domain per requester (I3), host page tables, the same IOVA window (I4)
Each attached device gets a domain ID from 1 and 4-level host page tables (Mode 4, 48-bit), for the DeviceID its requests carry (the IVRS alias where one is named). IOVAs come from [1 GiB, 2 GiB), never physical addresses. Directory entries name the next level (no level skipping) and allow read and write; the leaf decides.

### A4. Unmapping invalidates and waits (I5)
The PTE is cleared, then INVALIDATE_IOMMU_PAGES for the page and a COMPLETION_WAIT whose store the kernel polls; only then may the page be reused. A map is followed by an invalidation only where the capability block reports NpCache.

### A5. The command buffer only (I6)
AMD-Vi has no register-based invalidation. One page, 256 commands; every batch ends with a COMPLETION_WAIT (store, f bit), bounded at 100 ms. A stopped command buffer is reported as a failure.

### A6. Coherency (I7)
Page-table and interrupt-table walks are snooped (DTE.SD clear; interrupt walks always are). Device-table reads follow Control.Coherent, set as the IVHD recommends; when it is clear, each device table line the kernel writes is flushed.
*Added 2026-11-09, for the owner's review:* the DMA itself is made coherent too. Every AMD-Vi leaf sets FC, so a device's No Snoop request cannot bypass the caches the kernel's buffers live in; every VT-d leaf sets SNP where `ECAP.SC` is reported, and without SC VT-d offers no such control (a follow-up clears Enable No Snoop in the device instead). Enabling the IOMMU keeps the Control bits the firmware owns, its SMI filter above all, since no DTE field controls SMIs. Research ledger §7.

### A7. Interrupt remapping per device (I8)
On first use a device gets its own 256-entry interrupt remapping table (IV, IntCtl 10b, IntTabLen 8). Its MSI/MSI-X messages carry `0xFEE0_0000` and data = the IRTE index, below 256 so the message type stays fixed; the vector and destination exist only in the IRTE (fixed delivery, physical destination, no EOI request). With GASup and XTSup, the IOMMU runs GAEn and XTEn: **128-bit IRTEs with 32-bit x2APIC destinations** (GuestMode 0), and `iommu::msi_message` accepts any APIC ID. Without them, 32-bit IRTEs and 8-bit destinations, and APIC IDs above 255 are refused. Every IOMMU in the system uses the same IRTE size (§2.2.5.2), so the 128-bit form is used only if all of them support it (added 2026-11-09). VT-d checks the source ID in every IRTE; AMD-Vi needs no such field, because a device can reach only the table its own device table entry points to.
*Alternatives:* one shared table per IOMMU (impossible: the table hangs off each DTE); 2048-entry tables (NumIntRemapMode 01b) — not needed yet, and the indexes above 255 change the message type.

### A8. Faults polled from the event log (I9)
Event interrupts stay off; `iommu::take_faults` drains the event log (restarting it after an overflow) and decodes by the specification. Both IOMMUs' faults reach callers as one neutral record (page not mapped, page denied, interrupt blocked, other), with the vendor's own record attached for the report. An I/O page fault counts as an interrupt only if I is set and its address is in the interrupt range — on hardware the two agree; QEMU 11.1 sets I on DMA faults too.

### A9. Refused rather than half-done (I11)
An IVRS that fails any check fails the boot proof loudly; an IOMMU the firmware left enabled is not taken over; a PCI segment other than 0 is refused; a device an IVMD names (unity maps, exclusion ranges) is refused at attach, as RMRR devices are, so its DMA stays blocked rather than unchecked; if one IOMMU fails to come up, the ones already running are turned off again.

### A10. What the QEMU proofs cannot show (CLAUDE.md §74)
QEMU 11.1's `amd-iommu` translates emulated devices only with `dma-remap=on`, writes every event with address 0 (undefined behaviour in its event encoder, fixed only after 11.1), logs no interrupt-remapping fault and caches no IRTEs. The kernel follows the specification on each point. The AMD-Vi boot test pins QEMU's behaviour (address 0, no interrupt fault record, QEMU's own stderr line for the blocked interrupt), so a QEMU that changes is noticed; **hardware qualification must require the IOVA in the fault record and the interrupt fault event.**

## Consequences

- Drivers allocate DMA memory through `dma::DeviceDma` (attach once, then pages that come mapped) and program bus addresses, never physical ones; MSI-X entries are programmed through `iommu::msi_message`. Bus mastering is set only by `DeviceDma::enable_bus_master`, after attaching (I12).
- The boot needs `-device intel-iommu,intremap=on,eim=on` and `iommu_platform=on` on virtio devices (xtask). With `ORIGIN_QEMU_IOMMU=amd` it boots instead with `-device AMDVI-PCI,id=iommupci0,addr=05.0 -device amd-iommu,pci-id=iommupci0,intremap=on,xtsup=on,dma-remap=on`; `boot-matrix` runs both IOMMUs at 1, 2 and 4 CPUs. Any other value of `ORIGIN_QEMU_IOMMU` must be an `intel-iommu` device, booted with the VT-d markers (negative controls such as `intel-iommu,intremap=off`).
- Legacy IRQs are routed through `iommu::route_ioapic_pin` and a remappable-format redirection entry whenever VT-d remaps interrupts (the PIT's IRQ0 is proven at every boot); a compatibility-format I/O APIC entry is used only before the IOMMU is up, or without interrupt remapping. Under AMD-Vi the pins are not routed yet (`iommu::routes_ioapic_pins` is false) and stay masked: see below.
- Faults reach the proofs through the neutral `iommu::Fault`; the IOMMU proofs in `storage.rs` and the I/O APIC proof in `irq.rs` read it.
- Not yet for AMD-Vi: IVMD unity maps and exclusion ranges, segments other than 0, taking over an IOMMU the firmware enabled (IVinfo DMA remap support), guest translation, guest virtual APIC (posted interrupts), ATS, PPR, the event-log interrupt, 2048-entry interrupt tables, device-table segmentation; and the I/O APIC under AMD-Vi, whose device table entry (DeviceID from the IVRS special entry) blocks its fixed interrupts until it gets an interrupt table.
- Not yet: scalable mode, RMRR identity maps, taking over a unit the firmware enabled (pre-boot DMA protection, `DMA_CTRL_PLATFORM_OPT_IN`), freeing IOVAs and table pages, large-page mappings, device-TLBs (ATS), a fault-event interrupt, hot-plug, and a live level-triggered I/O APIC pin (PCI INTx) with its EOI path.

## Trade-offs

- **One domain per device (I3):** the strongest isolation available here, at the cost of a set of translation tables per device.
- **Strict, synchronous invalidation on unmap (I5):** it closes the window that deferred invalidation leaves open, at a cost on every unmap. That cost has not been measured (CLAUDE.md rule 21); no driver unmaps yet.
- **IOVAs are never physical (I4):** a failed translation cannot pass unnoticed, but there is no identity-mapped fast path.
- **4 KiB read/write mappings only:** simple and uniform, but more IOTLB misses than large pages, and a device can write pages it only needs to read.
- **Queued invalidation only (I6):** one invalidation path to get right; a unit without it goes unused.
- **Polled faults (I9, A8):** no interrupt path to harden, but nothing notices a fault after the boot proofs.
- **Refusing rather than half-doing (I11, A9):** RMRR and IVMD devices fail closed, at the cost of devices that will not work until identity maps exist. A failed bring-up, by contrast, fails *open*: threat T6 in `docs/engineering/THREAT_MODEL_IOMMU.md`.

## Migration consequences

None on disk and none at any ABI: all of this is kernel-internal, and no persistent state depends on it.
- Scalable mode (I1) replaces table formats behind `iommu`; drivers do not change.
- The IOVA window (I4) can move or grow, because drivers use only the addresses `map` returns.
- RMRR and IVMD identity maps are additive.
- When user-mode drivers arrive (§31, §47), their DMA interface needs its own ADR. It must be capability-checked and must not expose the window or the table formats; `dma::DeviceDma` stays beneath it.

## Amendment 1 (Accepted, 2026-11-13): identity maps for firmware-reserved memory

**Status: Accepted by the owner on 2026-11-13, each item with the recommended answer (decision sheet items 247 to 251; CLAUDE.md §84).** The owner gave the answer first in a fork of the coordinating session ("Okay, Gohead", to "Accept all 5") and confirmed it in the coordinating session ("Yes, accept all 5"). Questions 4 and 8 accepted refusing devices that firmware-reserved memory names (RMRRs under VT-d, IVMDs under AMD-Vi) only until identity maps exist, and set the deadline "identity maps must land before real hardware". This decides their shape. TASKS item 40 built it on `claude/rmrr-ivmd-maps` and it has not landed; its research ledger, `docs/research/2026-11-12-reserved-memory-identity-maps.md`, holds the reasoning and reaches `main` with that task. Rental A's server showed a real IVRS with no IVMD.

| # | Item | Decision | Refines |
|---|---|---|---|
| R1 | 247 | **Identity maps are made when a driver attaches the device.** The device's own domain holds the identity maps for every firmware-reserved range that names it before the device's entry turns present. A device that no driver attaches gets none: I12 keeps its bus mastering off, so a map would gain nothing, and making maps at bring-up would change I2. | I11, A9; I2, I12 |
| R2 | 248 | **The IOVA window skips identity-mapped pages.** Each domain's window, [1 GiB, 2 GiB), leaves out the pages identity-mapped in that domain. I4 now reads "IOVAs are not physical addresses, except within a firmware-reserved range identity-mapped by R1, where a device's bus address is the physical address the firmware gave it". | I4, A3 |
| R3 | 249 | **What a firmware-reserved range must be before it is mapped.** Every page of it is Reserved, RuntimeCode or RuntimeData in the UEFI memory map; it lies outside the interrupt range (and, under AMD-Vi, outside the ranges the AMD IOMMU specification reserves); it is clear of the platform's register ranges; and it lies within the domain's address width. A range that fails any of these is not mapped and is reported, and the devices it names stay refused at attach, as I11 and A9 have them today. The boot goes on. | I11, A9 |
| R4 | 250 | **What an IVMD's flags give.** Unity with IR and IW set as given: an identity map with exactly those permissions. IR or IW without Unity: an identity map as well. IR and IW both clear: `map` refuses a frame inside the range for the devices it covers. An ExclusionRange: an identity map, read and write, in each named device's domain; the IOMMU's exclusion registers are never programmed. | A9 |
| R5 | 251 | **Which devices an IVMD covers.** Type 20h: every device of the IOMMU whose description comes before it. Types 21h and 22h: the devices named by their own segment and DeviceID, or by the alias the device's requests carry, on whichever IOMMU translates that device. | A1, A9 |

**Options that were put.** Each item was put with one recommendation; the alternatives considered are in TASKS item 40's research ledger. The main ones: maps made at bring-up for every device (R1), which gains nothing while I12 keeps bus mastering off; a separate window for identity maps (R2); mapping a range the memory map does not mark as reserved (R3), which would let firmware tables open ordinary RAM to a device; programming AMD's exclusion registers (R4), which bypass translation for the range on every device of that IOMMU.

**Performance cost (ADR-0018 V3).** Unmeasured, owed (cost ledger row CL39, added when the ledger and this amendment are in one tree): at a device's attach, one identity map for each page of each reserved range that names it; at each IOVA allocation in a domain that has identity maps, the skipped pages. A DMA through an identity map is translated as any other. Nothing changes for a device that no reserved range names.

**Trade-offs.** A device that firmware names is reached at the addresses firmware chose, which I4 otherwise forbids, and a device learns those physical addresses. In return such devices work at all, which question 4 required before real hardware.

**Migration consequences.** None for a program or a driver: nothing of the Driver ABI changes. ADR-0013 gains TASKS item 40's boot proof and controls when that task lands.

**What stays unknown.** No machine in the declared scope has been seen with an RMRR or an IVMD. QEMU has neither, so TASKS item 40's boot test plants one in its tables (a test-only step). Rental A's server had an IVRS with no IVMD.

## Questions for the owner (1–9 answered 2026-11-08; 10 open)

1. Accept default-deny with one domain per requester ID (I2, I3)? **Yes (2026-11-08).**
2. Accept the [1 GiB, 2 GiB) IOVA window and "IOVAs are never physical addresses" (I4)? **Yes (2026-11-08).**
3. Accept legacy mode now, with scalable mode as a later revision (I1)? **Yes (2026-11-08).**
4. Accept refusing devices that need RMRRs until identity maps exist (I11), rather than leaving them untranslated? **Yes (2026-11-08), with a deadline:** identity maps must land before real hardware.
5. Policy when a platform has no DMAR or IVRS (today: run with DMA unconfined, reported): should production refuse to enable bus mastering for untrusted (external, hot-plugged) devices without an IOMMU (§32 external-device authorization)? **Yes (2026-11-08),** and an IOMMU is part of the declared hardware scope.
6. Accept, for AMD-Vi, blocking device table entries (V=1, TV=0; IV=1, IntCtl 00b) and one interrupt remapping table per device (A2, A7)? **Yes (2026-11-08).**
7. Accept the two QEMU allowances in the AMD-Vi proofs — a fault record with address 0, and no record of the blocked interrupt — as documented blind spots of QEMU 11.1, with hardware qualification required to check both (A10)? **Yes (2026-11-08),** as QEMU limits; hardware qualification must check both.
8. Accept refusing devices that an IVMD names, as for RMRRs (A9, I11)? **Yes (2026-11-08),** with question 4's deadline: identity maps must land before real hardware.
9. Should the kernel honour the firmware's `DMA_REMAP_OPT_OUT` and `X2APIC_OPT_OUT` requests (Rev 5.20 §8.1)? **No (2026-11-08): report them and carry on.** §58 requires DMA protection, and the kernel depends on x2APIC. A platform that truly needs either is outside the declared hardware scope.
10. *(Accepted by the owner on 2026-11-09, decision sheet item 30; added with T4, merged 2026-11-09.)* Accept clearing bus mastering on every function at enumeration (I12), including the bridges above devices that are not driven yet, and leaving the AMD IOMMU's own function alone pending hardware evidence? Clearing it may break firmware-owned controllers on real hardware (USB legacy, a firmware framebuffer) until drivers exist or exceptions are decided.
