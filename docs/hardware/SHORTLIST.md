# Hardware shortlist for the first physical tests

> **2026-11-09:** the owner cannot buy machines for now. See `docs/research/2026-11-09-testing-without-buying-hardware.md` for what can be tested without buying. This list stays as the record of what would be bought if that changes.

| | |
|---|---|
| Answers | Owner sheet item 24 ("Research a shortlist", 2026-11-08) |
| Rules | CLAUDE.md header ("Intel and AMD platforms introduced in 2026 or later"), §17, §74, §76, §80, rule 30 |
| Status | **Research only. Nothing has been bought, booted or tested.** The decision is in `docs/adr/ADR-0009-hardware-targets.md`: Claude chose at the owner's request, 2026-11-08. |
| Method | Vendor product pages, manuals and datasheets where they could be read; press and reseller pages for dates and prices. Prices were seen on 2026-11-08, are undated on the pages themselves, and are mostly US. Availability in your own country was not checked. |

Marketing names only help us find a machine. The kernel decides what a CPU can do from CPUID and ACPI (§24).

## Recommendation

**Start with #2 (Intel) and #5 (AMD), if "introduced in 2026 or later" means the platform and not just the CPU.** On that reading four of the six are in scope. Panther Lake (#2, #3) is the only new client silicon of 2026. Gorgon Point (#5, #6) is a 2026 product line, though a refresh of Zen 5 [48]. Of those four, #3 costs $3,945 and #6 has no serial port, which leaves #2 and #5.

**If a 2026 CPU on an older socket is acceptable, #1 and #4 are much easier first machines.** Both are socketed boards with a COM header and a separate management controller. But their platforms are older: Core Ultra 200S dates from October 2024 [47], the W880 board manual from March 2025 [4], and Ryzen 9000 predates 2026.

The owner asked Claude to decide, and ADR-0009 takes the first reading. A pair is only a start either way. §74 requires several Intel and several AMD models before anything is qualified, and §76 ties any claim to a declared list.

Two warnings about the recommended pair:

- **#5 is the weakest link.** Serial is one of several options for Flex I/O port 1, on selected models only [34]. Lenovo's page gives no type or address for the port and does not say whether it can be added later. It also does not mention the IOMMU or name the 450GE, listing only "AI PRO 400 Series". If that port turns out not to be a plain 16550 at `0x3F8`, this machine has no console for the kernel at all. North American sales were "coming soon" in September 2026 [37].
- **#2 cannot be bought off a shelf.** It is quote-only [15]. Intel lists no-fix errata on this CPU that touch VT-d (PTL003, PTL023) and xHCI (PTL002, 006, 007, 009) [17]. They were reported by the research and not re-read here. They need reading before the first boot, because this kernel depends on VT-d.

## What a first machine needs

| Need | Why |
|---|---|
| A real serial port at I/O address 0x3F8 | The kernel's only console is a 16550 at `0x3F8` (`framekernel/src/main.rs`). Without it, a failed boot shows nothing. *(Since the merge of ADR-0010's work on 2026-11-09, the kernel also draws on the GOP framebuffer and takes the UART's address from the ACPI SPCR when firmware names one, so this is no longer the only path, though a serial line is still the most useful one on a first boot.)* |
| An IOMMU the firmware exposes | §32, §58. VT-d or AMD-Vi with a DMAR or IVRS table. |
| PCIe Gen 5 NVMe | §17 ("Target actual PCIe Gen 5/6 hardware initially"), §33. |
| CET shadow stacks, `x86-64-v3`, x2APIC | §14, §24. CET and real IOMMU behaviour cannot be shown under QEMU. |
| A way to enrol our own Secure Boot keys | §19, §58. |
| Hybrid cores or SMT | §27, §74. |
| USB4 or Thunderbolt | §35. |

**A management controller's serial-over-LAN may not help.** It often appears as a separate PCI serial port. One Intel AMT example sits at `0xE000` [45], which the fixed `0x3F8` console would not reach (inference). Boards #1 and #4 have a separate physical COM header, so this matters less for them.

## The six machines

"Checked" means the claim was re-read on the vendor's own page or manual after the research. "Agent" means it comes from the research alone.

