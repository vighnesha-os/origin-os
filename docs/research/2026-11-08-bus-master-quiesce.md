# Research ledger: quiescing bus mastering before the IOMMU (threat T4)

| | |
|---|---|
| Date | 2026-11-08 |
| For | `framekernel::pci` (the host-tested bus-mastering helpers), the kernel's `pcie` module (`quiesce`, `settle`, the bus-mastering checks), `dma::DeviceDma::enable_bus_master`, the boot order in `main.rs`, and ADR-0007 I12 (**Proposed**) |
| Requires | CLAUDE.md §16 (research first), §31 (DMA mapping), §32 (PCIe enumeration, DMA), §58 (DMA protection), §72 (fault injection, kernel boot tests), §74 (QEMU is not hardware) |
| Method | Specification first, where one could be read. **The PCI Express Base Specification is not public**, and it was not read: §2 says exactly which facts could be confirmed elsewhere and which rest on memory. QEMU's device model was read in its source, because it is the device under test. OVMF was measured from inside the boot, and QEMU's reset state through its monitor. No operating system's PCI or IOMMU driver was read or copied (CLAUDE.md §3); two Linux mailing-list threads were read for their reasoning only (L1, L2). |

## 1. Sources

| ID | Source | What it establishes |
|---|---|---|
| P1 | PCI Express Base Specification (current releases 6.4 and 7.0 per CLAUDE.md §17). **Not read.** | Everything marked "(recalled)" in §2 is the author's memory of the text; section numbers are not cited because they could not be checked. |
| S1 | Intel, *13th Generation Intel Core Processor Datasheet, Volume 2 of 2*, DEVICECTL_DEVICESTS register (Transaction Pending field) | Transactions Pending covers Non-Posted Requests that have not completed; it reads clear only once all have completed or been ended by Completion Timeout; an FLR clears it. Second public wording of the same register behaviour as P1. |
| S2 | Intel, *12th Generation Intel Core Processor Datasheet, Volume 2 of 2*: device status register (bit 5, Transactions Pending) and COMMAND_STATUS | The same bit position and wording as S1; on the Command register, Bus Master Enable controls a function's memory and I/O requests and a Root or Switch Port's forwarding of them upstream. |
| S3 | Intel, *Core Ultra Processors for H-series and U-series Platforms, SOC I/O Registers*, PCI Command | A port's Bus Master Enable at 0 makes the device's cycles abort; and the Received Master Abort status records it. |
| S4 | Microsoft, `PCI_EXPRESS_DEVICE_STATUS_REGISTER` (WDK) | The same clearing rule for that status bit as S1 (the field is named differently there). |
| S5 | `linux-support` Rust crate, `pci_express/configuration_space/Command.rs` (docs.rs), citing PCI Express Base 3.0 §7.5.1.1 | Third-party restatement of the Command register: an endpoint with Bus Master Enable clear may issue no memory or I/O requests, MSI and MSI-X being in-band memory writes stop with it; at a Root or Switch Port, upstream memory and I/O requests are Unsupported Requests while it is clear, and non-posted ones get a UR completion. |
| S6 | Intel, FPGA IP guides (Stratix V, Arria 10 Avalon-ST) on the completion-pending input and on Completion Timeout | Transactions Pending reflects outstanding completions in real IP; the four programmable timeout ranges (A 50 µs–10 ms, B 10–250 ms, C 250 ms–4 s, D 4–64 s) and a mandatory default range of 50 µs to 50 ms. |
| S7 | Intel 82575 datasheet (register listing) | Device Control 2 encodes the default range and has a Completion Timeout Disable bit; it recommends a timeout not expiring in under 10 ms. Old part, but public text. |
| E1 | edk2: "MdeModulePkg/PciBus: Revert 'disable all BME when entering RT'" (R. Ni, 2017, patchew) | PciBusDxe once cleared Bus Master Enable on the whole tree at ExitBootServices; that was reverted for a Windows S4-resume problem. So the firmware does not promise it. |
| E2 | edk2: "MdeModulePkg/AtaAtapiPassThru: disable the device at ExitBootServices()" (L. Ersek, 2017), with a reply quoting the UEFI Driver Writer's Guide | Individual UEFI drivers are expected to stop their own DMA at ExitBootServices (the guide names disabling the PCI bus master as the typical action); nothing guarantees it for every device. |
| L1 | Linux EFI-stub patch "Allow disabling PCI busmastering on bridges during boot" (Matthew Garrett, December 2019) | Reasoning only: a window exists between firmware teardown and the OS's IOMMU, and clearing a bridge's bus master bit covers everything behind it; poorly behaved devices may need testing. |
| L2 | The replies to L1 (December 2019, including L. Ersek's comments) | Reasoning only: running devices with live descriptor rings should not lose their bus mastering without warning, and an option ROM's driver can turn it back on before the OS takes over. |
| Q1 | QEMU 11.1.0, `hw/pci/pci.c`: `pci_default_write_config`, `pci_set_master`, `pci_init_bus_master`; `hw/pci/pcie.c`: `pcie_cap_init`, `pcie_cap_deverr_init` | QEMU's model: §3. |
| V8 | Intel, *Virtualization Technology for Directed I/O Architecture Specification*, Rev 5.20 (D51397-019), §3.4.4 "Abort DMA Mode", §6.5 (256-bit invalidation descriptors), ECAP_REG.ADMS (bit 52), RTADDR_REG.TTM. Pointed out by the VT-d session; **read for this ledger** from the copy the owner approved downloading (kept outside the repository) | §7: enabling remapping before any translation table exists. |
| M | Measured on the M1 Max, QEMU 11.1.3 (TCG), OVMF as shipped with QEMU (EDK II, UEFI 2.7 system table) | §4 and §6. |

**Gaps.** The specification itself. No public text states "a completion must not pass a posted request" in terms this repository can cite; the transaction-ordering claim below is therefore from memory of the ordering table and from the general rule that a read flushes posted writes (mailing-list and vendor material only). The UEFI specification's wording on ExitBootServices and DMA was not read either (only E2's quotation of the Driver Writer's Guide). **Before hardware qualification, check all of §2 against the specification text.**

## 2. What clearing Bus Master Enable does

**What a function may do after the write (confirmed: S2, S3, S5):** an endpoint must not issue new memory or I/O requests, and MSI and MSI-X messages stop too, because they are memory writes. At a Root Port or a Switch Port the bit gates forwarding: upstream memory and I/O requests are refused as Unsupported Requests (completions, which travel the other way, are not gated). So clearing the bit on a bridge stops everything behind it, whether or not the device behind it obeys.

**Requests already in flight:**

- *Non-posted requests the function issued before the write* (memory reads) finish normally. Their completions are not gated. Transactions Pending reports them, and clears only once they completed or were ended by the function's Completion Timeout (S1, S2, S4). A completion delivers data **to the device**, so an outstanding read cannot write system memory. Waiting for it is about knowing the device has finished, not about stopping a write.
- *Posted writes the function issued before the write* are not counted by Transactions Pending (S1: non-posted only). They may still reach memory after the Command write completes. A read back from the same function is the way to wait for them: a read completion travelling upstream must not pass earlier posted requests from the same source (recalled; §1 gaps), with no Relaxed Ordering escape because a configuration read's completion is strongly ordered (recalled). So the kernel always reads the Command register back after the write (`pci::clear_bus_master`), which is both the proof that the write took and the flush. **The ordering half of this is from memory and is listed for hardware checking.**
- *A function that ignores the bit* is a hostile or broken function. Bus Master Enable is advisory to the device; only a bridge above it, or the IOMMU, enforces anything. See §5.

**How long Transactions Pending can stay set:** until the Completion Timeout expires, and that is programmable (S6, S7): the default range ends at 50 ms, the longest range reaches 64 s, and the timeout can be disabled. The kernel does not wait for that: it waits 100 ms once, for all functions together (`pcie::SETTLE_MS`), so a device cannot stretch the boot by holding the bit, and reports what is still pending. A pending completion cannot touch memory (above), and the wait is a report, not a safety condition.

## 3. What QEMU 11.1 models (Q1)

- A write to the Command register toggles the function's `bus_master_enable_region` (`pci_default_write_config` → `pci_set_master`); DMA and MSI through `bus_master_as` follow it. So clearing the bit on an emulated device really stops its DMA (control C3, §6).
- **A bridge's Bus Master Enable is not modelled for devices behind it:** `pci_init_bus_master` aliases only the device's own region, and nothing on the DMA path looks at a parent bridge's command register. The `boot-bridge` proof therefore shows that the kernel clears and re-opens the bridge's bit (read back), not that a bridge's bit stops traffic.
- **Transactions Pending is never set.** `pcie.c` does not touch the Device Status register except the error bits, and no Completion Timeout is emulated. The wait can only be exercised on the host (`pci::tests`) and by forcing the bit (control C7).
- Reset value of the Command register: 0 on every function QEMU creates here (§4), so nothing masters the bus until firmware says so.

## 4. What OVMF under QEMU actually leaves enabled (M)

Read by the kernel at hand-off, before it changes anything, on q35 with OVMF:

| Machine | Functions | Bus Master Enable left on | Read back clear after the quiesce |
|---|---|---|---|
| boot-test / boot-matrix (VT-d) | 7 | **7 of 7**: 00:00.0 host bridge, 00:01.0 VGA, 00:02.0 NVMe, 00:03.0 virtio-net, 00:1f.0 ISA bridge, 00:1f.2 SATA, 00:1f.3 SMBus | 7 of 7 |
| boot-test (AMD-Vi) | 8 | the same 7, and 00:05.0 (the IOMMU's own function), set too | 7 of 7 (the IOMMU's function left alone) |
| boot-bridge (root port at 00:04.0, NIC behind it at 01:00.0) | 8 | 8 of 8, the bridge included | 8 of 8 |

**The firmware set every one of those bits.** With QEMU started with `-S` (no firmware instruction run), reading the Command register through 0xCF8/0xCFC from the monitor gives 0 in the low 16 bits for all seven functions (0x00000000 for 00:00.0, 00:01.0, 00:1f.0, 00:1f.3 and status-only for 00:02.0, 00:03.0, 00:1f.2).

So OVMF leaves bus mastering on for **every** function here, including devices with no DMA (the host bridge, SMBus). The earlier handoff's lesson ("OVMF had already set bus mastering, so a control that only stopped setting it proved nothing") understated this. What is established is what it leaves on; **why** could not be taken from its source with confidence: the PciBusDxe code that reads the capability bits and programs the Command register was not read conclusively, and E1/E2 only show that clearing it at ExitBootServices is not done by the bus driver and is left to each device's driver. E2's hook disables the SATA controller's bus mastering at ExitBootServices, yet 00:1f.2 still read set here: not investigated (this machine has no SATA disk).

**The consequence for real firmware:** a real platform's firmware may leave a different set. The kernel does not rely on any particular set: it clears whatever is set, and reports the count.

## 5. Decisions (ADR-0007 I12, Proposed; alternatives there)

1. **Every function on every ECAM window** has Bus Master Enable cleared, as early as the kernel can reach configuration space, bridges included (a bridge's bit is enforced by the bridge, §2). The walk is in bus order, so a bridge is cleared before the functions behind it.
2. **Read back, always** (§2), and report per function.
3. **Transactions Pending** is waited for once, 100 ms at most over all functions, after the TSC is calibrated; a function still pending is named in the report.
4. **Drivers set it again only through `DeviceDma::enable_bus_master`**, which checks that the IOMMU has the function attached (or that no IOMMU translates), refuses if the bit is already on, and opens every bridge above the function, nearest the root first (§2). `pcie::enable` is gone: nothing else in the kernel can set the bit.
5. **The IOMMU's own PCI function** (AMD-Vi; named by the IVRS, never by class code) is not touched. It is the hardware that enforces domains, and AMD's specification (48882 Rev 3.11) says nothing of its Bus Master Enable (no occurrence of "bus master" in its text); whether its table walks need that bit could not be established. **Check on real AMD hardware.**
6. **Translation earlier:** §7.

## 6. Local experiments (M1 Max, QEMU 11.1.3, TCG)

- **Timing of the quiesce:** the clearing walks all 256 buses of the ECAM window; it took 6.6–7.4 ms of boot time in most runs and 66–69 ms when the host was saturated by a parallel `yes` stress run. Transactions Pending: 124–159 µs of waiting (the time to read every capability) — nothing pending.
- **The window** (§7): measured from kernel entry to the end of the clearing, 17.9–19.3 ms on the idle host.
- **Controls** (each makes the boot fail; `docs/engineering/SOURCE_PROVENANCE.md` has the outcome strings): C1 the Command write skipped (the clear did nothing) → the read-back line fails; C2 one function's bit set again after the quiesce → "off on all … false"; C3 NVMe never enables bus mastering → an admin command times out (the clear stops the device in QEMU); C4 NVMe enables it before attaching → refused; C5 a stray function enabled through the raw path → the census names it; C6 the bridge not opened above the attached NIC → the census names it; C7 Transactions Pending forced on → reported, boot fails; C8 the device's ACCESS_PLATFORM removed (`iommu_platform=off`) → the driver refuses DMA; C9 the IOMMU's function's bit changed → the census names it.

## 7. Bringing the IOMMU up earlier (evaluated)

**Done:** `iommu::init` moved from after `pcie::init` (after all processors, the scheduler proofs and memory reclaim) to immediately before `smp::start`. Translation is on before any application processor runs and before the loader's memory is reclaimed; drivers still attach after enumeration, as before.

**Why not earlier still:** `iommu::init` needs three things that come later than the quiesce:
- the clock: `iommu::poll` (and the AMD-Vi driver, which uses it) bounds every register and invalidation wait with `time::monotonic_nanos`, which reads 0 until `time::init` publishes the TSC scale. Publishing is once-only (`clock::SharedScale`), and the rate is chosen with the RTC second (about 1 s, up to 2 s of boot); moving the IOMMU before it would need a second time base inside `iommu`, or making `time::init`'s publish refinable, which is changing the clock's contract (out of scope and not "simple").
- the compatibility-format I/O APIC proof (`irq::prove`): the IOMMU blocks compatibility-format interrupts once interrupt remapping is on, on hardware (QEMU does not enforce it), so that proof must run first. It takes about 200 ms.
- the AMD-Vi driver needs configuration space (its capability block), which the quiesce already provides.

**Measured result (boot-matrix host, idle):** translation on 2.3–2.4 s after kernel entry (`origin-kernel: IOMMU: translating …`), of which about 1.8 s is `time::init`'s RTC proof and 0.5 s the APIC timer proof. Bus mastering is cleared 18 ms after entry.

**Abort DMA Mode (V8), the way to do better on hardware that has it.** With `ECAP.ADMS` set, programming `RTADDR_REG.TTM` to 11b and issuing SRTP makes the unit abort all DMA as if no root table were present, so remapping can be enabled *before* any translation table is built, which the specification recommends: abort-dma mode, enable, build the tables, then a second SRTP into the operating mode. That would bring translation forward from "after the clock proofs" to about where the bus-master clear is, and would close the small gap that the kernel builds its root table and queues in ordinary memory before translation is on (only bus mastering being off protects them meanwhile, and only against a compliant device). Not done here: QEMU 11.1's ECAP (0xf00f5a) does not report ADMS, so it cannot be proven (CLAUDE.md §74); the mode needs the invalidation queue to use 256-bit descriptors (§6.5), a path the kernel does not have; the unit's register handshakes are bounded by the clock, as above; and the specification warns against it while a device that needs an RMRR is active. **A candidate for the first VT-d hardware bring-up**, alongside T5. The AMD-Vi equivalent was not researched.

## 8. The window that remains

From ExitBootServices to the end of the clearing, a device the firmware left bus-mastering can write any physical memory. That window cannot be closed by the kernel alone:
- The loader runs after ExitBootServices with no measurement of its own (the kernel entry is a few instructions later); the kernel then needs its page tables and heap, and reaches the clearing about 18 ms after entry (mostly serial output of the boot report under TCG; not qualified on hardware).
- A device that ignores its cleared Bus Master Enable (hostile) is stopped only by the bridge above it, if any, and always by the IOMMU, from translation on (2.3–2.4 s after entry here).
- Closing it would take firmware support (pre-boot DMA protection: the firmware keeps its IOMMU on across ExitBootServices, then T5's takeover) or a loader that clears bus mastering before ExitBootServices (E1 and L2 show why that is contentious).

## 9. Risks to check on hardware (CLAUDE.md §74)

- All of §2 against the specification text.
- That the AMD IOMMU's PCI function needs its bit neither cleared nor set (§5 item 5).
- Firmware-owned controllers: USB legacy emulation (SMM on an xHCI controller), and scan-out from a firmware framebuffer, depend on bus mastering. No kernel driver uses them yet; the first driver for the firmware framebuffer (§36) or for USB must attach and re-enable, or be excepted. Otherwise clearing the bit may blank the display or hang an SMI handler.
- Devices that break when bus mastering stops under load (L1): QEMU cannot show this.
- Non-compliant functions whose bit reads set after the write: reported, and the boot proof fails.
