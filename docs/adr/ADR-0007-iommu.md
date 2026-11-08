# ADR-0007: DMA isolation and interrupt remapping with the IOMMU (Intel VT-d first)

| | |
|---|---|
| Status | **Proposed** (2026-11-08). The owner accepts ADRs (CLAUDE.md §18, §84). This is a kernel-internal security mechanism, not an ABI or format decision, so it is implemented and boot-proven while Proposed; the parts that will reach the Driver ABI (how a user-mode driver asks for DMA memory, §31) are decided later, against this. |
| Requires | CLAUDE.md §31 (DMA mapping, interrupt management, driver isolation), §32 (Intel VT-d, AMD IOMMU, interrupt remapping), §58 (DMA protection) |
| Evidence | `docs/research/2026-11-08-vt-d-iommu.md` (VT-d Rev 5.20, D51397-019, 2026-05-14; what QEMU 11.1's `intel-iommu` emulates); the boot proofs and negative controls in `docs/engineering/SOURCE_PROVENANCE.md` ("Intel VT-d DMA and interrupt remapping"; "I/O APIC interrupts through VT-d interrupt remapping") |
| Related | ADR-0001 D2 (first-party kernel drivers), ADR-0004 (address-space layout: the MMIO window holds the units' registers) |

## Context

Until now every bus-mastering device could read and write all of physical memory: a buggy or malicious device, or a driver that hands a device a wrong address, could overwrite the kernel. MSI-X messages could reach only APIC IDs 0–255 (compatibility format), so `pci::msi_address` refused any higher ID, and 2026+ platforms with many cores have x2APIC IDs above 255. Both need the IOMMU: DMA remapping for the first, interrupt remapping for the second. QEMU emulates Intel VT-d (and AMD-Vi), so this can be proven in the boot test before hardware exists (CLAUDE.md §74 still applies: nothing here is qualified on hardware).

## Proposed decisions

### I1. Legacy-mode translation first; scalable mode later
Root table → per-bus context tables → second-level tables (4 levels, 48-bit, where the unit supports it; else 3 levels, 39-bit), translation type 0. It is the smallest correct design and QEMU's default.
*Alternative:* scalable mode (PASID tables, first-level tables, nested translation) — needed later for shared virtual addressing and PASID-granular isolation of user-mode drivers; a later revision of this ADR. Whether 2026+ platforms keep legacy mode must be checked against Rev 5.20 before hardware qualification (research ledger, gap).

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
A virtio device that offers `VIRTIO_F_ACCESS_PLATFORM` sits behind the IOMMU: the driver accepts the feature (QEMU refuses `FEATURES_OK` otherwise) and uses IOVAs. One that does not offer it uses physical addresses (virtio 1.1 §6) and is given them.

### I11. What is refused rather than half-done
A DMAR that fails any check (`framekernel::dmar::parse`) leaves the IOMMU off and fails the boot proof loudly; a unit the firmware left enabled is not taken over; a device named by a reserved memory region (RMRR) is refused at attach (identity-mapping RMRRs is not implemented yet), so its DMA stays blocked rather than unchecked; if one unit fails to come up, the units already running are turned off again so drivers are not left blocked with nobody to attach them.

## Consequences

- Drivers allocate DMA memory through `dma::DeviceDma` (attach once, then pages that come mapped) and program bus addresses, never physical ones; MSI-X entries are programmed through `iommu::msi_message`.
- The boot needs `-device intel-iommu,intremap=on,eim=on` and `iommu_platform=on` on virtio devices (xtask).
- Legacy IRQs are routed through `iommu::route_ioapic_pin` and a remappable-format redirection entry whenever interrupts are remapped (the PIT's IRQ0 is proven at every boot); a compatibility-format I/O APIC entry is used only before the IOMMU is up, or without interrupt remapping.
- Not yet: AMD-Vi (a separate, later step), scalable mode, RMRR identity maps, taking over a unit the firmware enabled (pre-boot DMA protection, `DMA_CTRL_PLATFORM_OPT_IN`), freeing IOVAs and table pages, large-page mappings, device-TLBs (ATS), a fault-event interrupt, hot-plug, and a live level-triggered I/O APIC pin (PCI INTx) with its EOI path.

## Questions for the owner

1. Accept default-deny with one domain per requester ID (I2, I3)?
2. Accept the [1 GiB, 2 GiB) IOVA window and "IOVAs are never physical addresses" (I4)?
3. Accept legacy mode now, with scalable mode as a later revision (I1)?
4. Accept refusing devices that need RMRRs until identity maps exist (I11), rather than leaving them untranslated?
5. Policy when a platform has no DMAR (today: run with DMA unconfined, reported): should production refuse to enable bus mastering for untrusted (external, hot-plugged) devices without an IOMMU (§32 external-device authorization)?
