# Research ledger: speculation controls and processor hardening on the declared processors

| | |
|---|---|
| Date | 2026-11-10 |
| For | `docs/adr/ADR-0017-speculation-processor-hardening.md` (the owner accepted it on 2026-11-10, after this ledger was written); `docs/engineering/THREAT_MODEL_SYSTEM.md`; row E12 of `docs/engineering/THREAT_MODEL_SYSCALLS.md`; TASKS item 19 (decision item 112) |
| Requires | CLAUDE.md §3 (original source), §9 (verify the compiler locally), §14, §16 (research first, at today's revision), §24, §58, §74 and rule 30 (QEMU is not hardware) |
| Method | Intel's and AMD's own documents only, read on 2026-11-10 from their own servers. Each PDF was fetched into memory, hashed and searched there; none was saved (decision items 18 to 20 and 23). Web pages were read in a browser. No operating system's source was opened (§3), and no secondary source is used below. Section 8 says how, and what went wrong. |
| Scope | The processors CLAUDE.md's header and ADR-0009 Amendment 1 declare: Intel generations launched in 2026 or later, and AMD generations launched in 2026 or later with Zen 5 or newer cores |
| Evidence | Documents only. **No processor was run.** What a document says a processor reports is not what a machine was seen to report (§74) |

## 1. The question

Before a program that is not a boot proof runs in ring 3, the kernel needs a decision on speculative execution and on the processors' own defences (decision item 112). That decision needs facts:

- what `IA32_ARCH_CAPABILITIES` says, bit by bit, and what each bit means on a processor in scope;
- what enhanced IBRS, automatic IBRS, IBPB and STIBP each promise, and what they leave to software;
- which branch-history and return-predictor issues still apply to those processors;
- what FRED, LASS and supervisor shadow stacks are, and which processors have them;
- what the vendors promise about the instructions an index-masking primitive would use;
- what the pinned compiler can do.

## 2. Sources

Sizes and SHA-256 values are of the bytes the vendor's server sent on the day.

### Intel

| ID | Document | Revision and date | Read |
|---|---|---|---|
| I1 | SDM Volume 1, order 253665-093 | September 2026 (file dated 2026-09-17). 3,656,572 bytes, `f6ffc430dfa2dada4f791b1ecb462810e59827befe9a9c23f9160941eea642f9` | Chapter 18 (CET): §18.1, §18.1.3, §18.2 to §18.2.3, §18.2.6, §18.3.8. Chapter 21: the CPUID leaf 07H tables |
| I2 | SDM Volume 3 (3A to 3D), order 325384-093 | September 2026. 9,589,981 bytes, `77919be320f35da2782771ef2cb65a868cf68e62c8f8279f2ca61199cb70d1f8` | §4.3 (LASS); §7.7 (NMI); §7.12.1.1; §7.14.5 (IST); Chapter 8 (FRED) §8.1 to §8.3.4 and §8.4 to §8.5.5; §12.7.2; §34.8 |
| I3 | SDM Volume 4, order 335592-093 | September 2026. 3,008,109 bytes, `79f54b67b9bb3836856f04e0c45f50406c4d4b932c64800eb788b6b705a83dbd` | Table 2-1 (first rows); Table 2-2 for MSRs 48H, 49H, 10AH, 10BH and part of 123H; Table 2-52 for BFH |
| I4 | SDM Volume 2 (2A to 2D), order 325383-093 | September 2026. 11,294,447 bytes, `d137a7887787bbb06c1a8b8bafd51958d8d2e16b4dff987a03226c845cca1516` | Only to find that the CPUID leaf tables now live in Volume 1, Chapter 21 |
| I5 | "CPUID Enumeration and Architectural MSRs", ID 758381 | Version 8.0, updated 2026-10-22 | The whole page |
| I6 | "Speculative Execution Side Channel Mitigations", ID 758363 | Revision 2.0, updated 2021-05-26 (first published May 2018) | §2.3.2 to §2.4.3, §3, §4.1.2, the footnotes |
| I7 | "Branch History Injection and Intra-mode Branch Target Injection", ID 824191 | Version 2.6, updated 2025-05-12 | Key takeaways, introduction, background, mitigation recommendations, the indirect branch predictor controls, CET-IBT, the guidelines for operating systems, IBHF, enumeration, affected processors, branch history and barriers. **Not read:** retpoline, the software clearing sequences, the sections for hypervisors |
| I8 | "Return Stack Buffer Underflow", ID 737306 | Updated 2022-07-12 | All but the last few hundred characters |
| I9 | "Affected Processors: Guidance for Security Issues on Intel Processors" (the consolidated table) | Page last modified 2026-11-02 | The three tabs' column headings; the rows for 06_CCH, 06_D5H and 06_DDH; the key; the footnotes |
| I10 | Intel Security Center, the advisory list, and INTEL-SA-01423 | List read 2026-11-10, newest entries 2026-08-11. INTEL-SA-01423 revision 1.0, 2026-08-11 | The titles of the 2025 and 2026 processor advisories; INTEL-SA-01423 whole |
| I11 | Software Security Guidance, "Advisory Guidance" index | Read 2026-11-10; newest entry 2025-12-09 | The list |
| I12 | "Analyzing Potential Bounds Check Bypass Vulnerabilities", ID 758378 | Version 2.0, updated 2018-06-18 | The paragraphs on masking instructions only |
| I13 | "Speculative Behavior of SWAPGS and Segment Registers", ID 758371 | Updated 2019-08-06 | The mitigation paragraphs |
| I14 | "Data Operand Independent Timing ISA Guidance", ID 768873 | Updated 2025-09-10 | The paragraphs on the mode, its enumeration and Intel's recommendation |
| I15 | Architecture Instruction Set Extensions and Future Features Programming Reference, 319433-062 | June 2026. 8,076,724 bytes, `1e38ce00d182ce4b8d82e9153f12668518f876dbaaad9d59938b472f5d4bfc0a` | Table 1-1 (its last rows) and Table 1-2 |
| I16 | Flexible Return and Event Delivery (FRED) Specification, 346446-007US | Revision 6.1, December 2023. 2,155,436 bytes, `44d3409ddf43c37331951a20d32c36faef5f70443e7eaf18878f3b3682411479` | Title page and contents only. I15 still points to it; the SDM's Chapter 8 (I2) is the newer text and is the one used |

### AMD

| ID | Document | Revision and date | Read |
|---|---|---|---|
| A1 | AMD64 APM Volume 2, publication 24593 | Rev. 3.46, October 2026. 15,813,681 bytes, `e4130d5aa26227b6a116d75af9e0f6e5adb4c034ff0d49a3c0fe73c3f49794cf`. The owner's local copy, kept outside the repository, has the same size and hash as AMD's copy fetched today | §3.1.7 (EFER.AIBRSE); §3.2.9; §8.2.3; §8.9.4 (its opening); §10.3.1 and §10.3.3; §18 to §18.1.1; §18.6.2 to §18.6.5; §18.9; the revision history for 3.46 and 3.45; every mention of FRED |
| A2 | AMD64 APM Volume 3, publication 24594 | Rev. 3.39, October 2026. 16,226,519 bytes, `b598514b19f204d973040d9867a7ffc48c949009a8b3dc86b20c9188e721ee45` (the hash decision item 23 recorded) | The revision history for 3.39 and 3.38; CPUID Fn0000_0007 (ECX_x0, EDX_x0, EAX_x1), Fn8000_0008_EBX, Fn8000_0021 (EAX, EBX, ECX) |
| A3 | White paper "Software Techniques for Managing Speculation on AMD Processors"; AMD's portal lists it as publication 70628 | Revision 5.09.23 (2023-05-09). 1,001,827 bytes, `d86742c1bf84c7c9a543e014a42152d525bd3cda05eb7f67b453fe5df72036a6` | All nine pages. Pages 5 to 7 hold their text as pictures and were read as pictures |
| A4 | Processor Programming Reference for AMD Family 1Ah Model 51h, Revision B0, publication 58264 (the portal's title says Model 50h) | Rev 0.45, 2026-09-23, Volume 1 of 6. Archive 6,205,201 bytes, `636c6d20e2f412db2faea57662fb364be93c325aa34631664f123188089079d1`; volume 1 `89972546d8f47fb86c8d79916e4e7f302ef34edf22a95ca50d8ea26035c08029` | CPUID Fn0000_0007 (ECX_x00, EDX_x00, EAX_x01), Fn8000_0008_EBX, Fn8000_0021 (EAX, EBX, ECX); the FRED MSRs |
| A5 | Processor Programming Reference for AMD Family 1Ah Model 02h, Revision C1, publication 57238 (the portal: Models 00h to 0Fh) | Rev 0.49, 2026-03-06, Volume 1 of 7. Archive 6,186,003 bytes, `e91d9fe3ae32df3d6c88012cdac261440cc06e089a0d83d191247a06c8c91e99`; volume 1 `e4a55e3db7863ff0d70abb5d01d67a133fa3c6ba35899a66a9f4a7d9ce45c0a4` | The same registers |
| A6 | Revision Guide for AMD Family 1Ah Models 00h-0Fh Processors, publication 58251 | Rev. 1.30, July 2025. 1,016,975 bytes, `0a6161202dfe5b15d37f4a4f031bf1c9fa3d8366467033e18c10e06177336220` | Searched, not read through: erratum 1464, and the titles of 1617 and 1621 |
| A7 | AMD Product Security, the bulletin list and bulletins | List read 2026-11-10: 57 bulletins numbered 7xxx, the newest dated 2026-11-04 | Whole, or their summary and affected products: AMD-SB-7026, 7030, 7033, 7034, 7046, 7051, 7052, 7053, 7055, 7060, 7061, 7062, 7068, 7069. In part: 7005 and 7029 |
| A8 | AMD Technical Information Portal, its search | Read 2026-11-10 | The current revisions: 24593 is 3.46 (2026-10-28), 24594 is 3.39 (2026-10-28), 40332 is 4.11 (2026-10-29), 24592 is 3.25 (2026-05-28) |

### The compiler

| ID | Source | Read |
|---|---|---|
| R1 | `rustc 1.99.1 (58cf8f9c3 2026-10-13)`, LLVM 23.1.1, on the M1 Max host, for the target `x86_64-unknown-none` | `rustc -C help`, `rustc --print target-features`, and one probe file compiled five ways (§3.12) |

## 3. Findings

### 3.1 `IA32_ARCH_CAPABILITIES`, bit by bit

The register is MSR 10AH, read-only, present when CPUID.07H.00H:EDX[29] is 1 (I3 Table 2-2; I5). AMD does not have it: bits 31:29 of AMD's CPUID Fn0000_0007_EDX_x0 are reserved (A2), and Model 51h reports them as 0 (A4). AMD's own enumeration is in §3.2.

"Intel's table" below is I9, for the three 2026 generations it lists: Panther Lake (06_CCH, steppings 2 and 3), Wildcat Lake (06_D5H, stepping 1) and Clearwater Forest (06_DDH, stepping 1). Its key says that "Not Affected" covers processors that are not affected or are mitigated in hardware, and that such a processor may not report the matching enumeration bit. So the table says what applies; only the processor says which bits it sets.

| Bit | Name | What Intel says it means (I3, I5) | The 2026 generations, by Intel's table | What follows for a kernel |
|---|---|---|---|---|
| 0 | RDCL_NO | Not susceptible to Rogue Data Cache Load (Meltdown) | Not affected | The kernel half stays mapped in every address space (ADR-0004 H5). A processor that leaves this clear is outside what that design defends |
| 1 | IBRS_ALL | Supports enhanced IBRS | Branch Target Injection: hardware and software | Set `IA32_SPEC_CTRL.IBRS` once on each processor and leave it (§3.3) |
| 2 | RSBA | A RET may use other predictors when the return stack buffer is empty, and those are not limited to the current domain (I8) | Not affected | Expected clear. Set, and without enhanced IBRS, it is a processor outside the scope |
| 3 | SKIP_L1DFL_VMENTRY | A hypervisor need not flush the L1 data cache on VM entry | L1TF: not affected | None: Origin runs no guests |
| 4 | SSB_NO | Not susceptible to Speculative Store Bypass | Hardware and software | SSBD is the control (§3.6) |
| 5 | MDS_NO | Not susceptible to Microarchitectural Data Sampling | Not affected | No buffer clearing on kernel exit |
| 6 | IF_PSCHANGE_MC_NO | No machine check from changing a code page's size without a TLB invalidation | Not affected | None: a hypervisor's concern |
| 7 | TSX_CTRL | `IA32_TSX_CTRL` exists | TAA: not affected | None decided; the kernel does not use TSX |
| 8 | TAA_NO | Not affected by TSX Asynchronous Abort | Not affected | None |
| 9 | MCU_CONTROL in the SDM (I3); "Reserved" on the web page (I5) | Supports `IA32_MCU_CONTROL` | | None. The two Intel documents disagree |
| 10, 11 | MISC_PACKAGE_CTLS, ENERGY_FILTERING_CTL | `IA32_MISC_PACKAGE_CTLS` exists, and its bit 0 makes the energy counters report filtered values | RAPL: software | Belongs to the Power interface: no unfiltered energy reading for unprivileged programs |
| 12 | DOITM | Supports Data Operand Independent Timing Mode | | §3.13 |
| 13, 14, 15 | SBDR_SSDP_NO, FBSDP_NO, PSDP_NO | Not affected by the three stale-data propagators (the MMIO stale data issues) | Not affected | None |
| 16 | MCU_ENUMERATION | `IA32_MCU_ENUMERATION` and `IA32_MCU_STATUS` exist | | For the microcode loader, when there is one |
| 17, 18 | FB_CLEAR, FB_CLEAR_CTRL | VERW overwrites fill buffers; `IA32_MCU_OPT_CTRL` bit 3 can turn that off | Not affected | None |
| 19 | RRSBA | May use other predictors for a RET on an empty return stack buffer, restricted to the current domain, unless `RRSBA_DIS_U` or `RRSBA_DIS_S` is set | Hardware and software | §3.6 |
| 20 | BHI_NO | Has the BHI_NO behaviour whatever `BHI_DIS_S` holds | Branch History Injection: hardware and software | Clear means set `BHI_DIS_S` (§3.6) |
| 21 | XAPIC_DISABLE_STATUS | `IA32_XAPIC_DISABLE_STATUS` exists; its bit 0 says the APIC is locked in x2APIC mode | | Consistent with a kernel that uses x2APIC only (ADR-0003 K5) |
| 22, 23 | MCU_EXTENDED_SERVICE, OVERCLOCKING_STATUS | Two more MSRs exist | | None |
| 24 | PBRSB_NO | Not affected by post-barrier return stack buffer predictions | Software | A hypervisor's concern (after a VM exit) |
| 25, 26 | GDS_CTRL, GDS_NO | Gather Data Sampling: the control bits exist; not affected | Not affected | None |
| 27, 28 | RFDS_NO, RFDS_CLEAR | Register File Data Sampling: not affected; VERW clears the affected register files | Not affected, the Darkmont E-cores included | None |
| 29, 30 | IGN_UMONITOR_SUPPORT, MON_UMON_MITG_SUPPORT | `IA32_MCU_OPT_CTRL` bits 6 and 7 exist. They concern an address-monitor table that can overflow and cost performance or power; CPUID.07H.02H:EDX[7] (MONITOR_MITG_NO) says a processor is free of it | | An availability matter. Bit 6 defaults to ignoring UMONITOR, and nothing here changes it |
| 31, 61:33 | Reserved | | | |
| 32 | PBOPT_SUPPORT | `IA32_PBOPT_CTRL` bit 0 exists | | None. Intel says the MSR exists only on Sapphire Rapids and Emerald Rapids and will not be carried forward (I5) |
| 62 | ITS_NO | Set by a hypervisor for its guest, not by hardware. Both I3 and I5 print the two values the other way round from what the name suggests | Indirect Target Selection: not affected | Read it only under a hypervisor, and treat it with care until Intel corrects one of the two |
| 63 | MSR_VIRTUAL_ENUMERATION_SUPPORTED (I5 only; reserved in I3) | Set by a hypervisor: a virtual MSR for sharing mitigation state exists | | Not used |

**Nova Lake and Diamond Rapids** have no row in I9 as of 2026-11-02. Nothing above is known for them.

### 3.2 The controls and how each vendor enumerates them

| Control | Intel | AMD |
|---|---|---|
| `SPEC_CTRL` (MSR 48H) bit 0, IBRS | CPUID.07H.00H:EDX[26] (I1 Chapter 21, I5) | CPUID Fn8000_0008_EBX[14] (A1 §3.2.9, A2) |
| bit 1, STIBP | CPUID.07H.00H:EDX[27] | Fn8000_0008_EBX[15] |
| bit 2, SSBD | CPUID.07H.00H:EDX[31] | Fn8000_0008_EBX[24]; bit 26 says it is not needed |
| bits 3 and 4, IPRED_DIS_U and IPRED_DIS_S | CPUID.07H.02H:EDX[1] | None |
| bits 5 and 6, RRSBA_DIS_U and RRSBA_DIS_S | CPUID.07H.02H:EDX[2] | None |
| bit 7, PSFD | CPUID.07H.02H:EDX[0] | Fn8000_0008_EBX[28] |
| bit 8, DDPD_U | CPUID.07H.02H:EDX[3] | None |
| bit 10, BHI_DIS_S | CPUID.07H.02H:EDX[4] | None |
| bits 11 to 14, SSBD_U, SSBD_S, PSFD_U, PSFD_S | On the web page only (I5, version 8.0), enumerated by a leaf 07H.02H EDX bit it names SSBD_PSFD_U_S; the SDM of September 2026 still has bits 63:11 reserved (I3) | None |
| `PRED_CMD` (MSR 49H) bit 0, IBPB | CPUID.07H.00H:EDX[26] | Fn8000_0008_EBX[12] |
| bit 7, SBPB | None | Fn8000_0021_EAX[27] (A1 §3.2.9) |
| Enhanced IBRS | `IA32_ARCH_CAPABILITIES` bit 1 | None by that name. Fn8000_0008_EBX[16] recommends leaving IBRS set; bit 18 says IBRS is preferred to a software mitigation; bit 19 says IBRS also limits predictions within one mode |
| Automatic IBRS | None | Fn8000_0021_EAX[8]; enabled by EFER bit 21, AIBRSE (A1 §3.1.7) |
| Return predictor cleared by IBPB | Yes, by definition (I6 footnote 4) | Only when Fn8000_0008_EBX[30], IBPB_RET, is 1 (A1 §3.2.9) |
| Branch-type predictions cleared by IBPB | | Fn8000_0021_EAX[28], IBPB_BRTYPE |
| Enhanced return-address predictor security (ERAPS) | | Fn8000_0021_EAX[24]; the predictor's size is in Fn8000_0021_EBX[23:16] |
| Predictions kept to their own context | With enhanced IBRS, by mode (§3.3) | Fn8000_0021_ECX[8], BtbCtxIsolation: a prediction can be influenced only by execution in the same context, where a context is user or supervisor and guest or host (A2) |
| LFENCE is a barrier | Always: no later instruction executes, even speculatively, until earlier ones have completed locally (I6 §3.2) | When Fn8000_0021_EAX[2] is 1, or after software sets MSR C001_1029 bit 1 (A2; A3 mitigation G-2) |

A write to `SPEC_CTRL` or `PRED_CMD` is not serializing on either vendor's processors. Both say it does not execute until earlier instructions have completed, and that later ones wait for it (I5; A1 §3.2.9).

### 3.3 Enhanced IBRS and automatic IBRS

**Intel, enhanced IBRS (I6 §2.4.1.3).**

- IBRS is set once and never cleared. While it is set, the predicted target of an indirect branch cannot be controlled by software that ran in a less privileged predictor mode, or on another logical processor.
- Intel recommends setting it and leaving it set on every processor that has it.
- STIBP is then redundant.
- It does not isolate two programs at the same privilege: that is IBPB's job (§3.4).
- A sequence that overwrites the return stack buffer may not be enough on these processors. For entries from user mode, Intel's answer is SMEP.
- With enhanced IBRS a processor's predictions do not use targets from branches in other predictor modes (I7).

**AMD, automatic IBRS (A1 §3.1.7, §3.2.9).**

- With EFER.AIBRSE set, code at CPL 0 has IBRS's protection without `SPEC_CTRL[IBRS]` being set. So does the host under SEV-SNP.
- Code at CPL 3 does not. A user thread is protected from its sibling thread only by STIBP.
- IBRS does not cover returns. If IBRS-style limits are wanted for RET, and the processor has no ERAPS, software clears the return predictor with 32 CALLs. AMD says none of that is needed when SMEP is on, the kernel and user address ranges do not overlap, and at least one unmapped 4 KiB page separates them.
- With automatic IBRS on and no ERAPS, the processor clears the return predictor on a VM exit.

Origin already meets AMD's three conditions: SMEP is on on every processor and proven at boot; user space is the lower half and the kernel the upper; the top page of the lower half is never mapped (ADR-0004 H1; `THREAT_MODEL_SYSCALLS.md` E5).

### 3.4 IBPB

- **Intel (I6 §2.4.3).** A command, not a mode: writing bit 0 of `IA32_PRED_CMD`. Software that ran before it cannot control the predicted targets of indirect branches that run after it on the same logical processor, and near returns count as indirect branches. Intel says to use it when the software running at one privilege changes identity, such as a switch to another application that does not trust the last one. Intel repeats that with enhanced IBRS on (I6 §2.4.1.3; I7).
- **AMD (A1 §3.2.9).** The same intent: for switches between contexts that do not trust each other. It covers indirect JMP and CALL. It also clears the return predictor only if IBPB_RET is 1, and branch-type predictions only if IBPB_BRTYPE is 1. SBPB is a second command that does not promise to clear branch-type predictions.
- **On an ERAPS processor** the return predictor is also cleared whenever the TLB is implicitly invalidated, on a MOV to CR3, and on an INVPCID other than for a single address (A1 §3.2.9).
- **What neither says:** the cost. No figure for one IBPB was found in any document read.
- **A barrier and branch history (I7).** On an Intel processor without BHI_NO, an indirect branch after a barrier may still be predicted from branch history gathered before it. Intel says it knows of no production code where that helps an attacker.

### 3.5 STIBP and SMT

- **Intel (I6 §2.3.2, §2.4.2).** Logical processors of one core may share indirect branch predictors; cores never do. STIBP stops one thread controlling its sibling's predictions, and with IBRS set it is not needed. Intel does not recommend STIBP for all application code on processors where it is costly.
- **AMD (A1 §3.2.9).** STIBP does the same on processors that share prediction resources. Returns are not influenced by a sibling. Setting IBRS turns STIBP on as well, but automatic IBRS covers only CPL 0 (§3.3). Fn8000_0008_EBX[17] says STIBP may be set once and left.
- **What is not branch prediction.** The data-sampling issues that once made SMT a question (MDS, L1TF) are "Not affected" for Intel's 2026 generations (I9). AMD's 2026 bulletins on contention side channels (AMD-SB-7051, 7060, 7069) call them variants of known side channels and recommend constant-time code and no secret-dependent control flow; none asks for SMT to be turned off (A7).
- **Which processors have SMT** was not established from a vendor document, except that the Zen 5 server parts run one or two threads to a core (A5). The kernel reads the topology (`docs/research/2026-11-08-cpu-topology-leaf-1fh.md`).

### 3.6 The issues that still apply

Intel's column is I9 for Panther Lake, Wildcat Lake and Clearwater Forest. AMD's is what the CPUID bits of A4 (Model 51h) and A5 (Model 02h, a Zen 5 server part) say, with the bulletins of A7.

| Issue | Intel's 2026 generations | AMD family 1Ah | The control |
|---|---|---|---|
| Bounds check bypass (Spectre v1) | Software | Applies to every processor (A3; AMD-SB-7062 of 2026-08-12 says existing guidance holds) | Software only: §3.11 |
| Branch target injection (Spectre v2) | Hardware and software | IBRS, STIBP and IBPB are present; IbrsPreferred, IbrsSameMode and AutomaticIBRS are 1 (A4, A5) | Enhanced or automatic IBRS, and IBPB |
| Branch History Injection | Hardware and software | No AMD statement on it was read. BtbCtxIsolation is 1 on Model 51h (A4); the register is not in A5 | `BHI_DIS_S` where BHI_NO is clear (I7) |
| Intra-mode branch target injection | Hardware and software | IbrsSameMode is 1 (A4, A5) | `IPRED_DIS_S` on Intel, where privileged code runs code a user wrote (I7). Intel recommends not having such a runtime |
| Return stack buffer underflow, RRSBA | Hardware and software; RSBA itself not affected | | Enhanced IBRS covers the published attack; `RRSBA_DIS_S` turns the alternate predictor off (I8) |
| Speculative Return Stack Overflow (AMD-SB-7005) | | SRSO_NO is 0 and SRSO_USER_KERNEL_NO is 1 on both models: not affected across the user and kernel boundary (A2, A4, A5). SRSO_MSR_FIX is 1 on Model 02h and 0 on Model 51h | Nothing at the user and kernel boundary. AMD's paper on it was not read |
| Branch type confusion | | BTC_NO is 1 (A4, A5) | None |
| Post-barrier return predictions | Software | | A hypervisor's |
| Indirect Target Selection | Not affected | AMD-SB-7034 (2025-05-13), on research into domain isolation against Spectre v2: AMD believes the techniques do not apply to its products | None |
| Indirect branch predictor delayed updates (INTEL-SA-01247) | Not affected | AMD-SB-7030 (2025-05-13): AMD believes it does not affect its processors | None |
| "Zero-at-ret" (INTEL-SA-01423, 2026-08-11) | Not affected; the advisory lists Ice Lake and Rocket Lake generation parts | | None |
| Transient Scheduler Attacks (AMD-SB-7029) | | TSA_SQ_NO and TSA_L1_NO are 1 and VERW_CLEAR is 0 on Model 51h (A4). A5 does not document that register; the bulletin's product tables, as read, list no Zen 5 product | Read the bits |
| Speculative store bypass, predictive store forwarding | Hardware and software | SSBD and PSFD present (A4, A5) | SSBD and PSFD, for each process that asks. Intel knows of no practical attack on an operating system that does not rely on language-based isolation (I6 §4.1.2) |
| SWAPGS (CVE-2019-1125) | Software | | LFENCE on each path that can skip or wrongly run SWAPGS (I13). `exception_common` has it |
| Straight-line speculation past a branch | | For RET: every AMD processor. For indirect JMP and CALL: only families below 19h (A3 mitigation G-5) | An INT3 after RET |
| Guest to host predictor isolation (AMD-SB-7046, 2025-09-11) | | Zen 1 to Zen 5 potentially | A hypervisor's. Not Origin's until it runs guests |
| Floating-point value injection, speculative code store bypass | Software (footnote 4 of I9) | AMD-SB-7050 (2026-04-17) was not read | For a JIT or a sandboxed runtime, not for the kernel |
| Meltdown, L1TF, MDS, TAA, SRBDS, the MMIO stale data issues, GDS, RFDS, LVI | Not affected | AMD has built no processor with the Meltdown behaviour (A3) | None |

Three more AMD bulletins of 2026 were read and do not reach these processors: AMD-SB-7052 (the op cache, Zen 2 only), AMD-SB-7053 (the floating-point divider, Zen and Zen+ only) and AMD-SB-7068 (families 15h and 16h). AMD-SB-7061 (2026-08-06) concerns a software mitigation for SRSO on Zen 1 to Zen 4.

### 3.7 FRED

All of this is I2, Chapter 8, unless marked.

- **What it is.** One way into ring 0 for every event, and two return instructions. Exceptions, interrupts, SYSCALL and SYSENTER are all delivered the same way, without the IDT. ERETU returns to ring 3 and ERETS returns within ring 0.
- **Enumeration and enabling.** CPUID.07H.01H:EAX[17] is FRED and bit 18 is LKGS. CR4 bit 32 enables it, and can be set only in 64-bit mode.
- **Entry points.** An event from ring 3 enters at the address in `IA32_FRED_CONFIG` with its low 12 bits cleared; an event from ring 0 enters 256 bytes later. So the handler knows which return to use.
- **GS.** Delivery from ring 3 and ERETU swap the GS base with `IA32_KERNEL_GS_BASE` themselves. SWAPGS raises #UD once FRED is on, as do SYSRET, SYSEXIT, CLRSSBSY and SETSSBSY. An IRET that would change the privilege level raises #GP.
- **Flags.** Delivery loads RFLAGS with the value 2: interrupts off, and AC, TF and DF clear.
- **Stacks.** Four stack levels, 0 to 3, replace the interrupt stack table. `IA32_FRED_STKLVLS` gives a level to each exception vector and to NMI, for events that arrive in ring 0. The new level is the higher of the current one and the event's. If neither the privilege level nor the stack level changes, RSP is not reloaded: it is moved down and aligned to 64 bytes. So an event that arrives while its own level is in use lands below what is there, and overwrites nothing.
- **The frame.** 64 bytes: the IDT's return state, with event type, vector, stack level and flags in the upper bits of the CS and SS slots, then event data (the faulting address for #PF, the debug status for #DB).
- **NMI.** Delivery of an NMI sets a bit in the frame. ERETS and ERETU unblock NMIs only when that bit is set. A return from an exception taken inside the NMI handler does not carry the bit and does not unblock them. An ERETS or ERETU that faults unblocks nothing. IRET still unblocks them (I2 §7.7.2).
- **Shadow stacks.** FRED does not use the supervisor shadow-stack token. The return instructions check the saved values against `IA32_FRED_SSP0` to `SSP3` instead. A system that turns FRED on need not consult CPUID.07H.01H:EDX[18] before enabling supervisor shadow stacks (§3.9).
- **Ordering.** ERETU gives SYSRET's guarantee: nothing after it executes, even speculatively, until everything before it has completed.
- **Checks the IDT used to make.** FRED delivery does not check an IDT gate's DPL for INT n. The handler must tell from the event type and vector what the IDT would have refused.

**Which processors.**

- **Intel.** I15's Table 1-2 gives Panther Lake, Clearwater Forest and Diamond Rapids for FRED and LKGS. It lists where a feature is introduced, not every processor that has it, and it does not name Wildcat Lake or Nova Lake for FRED. NMI-source reporting is given for Clearwater Forest only.
- **AMD.** A2 defines CPUID Fn0000_0007_EAX_x1[17] as FRED and bit 18 as LKGS (added in Rev. 3.38, July 2026). A1 lists the FRED MSRs at Intel's indices, 1CCH to 1D4H, and nothing more: it has no text for the delivery or for ERETS and ERETU, and A2 has no pages for those instructions. Model 51h reports FRED and LKGS as 1 (A4). Model 02h, a Zen 5 part, reports bits 31:6 of that register as reserved: no FRED (A5).

### 3.8 LASS

I2 §4.3.

- **What it does.** It refuses an access by which half of the address space it names, before any page walk. A user-mode access to an address with bit 63 set is refused. A supervisor-mode instruction fetch from an address with bit 63 clear is refused, whatever SMEP says. A supervisor-mode data access to such an address is refused when SMAP is on and either RFLAGS.AC is clear or the access is an implicit supervisor access.
- **Why it matters here.** Paging protection needs a page walk, and the time a walk takes tells unprivileged code about the kernel's layout. LASS decides without the walk. A prefetch, or an access made by a speculatively executed instruction, that would violate it is not performed and raises nothing.
- **Enumeration and enabling.** CPUID.07H.01H:EAX[6]; CR4 bit 27.
- **Faults.** #GP, or #SS for a stack access, with a zero error code.
- **Which processors.** I15 gives Sierra Forest, Grand Ridge, Arrow Lake H and Diamond Rapids as where it was introduced. AMD defines no such bit: bits 16:6 of Fn0000_0007_EAX_x1 are reserved (A2), and Model 51h reports them so (A4).
- **What it constrains.** Any ring-0 code that runs at an address in the lower half. In Origin that is the AP trampoline (`framekernel/frame/src/smp.rs`), and it would be any firmware runtime service called from the kernel.

### 3.9 Supervisor shadow stacks

- **Intel, with IDT delivery (I1 §18.2.3; I2 §7.12.1.1, §7.14.5).** A stack the processor may switch to holds a token at its base with a busy bit. The switch checks the token and sets the bit; the return clears it. If a push made after the token is taken faults, the processor goes back to the old shadow stack and the bit stays set. Intel tells software to enable supervisor shadow stacks only if that cannot happen. Where CPUID.07H.01H:EDX[18] (CET_SSS) is 1, it is enough that no such push can take a page fault. The token and the pushed frame must lie in one 32-byte aligned region. Stacks the interrupt stack table selects have their shadow stacks in a table that `IA32_INTERRUPT_SSP_TABLE_ADDR` names.
- **Intel, with FRED.** No tokens (§3.7).
- **AMD (A1 §18.6.3 to §18.6.5).** The same token, busy bit and checks, with the 32-byte containment check added in Rev. 3.45 (July 2026). CET_SS is CPUID Fn0000_0007_ECX_x0[7], and is 1 on both models read (A4, A5). AMD defines no CET_SSS bit in A2. AMD has no indirect branch tracking.
- **An erratum on the Zen 5 server parts (A6, erratum 1464).** A guest can be left with the busy bit set if the 32 bytes at the new shadow stack pointer cross a page, or the shadow stack is not in write-back memory. It is a statement about guests, but it names the two things a kernel's own shadow stacks should never do.
- **Speculation.** With shadow stacks on, a processor does not execute, even speculatively, at a RET address that the two stacks disagree on, unless the target also comes from the return predictor. Both vendors say so (I1 §18.2.6; A1 §18.9).
- **A second use on the IDT path.** An event that re-enters a stack of the interrupt stack table finds that stack's token busy, and the delivery faults instead of overwriting the stack. This is this ledger's reading of the rules above, not a vendor's statement.

### 3.10 NMIs, the interrupt stack table and system management mode

- **Blocking.** Once an NMI is delivered, further NMIs are blocked until the next IRET executes. Both vendors say so (I2 §7.7.2; A1 §8.2.3). Intel adds that they are unblocked as soon as an IRET starts, even if that IRET then faults.
- **The interrupt stack table.** A gate that names an IST entry switches stacks unconditionally (I2 §7.14.5; A1 §8.9.4). The processor loads the same top of stack each time.
- **So** an IRET executed inside the NMI handler, by a nested exception's handler, lets a second NMI in, and it starts at the same top of stack as the first.
- **System management mode, Intel (I2 §34.8).** NMIs are blocked on entry to SMM. If they were blocked before the SMI, they are blocked again after RSM.
- **System management mode, AMD (A1 §10.3.3).** An NMI that arrives during SMM is latched, and its handler is invoked straight after RSM. The text read does not say that blocking in force before the SMI is kept.
- **With FRED** the return carries the decision (§3.7).

### 3.11 Bounds checks: what the vendors promise

- **LFENCE.** Intel recommends it between a bounds check and the access (I6 §3.2). AMD calls it the preferred software answer to variant 1 (A3, mitigation V1-1 and conclusion), where LFENCE is dispatch-serializing (§3.2). It is 1 by design on both AMD models read (A4, A5).
- **Masking, Intel (I6 §3.2; I12).** CMOVcc, AND, ADC, SBB and SETcc also constrain speculation on the family 6 processors that were current when Intel wrote this, in 2018 and 2021. Intel does not promise it for later processors, and says it will publish guidance before processors that behave differently are released. Neither page has changed since. Nova Lake is family 18 and Diamond Rapids family 19 (I15), so by its wording the statement does not reach them.
- **Masking, AMD (A3, mitigations V1-2 and V1-3).** A conditional move that depends on the comparison, or an AND that masks the index, keeps a speculative access in bounds. AMD says both apply to all its processors, and that the AND works best for power-of-two sizes.
- **Two cautions from Intel.** A processor speculates through several branches, so one check does not protect an access guarded by another (I12). And if the bound itself was just written to memory, the comparison can run speculatively on the old value (I6 §3.2).

### 3.12 The compiler, as found on this machine

Rust 1.99.1 stable, for `x86_64-unknown-none` (R1). One file with an indirect call, a dense `match` and a bounds check was compiled with each setting, and the assembly read.

| Setting | On stable 1.99.1 | Seen |
|---|---|---|
| `-C jump-tables=no` | A stable option | The `match` no longer compiles to a table and an indirect jump |
| `-C target-feature=+retpoline-indirect-calls,+retpoline-indirect-branches` | Accepted with a warning that it will become a hard error, and that `-Zretpoline` replaces it (rust-lang issue 116344) | Retpoline thunks are emitted today |
| `-Z retpoline`, `-Z cf-protection=branch`, `-Z stack-protector=strong` | Refused: `-Z` is for the nightly compiler | |
| `-C target-feature=+harden-sls-ret,+harden-sls-ijmp` | Accepted with a warning: unknown and unstable, passed to the code generator | An INT3 follows each RET and indirect JMP |
| `-C target-feature=+shstk` | Accepted with the same warning | No change to this file: shadow stacks need no code generation for ordinary calls |
| `-C force-frame-pointers`, `-C overflow-checks`, `-C relro-level` | Stable options | Not probed here |

This confirms CLAUDE.md §14 for this compiler: there is no stable flag for IBT or for a stack protector.

### 3.13 Constant time

Intel's Data Operand Independent Timing Mode is bit 0 of `IA32_UARCH_MISC_CTL` (MSR 1B01H), present when `IA32_ARCH_CAPABILITIES` bit 12 is 1 (I5, I14). On Ice Lake and later cores, and Gracemont and later Atom cores, the listed instructions are data-independent only with the mode on. Intel does not recommend turning it on for the whole system, expects its cost to grow on later processors, and suggests an operating system offer it the way it offers SSBD. AMD's guidance on timing side channels was not read.

## 4. What the processors in scope report

Only AMD publishes register values model by model. Intel's values are known only as the table of §3.1.

| | Family 1Ah Model 51h, B0 (A4) | Family 1Ah Model 02h, C1 (A5) |
|---|---|---|
| In scope? | AMD's document names no product or core. The press ties models 50h to 57h to the 2026 server generation (`docs/research/2026-11-10-cpu-identification.md`, source A5 there) | No: EPYC 9005 launched in 2024. It is the nearest published Zen 5 reference; Gorgon Point, which is in scope, has no document of its own |
| IBPB, IBRS, STIBP, SSBD, PSFD | 1 | 1 |
| IbrsAlwaysOn (bit 16) | Reserved | Reserved |
| StibpAlwaysOn, IbrsPreferred, IbrsSameMode | 1 | 1 |
| BTC_NO, IBPB_RET | 1 | 1 |
| AutomaticIBRS | 1 | 1 |
| ERAPS; return predictor size field | 1; 08h (64 CALLs) | 1; 08h |
| SBPB, IBPB_BRTYPE | 1 | 1 |
| SRSO_NO / SRSO_USER_KERNEL_NO / SRSO_MSR_FIX | 0 / 1 / 0 | 0 / 1 / 1 |
| LFenceAlwaysSerializing | 1 | 1 |
| BtbCtxIsolation, TSA_SQ_NO, TSA_L1_NO | 1, 1, 1 | The register is not in the document |
| CET_SS | 1 | 1 |
| FRED, LKGS | 1, 1 | Reserved: neither |
| LASS | Reserved: none | Reserved: none |

## 5. What could not be read, and what is unverified

- **No processor was run.** Every bit above is a document's word. Which bits Panther Lake sets in `IA32_ARCH_CAPABILITIES` is not in any document read.
- **Nova Lake and Diamond Rapids** are in no affected-processors row, and no Intel statement on masking instructions reaches them (§3.11).
- **Gorgon Point** has no AMD document of its own. Its bits are assumed to be Zen 5's, and Zen 5's are taken from a server part (A5).
- **Whether Model 51h is Zen 6,** and whether it is the server generation of 2026, rests on the press, not on AMD.
- **AMD's FRED** is enumerated and its MSRs are listed, but AMD has published no text for the transitions. Whether it matches Intel's Chapter 8 in every detail was not established.
- **The cost of every control.** No vendor document read gives a figure for an IBPB, for `BHI_DIS_S`, for STIBP on these processors, or for LFENCE.
- **Not read:** AMD's papers on Speculative Return Stack Overflow, on Transient Scheduler Attacks, on branch type confusion and on timing side channels; AMD-SB-7050; the rest of the 57 bulletins beyond those listed in A7; Intel's retpoline paper, the software sequences of I7 and its sections for hypervisors; the pseudocode pages of I2 §8.3.5 and §8.6 (LKGS); the standalone FRED specification beyond its title page; Intel's specification updates (errata) for the 2026 generations, which ADR-0009 H6 already asks for; AMD's revision guide beyond three errata; Intel's guidance on managed runtimes.
- **LASS on Panther Lake, Wildcat Lake and Clearwater Forest** is likely, since earlier processors of each line introduced it, but I15 does not say so.
- **The SMI case on AMD** (§3.10) is a gap in the text read, not a statement by AMD that NMIs are unblocked.
- **The compiler probe** ran on the Mac for one small file. It shows the options exist and act; it is not a check of the kernel's image.

## 6. Found on the way, for other tasks

- **TASKS item 16 (the CPU identification table).** Intel now confirms Diamond Rapids as family 19, model 01H: I15's Table 1-1 has `13_01H`, and `12_01H` and `12_03H` for Nova Lake. I3's Table 2-1 names 06_CCH Panther Lake, 06_D5H Wildcat Lake and 06_DDH Clearwater Forest in the SDM itself.
- **TASKS item 18 (the random-number generator).** AMD-SB-7055 (2025-10-23, last revised 2026-01-15): on Zen 5 the 16-bit and 32-bit forms of RDSEED can return 0 while reporting success. The 64-bit form is not affected. AMD lists microcode and firmware fixes, and suggests treating a zero as a failure until they are applied. `cpu::rdseed` uses the 64-bit form.
- **The address-space threat model, T10.** A6 lists erratum 1621, "Spurious Page Fault May Occur for Some Initial Page Allocation Mappings", and 1617 on speculative page-table accesses, for the Zen 5 server parts. Their text was not read.
- **`THREAT_MODEL_SYSTEM.md`.** AMD-SB-7033 (2025-03-05, revised 2025-06-03): a weakness in how the processor checked a microcode patch's signature let an administrator load patches AMD had not signed. Zen 5 is among the affected products, and the fix comes in platform firmware. AMD-SB-7048 (2025-09-15), on Rowhammer attacks against DDR5 memory, was seen in the list by its title and date only.

## 7. What Origin's code has today

Checked against the tree at `2fbaea6`.

- `IA32_ARCH_CAPABILITIES`, `SPEC_CTRL` and `PRED_CMD` are not read or written anywhere: no such constant exists in `framekernel/frame/src/cpu.rs`.
- SMEP, SMAP and UMIP are on; `CR4.FSGSBASE` is off (`cpu.rs`, `init_cpu_state`, `enable_smep_smap`).
- `exception_common` has an LFENCE after its conditional SWAPGS, on both paths.
- The handle table's lookup is a bounds-checked slice access, `self.slots.get(index)`, on an index taken from the handle (`framekernel/src/handles.rs`, `HandleTable::get`).
- The call table is a `match` on the call number in generated code (`framekernel/syscalls/src/origin_kernel/kernel/server.rs`, `dispatch`).
- NMI and #MC use entries 2 and 3 of the interrupt stack table; the kernel returns with IRETQ everywhere (`cpu.rs`, `exception_saved`).
- The only code-generation flag set for the kernel and the loader is `-C force-frame-pointers=yes` (`.cargo/config.toml`).

## 8. Method, and what went wrong

- **Reading.** PDFs were fetched by `curl` into a pipe, parsed in memory with `pypdf`, and searched there; nothing was written to disk. AMD's manual Volume 2 was read from the owner's local copy after its hash matched AMD's. Pages on `intel.com` and `amd.com` were read in the desktop app's browser, because both sites refuse a plainly named command-line reader and that refusal was not worked around.
- **Three pages of A3** hold their text as pictures. They were drawn to PNG files in the session's scratch directory, read, and deleted.
- **A save dialog.** Opening A3's address in the browser made the browser offer to save the file. Nothing was saved by this session, and the address was not opened again.
- **A cached copy.** The web-fetch tool, tried on A3 once, kept a copy of the PDF in its own results folder outside the repository. It was deleted at once.
- **No operating system's source** was opened. Intel's own pages name Linux in places; nothing here rests on those passages.
