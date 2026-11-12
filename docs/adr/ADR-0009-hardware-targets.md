# ADR-0009: Hardware target scope and the first test machines

| | |
|---|---|
| Status | **Accepted** on 2026-11-08 by delegation. The owner told Claude to decide what "introduced in 2026 or later" covers and to choose the machines ("You do"). Claude decided. The owner has not reviewed this text and can change it. It is not an ABI or format decision (§84). **Amendment 1, accepted by the owner on 2026-11-10 (decision sheet items 88–90), tightens H1 for AMD and replaces H2: the kernel now names the core generation, and still never refuses to boot.** |
| Requires | CLAUDE.md header ("Target hardware"), §17, §18 (hardware qualification policy), §24, §27, §67, §74, §76, rules 14, 30 and 40 |
| Evidence | `docs/hardware/SHORTLIST.md`, which has the sources and says what was and was not checked. Research only: **nothing here has been bought, booted or tested.** |
| Related | ADR-0007 (sheet items 8, 9, 12 and 25 tie its policy to the declared scope), ADR-0008 (64 per-CPU slots, sheet item 14) |

## Context

The directive's header sets the target as "Intel and AMD platforms introduced in 2026 or later". §76 and rule 40 allow a production claim only against a declared list, and §74 requires a test matrix. No list existed.

The shortlist found four machines that fit if "platform" means a generation and the silicon it launched with, and two more, much easier to debug, that fit only if any CPU sold from 2026 counts.

## Decisions

**H1. What counts.** A platform is a CPU generation the vendor launched as a generation, with the SoC or chipset it launched with. It counts if that generation first launched in 2026 or later.

| Case | Counts? | Example |
|---|---|---|
| A new generation | Yes | Panther Lake (Core Ultra Series 3, January 2026) |
| A refresh the vendor launched as a new generation | Yes | Ryzen AI 400, "Gorgon Point" (January 2026) |
| A new model added to an older generation, on that generation's platform | No, even if the model launched in 2026 | Core Ultra 200S Plus (March 2026, on a platform from October 2024); Ryzen 7 9850X3D (January 2026, Ryzen 9000) |

The header says platforms, not CPUs. A refresh has to count because the header requires AMD as well as Intel, and no Zen 6 client part has launched.

*(H1 and H2 are changed by Amendment 1, at the end.)*

**H2. The date scopes claims, not code.** The kernel keeps deciding from CPUID and ACPI (§24) and contains no launch-date check. A machine outside the scope may be used on the bench, but no support or qualification claim can rest on it (§76, rule 40).

**H3. What a declared machine must also have.** An IOMMU the firmware exposes through a DMAR or IVRS (sheet item 9). No dependence on a firmware request to opt out of DMA remapping or x2APIC (item 25). `x86-64-v3`, x2APIC and CET shadow stacks (§14). A way to enrol our own Secure Boot keys (§19, §58). No more than 64 logical CPUs until per-CPU slots are sized at discovery (item 14). Most of this can only be confirmed by booting the machine.

**H4. The first machines.**

| Order | Machine | Why |
|---|---|---|
| 1 | #2: ASRock Industrial iBOX-358H, or the NUC-358H board, with the Core Ultra X7 358H | In scope. A documented COM port, PCIe Gen 5 NVMe (§17) and three core types for hybrid scheduling (§27). |
| 2 | #5: Lenovo ThinkCentre M75q Gen 6, Ryzen AI 7 PRO 450GE, Serial Flex I/O | The only AMD machine in scope with any serial path. It has SMT. |

Intel comes first because its console path is documented and the VT-d work is the furthest along.

**H5. Before buying.** Buying is the owner's (no machine is bought by this decision).

- #2 is quote-only. Ask ASRock Industrial or a distributor for a price and a lead time. If no reasonable quote arrives, use #3 instead.
- #5: get confirmation from Lenovo or a reseller that the 450GE can be ordered with the Serial Flex I/O. If it cannot, hold the AMD purchase until the console in H6 exists; #5 without serial and #6 then become usable.

**H6. Before the first boot.**

- Read Intel's no-fix errata for this CPU, on VT-d (PTL003, PTL023) and xHCI (PTL002, 006, 007, 009). The research reported them, and nobody has re-read them.
- Close or explicitly accept what the IOMMU threat model lists before real hardware: T4, T5, T6, T8, T9, T14 (RMRR and IVMD identity maps, the deadline in items 8 and 12) and T23. *(2026-11-12, items 229 and 230: for bench boots on rented servers only, T4's two windows, T5, T6, T8, T9 and T23 are accepted by name, and T14's deadline holds there too; `docs/research/2026-11-12-first-bare-metal-boot.md`.)*
- Give the kernel a console that does not depend on a 16550 at `0x3F8`, such as a UEFI GOP framebuffer or the address in the ACPI SPCR table. The only console today is at that fixed port, and neither ASRock nor Lenovo documents the address. Without it, a failed boot shows nothing. *(2026-11-08: built against ADR-0010, which the owner accepted; proven under QEMU only.)*

**H7. Later machines, for §74.**

