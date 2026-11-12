# Research ledger: the CPU identification table

| | |
|---|---|
| Date | 2026-11-12 |
| For | TASKS item 16 (contract in `docs/engineering/TASKS.md`); `docs/adr/ADR-0009-hardware-targets.md`, Amendment 1, A2 (decision sheet items 88, 89); ADR-0017 Z9 (item 132) |
| Requires | CLAUDE.md §3 (items 91–97: vendor documents first, today's versions), §16, §24 |
| Builds on | `docs/research/2026-11-10-cpu-identification.md`, which confirmed the numbers A2's rule uses. This ledger reads the vendors again on the day the table is built, and records what each of its rows rests on |
| Method | Intel's and AMD's own publications only. PDFs were fetched with `curl` and read in memory with `pypdf` under `python3 -I`; none was saved, opened in a browser or committed. Intel's HTML pages were read in the desktop app's browser. **No operating system's source or tables were opened** for this task, Linux's included |

## 1. Sources

Each was read on 2026-11-12. The SHA-256 is of the bytes as served that day.

| # | Document | Number, revision, date | What it gives the table | SHA-256 |
|---|---|---|---|---|
| I1 | Intel 64 and IA-32 SDM, Volume 1, §21.1.2 ("CPUID.01H:EAX") | 253665-093, September 2026 (served with last-modified 2026-09-24) | How DisplayFamily and DisplayModel are formed (PDF page 447) | `f6ffc430dfa2dada4f791b1ecb462810e59827befe9a9c23f9160941eea642f9` |
| I2 | Intel SDM, Volume 4, Table 2-1, "CPUID Signature Values of DisplayFamily_DisplayModel" | 335592-093, September 2026 | Family 6 models by product family, Clearwater Forest, Wildcat Lake and Panther Lake at the top; family 0Fh for the Pentium 4 generation (PDF pages 11–13) | `79f54b67b9bb3836856f04e0c45f50406c4d4b932c64800eb788b6b705a83dbd` |
| I3 | Intel Architecture Instruction Set Extensions and Future Features Programming Reference, Table 1-1 | 319433-062, June 2026 | The same table for future processors: 12_01H and 12_03H (Nova Lake desktop and mobile), 13_01H (Diamond Rapids) (PDF pages 15–16) | `1e38ce00d182ce4b8d82e9153f12668518f876dbaaad9d59938b472f5d4bfc0a` |
| I4 | Intel perfmon `mapfile.csv` | `intel/perfmon`, last change to the file at `409ce6f` (2026-10-13); repository head `8da5893` (2026-10-14) | `GenuineIntel-6-E5` maps to Panther Lake's two core event files (Cougar Cove, Darkmont); `-6-CC` and `-6-D5` to the same; `-6-DD` to Clearwater Forest; `-18-1` and `-18-3` to Nova Lake's Coyote Cove and Arctic Wolf | not hashed (read as text) |
| I5 | Intel microcode release notes, `releasenote.md` | `intel/Intel-Linux-Processor-Microcode-Data-Files`, `microcode-20261110` (published 2026-11-10; file at `d71fe4e`) | Family-model-stepping per product: CWF B0 06-DD-01 ("Intel Xeon 6900E+ Series", new in this release); WCL A1 06-D5-01; PTL 06-CC-02 and 06-CC-03; GNR 06-AD-01, 06-AE-01; SPR 06-8F-07, -08; EMR 06-CF-02; RPL 06-BA-02, -03 | not hashed |
| I6 | Intel product specifications (ark), the Xeon processors series listing | read 2026-11-12 | Xeon 6960E+, 6970E+, 6980E+ and 6990E+ (Clearwater Forest): launch date **Q2'26** | page |
| I7 | Intel product specifications, Core Ultra X7 358H; Intel Core 3 304 and the "Products formerly Wildcat Lake" listing; Intel newsroom, "Intel Launches Intel Core Series 3 Processors" | read 2026-11-12 | Panther Lake: Q1'26. Wildcat Lake: Q2'26, launched 2026-04-16 | pages |
| A1 | AMD64 Architecture Programmer's Manual, Volume 3, Appendix E.3.1–E.3.2 | 24594, revision 3.39, October 2026 (AMD's site: last updated 2026-10-28) | The vendor string; how Family[7:0] and Model[7:0] are formed (PDF pages 666–667) | `b598514b19f204d973040d9867a7ffc48c949009a8b3dc86b20c9188e721ee45` |
| A2 | Revision Guide for AMD Family 1Ah Models 00h–0Fh | 58251, revision 1.30 (2025-09-30) | EPYC 9005; CPUID `00B00F21h`, which AMD labels "Turin-Zen5-C1" | `0a6161202dfe5b15d37f4a4f031bf1c9fa3d8366467033e18c10e06177336220` |
| A3 | Revision Guide for AMD Family 1Ah Models 10h–1Fh | 58730, revision 1.30 (2025-09-30) | EPYC 9005; CPUID `00B10F10h`, "Turin-Zen5c-B0" | `fd66fb68ffdd58fbff75ad7354622ef4e2fd64002817d2daaf4eecbd13e7105d` |
| A4 | Revision Guide for AMD Family 19h Models 10h–1Fh | 57095, revision 1.05 (2025-11-11) | EPYC 9004; CPUID `00A10F11h` and `00A10F12h` (Genoa B1, B2) | `c279f4ee838329ea607a171988ca699f97be4dbc9f8ac11918ec37207c18326c` |
| A5 | Revision Guide for AMD Family 19h Models A0h–AFh | 57926, revision 1.05 (2025-11-11) | EPYC 9004 and 8004; CPUID `00AA0F02h`, "Zen4c-A2" | `0244dcb4d3ca209782518909ca17860b455046c32b5035864398733e795c8da5` |
| A6 | Revision Guide for AMD Family 17h Models 30h–3Fh | 56323, revision 1.03 (2026-02-27) | EPYC 7002; CPUID `00830F10h` (Rome B0) | `db6c4662272e0278b1a7e89bed3d17a600b15e07f18063fb648c281e7fe2bfff` |
| A7 | Revision Guide for AMD Family 17h Models 00h–0Fh | 55449, revision 1.21 (2023-08-31) | The first EPYC; CPUID `00800F12h` and `00800F11h` | `9345cdd3a297b317b2b02098b1b31c9fa7ec504f0ec36dd5da15ad99410e241f` |
| A8 | AMD's documentation search (`docs.amd.com`), the PPR listings | read 2026-11-12 | Family 1Ah continues in documents dated 2026: PPR 58264 (Model 50h, B0, 2026-09-30) and 58655 (Model C0h, A0, 2026-09-30); PPR 57930 (Model 70h, A0, 2025-06-27). Their titles were read; their contents were not needed | listing |