| # | Machine | CPU, launch | Cores | Serial | NVMe | USB4 / TB | Price | Checked |
|---|---|---|---|---|---|---|---|---|
| 1 | ASUS Pro WS W880-ACE SE (desktop board) with Core Ultra 7 270K Plus | Arrow Lake refresh, March 2026 [1][2] | 8P+16E, 24 threads, no SMT | COM header, AST2600 BMC [3][4] | M.2_1 Gen 5 x4, three Gen 4 x4 [3] | Rear TB4, internal header [3] | Board $450–782 [8][9], CPU $290–349 [1][10] | Checked |
| 2 | ASRock Industrial NUC-358H board or iBOX-358H (mini PC) | Panther Lake-H, January 2026 [11][12] | 4P+8E+4LPE, 16 threads | COM1 header, RS-232/422/485 [13][14] | Gen 5 x4 and Gen 4 x4 [13][15] | 1× USB4/TB4 [13][14] | Quote only [15] | Checked |
| 3 | Dell Pro Rugged 14 PR514260 (laptop), Core Ultra 5 335 vPro | Panther Lake, January 2026 [12][21] | 4P+4LPE, 8 threads | Serial port standard, RS-232 options [19] | Gen 4 x4 default, Gen 5 options [19] | 2× TB4 [19] | $3,945 [19] | Agent |
| 4 | ASUS Pro WS B850M-ACE SE (desktop board) with Ryzen 7 9850X3D | Zen 5, January 2026 [24] | 8C/16T, SMT, not hybrid | COM header, AST2600 BMC [25] | M.2_1 Gen 5 x4 confirmed; a second one reported [25] | Internal header only; needs a card (inferred) [25] | Board $360–380 [28][29], CPU $454–499 [31][32] | Checked |
| 5 | Lenovo ThinkCentre M75q Gen 6 (mini PC), Ryzen AI 7 PRO 450GE, Serial Flex I/O | Gorgon Point, March 2026 [35][36] | 8C/16T; one source says 4 Zen 5 + 4 Zen 5c [38] | One choice for Flex I/O port 1, selected models [34] | Gen 4 x4 and x2 [34] | Flex I/O port 2 can be USB4 [34] | From $1,379 [37] | Partly |
| 6 | Lenovo ThinkPad T14 Gen 7 AMD (laptop), Ryzen AI 7 PRO 450 | Gorgon Point, January 2026 [41] | 4 Zen 5 + 4 Zen 5c, 16 threads [40][41] | **None** [39] | Gen 4 x4 [39] | 2× TB4 [39] | $1,852–2,307 [43][44] | Agent |

Facts that decide between them:

- **#1 and #4:** The ASUS page for #1 names Series 2 CPUs only, while Intel says the Plus parts fit all 800-series boards [2]. Check the board's CPU support list before buying. For #4 the CPU list was not checked either.
- **#2:** The CPU page says AMT No [11]. The board's setup screen offers VT-d and Secure Boot menus, and sets the COM port's type and speed but not its address [13].
- **#3:** This is the only laptop here with a serial port, which makes it the way to test sleep and resume with a console (§67). It is also by far the most expensive.
- **#5:** Serial and USB4 are on different Flex I/O ports [34], but ordering both together was not confirmed. Page [34] conflicts with itself on PCIe 5.0 SSDs. AMD lists PCIe 4.0 [35].
- **#6:** No serial path at all, so a poor first machine. It belongs in the matrix later for sleep, resume and docks.

A cheaper Intel board with a COM header is the ASUS Pro Q870M-C V2-CSM: two COM headers, vPro and Gen 5 M.2, but no BMC or USB4, about $220 [54][55]. It has the same older-platform question as #1.

## Against §74

| §74 row | What the six give | Gap |
|---|---|---|
| Multiple Intel models | #1, #2, #3 | None |
| Multiple AMD models | #4, #5, #6 | None |
| Logical processor counts | 8, 16 and 24 | None |
| SMT on and off | AMD: #4, #5, #6. The setup option was not confirmed | Intel: of the 2026 parts found, only Xeon 600 has Hyper-Threading [50] |
| Hybrid scheduling | Intel #1, #2, #3. AMD compact cores on #6, perhaps #5 | Whether AMD's compact cores need hybrid scheduling is unknown until measured |
| NUMA | **None** | Later: a Xeon 600 workstation [49][50] on a board such as the Pro WS W890E-SAGE SE [51]. An AM5 "L3 as NUMA" setting [33] with a two-CCD CPU [52] only tests the ACPI tables |
| RAM capacities | #1 and #4 (DIMM slots), #3 (LPCAMM2) | Sizes to be chosen |
| Multiple NVMe controllers | #1 has four slots; #2 has two | Controllers themselves not chosen |
| PCIe Gen 5/6 | Gen 5: #1, #2, #4 | **Gen 6: none found on a client machine** |
| USB controllers, USB4/TB docks | #1, #2, #3, #6, and #5 if ordered | Docks not chosen. Thunderbolt specifications need an Intel licence (§17) |
| Graphics | Not researched | §36 starts with one GPU target, a separate choice |
| Network, audio | Intel 2.5GbE on #1 and #2; 10GbE and 2.5GbE on #4. Audio on #2 and #6 | Audio controllers not researched |
| Sleep and resume | Laptops #3, #6 | §67 says these need physical testing |
| Power interruptions | #1 and #4, with a switched supply | Rig not designed |