| Machine | For |
|---|---|
| #3: Dell Pro Rugged 14 | The only laptop in scope with a serial port: sleep and resume with a console (§67) |
| #6: ThinkPad T14 Gen 7 AMD | AMD laptop: sleep, resume and docks. No serial, so it needs H6's console |
| A Xeon 600 workstation | NUMA and Intel SMT. Needs item 14's per-CPU sizing first if it has more than 64 logical CPUs |

**PCIe Gen 6 remains a gap:** none was found on a client machine.

**H8. Not chosen: #1 and #4.** They are outside the scope under H1. If first boots on #2 or #5 are blind despite H6, either can be bought as a bench instrument under H2.

## Alternatives

| Alternative | For | Against |
|---|---|---|
| Any CPU sold from 2026 counts | #1 and #4 become the first pair: socketed, COM header, separate management controller | The header says platforms. It would declare platforms from 2024 as "2026 or later" |
| Only new silicon counts | The cleanest meaning | No AMD platform qualifies today, and the header requires both vendors |
| A capability floor with no date | Fits §24 | Contradicts the header, and §76 needs a list anyway |
| #3 instead of #2 first | Off the shelf, with a real serial port | $3,945, 8 threads, and its facts come from the research alone. It stays the fallback |

## Consequences

- The AMD half rests on a refresh. Gorgon Point's cores are Zen 5, so it adds no architecture coverage beyond 2024's. A Zen 6 client platform should join the list when one launches.
- The machines in scope cover 8 and 16 logical CPUs, and only #2 has PCIe Gen 5 NVMe. #5's SSD slots are Gen 4.
- A first boot may show nothing until H6's console exists.
- Graphics (§36) and audio (§55) played no part in this choice. §36's first GPU target is a separate decision.
- A pair is only a start. §74 requires several models from each vendor before anything is qualified.

## Trade-offs

This follows the directive's words at the cost of the easiest first machines. #1 and #4 would be quicker to bring up, but work on them would prove little about the declared target.

## Migration consequences

None in code or on disk: no binary format, ABI or persistent data depends on this. A later change is an edit to this ADR and the declared list. Removing a platform after a release has claimed support for it is a support change and must follow rule 39.

## Update, 2026-11-09: the machines cannot be bought for now

The owner cannot buy test machines. H4 and H5 stay as the list of what to get if that changes, but no work waits on a purchase.

Evidence now comes from these, in order, and the plan and its sources are in `docs/research/2026-11-09-testing-without-buying-hardware.md`:

1. **Free KVM on GitHub's hosted runners:** a real CPU with software devices. Tried on 2026-11-09: the kernel booted to its last line on an AMD EPYC 7763 in all seven KVM configurations.
2. **Whole servers rented by the hour:** real firmware, IOMMU and NVMe. These need the owner to sign up and pay a few hundred rupees at most.
3. **Volunteers' own machines,** with a safe USB image: client firmware, sleep and USB, and the only route to 2026 laptops.

A rented or borrowed machine outside H1's scope is bench equipment (H2). No support claim can rest on it. CLAUDE.md §74, §76 and rules 30 and 40 are unchanged: a release still needs the declared machines.

## Amendment 1, 2026-11-10: a stricter scope, and the kernel names the core generation

*Accepted by the owner on 2026-11-10, as recommended ("Accept all", decision sheet items 88–90; CLAUDE.md §18). Nothing here is built.*

The owner asked for a strict rule: "we are building for 2026+ hardware and in AMD Zen5 and Zen5+ / For Intel also 2026+ hardware". Since a CPU reports no year, the owner then asked whether the kernel could check for Panther Lake, Nova Lake, Zen 5, Zen 6 and so on instead.

### A1. The scope (item 88; replaces H1's rule, keeps its table)

- **Intel:** a platform counts if its generation first launched in 2026 or later. This is unchanged.
- **AMD:** a platform counts if its generation first launched in 2026 or later **and** its CPU cores are Zen 5 or newer (Zen 5, Zen 5c, Zen 6 and later). "Zen 5+" is read as "Zen 5 and newer"; AMD sells nothing by that name.

Zen 5 itself launched in 2024: Ryzen 9000, Ryzen AI 300 and EPYC 9005 are Zen 5 but outside the scope. The new AMD condition keeps out any 2026 generation built on older cores.

| As of 2026-11-10 | In scope | Outside the scope |
|---|---|---|
| Intel | Panther Lake (January 2026); Xeon 6+ Clearwater Forest (introduced March 2026); Wildcat Lake and Nova Lake when they ship | Lunar Lake, Arrow Lake (and its 2026 refresh), Granite Rapids, older |
| AMD | Ryzen AI 400 "Gorgon Point" (Zen 5 refresh, January 2026); EPYC "Venice" (Zen 6, introduced July 2026); Zen 6 Ryzen when it ships | Ryzen 9000, Ryzen AI 300, EPYC 9005; anything on Zen 4 or older |

Everything else is bench equipment: it may be used for testing, but no support or qualification claim rests on it (§76, rules 30 and 40).

### A2. The kernel names the core generation and reports the scope (item 89; replaces H2)

CPUID reports a vendor, a family and a model, which identify the core generation. These are hardware identifiers, not marketing names (§24).

