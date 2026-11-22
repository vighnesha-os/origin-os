# Research ledger: capacity and heat from the hardware (TASKS item 106)

| | |
|---|---|
| Date | 2026-11-21 (the reads); 2026-11-22 (the build and its measurements, §6) |
| For | TASKS item 106 (plan P15): ADR-0020 S9's sources 1 to 3 and S17's heat. Its contract and the coordinating session's rulings Q1 to Q15 of 2026-11-21 |
| Requires | CLAUDE.md §1, §3, §16, §17, §24, §27, §58, §67, §74; ADR-0009 Amendment 1; ADR-0013 R3; ADR-0018 V1 and V9; ADR-0019 A2, A3, A5, A6; ADR-0020 S9, S17, S23 |
| Method | Vendor documents and ACPI 6.6 only, read on 2026-11-21 between 22:23 and 22:39 UTC at today's revisions. Intel's and AMD's PDFs were piped from `curl` into `pypdf` in memory; AMD's processor programming references come as archives, whose PDFs were read the same way. ACPI 6.6 is the owner's local PDF, read with macOS PDFKit in memory; none of its text is copied here (CLAUDE.md §17). Every fact below is paraphrased, with section and PDF page. **No operating system's or other project's source code or documentation was read.** One web search for AMD's `_CPC` encoding returned only an operating system's documentation pages: none was opened, and nothing here rests on them. The kernel's own code was read on `land/batch-9` at `144e0cfa` and built on at `0ebef78b` |
| Follows | `docs/research/2026-11-20-m3-scheduling.md` (§2.3 to §2.9: the HFI, `_CPC`, HWP and CPPC as capacity sources), `docs/research/2026-11-21-scheduling-domains.md` (TASKS item 97) |

## 1. Sources

| ID | Document | Revision and date | What was read (PDF pages) | SHA-256 |
|---|---|---|---|---|
| I1 | Intel SDM Volume 1 (253665) | -093US, September 2026 | Table 21-16, pp. 462 and 463 | `f6ffc430dfa2dada4f791b1ecb462810e59827befe9a9c23f9160941eea642f9` |
| I3 | Intel SDM Volumes 3A to 3D (325384) | -093US, September 2026 | §13.4.8, §13.5.1, Table 13-6 (pp. 429 to 437, 462); §17.4 (pp. 534 to 547); §17.6 (pp. 553 to 563); §17.8 and §17.9 (pp. 564 to 576) | `77919be320f35da2782771ef2cb65a868cf68e62c8f8279f2ca61199cb70d1f8` |
| I4 | Intel SDM Volume 4 (335592) | -093US, September 2026 | Table 2-2: 19BH and 19CH (pp. 31 and 32); 1B1H and 1B2H (pp. 34 and 35); 770H to 777H (pp. 65 and 66); 17D0H to 17D2H (pp. 90 and 91) | `79f54b67b9bb3836856f04e0c45f50406c4d4b932c64800eb788b6b705a83dbd` |
| IFFH | Intel, *Intel Processor Vendor-Specific ACPI Interface Specification* (302223) | -007, December 2014, the newest at `cdrdv2-public.intel.com/772754` | all 13 pages | `6a5bd05f5eb848fd5fa897233ad709e7209ae34f50acc4e0542b3dae6bc4edaf` |
| A2 | AMD64 Architecture Programmer's Manual Volume 2 (24593) | 3.46, October 2026 | §16.4.4 (p. 735); §17.6 (pp. 767 to 773) | `e4130d5aa26227b6a116d75af9e0f6e5adb4c034ff0d49a3c0fe73c3f49794cf` |
| PPR70 | AMD PPR 57930, family 1Ah model 70h, A0 | 3.00, 30 April 2025 | the thermal LVT (pp. 57, 64, 182); Fn8000_0007 and 8000_0008 (pp. 100 and 101); the CPPC MSRs (pp. 234 and 235) | archive `6eb92cbcb64a41dc10d0ec3252b088768fc31e8c6a64907dc23e60ca707b0c52`, PDF `f16ccd11834e5b153a716fe8b7da4867344697403c7d35e4f499cabe6d9dee39` |
| PPRC0 | AMD PPR 58655, family 1Ah model C0h, A0 | 0.47, 6 November 2026 | volume 1: ThermalLvtEntry (p. 201), the CPPC MSRs (pp. 262 and 263) | archive `ff385a889d58e5be973e000fe20a108211b5cc1bde8260268ce8929a4fbd7fd6`, volume 1 `4a9286e93f1607729fbf4a892b43eebc70348928cef5b9781cf28c1c5ed01567` |
| ACPI66 | ACPI Specification 6.6 | May 2025, the owner's PDF | §5.6.6 Table 5.236 (p. 410); §6.2.12 Table 6.13 (pp. 474 and 475); §8.4.6 and §8.4.6.1 (pp. 639 to 646). The PDF page is the printed page plus 71 | `8c7542dd4de974ae47bba71bb0336637fe1e3838daad7692370ab4cf218efd35` |