## 2. How CPUID's numbers are formed

Both vendors give the family the same way, and the model differently.

| | Intel (I1) | AMD (A1) |
|---|---|---|
| Vendor | Leaf 0, EBX EDX ECX: "GenuineIntel" | Leaf 0, EBX EDX ECX: "AuthenticAMD" (`68747541h`, `69746E65h`, `444D4163h`) |
| Family | The base family (EAX[11:8]); where it is 0Fh, plus the extended family (EAX[27:20]) | The same |
| Model | The base model (EAX[7:4]), with the extended model (EAX[19:16]) above it **where the base family is 06h or 0Fh** | The base model, with the extended model above it **only where the base family is 0Fh** |
| Stepping | EAX[3:0] | EAX[3:0] |

The table decodes the model by the vendor's own rule. A processor of another vendor is decoded by Intel's rule, and the line prints the raw value of EAX beside it, since neither vendor's manual speaks for it.

## 3. Intel

| Display family, model | Intel's name | In the scope? | Rests on |
|---|---|---|---|
| 06_CCH | Panther Lake (Core Ultra Series 3) | Yes, a 2026 generation (Q1'26, I7) | I2, I3, I4, I5 |
| 06_D5H | Wildcat Lake (Core Series 3) | Yes (Q2'26, launched 2026-04-16, I7) | I2, I3, I4, I5 |
| 06_DDH | Clearwater Forest (Xeon 6+, the 6900E+ series) | Yes (Q2'26, I6) | I2, I3, I4, I5 |
| 06_E5H | No product named. Intel's perfmon map gives it Panther Lake's core event files | Yes, as A2 lists it | I4 only |
| 12_01H, 12_03H | Nova Lake, desktop and mobile; "future processors" | Yes: family 18 or higher | I3, I4 |
| 13_01H | Diamond Rapids; "future processors" | Yes: family 18 or higher | I3 |
| Any other model of family 12h or above | Not listed by Intel | Yes: family 18 or higher | A2's rule |
| 06_BDH; 06_C5H, 06_C6H, 06_B5H | Lunar Lake; Arrow Lake H, S and U | No | I2, I3, I5 |
| 06_ADH, 06_AEH; 06_AFH; 06_B6H | Granite Rapids; Sierra Forest; Grand Ridge | No | I2, I3, I5 |
| 06_AAH; 06_CFH; 06_8FH | Meteor Lake; Emerald Rapids; Sapphire Rapids | No | I2, I5 |
| 06_BAH, 06_B7H, 06_BFH; 06_97H, 06_9AH | Raptor Lake; Alder Lake | No | I2, I5 |
| 06_6AH, 06_6CH; 06_55H | Ice Lake (Xeon); Skylake, Cascade Lake or Cooper Lake (Xeon) | No | I2 |
| Any other model of family 6 | An older Intel family-6 processor | No | A2's rule |
| Family 0Fh | The Pentium 4 generation (NetBurst) | No | I2 |
| Families 7 to 11h, and below 6 | No x86-64 processor Intel lists | No | I2, A2's rule |

**Corrections to the ledger of 2026-11-10:**

- **Diamond Rapids is now confirmed by Intel:** I3, revision 062 (June 2026), lists 13_01H, and its revision history says that revision added it. The ledger of 2026-11-10 had found no Intel source. The rule never depended on it.
- **Nova Lake's family is 12h, written in hexadecimal by Intel** (I3: "12_01H"), which settles the earlier ledger's reading of perfmon's "18" as decimal.
- **06_E5H still has no product name** from Intel. It is in A2's list because Intel's own perfmon map gives it Panther Lake's cores; the table says that, and names no product.

**Launch dates, against ADR-0009 Amendment 1, A1:**

- A1 dates Clearwater Forest to "March 2026". **Intel's ark gives Q2'26** for all four Xeon 6900E+ processors (I6), and Intel's product brief is dated 2026-06-01. Q2 is April to June, so "March" is contradicted. The scope does not change (2026 either way). Changing A1's text is the owner's: it is sent to the coordinating session as a proposal.
- A1 lists Wildcat Lake as in the scope "when they ship". Intel launched it on 2026-04-16 (I7).
- Panther Lake: A1's "January 2026" agrees with Intel's Q1'26 (I7).

## 4. AMD

| Family | Generation | In the scope? | Rests on |
|---|---|---|---|
| 1Ah or higher | Zen 5 or newer. AMD labels `00B00F21h` "Turin-Zen5" and `00B10F10h` "Turin-Zen5c" | Yes, by A2's rule | A1, A2, A3, A8 |
| Below 1Ah | Older than Zen 5. Family 19h holds AMD's Zen 4c (`00AA0F02h`, A5) and EPYC 9004 (A4); family 17h holds the EPYC 7002 and the first EPYC (A6, A7) | No | A1, A4–A7 |

**What the table does not do.** It does not tell Zen 5 from Zen 6 inside family 1Ah, and it does not tell a 2024 Zen 5 generation from a 2026 one. AMD names no family 1Ah model as Zen 6 in anything read, and Gorgon Point's model number is still unpublished. A2 says so and accepts it: a 2024 Zen 5 machine reads as Zen 5 or newer, and that is not a support claim. The boot line says it too.

**Rental A's processor**, the EPYC 4244P (`docs/research/2026-11-12-rental-a-findings.md`, on `main`): family 19h, model 61h, stepping 2, so leaf 1 EAX `00A60F12h`. AMD's documents read here do not list model 61h by name. The table does not need them to: family 19h is below 1Ah, so the line says it is outside the scope, as rental B's report must.

## 5. Errata

ADR-0009 A2 makes the table the home for errata keyed by family, model and stepping. **It holds none.** An entry belongs there only when a vendor documents the erratum and the kernel acts on it, and nothing the kernel runs today works around a documented erratum.

- The documents an entry would come from: Intel's specification updates (Panther Lake 869992, Wildcat Lake 914196 and Clearwater Forest 917802, as I5's release names them) and AMD's revision guides (A2–A7).
- ADR-0009 H6 asks for Panther Lake's VT-d and xHCI errata to be read before the first boot of a Panther Lake machine. That is still open, and is not this task's: no such machine is planned, and the xHCI driver does not exist.
- None of those documents was read in full for this task.

## 6. Not read, and limits

- No operating system's source or table, Linux's included, was opened for this task (§3).
- AMD's uProf guide page, which the ledger of 2026-11-10 cited, answered 404 at its address on 2026-11-12. Its content is not needed: the revision guides give the same families with AMD's own CPUID values.
- AMD's product pages, which tie product series to Zen generations, were not read. The table's AMD generations come from AMD's revision guides' own labels (A2, A3, A5) and A2's rule.
- Every hash in §1 was computed twice on 2026-11-12, by the reading script and again with `shasum -a 256` over a second fetch, and the two agree.
- QEMU can be told to report any vendor, family and model, which is how the two boots test both answers. Neither shows what a real processor of that generation reports.
