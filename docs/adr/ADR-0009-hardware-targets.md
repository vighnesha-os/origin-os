# ADR-0009: Hardware target scope and the first test machines

| | |
|---|---|
| Status | **Accepted** on 2026-11-08 by delegation. The owner told Claude to decide what "introduced in 2026 or later" covers and to choose the machines ("You do"). Claude decided. The owner has not reviewed this text and can change it. It is not an ABI or format decision (§84). |
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
- Close or explicitly accept what the IOMMU threat model lists before real hardware: T4, T5, T6, T8, T9, T14 (RMRR and IVMD identity maps, the deadline in items 8 and 12) and T23.
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
