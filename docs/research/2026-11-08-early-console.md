# Research: an early console without a 16550 at 0x3F8 (2026-11-08)

Backing notes for ADR-0010 (CLAUDE.md §16). What was read, what was observed on QEMU 11.1.3 with OVMF, and what stays unknown.

## Authoritative sources

| Source | Version | Used for | How it was read |
|---|---|---|---|
| Microsoft, "Serial Port Console Redirection Table (SPCR)" | Spec revision 1.10, 2023-05-01 (table revision 4), learn.microsoft.com, page updated 2026-03-20 | Every field offset and value in `framekernel::spcr` | Read in full through WebFetch, 2026-11-08 |
| Microsoft, "Debug Port Table 2 (DBG2)" | 2023-04-10 revision; Table 3 and the notes on the Generic Address Structure and 16550-based UARTs | The interface subtypes SPCR revision 2 and later use; what register width and access size mean | Read in full through WebFetch, 2026-11-08 |
| UEFI 2.11 §12.9, `EFI_GRAPHICS_OUTPUT_PROTOCOL` | December 2024 | GUID, `Mode`, `EFI_GRAPHICS_OUTPUT_MODE_INFORMATION` | **Not read in full:** `uefi.org` returned HTTP 403 to the fetch tool. The ABI (GUID, field order, the pixel format enumeration) was confirmed against EDK II's `MdePkg/Include/Protocol/GraphicsOutput.h`, used as a reference for the interface only, and by booting OVMF: wrong offsets would have failed the boot proof |

## SPCR: what this kernel accepts

- **Interface Type** (offset 36). Revision 1: 0 full 16550, 1 full 16450 ("must also accept writing to the 16550 FCR register"). Revision 2 and later: the DBG2 serial subtypes. The 16550-compatible ones are 0x0000, 0x0001 (DBGP subset), 0x0012 (parameters from the Generic Address Structure) and 0x0014 (Intel LPSS). ARM, Qualcomm, TI, Samsung and similar subtypes are refused as not 16550.
- **Base Address** (offset 40, a 12-byte Generic Address Structure). "or 0 if console redirection is disabled." The DBG2 notes say for subtype 0x12 the Address Space ID and bit offset must be 0, register bit width is the **stride** ("must be a power of 2 that is at least as large as the access size"), and access size says byte, word, dword or qword. SPCR's own examples for COM1 and COM2 use space 1 (I/O), width 8 and access size **0**, so 0 is accepted as undefined there. In memory space a width of 0 is refused: there is no way to tell the stride.
- **Configured Baud Rate** (offset 58): 0 "as is", 3, 4, 6, 7 → 9600, 19200, 57600, 115200, others reserved. **Precise Baud Rate** (offset 80, revision 4) overrides it when non-zero. **UART Clock Frequency** (offset 76, revision 3): non-zero is the clock in Hz.
- Parity and stop bits are not enforced: the table defines only "no parity" and "1 stop bit", and the kernel programs 8N1 or keeps the firmware's format.

**Decision (ADR-0010 E4).** A stated rate and clock (revision 3 and later) are programmed. Without a stated clock, a divisor latch the firmware programmed is left alone, because the table's rate is "the baud rate the BIOS used". A latch of zero gets the table's rate on the PC clock if the UART is in I/O space, and is left alone in memory space, where the clock could be anything (Intel LPSS runs from another clock than the PC's 1.8432 MHz), and a wrong divisor would garble every line. If the table's rate cannot be reached to within 2 %, the UART is left as it is.

## QEMU 11.1.3 with OVMF: what was observed

- OVMF's GOP: one mode in use, `1280x800`, BGR (`red_mask 0xff0000, blue_mask 0xff`), stride 5120, at BAR0 of the VGA device (physical `0x80000000`). The loader line `origin-boot: framebuffer: 1280x800 at 0x80000000, stride 5120, red 0xff0000 green 0xff00 blue 0xff` is the evidence.
- QEMU's `screendump` works with `-display none`, through the monitor multiplexed on stdio (`-serial mon:stdio`, then Ctrl-A C), and writes a `P6` PPM of the guest's mode. This is what `xtask`'s screen check reads.
- A table added with `-acpitable file=` reaches OVMF's XSDT: OVMF installs tables from QEMU's linker script, and the XSDT lists it. The SPCR proof depends on this.
- An unassigned I/O port reads 0xFF, and a UART can be placed anywhere with `-device isa-serial,iobase=`. A UART that never reports room to transmit can be mimicked by putting `isa-debugcon,iobase=<UART+5>,readback=0` at its line-status port; the proof uses this for the give-up path.
- QEMU's x86 machines have **no memory-mapped 16550**: `isa-serial`, `pci-serial` (I/O BAR) and `serial-pci-multi` are all port-mapped, and sysbus `serial-mm` is not creatable on them. The MMIO path therefore has host tests with a mock register file (including a 32-bit stride) but no boot proof.

## Unknowns that only hardware can answer (§74, rule 30)

- Whether the machines in ADR-0009 publish an SPCR at all, and what subtype and address it names.
- The firmware framebuffer's behavior on a real integrated GPU once `iommu::init` turns translation on: the display engine scans out by DMA through the unit (T14; ADR-0010, "What this does not prove").
- Write-combining bandwidth of a real framebuffer; uncached was chosen for simplicity and is bounded per line (ADR-0010 E3).