Every hash matches the M3 ledger's of 2026-11-20 where that ledger has one. New here: IFFH and the two PPR archives' own hashes.

## 2. The HFI (I1 Table 21-16; I3 §17.6)

- **Discovery.** CPUID.06H:EAX bit 19 reports the HFI and bit 23 Thread Director; EAX bit 6 the package thermal registers. ECX\[15:8\] gives Thread Director's classes. EDX\[7:0\] are the table's capabilities and EDX\[11:8\] its size in pages less one, both in the package domain; EDX\[31:16\] is the reading processor's row, in the logical-processor domain. Intel says rows may be shared between processors and may leave gaps.
- **The layout follows CPUID, not the enable** (I3 §17.6.2, Table 17-7 and its notes 6 to 10). The header is an 8-byte timestamp and then each class's change indications; each row is every class's capabilities, rounded up to 8 bytes. Intel calls the HFI's own table the one-class, two-capability case of the same layout. When only configuration bit 0 is set, the hardware fills class 0 alone (§17.6.5). TASKS item 97 read the stride from CPUID, and this task keeps that reading: on a processor with Thread Director, setting bit 0 alone gives a table with Thread Director's stride whose other classes stay unwritten. **Not shown on hardware:** whether a part with Thread Director lays the table out so when only bit 0 is set. It needs ADR-0009 H4's machines.
- **Enabling** (§17.6.4, §17.6.5). The pointer register, 17D0H, takes the table's page address and a valid bit 0; the configuration register, 17D1H, bit 0 turns the HFI on, and bit 1 Thread Director, which also needs bit 0. Both are package-scoped. The pointer must be written before bit 0. Setting bit 1 where CPUID does not report Thread Director raises #GP. The kernel never sets it.
- **Notification** (§17.6.6). IA32_PACKAGE_THERM_STATUS bit 26 (R/WC0) says the hardware updated the table, and while it is set the hardware writes no further update; software clears it by writing 0. IA32_PACKAGE_THERM_INTERRUPT bit 25 enables an interrupt on each update, and its change from 0 to 1 makes the hardware send an initial notification with bit 26 set. Which processor of the package takes the interrupt is not stated (the M3 ledger's §7.2; still so in -093).
- **The rows' meaning** (§17.6.7 and Tables 17-5, 17-6, 17-8 and 17-9). A row whose class-0 cells are all zero asks that the processor be avoided; flag bit 1 with all-zero rows asks for idle to be forced; an efficiency of 255 asks for work to be consolidated there.
- **Disabling** (§17.6.5). Clearing bit 0 makes the hardware set bit 26 once it has stopped writing; only then may the memory be reclaimed. The kernel never disables the HFI at M3, so it never frees the table.
- **INIT and SIPI** (§17.6.4, §17.6.5, §17.6.9). The pointer and the configuration keep their values across INIT. GETSEC\[SENTER\] and GETSEC\[ENTERACCS\] reset them, which Origin never runs; INIT and the wait-for-SIPI state reset only the per-processor Thread Director enable, 17D4H, which M3 does not use. §17.6.9's last sentence still asks software to re-enable when it processes a SIPI. **This corrects ADR-0020 S9's migration note**, which says INIT and SIPI clear the enables (the coordinating session's ruling Q11: a dated note in S9). The kernel reads both registers back once every processor has started, and writes them again where they changed (the thermal round).

## 3. Heat (I3 §17.8.2.5, §17.8.4, §17.8.5, §17.9; I4 19BH, 19CH, 1B1H, 1B2H)

- **Intel.** CPUID.01H:EDX bit 22 says IA32_THERM_STATUS, IA32_THERM_INTERRUPT and the thermal LVT entry exist (§17.8.4). IA32_THERM_INTERRUPT bits 0 and 1 enable an interrupt at the high- and low-temperature transitions; IA32_THERM_STATUS bit 0 is read-only and says the core's PROCHOT# is active. Intel says the thermal monitor, once engaged, stays engaged at least about a millisecond (§17.8.2.5), which bounds how often one core can interrupt. The thermal interrupt is handled by the operating system or by SMM: the LVT entry's delivery mode says which. Fixed, SMI and NMI are its modes; INIT and ExtINT are not allowed.
- **IA32_PACKAGE_THERM_STATUS's bits** (§17.9, Figure 17-33): read-only 0, 2, 4, 6, 8, 10 and the readout 22:16; sticky R/WC0 logs 1, 3, 5, 7, 9 and 11; bit 26 the HFI's, R/WC0 (§17.6.6). The acknowledgement writes every log bit as 1, which leaves it, bit 26 as 0, the read-only bits as read and the reserved bits as 0.
- **AMD.** A2 §16.4.4 says the thermal LVT may not be implemented everywhere. PPR70 and PPRC0 document the LVT register (APIC 330h, x2APIC MSR 833h), but no thermal status or thermal event the operating system enables. **AMD heat is not supported at M3.**
- **ACPI's 0x83 and 0x84** (ACPI66 Table 5.236, p. 410): Guaranteed and Minimum Excursion changes, notified through the SCI. Not built (ruling Q9; ADR-0019 area C).

## 4. HWP and AMD's CPPC (I3 §17.4; A2 §17.6)

- **Intel** (§17.4.2, p. 535). Apart from IA32_HWP_INTERRUPT and MSR_PPERF, HWP's registers may be accessed only once HWP is enabled. IA32_PM_ENABLE is write-once until reset, package-wide, and makes the processor ignore IA32_PERF_CTL. So the kernel reads IA32_PM_ENABLE, and reads IA32_HWP_CAPABILITIES' Highest (bits 7:0) only where the firmware already set it (ruling Q2).
- **HWP's notification** (§17.4.5, §17.4.6, pp. 541 and 542). IA32_HWP_INTERRUPT bit 2 asks for an interrupt through the thermal LVT when Highest changes; CPUID.06H:EAX bits 8 and 15 report it. IA32_HWP_STATUS bit 3 then says Highest changed, and Intel says software must clear it by writing 0, so that the next change interrupts; its other R/WC0 bits are 0, 2, 4 and 5, and bit 1 is reserved. Changes come at most once a second. **The contract's list of writes did not name this clearing**: it is the necessary half of the Highest-change interrupt the contract set, and the build writes it (bit 3 alone; the hand-over says so).
- **AMD** (A2 §17.6.3, §17.6.4, pp. 768 and 769; PPR70 pp. 234 and 235; PPRC0 pp. 262 and 263). CPPC_CAPABILITY_1 (C001_02B0) and CPPC_CAPABILITY_2 (C001_02B2) read zero until CPPC is enabled. CPPC_ENABLE (C001_02B1) is write-once, enables CPPC for the whole package and makes the processor ignore P-state control; the PPRs give it as readable. CPUID Fn8000_0008_EBX bit 27 reports CPPC. So the kernel reads HighestPerf (bits 31:24) only where CPPC_ENABLE already reads 1. AMD documents no change notification for it.

## 5. `_CPC` (ACPI66 §8.4.6.1; IFFH)

- **Revision 4 has 25 entries** (Table 8.23, pp. 640 to 643); Highest Performance is entry 2, an Integer or a buffer holding one Register(); the NULL register marks an optional entry unsupported, and Highest is not optional. Revisions 2 and 3 are not in 6.6, and uefi.org refused `curl` for the 6.5 HTML and the 6.2 and 6.3 PDFs on 2026-11-21: TASKS item 97's ruling Q10 stands (only NumEntries against the length, and Highest's place).
- **Notify 0x85** (p. 644; Table 5.236, p. 410): the operating system reads Highest again only where it is a register; `_CPC` itself is not evaluated again.
- **FFH** (IFFH): Intel defines functional fixed hardware for `_PCT`, `_CST` and `_PTC` only, not for `_CPC`; no AMD document read defines one. IFFH's `_PDC`/`_OSC` bit 12 asks for native handling of CPPC notifications. ACPI66 Table 6.13 (pp. 474 and 475) gives `_OSC`'s platform-wide bits 5 (CPPC), 6 (CPPC2), 12 (Diverse Highest), 14 (Flexible Address Space) and 16 (Multiple PCC).
- **ADR-0019 A2 keeps the firmware's AML out of the kernel** (ruling Q1 (a)). `_CPC` is evaluated only in the AML proof, on QEMU's tables, and counted (`framekernel::cpc`); capacity's source 2 is empty at boot on every machine. **Limitation:** an AMD machine without the HFI, whose firmware did not enable CPPC, has equal capacity at M3. That includes Gorgon Point's Zen 5 and Zen 5c cores. The alternative, an owner item letting the kernel evaluate `_CPC` alone at boot (Q1 (b)), waits for an in-scope machine that could show what it changes.

## 6. Measured (QEMU 11.1.3 on the development Mac, 2026-11-22)

- **M1, QEMU's software processor** (`-accel tcg -cpu max`, started paused with `-S` and asked through QMP's `query-cpu-model-expansion`, with the default vendor, AMD's, and with `vendor=GenuineIntel`). Both report `acpi` (CPUID.01H:EDX\[22\]) and `arat` (CPUID.06H:EAX\[2\]), and neither `tm` (EDX\[29\]), `tm2`, the digital thermal sensor, the HFI, HWP nor AMD's CPPC. The first boot (`boot-test`, 03:16 UTC) showed the kernel the same: the thermal LVT entry, x2APIC MSR 833H, reads back masked, and is free; IA32_THERM_INTERRUPT reads 0 and drops the bits written to it, without #GP.
- **What that changed.** The contract set heat's enables wherever EDX\[22\] is set, and the first boot's proof failed on the two enables that did not read back. Intel's text puts the interrupt's source in a sensor, the thermal monitor (TM1, EDX\[29\]) or the digital thermal sensor (CPUID.06H:EAX\[0\]) (I3 §17.8.2.5, §17.8.4, §17.8.5.1), so heat now needs the registers and a sensor, and the LVT is taken only where a source would use it (the coordinating session's ruling of 2026-11-22, narrowing its Q12). Under QEMU nothing is written, and the proof says heat is not supported for want of a sensor.
- **M2, `_CPC` in QEMU's tables:** none. The AML proof's count read "0 of 2 processors" on the two-processor machine.
- **The x2APIC's reserved addresses** (SDM Vol. 3 §13.12.1.2 names 80EH and 831H): QEMU's software processor reads 831H without a fault, and raises #GP at 80EH, which the MSR probe recovers. So the proof's self-test reads 80EH, and shows the probe's #GP path on every QEMU boot.
- **The handler's work on 65 processors, modelled on the host** (CL51; `hfi::tests::handler_model`, the development Mac): one package of 65 members, its rows read, the rescale over 65 processors and their publication, then the acknowledgement; the MSR accesses free in the model. In a release build, p50 333 ns and p99 375 ns, with a largest of 17.7 µs in one of 2,000 runs (a debug build: p50 13.6 µs). A processor's figure is owed.

## 7. Inconsistencies in Intel's volumes

The kernel follows Volume 3.

- Volume 4 lists only bits 0 and 1 of IA32_HWP_INTERRUPT; Volume 3 adds bits 2 and 3.
- Volume 4 gives IA32_PACKAGE_THERM_STATUS bit 26 no access type; Volume 3 gives it as R/WC0.
- Volume 4 gives bit 1 of both thermal status registers as R/W; Volume 3 gives it as R/WC0.
- Volume 4 still lists only bit 0 of IA32_HW_FEEDBACK_CONFIG (the M3 ledger's note of 2026-11-20).

## 8. Open, for ADR-0009 H4's machines

- Every present path: the HFI's enable, its first update and later ones, "avoid", forced idle, efficiency 255; which processor of a package takes its interrupt.
- Whether a part with Thread Director lays the table out by CPUID's classes when only bit 0 is set (§2).
- Whether any firmware enables the HFI itself. The kernel leaves an HFI the firmware enabled as it is and does not use it, since it cannot know whose memory the pointer names; if firmware memory the kernel later reclaims were named there, the processor would write into it. No document read says firmware does this.
- Thermal transitions and the throttled flag; whether firmware holds the thermal LVT for SMM.
- HWP's and CPPC's capability reads where firmware enabled them, and HWP's Highest-change interrupt.
- `_CPC` on any firmware but QEMU's.
