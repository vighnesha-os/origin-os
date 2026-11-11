# Research ledger: Intel and AMD processor simulators

| | |
|---|---|
| Date | 2026-11-08 |
| Updated | 2026-11-10: gem5 added (§7); two §6 items rechecked |
| For | CLAUDE.md §74 and the header's hardware target (Intel and AMD platforms from 2026 on) |
| Outcome | No tool was adopted and no design changed. §5 is a recommendation; the spending and licence questions are the owner's. |
| Method | CLAUDE.md §16: vendor pages first, dates recorded, local experiments recorded |
| Scope note | Nothing was installed and no licence was accepted. Web sources were opened through a fetch tool that answers questions about a page, so they were not all read line by line. R4 was read in part (clauses 2, 4 and 5) as extracted text. Sources marked ‡ were seen only as search-result text. |
| Since | The owner cannot buy machines for now (ADR-0009's update of 2026-11-09), so §5's machine is on hold. What can be done without buying one is in [`2026-11-09-testing-without-buying-hardware.md`](2026-11-09-testing-without-buying-hardware.md). |

## 1. Sources

| ID | Source | What it establishes |
|---|---|---|
| R1 | intel.com/content/www/us/en/developer/articles/tool/simics-simulator.html (updated 2023-09-08) | Free public release of Simics. Hosts are 64-bit Linux and Windows 10/11. The Quick-Start Platform boots from TianoCore UEFI and runs modern Linux and Windows. Named users: commercial users developing designs based on Intel products, teachers, students and researchers, and hobbyists for non-commercial use. Wind River sells a supported version. |
| R2 | intel.github.io/simics/docs/installation-guide/index.single-page.html | The host must be a 64-bit Intel Architecture CPU (Haswell or newer performance core, or Gracemont or newer efficiency core). 16 GB RAM minimum, 32 GB recommended. Hardware-assisted simulation needs VT-x. macOS and ARM are not mentioned. |
| R3 | intel.github.io/simics/docs/getting-started/index.single-page.html | The QSP-x86 machine has processor class `x86QSP3` at 2.00 GHz, one core by default (`num_cores` raises it), 8 GiB RAM by default and 64 GiB at most, a `motherboard_x58_ich10` board, two SATA disks, a USB disk and VGA. `machine:hardware:storage:disk0:image` accepts craff, VHDX and raw images. NVMe, VT-d and IOMMU are not mentioned. |
| R4 | intel.com/content/dam/develop/external/us/en/documents/isim-tools-license-agreement.pdf (linked from R1; SHA-256 `470a2ef36564b423e79867148800aebcd0623154c6241093329e7adfbd924a44`; no date or revision found in its text) | The Simics licence. See §4. |
| R5 | intel.github.io/tsffs/simics/RELEASENOTES/index.html (Simics 6.0.x; first 100,000 of 411,301 characters read) | An NVMe drive model (`nvme-common`) first shipped in 6.0.167. The notes do not say whether it is in the public release or in QSP-x86. VT-d and IOMMU are not mentioned in the part read. |
| R6 | intel.com/content/www/us/en/developer/articles/tool/software-development-emulator.html | SDE 10.13.1, released 2026-07-28. Runs on IA-32 or Intel 64 hosts under Windows, Linux or OS X. Free for any use under the Intel Simplified Software License (2023-08-23). Runs applications under Pin. Emulates only the user-space aspects of CET. Corresponds to AVX10.2 specification version 7 and the APX specification. |
| R7 | intel.com/content/www/us/en/developer/articles/release-notes/intel-software-development-emulator-release-notes.html | 10.5 (2026-01-13) adds Nova Lake and drops Windows 10 and Server 2019 hosts. 10.7, 10.8 and 10.13.1 update APX or AVX10.2 for Diamond Rapids. The 2025 and 2026 entries list no AMD Zen target. |
| R8 | intel.com/content/www/us/en/developer/articles/tool/pin-a-dynamic-binary-instrumentation-tool.html | Pin analyses user-space applications on Linux and Windows, on IA-32 and x86-64. |
| R9 | developer.amd.com/simnow-simulator/ | HTTP 404 on 2026-11-08. |
| R10 | 15h.org/index.php/SimNow (community wiki, edited 2026-04-29) | Links a public `simnow-linux64-4.6.2pub.tar.gz` on archive.org, and describes SimNow as emulating Athlon 64 and Opteron systems. No Zen model is listed. |
| R11 | ‡ careers.amd.com/careers-home/jobs/79908 (HTTP 404 when opened) | The listing's text describes SimNow as a C++ simulator for validating the software-visible behaviour of next-generation AMD designs. |
| R12 | ‡ amd.com/en/developer/browse-by-resource-type/software-tools.html | SimNow is not named in the indexed text of AMD's software tools page. |
| R13 | ‡ amd.com/en/blogs/2025/introducing-the-amd-developer-cloud.html | The AMD Developer Cloud, as announced in 2025, gives access to Instinct GPUs. |
| R14 | intel.com/content/www/us/en/developer/tools/devcloud/prelaunch-services-signup.html | Redirects to Intel's 404 handler on 2026-11-08. |
| R15 | ‡ phoronix.com/news/Intel-Developer-Cloud-DevCloud; community.intel.com/t5/Edge-Software-Catalog/How-to-access-intel-Tiber-cloud-developer-access/m-p/1722530 | Intel's developer cloud offered pre-launch hardware access in the past. Forum replies now conflict: one says it was renamed, another says it is no longer available. |
| R16 | cloud.intel.com | The fetch returned only the title "Intel Cloud Services". |
| R17 | ‡ latitude.sh/docs/guides/ipmi-custom-os-deployment; alexhost.com/faq/custom-iso-installation-via-ipkvm-amd-epyc-7642/ | Commercial hosts offer custom ISO boot over IPMI or KVM. UEFI settings, Secure Boot control and availability of 2026-or-later CPUs were not established. |

## 2. Local experiments

Host: Apple M1 Max (`arm64`), QEMU 11.1.3, repository at commit `540fc2c`.

- **E1. Accelerators.** `qemu-system-x86_64 -accel help` lists only `tcg`.
- **E2. Named CPU models under TCG.** For each model I ran the command below and counted the distinct "TCG doesn't support requested feature" warnings.

  ```bash
  printf 'quit\n' | qemu-system-x86_64 -machine q35 -accel tcg -cpu MODEL -display none -S -monitor stdio -nodefaults
  ```

  | Model | Features refused | Refused features that matter here |
  |---|---|---|
  | `max` | 0 (it requests only what TCG has) | None |
  | `EPYC-Turin-v2` | 54 | PCID, INVPCID, AVX-512 |
  | `GraniteRapids-v7` | 48 | PCID, INVPCID, TSC-deadline, AVX-512, AVX10, AMX, CET shadow stacks and IBT |
  | `ClearwaterForest-v4` | 35 | PCID, INVPCID, TSC-deadline, CET shadow stacks and IBT |
  | `DiamondRapids-v2` | 77 | PCID, INVPCID, TSC-deadline, AVX-512, AVX10, APX, AMX, FRED, CET shadow stacks and IBT |

  No model lost AVX, AVX2, BMI1, BMI2, FMA, F16C, LZCNT, MOVBE, XSAVE or x2APIC. A named model therefore keeps the x86-64-v3 baseline but loses the features that make it new. Origin was not booted on these models.
- **E3. Origin's use of newer vector instructions.** `git grep` for AVX-512, AVX10 and APX in `*.rs` finds only feature-bit definitions in `framekernel/src/features.rs` and `runtime/onbf/format/src/manifest.rs`. There are no code paths for SDE to check yet.

## 3. Findings

| Tool | Kind | Boots an OS? | Hosts | Coverage of 2026-or-later chips |
|---|---|---|---|---|
| Intel Simics public release | Full-system simulator | Yes: UEFI and a replaceable disk image (R1, R3). Origin was not tried. | Linux or Windows on an Intel-architecture CPU (R1, R2) | None shown: a generic `x86QSP3` CPU on an X58-class board (R3) |
| Intel SDE 10.13.1 | Instruction emulator for user programs | No: it runs applications under Pin, which handles user space (R6, R8) | x86 only (R6) | Instructions only: Nova Lake and Diamond Rapids (R7) |
| AMD SimNow | Full-system simulator | No public build for current chips was found | The newest public build found is 64-bit Linux, judging by its file name (R10) | No public Zen model found (R9 to R12) |
| QEMU 11.1.3 (installed) | Full-system emulator | Yes, in daily use | Runs on the M1 Max, TCG only (E1) | Model names only; the newest features are refused (E2) |
| gem5 26.0 (added 2026-11-10, §7) | Full-system and syscall-emulation simulator | Not Origin: no UEFI path, and its CPU is below x86-64-v3 (R20 to R23) | Its developers use Apple Silicon (R19); not tried here | None: generic CPU models (R24) |

- **Neither Intel tool lists Apple Silicon as a host.** Running either on the M1 Max would be unsupported, and it was not attempted.
- **AMD.** AMD appears to keep SimNow for its own validation work (R11). Any access for Zen would have to be arranged with AMD directly; no public programme was found.
- **Remote access to real chips.** Intel's pre-launch route could not be confirmed as still open (R14 to R16). AMD's public developer cloud is described only in terms of GPUs (R13).

## 4. Simics licence (R4)

This is an engineering reading, not legal advice. The terms shown at download may differ from R4.

- **Clause 2.1, commercial users.** Use is limited to evaluating Intel products and developing commercial products based in whole or in part on them. The simulated system must contain an Intel product.
- **Clause 2.2, non-commercial users.** Use is personal and excludes developing commercial products. Academic institutions may use it for research and education.
- **Clause 4.** Benchmarking by commercial users is for internal use only. Results about pre-release models may never be published or disclosed.
- **Clause 5.** The materials are for internal use, may not be incorporated into products and may not be reverse-engineered.

How Origin OS will be released decides which of clauses 2.1 and 2.2 applies. Under 2.1, simulating an AMD system is not permitted.

## 5. Recommendation, checked against CLAUDE.md

| Recommendation | Reason | CLAUDE.md |
|---|---|---|
| Keep QEMU and OVMF as the daily test path. | It is the only full-system option that runs on the development host. | Header, §4, §84 |
| Plan one real x86 machine from 2026 or later. The cost is the owner's decision. | Physical testing is required regardless, and simulators have the same limit as QEMU. The machine could also run SDE and Simics, and test CET shadow stacks, which TCG cannot model (E2). | Header, §14, §74, §80, rule 30 |
| Adopt SDE as a host-only tool once AVX-512, AVX10 or APX code paths exist. | §14 requires runtime detection or per-image declaration of those features, and SDE can exercise such code. It runs Linux and Windows programs, so it would test host builds, not `.ose` files. None exist to test yet (E3). | §4, §5, §11, §14 |
| Do not adopt Simics now. | Its board is old, it cannot model AMD, device coverage is unconfirmed, and the licence question comes first. If it is adopted later, no Simics model source may enter Origin. | §3, §4, §6, §74 |
| Do not use simulator timings to support performance claims. | Simics' public platform is a functional model (R1), and QEMU's timing is not hardware timing. | §53, rules 21 and 22 |
| Do not adopt gem5 (added 2026-11-10; details in §7.3). | Origin cannot boot on it, and its timings are not hardware timings. | §14, §19, §53, §74, rule 30 |

## 6. Still to verify

- The current public Simics version. R1 dates from 2023, while later builds exist. Rechecked 2026-11-10: only secondary sources were found, giving Simics 7 from March 2025 and the end of life of commercial Simics 6 in July 2026 (R28). The public release's version is still unconfirmed.
- Whether the public Simics packages include NVMe or VT-d models.
- Whether `x86QSP3` exposes AVX2. Origin refuses a CPU below x86-64-v3 at boot. Rechecked 2026-11-10 without a result (R29).
- SDE's full list of CPU options, from `sde -help` on an x86 host. No Panther Lake entry was found in R7.
- Whether AMD offers SimNow for Zen under agreement.
- Whether Intel's developer cloud still exists and whether it allows booting a custom operating system.
- Which bare-metal hosts offer 2026-or-later CPUs with UEFI and Secure Boot control.

## 7. Addendum, 2026-11-10: gem5

The owner asked whether gem5 had been checked alongside Simics. A note on `origin/main`, `docs/research/2026-11-09-testing-without-buying-hardware.md`, covered it in one sentence. This addendum checks that sentence against gem5's source. R20 to R25 are raw files at a pinned commit, fetched through the GitHub API and read directly rather than through the fetch tool. Nothing was built or installed. Sources marked ‡ were seen only as search-result text.

### 7.1 Sources

| ID | Source | What it establishes |
|---|---|---|
| R18 | GitHub releases and branches of `gem5/gem5` (API, 2026-11-10) | v26.0.0.0 was published on 2026-10-21, after v25.1.0.1 on 2026-04-21. `stable` is the default branch. R20 to R25 were read at its head, `9d368896ce119c17e1289dcbb18f33624724b843` (2026-11-04). |
| R19 | `RELEASE-NOTES.md` on `stable` (first 100,000 of 149,684 characters read through the fetch tool) | Releases 24.0 to 26.0 never mention AVX, VEX, AVX-512, APX, UEFI, EDK2, NVMe or IOMMU. 24.0 says Apple Silicon machines were used regularly during development, and 26.0 mentions daily macOS builds. |
| R20 | `src/arch/x86/decoder.cc`, `src/arch/x86/isa/decoder/*.isa` and the `src/arch/x86/isa/insts/` tree | The decoder parses two- and three-byte VEX prefixes and passes the opcode to the legacy two-byte and three-byte tables. Instructions are defined only for the general-purpose, x87, MMX (`simd64`), SSE (`simd128`) and system groups. No AVX instruction is defined. |
| R21 | `src/arch/x86/X86ISA.py` and `src/arch/x86/cpuid.cc` | CPUID values are configuration parameters, listed as EAX, EBX, EDX, ECX. The defaults are vendor `HygonGenuine`, name "Fake gem5 x86_64 CPU", leaf 1 EAX `0x00020F51` (family 0Fh), leaf 1 ECX `0x00000209` (SSE3, MONITOR and SSSE3 only), and leaf 7 EBX `0x01800001` (FSGSBASE, CLFLUSHOPT and CLWB only). |
| R22 | `src/arch/x86/fs_workload.cc`, `src/arch/x86/linux/fs_workload.cc` and `src/arch/x86/X86FsWorkload.py` | gem5 writes its own GDT, identity-maps the first 4 GiB with 2 MiB pages, turns on long mode and starts an ELF kernel as if a boot loader had already run. It generates SMBIOS, Intel MP and ACPI tables itself, and the Linux variant also writes the E820 map and Linux boot parameters. |
| R23 | `src/arch/x86/bare_metal/workload.cc`, `src/dev/x86/X86QemuFwCfg.py` and GitHub code search of the default branch | `X86BareMetalWorkload` starts CPU 0 from the INIT state and halts the others, and a QEMU `fw_cfg` device exists. Code search finds no configuration that uses either one, and no mention of SeaBIOS, OVMF or EDK2. |
| R24 | The `src/dev/` and `src/cpu/` trees | The x86 devices are CMOS, an i8042 keyboard controller, an 82094AA I/O APIC, 8237 DMA, an 8254 timer, an 8259 interrupt controller, a PC speaker, a south bridge, an IDE controller and QEMU `fw_cfg`. There is no NVMe, AHCI, xHCI or USB model, and the only IOMMU model is Arm's SMMUv3. The CPU models are simple (atomic and timing), Minor, O3 and KVM; none is named after a real processor. |
| R25 | GitHub licence API for `gem5/gem5` | BSD-3-Clause. |
| R26 | ‡ doi.org/10.1109/ACCESS.2024.3359296 (IEEE Access, vol. 12, 2024; the DOAJ page returned HTTP 403) | "Gem5-AVX" by S. Lee, Y. Kim, D. Nam and J. Kim adds AVX, AVX2 and subsets of AVX-512 to gem5, validated against Haswell and Skylake hardware and SDE. It is not in upstream gem5. |
| R27 | ‡ github.com/seanzw/gem5-avx | A separate, unofficial fork with partial AVX2 and AVX-512 support. Not confirmed to be R26's code. |
| R28 | ‡ windriver.com: Simics 7 product overview (2025-04) and "Intel Simics 6 Transitioning to Legacy State" | Simics 7 since March 2025. Commercial Simics 6 reaches end of life in July 2026. |
| R29 | intel.github.io/tsffs/simics/targetguide-qsp-x86/introduction.html | The QSP-x86 guide's introduction names no processor model or feature. `index.single-page.html` in the same directory returned HTTP 404. |

### 7.2 Checks against Origin

- **E4. What Origin's kernel needs at entry.** Repository at commit `5b6ab78`. The kernel halts with "CPU baseline failed" on a CPU below x86-64-v3 (`framekernel/src/main.rs`, lines 188 to 201). It starts from the `BootInfo` structure that Origin's UEFI loader builds, which carries the UEFI system table and the loader's page tables (`boot/info/src/lib.rs`, line 133).

| Origin needs | gem5 26.0's x86 model | Effect |
|---|---|---|
| UEFI boot (§19) | None provided. gem5 sets up long mode and its own tables, then jumps into an ELF kernel (R22). A reset-state workload and a QEMU `fw_cfg` device exist, but nothing uses them (R23). | Origin would need an entry path used only on gem5. That path would never ship. |
| x86-64-v3 (§14) | The default CPU lacks CX16, SSE4.1, SSE4.2, POPCNT, MOVBE, XSAVE, AVX, AVX2, BMI1, BMI2, FMA and F16C (R21), so it is below even x86-64-v2. Editing CPUID does not help, because no AVX instruction is defined (R20). | Origin halts at boot (E4). Userspace built for x86-64-v3 (§11) could not run. |
| NVMe, IOMMU and USB (§32 to §34) | None on x86 (R24). | The storage, DMA-protection and USB paths cannot be tested. |
| Intel and AMD platforms from 2026 on (header) | Generic CPU models only (R24). CPUID reports family 0Fh (R21). | The same limit as QEMU. |
| The M1 Max as host | Its developers use Apple Silicon (R19). The KVM CPU runs guest code on the host processor, so on the M1 Max x86 would run only in software models. | Not tried here. |
| Licence | BSD-3-Clause (R25). | Acceptable for a host-only tool (§4). |

The 2026-11-09 note says gem5 has "no AVX decode". More precisely, the decoder parses VEX prefixes, but no AVX instruction is defined (R20). The note's conclusion stands. Out-of-tree work adds AVX (R26, R27), but neither is described as adding firmware or devices.

### 7.3 Recommendation, checked against CLAUDE.md

| Recommendation | Reason | CLAUDE.md |
|---|---|---|
| Do not adopt gem5 for testing Origin. | gem5 provides no UEFI path (R22, R23). Its CPU is below x86-64-v3 and cannot be raised (R20, R21), so Origin halts (E4). It has no NVMe, IOMMU or USB (R24). Booting Origin would need a gem5-only entry path that never ships. | Header, §14, §19, §74, rule 30 |
| Do not use gem5 timings to support performance claims. | Its CPU models are generic (R24), not the 2026 Intel and AMD cores that Origin targets. | §53, rules 21 and 22 |
| Reconsider gem5 only for a specific cache, coherence or pipeline question, and only as a host-only tool. | Its BSD-3-Clause licence (R25) suits host-only use. No gem5 source may enter Origin. | §3, §4, §6, §16, §66 |

### 7.4 Still to verify

- Whether OVMF or another UEFI build runs on gem5's bare-metal workload. It is untested, and Origin would still lack x86-64-v3 there.
- Whether R26's source is public, and whether it supports full-system mode.
- Whether gem5 26.0 builds on the M1 Max.
