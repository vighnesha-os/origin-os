# Research ledger: identifying 2026 core generations from CPUID

| | |
|---|---|
| Date | 2026-11-10 |
| For | `docs/adr/ADR-0009-hardware-targets.md`, Amendment 1, A2 (decision sheet item 89); TASKS item 16 (the CPU identification table) |
| Requires | CLAUDE.md §3 (original source), §16 (primary sources first), §24 (capability discovery), §74, §76 |
| Method | Intel's and AMD's own publications only, read on the web. No PDF was saved by hand; one AMD PDF could not be read (A6). See "How Linux came into this" for a departure from the earlier ledgers' method. |

## 1. The question

Can the kernel tell from CPUID whether a processor belongs to the declared scope: Intel generations launched in 2026 or later, and AMD generations launched in 2026 or later with Zen 5 or newer cores? CPUID gives a vendor, a family, a model and a stepping, but no year.

## 2. Sources

| # | Source | Publisher | What it gives |
|---|---|---|---|
| I1 | [Microcode release notes](https://github.com/intel/Intel-Linux-Processor-Microcode-Data-Files/blob/main/releasenote.md), through `microcode-20260925` | Intel | Family-model-stepping per product: PTL 06-CC-02 and 06-CC-03 ("Intel Core Ultra processor (Series 3)", first in `microcode-20260512`); WCL 06-D5-01 ("Intel Core Processors (Series 3)", `microcode-20260811`); ARL 06-C5-02, 06-C6-02, 06-B5-00; LNL 06-BD-01; GNR 06-AD-01, 06-AE-01 |
| I2 | [perfmon `mapfile.csv`](https://github.com/intel/perfmon/blob/main/mapfile.csv) | Intel | `GenuineIntel-6-CC`, `-6-D5` and `-6-E5` map to Panther Lake's Cougar Cove and Darkmont event files; `-6-DD` to Clearwater Forest; `-18-1` and `-18-3` to Nova Lake's Coyote Cove and Arctic Wolf. No family 19 entry |
| A1 | [uProf User Guide 5.1, "Supported Platforms"](https://docs.amd.com/r/en-US/57368-uProf-user-guide/Supported-Platforms) (57368, 2025-06-09) | AMD | EPYC 7002, 7003 and 9000 Series as family 17h (models 30h–3Fh, 60h–6Fh), 19h (00h–0Fh, 10h–1Fh, 20h–2Fh, 60h–6Fh, 70h–7Fh, 90h–9Fh, A0h–AFh) and 1Ah (00h–0Fh, 10h–1Fh, 20h–2Fh, 40h–4Fh, 60h–63h). AMD's text writes the second 19h range "0x1 - 0x1F", read as 10h–1Fh |
| A2 | [PPR, Family 1Ah Models 00h–0Fh, Revision C1](https://docs.amd.com/v/u/en-US/57238_C1_pub_049) (57238, 2026-03-13) | AMD | Family 1Ah exists for the EPYC 9005 models |
| A3 | [PPR, Family 1Ah Model 11h, Revision B0](https://docs.amd.com/v/u/en-US/57883) (57883) | AMD | A second family 1Ah server model |
| A4 | [PPR, Family 1Ah Model 50h, Revision B0](https://docs.amd.com/v/u/en-US/58264_B0_pub_045) (58264, 2026-09-30) and [PPR, Family 1Ah Model C0h, Revision A0](https://docs.amd.com/v/u/en-US/58655_A0_pub_042) (58655, 2026-09-30) | AMD | Family 1Ah continues into models first documented in 2026. The viewer pages name no product or microarchitecture |
| A5 | Performance Monitor Counters for AMD Family 1Ah Models 50h–57h (69163, 2025-12-17), through [Tom's Hardware](https://www.tomshardware.com/pc-components/cpus/amd-pubs-first-zen-6-document-for-developers-a-brand-new-8-wide-cpu-core-with-strong-vector-capabilities) and [HotHardware](https://hothardware.com/news/amd-zen-6-performance-monitor-counters) | AMD (read through press) | AMD's document does not say "Zen 6". The press ties models 50h–57h to EPYC Venice and Zen 6 |
| A6 | EPYC 9005 Architecture Overview (58462) | AMD | The search engine's extract shows "Family 1Ah" with Zen 5 cores. The PDF itself timed out twice and was not read |

## 3. Findings

**Intel.** Intel's own publications confirm every number ADR-0009 Amendment 1 uses except one:

| Rule | Confirmed by | Note |
|---|---|---|
| Family 6, model CCh: Panther Lake | I1, I2 | |
| Family 6, model E5h: Panther Lake cores | I2 | No microcode row yet, so the product is not named |
| Family 6, model D5h: Wildcat Lake | I1, I2 | Uses Panther Lake's core event files, so the same cores |
| Family 6, model DDh: Clearwater Forest | I2 | |
| Family 18 (12h), models 1 and 3: Nova Lake | I2 | Intel writes the family in decimal |
| Family 19 (13h): Diamond Rapids | **Not confirmed** by Intel | The rule does not depend on it: family 18 or higher is enough |

Intel's x86-64 processors before Nova Lake report family 6, or 15 (0Fh) for the Pentium 4 generation, so "family 18 or higher" admits only Nova Lake and later. *(Corrected 2026-11-10: the first version said "no Intel product", which overstated it.)*

**AMD.** AMD's documents put every pre-Zen 5 product it lists (EPYC 7002, 7003 and 9004) in family 17h or 19h (A1). Family 1Ah holds the EPYC 9005 generation (A1, A2, A3), which is Zen 5 (A6, and AMD's product naming), and models first documented from December 2025 (A4, A5).

AMD's cores before Zen used lower families still (0Fh to 16h). So "vendor AMD and family 1Ah or higher" means Zen 5 or newer. The rule does not need AMD to say which family 1Ah models are Zen 6: anything at or above 1Ah is Zen 5 or later.

**Not found:**

- Gorgon Point's CPUID model. AMD lists no separate document for it, and A1's 1Ah ranges predate its launch.
- An AMD document using the words "Zen 6" next to a family number.
- A6's text, read directly.

## 4. How Linux came into this

Earlier ledgers (for example `2026-11-09-tsc-consistency.md`) record that no operating-system source was opened. On 2026-11-10, while answering the owner's question, Claude first read two Linux source files for their CPU tables: `arch/x86/include/asm/intel-family.h` and `arch/x86/kernel/cpu/amd.c`. That departs from the earlier method.

Nothing from them is in Origin:

- The facts used are hardware identifiers, which this ledger now confirms from Intel and AMD.
- Linux's own grouping of AMD model ranges into Zen 5 and Zen 6 is not used. The rule needs only "family 1Ah or higher".

The owner asked about it the same day, and this ledger replaces those files as ADR-0009's evidence.

## 5. For the table (TASKS item 16)

- Build it from these sources and from the SDM Volume 4 and AMD's PPRs, not from any operating system's tables.
- Record each row's source.
- Add Diamond Rapids only once Intel publishes its numbers.
