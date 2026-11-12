# The first boot on a rented bare-metal machine: a plan

| | |
|---|---|
| Date | 2026-11-12 |
| Kind | Research ledger (CLAUDE.md §16). Documents only: nothing was built, signed up for, paid, downloaded or booted |
| Asked by | The owner, on 2026-11-12: "Yes, plan it", to the question whether bare metal is possible for testing |
| Origin read at | `land/s3-part2` at `e6456dc` (pull request 16, the second step-3 batch, not yet on `main`), read with `git show`; `main` at `405495c` for CLAUDE.md 7.6, which is the same on both |
| Starts from | `docs/research/2026-11-09-testing-without-buying-hardware.md`, which priced the first candidates on 2026-11-09 |
| Prices | Excluding tax. Rupees at the ECB rate of 2026-11-11, read on 2026-11-12: $1 = ₹93.69, €1 = ₹104.25 ([Frankfurter, USD](https://api.frankfurter.dev/v1/latest?base=USD&symbols=INR,EUR), [EUR](https://api.frankfurter.dev/v1/latest?base=EUR&symbols=INR)) |
| Owner's decisions | Section 6, unnumbered: the coordinating session numbers them |
| Decided | On 2026-11-12 the owner accepted all eleven with the recommended answer ("Accept all 11"), as decision items 224–234, in section 6's order. This ledger is kept as it was put. |
| Corrected | 2026-11-12, after the answer: **the hourly prices below are wrong.** Hourly billing is $0.81 an hour for `m4.metal.small` in the US, Canada, Germany and the Netherlands ($0.96 in Japan and Singapore, $1.11 elsewhere), and $6.04 for `f4.metal.large`, which is in stock only in Singapore and Tokyo ($7.49 in São Paulo). These were read from Latitude's API (`/plans`, `pricing.USD.hour`) by the bare-metal session with the owner's key. The pricing page's "$0.41/hr" is the monthly price, $296, over 730 hours. So rental A costs about $0.81 (at most $1.62 if two clock hours are billed), rental B about $2.43, and rental C's worst case passes item 225's $10 cap. Section 3's table is left as put. |

**How to read it.** **Page** means a provider's or vendor's own page, read on 2026-11-12 at the address given. **Repo** means a file at `land/s3-part2` (`e6456dc`) unless another revision is named. **Reasoning** means no source says it. Hetzner's and the other providers' pages were read for this ledger by research subagents of this session, which quoted each page; Latitude.sh's, AMD's and GitHub's pages were read directly. Nobody signed up with any provider.

## Summary

- **Origin is not ready to boot on real hardware by its own rules, and a provider's rescue Linux is.** The owner set a deadline on 2026-11-08: identity maps for firmware-reserved memory "must land before real hardware" (ADR-0007 questions 4 and 8, decision items 8 and 12). They do not exist. Of the seven IOMMU threats ADR-0009 H6 says must be closed or accepted first, none is closed and none is accepted. The console H6 asks for is built, under QEMU only.
- **The first rental should boot no Origin code.** One hour on the provider's rescue Linux copies the ACPI tables, shows the boot mode, the IOMMU tables and the serial port, and reads what the processor reports. It risks nothing, and its DMAR or IVRS gives the identity-map work a real table to be tested against.
- **The provider is Latitude.sh, as on 2026-11-09.** Its `m4.metal.small` is still $0.41 an hour (₹38.4), with no setup fee shown and a remote console "included at no cost". It is still the only hourly provider read whose documentation promises a serial console from the firmware onward. Its documentation still says nothing about UEFI, Secure Boot or the IOMMU.
- **Nothing rentable is inside the declared scope.** No host offers a 2026 Intel platform, or a 2026 AMD platform with Zen 5 or newer cores, as bare metal that boots one's own loader. The nearest are Intel's request-only Panther Lake machines, which run Intel's choice of system, and Azure's preview of virtual machines on AMD's Venice. One thing is new since 2026-11-09: Vultr rents a Zen 5 EPYC 4245P for $0.41 an hour, but with a video console only.
- **A boot there would be bench evidence.** It would show the speculation controls written and read back on silicon, which moves I2 forward without closing it. It qualifies nothing (CLAUDE.md §74, ADR-0009 A1).
- **A self-hosted GitHub runner is not proposed.** The repository is public, and GitHub's own guidance says such runners "should almost never be used for public repositories".

## 1. What must be true of Origin first

### 1.1 The prerequisites, one by one

| # | Prerequisite | Where it is required | State at `e6456dc` | Shown by |
|---|---|---|---|---|
| P1 | **Identity maps for firmware-reserved memory**: RMRRs (Intel) and IVMD unity maps (AMD) | ADR-0007 question 4 (I11) and question 8 (A9): "identity maps must land before real hardware"; decision items 8 and 12; `THREAT_MODEL_IOMMU.md` T14 | **Not done.** A device a reserved region names is refused at attach, so its DMA stays blocked | `docs/STATUS.md` line 146: "Not yet: … RMRR identity maps and IVMD unity maps"; ADR-0007 "Consequences": "Not yet: … RMRR identity maps"; TASKS item 11 ("IVMD semantics and placement, with question 4's identity maps") |
| P2 | **The IOMMU threat model's list "before real hardware"**: T4, T5, T6, T8, T9, T14, T23 | ADR-0009 H6, second bullet: "Close or explicitly accept"; `THREAT_MODEL_IOMMU.md`, "What must be closed before real hardware" | **None closed, none accepted.** T4 Partly (proven in QEMU for a function that honours Bus Master Enable; two windows remain). T5 Open (a unit the firmware left enabled is refused, which leads to T6). T6 Partly (fail-open if bring-up fails). T8 and T9 Open (no ACS; QEMU cannot show them). T14 By design (refused; see P1). T23 Open (runtime faults are never read) | The threat model's rows; no decision item accepts any of them for a first boot (`OWNER_DECISIONS.md` searched for T4–T23 on 2026-11-12) |
| P3 | **Errata read before the first boot** | ADR-0009 H6, first bullet: Intel's no-fix errata PTL003, PTL023 (VT-d) and PTL002, 006, 007, 009 (xHCI) | **Not done**, and written for Panther Lake, not for a rented machine. Reasoning: the same duty applies to whatever processor is rented, so the rented processor's own revision guide is read before Origin boots on it | ADR-0009 H6: "nobody has re-read them" |
| P4 | **A console that does not need a 16550 at `0x3F8`** | ADR-0009 H6, third bullet; ADR-0010 | **Done under QEMU only.** The framebuffer from the Graphics Output Protocol, and the UART the ACPI SPCR names, with earlier output replayed there (E4). A UART in memory space is tested on the build host only, with mocks | ADR-0010 status (items 27–29, 52, 53); `cargo xtask boot-console`; ADR-0010 "What this does not prove" |
| P5 | **The boot path** | ADR-0002 B1–B8 | **Done under QEMU, in developer mode.** The loader reads `\EFI\ORIGIN\KERNEL.ONBF` from the volume it was started from (B1), verifies it and enters the kernel. A developer-mode loader accepts an unsigned kernel and says so (B8) | `docs/STATUS.md` M1 row: "done (developer mode)" |
| P6 | **Secure Boot** | ADR-0002 B7 and Amendment 1 (Origin's own keys enrolled in `db` by default, item 78) | **Not built.** No release key exists (errata V4), the loader binary is not signed for the firmware, and the installer that would enrol Origin's keys does not exist. So Origin can boot only with Secure Boot off | ADR-0002 "Consequences"; Amendment 1, "It is not yet built" |
| P7 | **ADR-0017's speculation controls** (row I2) | `THREAT_MODEL_SYSCALLS.md` row I2 and "What must be closed", item 3 | **Built, never exercised.** Z1, Z2, Z3, Z5, Z9 and Z10 are code (task 19b). No control has ever been written on a processor that has it: QEMU's software processor and CI's KVM guests offer none. **I2 stays Partly until a boot on a processor in the declared scope shows each one read back as written** (item 213). The "speculation controls" proof is a Report row, so a machine that lacks a control still boots (item 131), and every boot without the controls ends `degraded` (item 154) | Row I2; ADR-0017 "As built"; decision items 131, 154, 213 |
| P8 | **The nested NMI** (row E12) | `THREAT_MODEL_SYSCALLS.md`: "Before any claim about real hardware: E12" | **Open.** The guard for the IDT path (ADR-0017 Z12) and FRED (Z6) are decided and not built (task 19c not started). What keeps the window shut today is how little the NMI handler does | Row E12 |
| P9 | **The scope line**: the kernel says at boot whether the machine is in the declared scope or bench equipment | ADR-0009 Amendment 1, A2 (item 89) | **Not built.** The amendment says "Nothing here is built", and no such line is in `framekernel` (searched 2026-11-12). The record must therefore say it by hand, from CPUID | ADR-0009 A1, A2 |

**What this means (Reasoning).** P1 is the owner's own deadline and it is not met, so Origin should not boot on a rented machine until either identity maps land or the owner changes the deadline. P2 asks for each listed threat to be closed or accepted by name. P7 and P8 do not stop a boot. They stop a *claim*, and section 4.2 says what a bench record may say. P3, P6 and P9 cost a step of the procedure each, not a delay: read the rented processor's errata, turn Secure Boot off, and write the scope verdict into the record.

### 1.2 What the kernel does to a machine that is not QEMU

The owner's money buys hours on someone else's hardware, so what Origin does to it matters (P0). From the code at `e6456dc`:

- **Disks.** The boot proofs write only to the boot test's own disk, "named positively" by its GPT disk GUID `28a71590-…-8dff96984d99` and its size of 32,768 blocks of 512 bytes. "Any other disk gets no write and no flush during boot" (`framekernel/src/storage.rs`, `framekernel/src/blockcache.rs`, `test_disk`). A rented disk does not match. So the NVMe controller is started and read, and nothing is written.
- **UEFI variables.** Neither the loader nor the kernel calls `SetVariable` (searched `boot/` and `framekernel/` on 2026-11-12).
- **Network.** The only network driver is virtio-net (`framekernel/src/virtionet.rs`). A physical network card gets no driver, so Origin sends nothing on the provider's network.
- **PCI devices.** Bus mastering is cleared on every function at enumeration, bridges included (ADR-0007 I12, item 30). Reasoning: a platform reset sets this back, and the server is reset before anyone else uses it.
- **Processor registers.** The speculation controls are written to model-specific registers that a reset clears (ADR-0017 Z1).
- **The end of a boot.** The kernel prints a verdict and halts. It never resets the machine (ADR-0013 R5, R6). Someone must power-cycle it from the provider's dashboard.
- **Processors.** At most 64 are started, and the rest stay in the firmware (`MAX_CPUS` in `framekernel/frame/src/limits.rs`).
- **The boot volume.** `cargo xtask` hands QEMU the boot files as a directory (QEMU's `fat:rw:` drive). It has no command that writes a FAT file system, so on a real machine the EFI system partition is made by the provider's rescue Linux (section 3.2).

Reasoning: nothing here should leave the rented machine changed after a reset, apart from the partition rental B writes on the second disk (section 3.2), and the provider erases every disk after deletion (section 2.1).

## 2. Rental options, re-checked on 2026-11-12

### 2.1 Latitude.sh: the first choice, as on 2026-11-09

All from Latitude.sh's own pages, read on 2026-11-12. The documentation pages were read as text through the index that the site publishes for that purpose ([llms.txt](https://www.latitude.sh/docs/llms.txt)).

| Question | Answer | Source |
|---|---|---|
| Machine | `m4.metal.small`: "AMD 4244P", 6 cores at 3.8 GHz, 48 GB, 2 × 960 GB NVMe. Unchanged since 2026-11-09 | Page: [pricing](https://www.latitude.sh/pricing) |
| Price | **$0.41 an hour (₹38.4)**, or $296 a month. Hourly prices are worked out "based on an average of 730 hours each month" | Page: [pricing](https://www.latitude.sh/pricing); [on demand vs. reserved](https://www.latitude.sh/docs/billing/on-demand-vs-reserved.md) |
| Setup fee | None shown | Page: [pricing](https://www.latitude.sh/pricing) |
| Console fee | None: "Remote Access is included at no cost" | Page: [remote access](https://www.latitude.sh/docs/servers/remote-access.md) |
| Stock | **Not visible without an account.** The pricing page shows the plan in "+7" locations, as a count. Stock is reported per region by the `/plans` API, which answered 401 (needs a key). Where a plan is out of stock the deploy page offers "Join waitlist" | Page: [pricing](https://www.latitude.sh/pricing); `https://api.latitude.sh/plans` returned HTTP 401; [deploying a server](https://www.latitude.sh/docs/servers/deploying-a-server.md) |
| Serial console | **Yes.** "Out-of-Band" is serial-over-LAN through SSH: "see and interact with the system before and during boot, including BIOS, GRUB, and early kernel output"; the BIOS "appears in text mode" | Page: [out-of-band](https://www.latitude.sh/docs/servers/out-of-band.md) |
| Graphical console | An HTML5 IPMI console in the browser. "Some server models don't support remote access" | Page: [remote access](https://www.latitude.sh/docs/servers/remote-access.md) |
| Rescue Linux | "a lightweight Ubuntu image is loaded into your server's memory", with root credentials shown on the server page. "Rescue mode is not available at certain locations" | Page: [rescue mode](https://www.latitude.sh/docs/servers/rescue-mode.md) |
| Custom image | Through an iPXE script at a public address; the server can report "On" before the image has finished | Page: [custom images](https://www.latitude.sh/docs/servers/custom-images.md) |
| UEFI | **Not documented.** The whole documentation (572 KB as published in [llms-full.txt](https://www.latitude.sh/docs/llms-full.txt)) contains no "UEFI", "Secure Boot", "IOMMU" or "boot mode". So whether the machine boots in UEFI mode, and whether a custom loader can run, is unknown until the rescue hour | Page, searched 2026-11-12 |
| Which serial port | Not documented. Reasoning: Latitude's own Harvester guide passes `console=ttyS1,115200n8` to the kernel it boots, which suggests the serial-over-LAN port is the second COM port on at least those servers ([Harvester guide](https://www.latitude.sh/docs/guides/private-cloud-harvester.md)) | Page and Reasoning |
| What the owner gives | A credit card with a **$5 hold (₹468)**, "refunded after 7 days"; then **either** a government ID through Stripe Identity **or** "$100 in credits" (₹9,369) charged to the card; possibly a phone check "via WhatsApp" or a manual review. New accounts start with a "$100 spending limit" on hourly plans | Page: [verify account](https://www.latitude.sh/docs/account-setup/verify-account.md); [payment methods](https://www.latitude.sh/docs/billing/payment-methods.md) |
| Deleting | Immediate from the dashboard, or scheduled. "It may take up to an hour for the deletion to reflect on your billing summary" | Page: [deleting a server](https://www.latitude.sh/docs/servers/deleting-a-server.md) |
| After deletion | Every disk, the boot disk included, is erased by its own sanitize command and checked before reuse | Page: [disk sanitization](https://www.latitude.sh/docs/servers/disk-sanitization.md) |
| Billing trap | Monthly billing gives no refund on deletion; choose hourly | Page: [on demand vs. reserved](https://www.latitude.sh/docs/billing/on-demand-vs-reserved.md) |

**The processor.** AMD's page puts the EPYC 4244P in the "EPYC 4004 Series", platform AM5, 6 cores and 12 threads ([AMD, 4244P](https://www.amd.com/en/products/processors/server/epyc/4004-series/amd-epyc-4244p.html)), and the series page says it extends the "Zen4" core architecture ([AMD, 4004 series](https://www.amd.com/en/products/processors/server/epyc/4004-series.html)). Zen 4 is below ADR-0009 A1's AMD line on both counts: it is not Zen 5 or newer, and it launched before 2026.

**Latitude's Zen 5 machines.** The same pricing page lists three plans on EPYC 9005 processors: `f4.metal.large` (EPYC 9275F, $2.62 an hour, ₹245), `c4.metal.large` (9655P, $4.00) and `m4.metal.xlarge` (9455P, $4.76). AMD gives the 9275F a launch date of 10 October 2024 and the 9005 series "Zen 5" cores ([AMD, 9275F](https://www.amd.com/en/products/processors/server/epyc/9005-series/amd-epyc-9275f.html), [AMD, 9005 series](https://www.amd.com/en/products/processors/server/epyc/9005-series.html)). They are Zen 5 but launched in 2024, so they are bench equipment too. Reasoning: at six times the price, the one thing they add to a first boot is a Zen 5 core, which is why rental C (section 3.3) uses one for an hour and no more.

### 2.2 Hetzner and the other hosts

All pages read on 2026-11-12. "Serial" means a console that carries text from the firmware onward, such as serial-over-LAN. A video console shows the screen only, so nothing can be copied out of it but pictures.

| Machine | Processor | Per hour | Setup fee | Console, and its fee | Serial | UEFI and own loader | What the owner gives | Sources |
|---|---|---|---|---|---|---|---|---|
| **Hetzner EX44** | Core i5-13500 (2023), hybrid | €0.0945 (₹9.9); capped at €59 a month | €0 | Video only, attached by a technician. **Two pages disagree on the fee:** "free for 3 hours" (last changed 2025-10-01) against "10.00 one-time … charged each time" (last changed 2026-10-07). Budget €10 (₹1,043) | **No.** Hetzner's serial-over-LAN page covers only old models (last changed 2020) | UEFI by default on new models since 2020. A custom OS goes through an ISO on the console; Hetzner gives no support for it. Secure Boot: "You can enable it, but we do not support it" | "May" ask for a government ID or an advance card payment | [EX44](https://www.hetzner.com/dedicated-rootserver/ex44/), [console](https://docs.hetzner.com/robot/dedicated-server/maintenance/kvm-console/), [price list](https://docs.hetzner.com/robot/general/pricing/price-list-for-additional-products/), [serial-over-LAN](https://docs.hetzner.com/robot/dedicated-server/maintenance/serial-over-lan/), [UEFI](https://docs.hetzner.com/robot/dedicated-server/operating-systems/uefi/), [custom images](https://docs.hetzner.com/robot/dedicated-server/operating-systems/installing-custom-images/), [fraud FAQ](https://docs.hetzner.com/general/security-and-identify/fraud-prevention-faq/) |
| Hetzner EX63 | Core Ultra 7 265 (Intel gives Q1 2025) | €0.2388 (₹24.9) | €74 (₹7,715) | As EX44 | No | As EX44 | As EX44 | [EX63](https://www.hetzner.com/dedicated-rootserver/ex63/) |
| **Vultr vbm-6c-32gb-amd** (new since 2026-11-09) | **EPYC 4245P: EPYC 4005 series, "Zen5", launched 13 May 2025** | **$0.41 (₹38.4)**, or $295 a month; 20 locations, none in India | None seen | Video (VNC in the browser), reaches the firmware's setup | **No** | An ISO "Legacy (BIOS) or UEFI" is written with `dd` to the second disk; iPXE "does not support EFI images" on bare metal | A card; no ID check documented. The 2026-11-09 ledger cites one review reporting ID requests | [plans API](https://api.vultr.com/v2/plans-metal) (read directly), [custom ISO](https://docs.vultr.com/custom-iso-on-bare-metal), [iPXE](https://docs.vultr.com/ipxe-boot-feature), [account](https://docs.vultr.com/platform/create-an-account), [AMD, 4245P](https://www.amd.com/en/products/processors/server/epyc/4005-series/amd-epyc-4245p.html), [AMD, 4005 series](https://www.amd.com/en/products/processors/server/epyc/4005-series.html) |
| Vultr vbm-4c-32gb | E3-1270 | $0.164 (₹15.4) | None seen | As above | No | As above | As above | [plans API](https://api.vultr.com/v2/plans-metal) |
| Cherry Servers E3-1240v5 | E3-1240v5 | €0.101 (₹10.5); 2 in stock in Lithuania | None stated | Video IP-KVM, free, at most 24 hours a session | No | iPXE is a beta; UEFI not mentioned | Passport or ID card and a camera | [plans API](https://api.cherryservers.com/v1/plans), [console](https://www.cherryservers.com/knowledge/docs/compute/configuration-management/out-of-band-management-console), [iPXE](https://www.cherryservers.com/knowledge/docs/compute/how-to/how-to-deploy-a-custom-operating-system-using-ipxe), [ID](https://www.cherryservers.com/knowledge/docs/usage-billing/identity-verification) |
| Scaleway EM-A116X-SSD | Xeon E3 1220 | €0.077 (₹8.0) | "No setup fee" | Video KVM; its fee is "Free" on one page and billed on another | No | Not documented; "Users are not allowed to change the BIOS settings" | A card; ID for most models | [pricing](https://www.scaleway.com/en/pricing/elastic-metal/), [FAQ](https://www.scaleway.com/en/docs/elastic-metal/faq/), [quotas](https://www.scaleway.com/en/docs/organizations-and-projects/organization/organization-quotas/) |
| OVHcloud Eco | Old Xeons; RISE-S Ryzen 7 9700X | Monthly only, from €9.99 plus the same again as setup; most unavailable | One month | IPMI with serial-over-LAN, but "might be unavailable or limited" on Eco machines | Possibly | Not checked | Not checked | [catalogue](https://eu.api.ovh.com/1.0/order/catalog/public/eco?ovhSubsidiary=IE), [availability](https://eu.api.ovh.com/1.0/dedicated/server/datacenter/availabilities), [IPMI](https://docs.ovhcloud.com/en/guides/bare-metal-cloud/dedicated-servers/ipmi) |
| phoenixNAP s0.d1.small | E3-1240v3 | $0.08 (₹7.5); one hour minimum | None mentioned | A web console; whether serial or video is not stated | Unknown | iPXE "at no additional cost"; UEFI not stated | Not documented | [pricing JSON](https://phoenixnap.com/wp-json/pnap/v1/bmc-pricing), [billing](https://phoenixnap.com/kb/phoenixnap-bare-metal-cloud-billing-models), [iPXE](https://phoenixnap.com/kb/ipxe-bare-metal-cloud), [console](https://phoenixnap.com/kb/bmc-remote-console) |

**What changed since 2026-11-09.**
- **A Zen 5 machine now costs the same as Latitude's Zen 4 one.** Vultr's EPYC 4245P is $0.41 an hour, exactly Latitude's 4244P. Zen 5 is the core of Ryzen AI 400 "Gorgon Point", which is in the declared scope (ADR-0009 A1), so its controls are the nearest that can be rented to an in-scope AMD core. Reasoning: it still reads as bench equipment, because the 4245P launched in 2025 (A1), and ADR-0009 A2 notes that the kernel's CPUID rule cannot tell a 2024–2025 Zen 5 from a 2026 one. Its console is video only.
- **Hetzner's console fee is still contradicted** by its own pages, as on 2026-11-09.
- Prices for Latitude, Hetzner EX44 and Vultr's E3 are unchanged.

**Reasoning: why Latitude stays first.** It is the only hourly provider read on either date whose documentation promises serial output from the firmware through the early kernel. A serial log is text that can be kept, compared and committed. A video console gives pictures of the last screenful, and Origin's lines scroll. The Zen 5 machines that have a serial console cost $2.62 an hour at Latitude; the one at $0.41 has none.

### 2.3 Is anything rentable inside the declared scope?

**No. Nothing is.** No provider read on 2026-11-12 offers, as bare metal with a documented way to boot one's own UEFI loader, an Intel platform first launched in 2026 or later or an AMD platform first launched in 2026 or later with Zen 5 or newer cores. The nearest things:

| What | Processor | In scope? | Why it does not serve | Source |
|---|---|---|---|---|
| Intel Cloud Services, `bm-ptl` | Panther Lake (Core Ultra Series 3), bare metal | Yes | Request-only, approved case by case, a reservation of at most 30 days, and "Windows* OS or Linux* OS launches in your browser". Nothing in its documentation mentions one's own image, UEFI or a console. Its public code now also lists `bm-nvl`, Nova Lake, request-only | [how to request](https://cloud.intel.com/docs/how_to_request.html), [console code](https://cloud.intel.com/assets/index-DL8eaOfS.js) |
| Azure HDv2 | AMD EPYC "Venice" (Zen 6) | The processor is | A virtual machine, not bare metal, in "preview by registration" (posted 2026-11-10); no price or regions | [Microsoft](https://techcommunity.microsoft.com/blog/azurehighperformancecomputingblog/accelerating-ai-data-processing-hpc-and-agentic-workloads-with-azure-hdv2-virtua/4567877) |
| Oracle E7 | Venice | The processor is | Not yet offered. AMD wrote on 2026-11-10 that Azure and OCI instances "will be generally available soon". Oracle's shapes page lists `BM.Standard.E6` as its newest AMD bare metal, and Oracle bars custom images on bare metal (2026-11-09 ledger) | [AMD](https://ir.amd.com/news-events/press-releases/detail/1305/), [Oracle shapes](https://docs.oracle.com/en-us/iaas/Content/Compute/References/computeshapes.htm) |
| Xeon 6+ "Clearwater Forest" | Intel 2026 | Yes | No cloud or hosting offer found | [Intel, 6990E+](https://www.intel.com/content/www/us/en/products/sku/246074/intel-xeon-6990e-processor-576m-cache-2-20-ghz/specifications.html) |

The Azure, Oracle, AWS, Google and Intel Cloud facts were read by a research subagent of this session on 2026-11-12. No rental service for Panther Lake or Ryzen AI 400 laptops or mini-PCs with permission to boot one's own system was found.

**Found, not changed.** Intel's specification page for the Xeon 6990E+ gives "Launch Date | Q2'26" (read directly, 2026-11-12). ADR-0009 Amendment 1's table says Clearwater Forest was "introduced March 2026". It is in the scope either way. The date in the ADR's table is for the coordinating session to correct.

**What follows (Reasoning).** Every machine in this plan is bench equipment. The owner's earlier decision stands: buying is off, so the declared machines remain out of reach, and the result of any rental qualifies nothing.

## 3. The plan, step by step

Two rentals, perhaps three, all on one Latitude.sh account, each deleted the day it is made.

| Rental | Machine | Boots | When | Hours planned | Cost planned |
|---|---|---|---|---|---|
| A | `m4.metal.small` (EPYC 4244P, Zen 4) | The provider's rescue Linux only | Once the owner has signed up | 2 | $0.82 (₹77) |
| B | `m4.metal.small` | Origin | After P1 lands and the owner has decided on P2 (section 6) | 3 | $1.23 (₹115) |
| C (optional) | `f4.metal.large` (EPYC 9275F, Zen 5) | Origin, the same image as B | After B, if B's console worked | 2 | $5.24 (₹491) |
| | | | | **Total** | **$7.29 (₹683)** |

The hours allow for deploying the server and loading rescue mode. Latitude gives 10 to 30 minutes for a custom image and no figure for its own systems ([custom images](https://www.latitude.sh/docs/servers/custom-images.md)). Rental A may get a different physical machine from B, so each records its own board and firmware.

### 3.1 Rental A: one hour on rescue Linux, no Origin code

**Before it (free).** Write the commands below into a checklist file outside the repository, and give the owner the steps for the dashboard.

**On the dashboard (the owner).** Deploy `m4.metal.small` with **hourly** billing and any Ubuntu, with an SSH key made for this rental only. Set a root password, which out-of-band login needs. Enable Out-of-Band, then put the server into rescue mode.

**On the Mac.** Capture the serial console from the moment it connects: `script -q rental-a-sol.log ssh -p 2222 <server_id>@<server_ip>` (the address comes from the dashboard; [out-of-band](https://www.latitude.sh/docs/servers/out-of-band.md)).

**In rescue Linux, reading only.** Each answers a question the Origin boot depends on.

| Question | Command (Ubuntu's own tools) | What it decides |
|---|---|---|
| UEFI or legacy BIOS? | `test -d /sys/firmware/efi && cat /sys/firmware/efi/fw_platform_size` | If legacy, stop: ask Latitude support whether UEFI can be enabled. Origin's loader is UEFI only (CLAUDE.md §19) |
| Secure Boot on? | `od -An -tu1 /sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c` (last byte 1 means on) | If on, it must be turned off for rental B (P6) |
| Which tables? | `ls -l /sys/firmware/acpi/tables/` | Whether IVRS (AMD) or DMAR (Intel), SPCR, MADT, SRAT, SLIT, HPET and MCFG exist |
| The tables themselves | `cp` each from `/sys/firmware/acpi/tables/` and `/sys/firmware/acpi/tables/data/`, then `sha256sum` | Copied to the Mac over SSH (section 6, item on what is kept). Origin's own parsers can then be run on them in host tests, uncommitted (Reasoning) |
| Which UART is the serial-over-LAN port? | `cat /proc/tty/driver/serial`; then `echo port0 > /dev/ttyS0; echo port1 > /dev/ttyS1` while watching the console | Whether SPCR names the port the console carries. Origin starts on COM1 at `0x3F8` and moves to the SPCR's UART once it reads the ACPI tables (ADR-0010 E4) |
| The processor | `lscpu`; `grep -m1 microcode /proc/cpuinfo`; `grep -o -w -e ibpb -e ibrs -e stibp -e ssbd -e auto_ibrs -e user_shstk -e x2apic /proc/cpuinfo \| sort \| uniq -c`; `grep . /sys/devices/system/cpu/vulnerabilities/*` | Family and model for the scope verdict (P9) and the errata to read (P3); what ADR-0017 Z1 should find |
| What the firmware left on | `dmesg \| grep -i -e AMD-Vi -e IOMMU -e DMAR -e x2apic -e SPCR -e 'efi:'` | Whether an IOMMU was left enabled (T5) and what the memory map holds |
| Reserved memory | `grep -i reserved /proc/iomem` (as root) | Where an IVMD or RMRR range sits, for the identity-map work (P1) |
| Devices | `lspci -nn`, `lspci -vvv` saved; read for ACS capabilities and the BMC's display device | T8 and T9; whether the screen comes from a BMC's own memory or by DMA (T14) |
| Firmware version | `dmidecode -t 0 -t 2` | The record's Firmware row. Serial numbers and UUIDs are not copied out |
| Boot entries | `efibootmgr -v` (reads only) | How the firmware will find a disk's loader in rental B |

**In the firmware setup over the serial console, looking only.** Reboot from rescue and press the setup key the console shows ("Enter BIOS (DEL/ESC)", [out-of-band](https://www.latitude.sh/docs/servers/out-of-band.md)). Note the IOMMU setting, SVM, Secure Boot, the boot mode, and the console redirection's port and speed. Change nothing.

**After it.** Delete the server (section 3.5). Write a research ledger with the facts in Origin's own words, and read the rented processor's revision guide from AMD (P3).

### 3.2 Rental B: Origin's first boot on a physical machine

**Before it.**
- P1 has landed, and the owner has decided on P2.
- Build from `main` once pull request 16 has merged: `cargo xtask build` prints the paths of the developer-mode loader and `KERNEL.ONBF`. The loader is copied as `BOOTX64.EFI`, as xtask's own staging does for QEMU (`stage` in `developer-tools/xtask/src/main.rs`). Record the commit and both files' SHA-256.
- Fill in the scope verdict by hand from rental A's CPUID, by ADR-0009 A2's rule. For the 4244P, Zen 4 is expected to read "AuthenticAMD, family 19h: older than Zen 5; bench equipment".

**Putting Origin on the disk.** Latitude's custom-image route is an iPXE script fetched from a public address ([custom images](https://www.latitude.sh/docs/servers/custom-images.md)). It is not used. Origin's loader reads `\EFI\ORIGIN\KERNEL.ONBF` from the volume it was started from (ADR-0002 B1), and an iPXE download is not such a volume as far as iPXE's own command reference says ([imgfetch](https://ipxe.org/cmd/imgfetch), last modified 2025-02-24). Instead:

1. Deploy as in rental A, enable Out-of-Band and start the capture (`rental-b-sol.log`), and enter rescue mode.
2. Copy the two files to rescue Linux with `scp`.
3. Make an EFI system partition on the **second** disk. The first keeps the provider's system, so the server can still start something if Origin's disk is not found (Reasoning). With Ubuntu's tools, installed by `apt` if missing:

   ```sh
   sgdisk --zap-all /dev/nvme1n1
   sgdisk -n 1:1MiB:+256MiB -t 1:EF00 -c 1:ORIGIN-ESP /dev/nvme1n1
   mkfs.vfat -F 32 -n ORIGIN /dev/nvme1n1p1
   mount /dev/nvme1n1p1 /mnt
   mkdir -p /mnt/EFI/BOOT /mnt/EFI/ORIGIN
   cp BOOTX64.EFI /mnt/EFI/BOOT/ && cp KERNEL.ONBF /mnt/EFI/ORIGIN/
   sha256sum /mnt/EFI/BOOT/BOOTX64.EFI /mnt/EFI/ORIGIN/KERNEL.ONBF
   umount /mnt
   ```

   Check the disk's name with `lsblk` first. The names above are an example.
4. Reboot, and pick the second disk's UEFI entry once from the firmware's boot menu over the serial console. This writes no boot entry of Origin's. If the menu has no such entry, stop: adding one with `efibootmgr` writes a UEFI variable on the provider's machine, which section 6 recommends against.

**What the console should show (Reasoning, from the code).**
- The firmware's text screens, through its console redirection.
- The loader's `origin-boot:` lines. The loader writes through the firmware's text output (`boot/uefi/src/console.rs`), so the firmware's redirection carries them.
- The kernel's lines on COM1 until it reads the ACPI tables, then on the SPCR's UART with the earlier lines replayed (ADR-0010 E4). If the serial-over-LAN port is not COM1 and the SPCR does not name it, the kernel's lines appear only on the browser console's screen, and the record is built from pictures.
- The verdict on every console before the halt: `origin-kernel: boot verdict: …` (ADR-0013 R6).

**Then.** Power-cycle from the dashboard and boot the same image twice more, three boots in all, to see what varies. Then delete the server.

**If something fails.** A boot that shows nothing on either console after the loader's lines is recorded as such, with the last line seen. It is not debugged while the meter runs: delete the server, and read the log afterwards.

### 3.3 Rental C (optional): the same image on a Zen 5 core

One deployment of `f4.metal.large` (EPYC 9275F, 24 Zen 5 cores, $2.62 an hour), on the same account and with the same steps as rental B, only to read ADR-0017's controls back on a Zen 5 core. Reasoning: Zen 5 is the core of Ryzen AI 400, which is in scope, so this is the nearest to an in-scope AMD core that can be rented with a serial console. The kernel starts at most 64 processors, and this machine has 48 threads, so all of them start. Its record says bench equipment too: by ADR-0009 A2's rule it reads as family 1Ah, "Zen 5 or newer", but it launched in 2024, which A2 says the CPUID rule cannot see.

### 3.4 What each boot proof can and cannot show there

**What it can show for the first time (Reasoning, from ADR-0013's inventory).** Every line is from a machine outside the scope.

| Area | Boot proofs | What a physical machine adds to QEMU and to CI's KVM guests |
|---|---|---|
| Firmware hand-off | Relocations, boot info, page tables, frame allocator | A real UEFI memory map and a real Graphics Output Protocol, from firmware that QEMU's team did not write |
| ACPI | CPU topology, NUMA, clock sources, the console's SPCR | Tables written by a board's maker, parsed as untrusted input |
| Processors | Application processors, NMI delivery, TLB shootdown, TSC consistency, preemption | A real APIC with no hypervisor between, on 12 threads |
| Speculation controls | "Speculation controls", "IBPB at each change of process", "Index and address clamps" | **The first time any control is written on a processor that has it,** and read back. The cost of one IBPB on real silicon. On Zen 4 and Zen 5, ADR-0017 Z2 and Z5 call for automatic IBRS and STIBP where the processor reports them. Whether each is reported, and so whether the row passes, is not known until the boot prints it |
| IOMMU | Bus-master quiesce, bus transactions settled, IOMMU bring-up, bus mastering after the drivers | A real AMD IOMMU, and a real Transactions Pending bit, which QEMU never sets (`THREAT_MODEL_IOMMU.md`) |
| Random numbers | Random-number generator | RDRAND and RDSEED's start-up tests on bare silicon |
| Storage | PCIe enumeration, NVMe | A real NVMe controller started and read. Nothing is written (section 1.2) |
| Console | Concurrent console, its interrupt and screen sections | The real cost of a line on a real UART and display. Under KVM in CI a line took 4.3 to 4.6 ms on average on 2026-11-09, before ADR-0010's two amendments split the work into sections |

**What it cannot show.**
- **Anything about the declared scope.** Zen 4 and a 2024 Zen 5 part are bench equipment (ADR-0009 A1).
- **Whether a control works.** No boot proof can show what a processor does on a wrongly guessed path (ADR-0017, "What this does not prove").
- **E12.** No proof takes a second NMI inside the first.
- **Writes to a disk, the GPT proofs and the file system.** No disk there is the boot test's own.
- **virtio-net and TCP.** No virtio device exists there.
- **Intel's VT-d, RMRRs and hybrid cores.** It is an AMD machine with one kind of core.
- **The I/O APIC under AMD-Vi.** Its pins stay masked until the I/O APIC has an interrupt table there (`docs/STATUS.md` line 146).
- **T8 and T9.** No device of an attacker's is present.
- **Secure Boot, sleep and resume, USB, graphics drivers, laptops.** Secure Boot is off, and the others are not built or not on a server.
- **Performance.** One machine and three boots give figures to record as trends, not budgets (CLAUDE.md §53, item 115).

### 3.5 Deleting the server the same day

- Delete each server from the dashboard as soon as its last boot ends: Actions, then "Delete server", then type its hostname ([deleting a server](https://www.latitude.sh/docs/servers/deleting-a-server.md)).
- Check the billing summary an hour later, since deletion "may take up to an hour" to show there, and again the next day.
- The $5 card hold is refunded after 7 days ([payment methods](https://www.latitude.sh/docs/billing/payment-methods.md)).
- Latitude then erases every disk with the disk's own sanitize command and checks it ([disk sanitization](https://www.latitude.sh/docs/servers/disk-sanitization.md)). Nothing Origin leaves on the second disk outlives the rental.
- Remove the rental's SSH key from the account, and delete the key itself on the Mac.

## 4. What the result may claim

### 4.1 The evidence and where it goes

| What | Where | Committed? |
|---|---|---|
| The hardware record | `docs/hardware/records/<date>-latitude-m4-metal-small.md`, one per machine and rental | Yes |
| Origin's serial output, firmware to verdict | Beside the record, as `.log`, with IP addresses and server IDs removed | Yes: it is Origin's own text (section 6) |
| Screen pictures from the browser console | Outside the repository | No |
| Raw ACPI tables, `lspci -vvv`, `dmidecode`, `/proc/iomem` | Outside the repository; their SHA-256 in the record | No (section 6) |
| What rental A found, in Origin's words | A research ledger in `docs/research/` | Yes |

**The record's form.** The requirements gate reads a table with a row for each of Machine, Firmware, Source revision, Date and Result, and fails a row that names a record lacking one of them (`docs/requirements/README.md`, "Evidence"; `RECORD_KEYS` and `unsaid` in `developer-tools/xtask/src/requirements.rs`). Rows beyond those five are allowed, and this plan adds the facts a reader needs to weigh it:

```markdown
# Bench boot: Latitude.sh m4.metal.small

| | |
|---|---|
| Machine | Latitude.sh `m4.metal.small`, <site>: AMD EPYC 4244P, CPUID family 19h, model <..>h, stepping <..>; board <maker and model>; 12 threads |
| Firmware | <maker> <version> (<date>), UEFI mode, Secure Boot off; console redirection on COM<n> at <rate> |
| Source revision | <commit> on `main`; `cargo xtask build`, developer-mode loader; `BOOTX64.EFI` SHA-256 <..>; `KERNEL.ONBF` SHA-256 <..> |
| Date | <yyyy-mm-dd> |
| Result | Boot 1: `<the verdict line, verbatim>`. Boots 2 and 3: <same, or what differed> |
| Scope | Bench equipment: Zen 4, below ADR-0009 A1's AMD line. It qualifies nothing and supports no hardware claim (CLAUDE.md §74, §76, rules 30 and 40) |
| Environment | A physical machine with no hypervisor; the provider's firmware and BMC |
| Speculation controls | <the boot's line, verbatim> |
| IOMMU | <IVRS present or absent; what bring-up printed> |
| Log | `<file>.log`, SHA-256 <..> |
| Not kept here | ACPI tables, `lspci -vvv`, `dmidecode`: SHA-256 <..>, held by <who> |
```

**The requirements rows.** A row the boot is evidence for names the record as `file:docs/hardware/records/<file>.md`, and its environment follows the README's rule. Reasoning: no row becomes `done` on it, because the shared criterion asks for the proofs to pass "on each declared machine (§74)", and the README says a row whose scope is hardware is not marked `done` on that sentence alone. The matrix's header, "Nothing has run on a physical machine", and `docs/STATUS.md` would change with the first record.

### 4.2 What it may say

- **It is bench evidence.** The machine is outside ADR-0009 A1's scope, and A1 says of such machines: "it may be used for testing, but no support or qualification claim rests on it". CLAUDE.md §74 says "Do not claim physical hardware support from QEMU alone", §76 qualifies a release "only for its declared supported scope", and rules 30 and 40 tie qualification to the declared hardware.
- **It moves I2 forward without closing it.** It would be the first boot in which ADR-0017's controls are written on a processor that has them and read back. I2 closes only on "a boot on a processor in the declared scope" (item 213). So I2 stays Partly.
- **E12 stays Open.** The threat model asks for E12 to close "Before any claim about real hardware". Reasoning: a record that says what one bench machine printed, and says that it claims nothing, is not such a claim. Section 6 asks the owner to confirm that reading, since `THREAT_MODEL_SYSCALLS.md` makes "any claim about real hardware" a gate for both E12 and I2.
- **Its figures are trends.** Any timing it prints is recorded as a measurement of one bench machine, not as a budget or a comparison (CLAUDE.md §53, item 115).

## 5. Why not a self-hosted GitHub runner

**The risk.** The repository is public. GitHub's own guidance: "self-hosted runners should almost never be used for public repositories on GitHub, because any user can open pull requests against the repository and compromise the environment" ([secure use](https://docs.github.com/en/actions/reference/security/secure-use), read 2026-11-12). Destroying the runner after each job is not enough, because "there is no way to guarantee that a self-hosted runner only runs one job" (the same page). Reasoning: a pull request from a fork can edit the workflow it runs, so a label alone does not keep fork code off the machine.

**Why it fits badly here as well (Reasoning).**
- To boot Origin, the machine must leave the runner's own Linux. So the runner would have to drive the provider's console and power controls through the provider's API, and that key would sit where a workflow can reach it.
- An always-on `m4.metal.small` is $296 a month (₹27,732; [pricing](https://www.latitude.sh/pricing)), which is external spending and the owner's (CLAUDE.md §1, §80).
- The repository belongs to a personal account. Runner groups, which can limit a runner to named workflows, belong to organizations and enterprises, and by default only private repositories can use them (GitHub Enterprise Cloud documentation, [runner groups](https://docs.github.com/en/enterprise-cloud@latest/actions/hosting-your-own-runners/managing-self-hosted-runners/managing-access-to-self-hosted-runners-using-groups), seen as a search snippet only on 2026-11-12).

**What it would need, if the owner ever wants one (Reasoning).**
1. A home that forks cannot reach: hardware jobs in a separate **private** repository that takes only commits a maintainer has pushed to the public one, or an organization whose runner group is limited to named workflows on protected branches. No `pull_request` trigger from forks either way.
2. Just-in-time runners, which "perform at most one job before being automatically removed" ([secure use](https://docs.github.com/en/actions/reference/security/secure-use)), created per job by an orchestrator that lives off the machine.
3. The provider's API key and the console credentials kept by that orchestrator, never on the machine.
4. The machine wiped and reinstalled by the provider after each job.
5. A monthly cost and a cap the owner has accepted, and an ADR (CLAUDE.md §18, §80).

Until then, rentals are by hand, on the owner's account, a few hours at a time.

## 6. The owner's decisions

Unnumbered, for the coordinating session to number from item 224. Each has a recommendation.

- **Provider and machines.** Latitude.sh, hourly billing: `m4.metal.small` (EPYC 4244P, Zen 4, $0.41 an hour) for rentals A and B, then optionally one `f4.metal.large` (EPYC 9275F, Zen 5, $2.62 an hour) for rental C. **Recommended: accept.** It is the only cheap host whose documentation promises a serial console from the firmware onward. Not chosen: Vultr's EPYC 4245P (Zen 5, $0.41) has a video console only; Hetzner's EX44 (Intel, hybrid cores) has a video console that a technician attaches for €10, and its integrated graphics need identity maps first, so it comes later if at all.
- **Budget cap.** **Recommended: $10 (₹937) of machine time for all three rentals together.** The plan spends about $7.29 (₹683). Whoever is at the dashboard stops and asks the owner before a rental would take the total past the cap. The $5 card hold (₹468), refunded after 7 days, is outside it.
- **Who signs up, and how the account is verified.** **Recommended: the owner, personally.** Sign-up, the card and identity checks are the owner's alone (CLAUDE.md §1), and Claude may not create accounts or enter payment or identity details. Latitude then asks for either a government ID through Stripe Identity, which spends nothing, or $100 of credit (₹9,369) charged to the card. **Recommended: the ID, if the owner is content to share it with Stripe;** otherwise the credit.
- **Who does what on the machine.** **Recommended:** the owner does every dashboard action (deploy, rescue mode, out-of-band, power, delete) and holds every credential. Claude prepares the checklist and the image, reads the logs, and runs only rental A's reading commands and rental B's disk steps in rescue Linux, over SSH with a key made for that rental, if the owner adds it.
- **When.** **Recommended:** rental A as soon as the owner has signed up, without waiting for pull request 16. Rental B after identity maps for RMRR and IVMD land (next item) and pull request 16 has merged, built from `main`. Rental C after B, and only if B's serial console worked.
- **The identity-map deadline.** Does ADR-0007's "identity maps must land before real hardware" (items 8 and 12) apply to a rented server? **Recommended: yes, keep it.** The maps are additive (ADR-0007, "Migration consequences"), and rental A's real tables give them something to be checked against on the build host. The alternative is to waive it for server-class bench boots with a serial console. On such a server the console probably does not depend on reserved memory (Reasoning), but the deadline as the owner wrote it does not make that exception.
- **The IOMMU threats that stay open.** ADR-0009 H6 asks for T4, T5, T6, T8, T9, T14 and T23 to be closed or accepted before the first real-hardware boot. **Recommended: accept, by name and for bench boots on rented servers only, the ones that will still be open: T4's two windows, T5, T6, T8, T9 and T23.** On a rented server with no device of an attacker's and no data of value, nothing is there to exploit them (Reasoning). The acceptance would not extend to volunteers' machines, a release, or any claim. T14 is closed by the identity maps of the item above.
- **How Origin boots there.** **Recommended:** the developer-mode loader with Secure Boot off; no keys enrolled and no UEFI variable written; the disk picked once from the firmware's boot menu; and no firmware setting changed without the provider's word, and any setting changed put back before deletion.
- **What is kept, and where.** **Recommended:** commit the record and Origin's own serial log, with IP addresses and server IDs removed. Keep raw ACPI tables, `dmidecode` and `lspci -vvv` output out of the public repository, held privately with their SHA-256 in the record, and write what they say in Origin's own words. Reasoning: the tables are the board maker's data, with code in them, and the other dumps carry serial numbers.
- **What the record claims.** **Recommended:** bench evidence only. The record names the machine and says that it is outside ADR-0009 A1's scope and qualifies nothing. It is not "a claim about real hardware" in `THREAT_MODEL_SYSCALLS.md`'s sense, so I2 stays Partly and E12 stays Open, and no requirements row becomes `done` on it.
- **A self-hosted GitHub runner.** **Recommended: no, not now** (section 5).

## Sources and their limits

- The Hetzner, Cherry Servers, Scaleway, OVHcloud and phoenixNAP pages in section 2.2, and the Azure, Oracle, Google, AWS and Intel Cloud pages in section 2.3, were read by research subagents of this session on 2026-11-12, which quoted each page. Checked directly the same day: Vultr's plans API, Hetzner's two console-fee pages, and Intel's and AMD's processor pages.
- GitHub's runner-groups page was seen as a search snippet only.
- No Linux or other open-source code was read for this ledger (CLAUDE.md §3). The only code read outside Origin was Latitude.sh's published iPXE examples, for the console argument they pass ([examples](https://github.com/latitudesh/examples/tree/main/custom-images-v2), at `3e6c1c3`, 2026-11-09).
- No vendor PDF was opened. AMD's revision guide for the rented processor is read after rental A (P3), in memory, and stays out of the repository.
