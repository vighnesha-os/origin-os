# Threat model: DMA and interrupt remapping

| | |
|---|---|
| Required by | CLAUDE.md §58 ("Threat-model every privileged interface"), §32, §47 |
| Covers | `framekernel/src/iommu.rs`, `iommu/amd.rs`, `dma.rs`, `pcie.rs`, `pci.rs`, `vtd.rs`, `amdvi.rs`, `dmar.rs`, `ivrs.rs`, `reserved.rs`; ADR-0007 |
| Written | 2026-11-08, **after** the implementation. §58 and §83 step 12 expect this during design. |
| Evidence | QEMU only. "Proven" below means a boot proof with a negative control under QEMU 11.1. Nothing has run on hardware (§74). Every VT-d encoding has been checked against Rev 5.20 (research ledger, last section). |

## What is being protected

- Kernel memory, other processes' memory and other devices' DMA buffers.
- The IOMMU's own structures: root, context and device tables, page tables, interrupt remapping tables, invalidation queues, command buffers and event logs.
- The CPUs' interrupt state: nothing outside the kernel should choose a vector, a destination or a delivery mode.

## Who the attackers are

| Actor | Can | Today |
|---|---|---|
| A hostile or compromised internal PCIe device (for example, malicious NVMe or NIC firmware) | Issue any DMA or MSI, reply to the driver with anything | The main threat. |
| A hostile external device (Thunderbolt, USB4, hot-plug; compare Thunderclap, NDSS 2019) | All of the above, and it is attached by whoever holds the machine | No external or hot-plug support exists yet. |
| A buggy first-party kernel driver (kernel-mode drivers are first-party only, ADR-0001) | Program a device with a wrong address | In scope, as error rather than malice. |
| A third-party user-mode driver (§47) | Would ask for DMA mappings | No such interface exists yet. It needs its own ADR. |
| Firmware | Supplies the DMAR/IVRS and may leave devices or units running | Trusted to describe the platform, but its tables are validated. |

**Out of scope:** physical attacks on memory or debug ports, malicious CPU microcode, a kernel already compromised some other way, and side channels.

## Threats

Status: **Proven** (boot proof and control, QEMU), **By design** (no proof), **Open**.

### DMA