Nova Lake-S has no announced date; leaks say early 2027 [53]. No Zen 6 client launch was found.

## What only a first boot can confirm

The documents leave these open for most or all of the six:

- **The COM port's I/O address.** ASUS's sibling manuals offer `IO=3F8h; IRQ=4` [5][27]. ASRock documents no address [13]. If it is not `0x3F8`, the kernel prints nothing.
- **The DMAR or IVRS table's contents**, and whether RMRR or IVMD entries name devices we need. The kernel refuses those today (sheet items 8 and 12).
- **x2APIC.** It is in Intel's Arrow Lake datasheet [7] and not confirmed for the others.
- **Full `x86-64-v3`.** Intel's pages list AVX2 only.
- **Enrolling our own Secure Boot keys.** MSI's 800-series guides document it [33][46], and ASUS's sibling manuals do too [5][27]. It is not confirmed for these six.
- **CET on AMD.** The source is an AMD engineer's patch saying "Zen3 and newer" [26], not a product page.

AMD.com pages [24][35][40] timed out during research, so those facts come from search excerpts.

## Sources

1. <https://www.intel.com/content/www/us/en/products/sku/245692/intel-core-ultra-7-processor-270k-plus-36m-cache-up-to-5-50-ghz/specifications.html>
2. <https://www.intel.com/content/www/us/en/newsroom/news/client-computing/intel-announces-new-intel-core-ultra-200s-plus-series-desktop-processors.html>
3. <https://www.asus.com/motherboards-components/motherboards/workstation/pro-ws-w880-ace-se/techspec/>
4. <https://dlcdnets.asus.com/pub/ASUS/mb/LGA1851/Pro_WS_W880-ACE_SE/E25890_Pro_WS_W880-ACE_SE_EM_WEB.pdf?model=Pro+WS+W880-ACE+SE>
5. <https://dlcdnets.asus.com/pub/ASUS/mb/LGA1700/Pro_WS_W680-ACE_IPMI/E23138_Pro_WS_W680-ACE_BIOS_Manual_EM_V2_WEB.pdf?model=Pro+WS+W680-ACE>
7. <https://edc.intel.com/content/www/us/en/design/products/platforms/details/arrow-lake-s/core-ultra-200s-series-processors-datasheet-volume-1-of-2/intel-64-architecture-x2apic/>
8. <https://www.shidirect.com/product/50423623/ASUS-Pro-WS-W880-ACE-SE>
9. <https://www.memoryc.com/77732-asus-pro-ws-w880-ace-se-intel-w880-lga-1851-socket-v1-atx.html>
10. <https://pangoly.com/en/product/intel-core-ultra-7-270k-plus>
11. <https://www.intel.com/content/www/us/en/products/sku/245527/intel-core-ultra-x7-processor-358h-18m-cache-up-to-4-80-ghz/specifications.html>
12. <https://download.intel.com/newsroom/2026/CES2026/CES2026-presskit-supporting-quotes.pdf>
13. <https://download.asrock.com/IPC/Manual/NUC-358H.pdf>
14. <https://download.asrock.com/IPC/Download/e-catalog/iBOX-358H.pdf>
15. <https://www.cnx-software.com/2026/02/19/asrock-industrial-nucs-ultra-300-box-series-is-powered-by-intel-core-ultra-5-325-or-ultra-7-358h-panther-lake-soc/>
17. <https://edc.intel.com/output/DownloadPdfDocument?id=11136>
19. <https://www.dell.com/en-us/shop/laptop-computers/spd/dellprorugged14pr514260>
21. <https://www.intel.com/content/www/us/en/products/sku/245727/intel-core-ultra-5-processor-335-12m-cache-up-to-4-60-ghz/specifications.html>
24. <https://www.amd.com/en/products/processors/desktops/ryzen/9000-series/amd-ryzen-7-9850x3d.html>
25. <https://www.asus.com/motherboards-components/motherboards/workstation/pro-ws-b850m-ace-se/techspec/>
26. <https://patchew.org/linux/20250908201750.98824-1-john.allen@amd.com>
27. <https://dlcdnets.asus.com/pub/ASUS/mb/SocketsTR5/Pro_WS_WRX90E-SAGE_SE/E22761_AMD_TR5_Series_BIOS_manual_EM_WEB.pdf?model=Pro+WS+TRX50-SAGE+WIFI>
28. <https://www.centralcomputer.com/asus-ws-pro-ws-b850m-ace-se-matx-motherboard-am5-socket-amd-b850-chipset-dddr5-max-256gb-10-gb-2-5-gb-lan-pcie-5-0.html>
29. <https://www.shidirect.com/product/50701172/PRO-WS-B850M-ACE-SE>
31. <https://www.phoronix.com/news/Ryzen-7-9850X3D-Price>
32. <https://wccftech.com/amd-ryzen-7-9850x3d-drops-to-454-for-the-first-time//amp/>
33. <https://download-2.msi.com/archive/mnu_exe/mb/AMDAM5800BIOS_English.pdf>
34. <https://psref.lenovo.com/syspool/Sys/PDF/ThinkCentre/ThinkCentre_M75q_Gen_6/ThinkCentre_M75q_Gen_6_Spec.html>
35. <https://www.amd.com/en/products/processors/desktops/ryzen-pro/ai-400-series/amd-ryzen-ai-7-pro-450ge.html>
36. <https://newsroom.amd.com/news/amd-gives-consumers-and-businesses-more-ai-pc-opti/>
37. <https://www.notebookcheck.net/Lenovo-releases-new-ThinkCentre-mini-PC-earlier-than-expected-with-64-GB-RAM.1393243.0.html>
38. <https://www.cpu-monkey.com/en/cpu-amd_ryzen_ai_7_450g>
39. <https://psref.lenovo.com/syspool/Sys/PDF/ThinkPad/ThinkPad_T14_Gen_7_AMD/ThinkPad_T14_Gen_7_AMD_Spec.pdf>
40. <https://www.amd.com/en/products/processors/laptop/ryzen-pro/ai-400-series/amd-ryzen-ai-7-pro-450.html>
41. <https://www.notebookcheck.net/AMD-Ryzen-AI-7-PRO-450-Processor-Benchmarks-and-Specs.1198883.0.html>
43. <https://www.cdw.com/product/lenovo-thinkpad-t14-gen-7-amd-ryzen-ai-7/9158096>
44. <https://www.bhphotovideo.com/c/product/1987088-REG/lenovo_21wv0006us_14_thinkpad_t14_gen.html>
45. <https://manpages.debian.org/trixie/amtterm/amt-howto.7.en.html>
46. <https://download-2.msi.com/archive/mnu_exe/mb/Intel800BIOS_English.pdf>
47. <https://www.tomshardware.com/pc-components/cpus/intel-launches-arrow-lake-core-ultra-200s-big-gains-in-productivity-and-power-efficiency-but-not-in-gaming>
48. <https://www.tomshardware.com/pc-components/cpus/amds-ryzen-ai-400-series-includes-the-first-copilot-desktop-cpu-team-red-refreshes-zen-5-apus-and-strix-halo>
49. <https://cdrinfo.com/d7/content/intel-launches-new-intel-xeon-600-processors-workstation>
50. <https://www.intel.com/content/www/us/en/products/sku/244336/intel-xeon-654-processor-72m-cache-3-10-ghz/specifications.html>
51. <https://www.asus.com/motherboards-components/motherboards/workstation/pro-ws-w890e-sage-se/techspec/>
52. <https://www.thefpsreview.com/?p=149310>
53. <https://www.tomshardware.com/pc-components/cpus/intels-core-ultra-400-nova-lake-launch-schedule-leaks-out-mass-production-in-q4-first-nova-lake-cpus-in-q1-2027>
54. <https://www.asus.com/us/motherboards-components/motherboards/csm/pro-q870m-c-v2-csm/techspec/>
55. <https://www.shidirect.com/product/52068477/ASUS-PRO-Q870M-C-V2-CSM-INTEL-Q870-MICRO-ATX-(MATX)-INTEL-VPRO-AI>

Source numbers follow the research list, so gaps are sources this page does not cite.