| Answer | Rule |
|---|---|
| AMD, Zen 5 or newer | Vendor `AuthenticAMD` and family 1Ah or higher. Every earlier AMD core is below it: Zen to Zen 4 in families 17h and 19h, per AMD's own documents, and the cores before Zen in 0Fh to 16h. Family 1Ah begins with Zen 5 (EPYC 9005) and continues into models first documented from December 2025. So no model list is needed |
| Intel, a 2026 generation | Vendor `GenuineIntel` and family 18 (12h) or higher (Nova Lake is 18); or family 6 with model CCh or E5h (Panther Lake cores), D5h (Wildcat Lake) or DDh (Clearwater Forest) |
| Anything else | Outside the scope, including other vendors and every older family or model |

- **Where the numbers come from.** They come from one CPU identification table built from Intel's and AMD's own publications. The numbers above were confirmed on 2026-11-10 against Intel's microcode release notes and perfmon map, and against AMD's uProf guide and processor references (`docs/research/2026-11-10-cpu-identification.md`). The same table holds the errata workarounds keyed by family, model and stepping (P0). New 2026+ family-6 models are added to it as Intel publishes them.
- **At boot.** The kernel prints one line saying whether the machine is in scope or a bench machine, and why.
- **It never refuses to boot.** Every machine Origin can be tested on today is outside the scope: GitHub's runners, QEMU on the Mac, and every rentable server found on 2026-11-09. A refusal would stop all testing.
- **Features still come from feature bits.** Instructions are chosen from CPUID feature bits (§14, §24). The generation decides only the scope line.
- **Both answers are tested.** QEMU can be told to report any family and model, so CI can boot an in-scope and an out-of-scope identity.
- **What cannot be checked.** The check gives the core generation, not the launch year. Gorgon Point refreshes 2024's Strix and Krackan Point on the same socket, probably with the same model numbers (unconfirmed). So "AMD from 2026" stays a rule about claims: a 2024 Zen 5 machine reads as Zen 5 or newer, and that is not a support claim.
- **It is not a security control.** A virtual machine reports its host's CPU, or whatever QEMU is told to report.

### A3. The installer, decided at M9 (item 90)

At M9, the owner decides whether the production installer warns on or refuses machines outside the scope, using A2's check. Developer builds are exempt, like ADR-0002 B8's developer mode. The trade-off to weigh then: a cloud virtual machine on an older host fails the check, which matters for the Server edition's first platform (ADR-0014 S3).

### Alternatives

| Alternative | Why not |
|---|---|
| Check the launch year | CPUID reports none |
| A model list for AMD | Unneeded: family 1Ah or higher is Zen 5 or newer |
| A feature floor instead of the generation | One floor cannot fit both vendors: what is new in Intel's 2026 cores (FRED, for example) is missing from Zen 5 |
| Refuse to boot outside the scope | Stops every test available today |
| No check (H2 as written) | The owner asked for a strict rule, and the scope could not be reported |
| Count AMD Zen 5 from any year | Would make rentable EPYC 9005 and Ryzen 9000 machines qualifying hardware, but breaks "2026+"; offered and not chosen |

### Evidence

Checked 2026-11-10:

- **Family and model numbers, from Intel's and AMD's own publications:** `docs/research/2026-11-10-cpu-identification.md`.
  - Intel confirms every Intel number above. Diamond Rapids, which Linux lists as family 19, is not confirmed by Intel, and the rule does not depend on it.
  - AMD confirms family 1Ah for EPYC 9005 and for the models documented in 2026, and family 17h and 19h for the older EPYC generations.
  - Before this, the numbers were first read from two Linux source files. That ledger's section 4 records it, and they are no longer the evidence.
- **Gorgon Point is Zen 5, family 1Ah:** [Wikipedia](https://en.wikipedia.org/wiki/Zen_5). Its model number is still unconfirmed.
- **Gorgon Point is a refresh:** [Igor'sLAB](https://www.igorslab.de/amd-ryzen-ai-400-alias-gorgon-point-viel-neue-etiketten-aber-nach-bisherigem-stand-eher-ein-refresh-von-strix-und-krackan/), [Tom's Hardware](https://www.tomshardware.com/pc-components/cpus/amds-ryzen-ai-400-series-includes-the-first-copilot-desktop-cpu-team-red-refreshes-zen-5-apus-and-strix-halo).
- **Launches:** Zen 6 EPYC in July 2026 ([Club386](https://www.club386.com/amd-confirms-zen-6-launch-date/)); Intel's 2026–2028 roadmap ([Tom's Hardware](https://www.tomshardware.com/tech-industry/semiconductors/intel-chip-roadmap-2026-2028)).

Each row the table adds later needs the same kind of vendor source, recorded beside it.

### Trade-offs and migration

- **What it costs.** One table to maintain, and every new Intel family-6 model must be added to it. AMD needs nothing more for future generations.
- **What it gains.** The scope becomes visible on every boot, and the table errata need anyway gets a second use.
- **Migration.** CLAUDE.md's header and §24 say this (revision 7.3). Nothing built changes. New task: the table, the scope line and the two QEMU identities.