| # | Threat | Defence | Status |
|---|---|---|---|
| T1 | A device reads or writes memory it was not given | Default deny; one domain per device; only the driver's own pages are mapped. Rests on T25 | **Proven** |
| T2 | A device keeps using a page after it is unmapped | The unmap invalidates, drains and waits before returning (I5). No driver unmaps yet, and no DMA page is ever freed, including one whose map failed | **Proven** (control C1) |
| T3 | A device reaches the IOMMU's own tables | They are never mapped in any domain | By design |
| T4 | **Early-boot DMA.** A device the firmware left bus-mastering writes memory before translation is on | `pcie::quiesce` clears Bus Master Enable on every function (bridges too) as soon as the kernel can reach configuration space, before the IOMMU, the timer or any other processor; the IOMMU then comes up before SMP start. Drivers set the bit again only through `DeviceDma::enable_bus_master`, after attaching. | **Partly.** Proven in QEMU for a function that honours the bit: boot proof and controls C1–C10, ledger `docs/research/2026-11-08-bus-master-quiesce.md`. Two windows remain (below) |
| T5 | **A unit the firmware left enabled** (pre-boot DMA protection). Bring-up refuses it, which leads to T6 | None | **Open** |
| T6 | **Fail-open.** If bring-up fails, or no DMAR/IVRS exists, DMA runs unconfined. That includes a unit with no second-stage translation (`SSTS` clear). It is reported, and the boot test fails. On a unit with `ECAP.IRREQ`, turning remapping off also blocks every MSI | A present IOMMU that fails, an unreadable DMAR or a failed quiesce refuses every driver (`DeviceDma::attach`). With no DMAR or IVRS at all, internal devices still bind (sheet item 9) | **Partly** |
| T7 | A device claims its addresses are already translated, through ATS (Thunderclap's Linux bypass) | ATS is never enabled. VT-d context entries use translation type 0, which blocks Translated and Translation Requests (Rev 5.20 §9.3). AMD device table entries leave `I` clear, which the decoder enforces | By design |
| T8 | **Peer-to-peer.** Traffic between devices under a shared switch never reaches the IOMMU | None: the kernel does not configure ACS. QEMU cannot show this | **Open** |
| T9 | **Requester-ID spoofing.** Isolation assumes the source ID is genuine | None: needs ACS source validation. QEMU cannot show this | **Open** |
| T10 | A device behind a PCIe-to-PCI bridge carries a different source ID | VT-d: not handled, so its DMA faults (fails closed). AMD-Vi: the IVRS alias is used | Partly |
| T11 | A device overwrites a page it only needed to read | None: every mapping is read/write (I4) | **Open** |
| T12 | A device reaches kernel data that shares a page with its buffer (compare Markuze et al., ASPLOS 2016) | `DeviceDma::page` allocates whole pages. But `iommu::map` is crate-visible, so kernel code could misuse it | By design |
| T13 | A device changes shared memory after the driver checked it | Drivers validate what a device supplies, and hostile-ring tests exercise it. Virtio rings are read with volatile accesses and received frames are copied once before parsing. Most kernel-side refusals are host-tested only | Partly |
| T14 | Firmware requires a device to reach reserved memory (RMRR, IVMD) | Since TASKS item 40 (2026-11-12): when a driver attaches the device, its domain identity-maps every range the firmware names for it, with the firmware's permissions, before the device's entry turns present. An IVMD's read-only range stays read-only. An exclusion range is identity-mapped, read and write, and never let through the exclusion registers. The IOVA window keeps clear of these pages, and they are never unmapped. A range is mapped only if T26's checks pass; otherwise the devices it names are still refused at attach (fails closed). A device with no driver still gets no entry (I2), and its bus mastering is cleared at enumeration (I12). So a firmware-owned device still stops when translation comes up: USB legacy emulation under SMM, or an integrated GPU scanning out the firmware framebuffer (ADR-0010's console goes dark there). Research ledger `docs/research/2026-11-12-reserved-memory-identity-maps.md`; R1–R5 accepted by the owner on 2026-11-13 (ADR-0007 Amendment 1, decision items 247 to 251). The boot proof writes only into pages the test-only step filled, so on a real machine it never writes memory the firmware keeps for itself (TASKS item 40's review, M2) | **Proven** for attached devices (QEMU, both IOMMUs, with ranges a test-only firmware step plants; boot proofs and controls, `SOURCE_PROVENANCE.md`, "Identity maps for firmware-reserved memory"). **Open** for firmware-owned devices without a driver: R1's option (b) together with I12's exceptions, at hardware qualification |
| T15 | IOVAs run out: 1 GiB per device, and they are not reused | None. No driver maps per request yet | **Open** |

### Interrupts

| # | Threat | Defence | Status |
|---|---|---|---|
| T16 | A device writes to the interrupt range to raise any vector or delivery mode (compare Wojtczuk and Rutkowska, 2011) | Interrupt remapping: only an IRTE decides, with fixed delivery. Compatibility format stays blocked (`CFI` clear), which QEMU does not enforce | **Proven** for remapped messages; needs hardware for compatibility format |
| T17 | A device uses another device's IRTE | VT-d: every IRTE checks the source ID. AMD-Vi: a device reaches only its own table | **Proven** (controls C6, C7) |
| T18 | A forged source ID on an I/O APIC interrupt | The IRTE checks the DMAR's source ID, but QEMU's I/O APIC sends none, so the check cannot run. Under AMD-Vi the pins stay masked | Needs hardware |
| T19 | A device floods its own interrupt vectors | None: no rate limit | **Open** |
| T20 | A torn IRTE is read while being written | Present is written last on the normal path. Only the boot proofs rewrite a present entry | By design |

### Firmware and operation

| # | Threat | Defence | Status |
|---|---|---|---|
| T21 | A malformed DMAR or IVRS | Validated whole before use; mutation soaks | **Proven** (host) |
| T22 | Firmware over-reports capabilities | AMD-Vi requires both the register and the IVRS to agree. VT-d trusts `CAP` | Partly |
| T23 | **An attack goes unnoticed.** Faults are blocked and recorded, but `take_faults` is only called by the boot proofs, so a runtime fault is never read or logged (§69) | None | **Open** |
| T24 | A stuck unit stalls the kernel | Each invalidation waits at most 100 ms with interrupts off, then fails. A failed unmap never hands the page back, so it cannot be reused. An NVMe poll no longer holds the driver lock with interrupts off | Partly |
| T25 | A device reports BAR values that aim the kernel's own register writes at RAM, at ECAM or at an IOMMU's registers | A memory BAR is used only if non-zero, naturally aligned, at most 1 GiB and clear of RAM, of the platform's reserved ranges and of other BARs; `map_mmio` refuses RAM. A BAR moved after sizing is not noticed | **Proven** (host controls and one boot control) |
| T26 | **Firmware names "reserved" memory a device should not reach**: RAM the kernel allocates or reclaims, the IOMMU's own registers or another platform register range, the interrupt range, or more than the domain can address. An identity map would then hand a device that memory | `reserved::check`, once at bring-up, for every range. Every page must lie in memory the memory map calls Reserved, RuntimeCode or RuntimeData (VT-d §8.4 asks firmware for exactly that). The range must lie outside the interrupt range and, under AMD-Vi, outside the special range of 48882 Table 3. It must be clear of the IOMMUs', the APICs', the HPET's and every ECAM window's registers, inside the domain's address width, and at most 4 GiB long. If devices may write it, it must also be clear of every firmware table the kernel already holds as a Rust reference (the DMAR, the IVRS and the rest `vm::firmware_bytes` has lent; TASKS item 40's review, M1). A range that fails is reported by name and never mapped. Its devices are refused at attach. A device's ranges that ask for one page to be both mapped and never mapped refuse the device too, and so do ranges that add up to more than 4 GiB for one device. A device that shares an AMD-Vi alias with one attached first must find all of its ranges already mapped there. Memory a device may write is never handed out as a Rust reference, in either order: `vm` keeps lent and device-writable memory under one lock, `firmware_bytes` refuses memory a device may write, and a grant of writes over a lent table is refused; the proof reads such memory with volatile reads. No device's BAR may sit inside an accepted identity range (review L3) | **Proven** (host tests at every boundary, of both orders of lending and granting, and of a BAR inside an identity range; at boot, a range in RAM, one in the interrupt range and one over the table copy the kernel holds refused by name; controls that skip the memory-map check or the record of lent tables fail the boot) |

### T4 in detail: what remains

- **From ExitBootServices to the end of the clearing**, any device the firmware left bus-mastering can write all of memory. In QEMU that is about 18 ms after kernel entry (the loader's own time after ExitBootServices was not measured; most of the 18 ms is the boot report going to a serial port under TCG). The kernel cannot shorten this past the point where it can map configuration space; only the firmware can close it (pre-boot DMA protection, which is T5's problem), or a loader that clears bus mastering before ExitBootServices (rejected for now, ADR-0007 I12).
- **Against a device that ignores its Bus Master Enable** (hostile, or broken) clearing the bit does nothing: only a bridge above it, or the IOMMU, stops it. Translation is on 2.3–2.4 s after kernel entry in QEMU, before any application processor starts and before the loader's memory is reclaimed. Before it, such a device can still write memory, including the root table and queues the kernel builds in ordinary memory just before enabling translation. VT-d's Abort DMA Mode (`ECAP.ADMS`, Rev 5.20 §3.4.4) can enable remapping before those tables exist, but QEMU cannot prove it; bringing translation up earlier otherwise needs a time base the IOMMU code does not have before the RTC proof (ledger §7).
- A function that keeps Transactions Pending set past the 100 ms deadline is named in the report; its outstanding requests can only return data to the device itself.

**QEMU cannot show** a bridge's Bus Master Enable stopping traffic (the bridge's bit is not on the DMA path in QEMU's model) or a real Transactions Pending bit (it never reads set): ledger §3. The bit-level behaviour rests on the vendor datasheets and memory of the specification (ledger §2); **hardware qualification must check it.**

## What must be closed before real hardware

1. **T4** (the windows above), **T5 and T6.** Today confinement can be absent exactly when firmware or bring-up misbehaves, and is always absent before translation turns on.
2. **T8 and T9.** The per-device isolation claim is unproven on any machine with a PCIe switch.
3. **T14.** Identity maps exist for attached devices (TASKS item 40, 2026-11-12), so a USB or integrated-graphics driver can attach a device that needs one. Still open: firmware-owned devices with no driver, which I12 stops at enumeration. Their exceptions are a hardware-qualification decision (R1 (b)).
4. **T23.** Blocked attacks should at least be logged.

**Accepted for bench boots on rented servers only** (decision item 230, 2026-11-12): T4's two windows, T5, T6, T8, T9 and T23, by name. Such a server holds no device of an attacker's and no data of value. The acceptance does not reach a volunteer's machine, a release, or any claim about hardware, and T14 is not accepted: identity maps close it, and the deadline of items 8 and 12 holds for rented servers too (item 229). The plan is `docs/research/2026-11-12-first-bare-metal-boot.md`.

Before external devices are supported: T8, T9, and §32's external-device authorization (sheet item 9). Before third-party drivers: a capability-checked DMA interface, under its own ADR and threat model.

## References

- Markettos et al., *Thunderclap: Exploring Vulnerabilities in Operating System IOMMU Protection via DMA from Untrustworthy Peripherals*, NDSS 2019: <https://www.csl.sri.com/~neumann/ndss-iommu.pdf>
- Wojtczuk and Rutkowska, *Following the White Rabbit: Software Attacks Against Intel VT-d Technology*, 2011: <https://theinvisiblethings.blogspot.com/2011/05/following-white-rabbit-software-attacks.html>
- Markuze, Morrison, Tsafrir, *True IOMMU Protection from DMA Attacks: When Copy Is Faster Than Zero Copy*, ASPLOS 2016: <https://www.cs.tau.ac.il/%7Emad/publications/asplos2016-iommu.pdf>
- Peleg, Morrison, Serebrin, Tsafrir, *Utilizing the IOMMU Scalably*, USENIX ATC 2015: <https://www.usenix.org/conference/atc15/technical-session/presentation/peleg>
- On ACS and peer-to-peer isolation (secondary source; the PCIe specification was not read): <https://docs.redhat.com/en/documentation/red_hat_enterprise_linux/7/html/virtualization_deployment_and_administration_guide/sect-iommu-deep-dive>
