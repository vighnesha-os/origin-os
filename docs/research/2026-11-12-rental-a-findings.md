# Rental A: what a rented bare-metal server showed before Origin runs on it

| | |
|---|---|
| Date | 2026-11-12 |
| Kind | Research ledger (CLAUDE.md §16). Rental A of `docs/research/2026-11-12-first-bare-metal-boot.md` |
| Decided by | Items 224–234 (the plan), 235 (rentals through Latitude's API, changing item 227); the owner approved this rental's cost in chat: "Yes, rent it" |
| Ran | No Origin code. Ubuntu 24.04, as Latitude deploys it, read the machine and wrote nothing but a temporary directory |
| Tool | `developer-tools/bench/latitude.py` (`rental-a`), at `3eb311d` |
| Raw data | Kept off the repository (item 232), under the owner's `~/.local/state/origin-bench/`. The SHA-256 of the tables cited are below. No IP address, MAC address, serial number or server ID is written here |

**The machine is bench equipment.** It is outside ADR-0009 Amendment 1's scope: its cores are Zen 4. It qualifies nothing, and nothing here is a claim about Origin on real hardware (item 233).

## The rental

| | |
|---|---|
| Provider, plan, site | Latitude.sh, `m4-metal-small`, Amsterdam (`AMS`) |
| Deployed, deleted | 00:43:20 and 00:52:31 UTC: 9 minutes 11 seconds. The deletion was confirmed by the API: the server no longer exists, and the project lists no servers |
| Charged | **$0.81**: Latitude's billing API shows one metered hour of `m4.metal.small` at 81 cents. A rental shorter than an hour is billed as a whole hour |
| Price | $0.81 an hour with hourly billing, from the API's `pricing.USD.hour`. The public pricing page's "$0.41/hr" is the monthly price, $296, over 730 hours |
| Deploy | Instant: the server answered 21 seconds after the request. Its Linux had been running for about 6.4 hours, so Latitude hands over a server already booted |

## What the machine is

| | |
|---|---|
| Board, firmware | Supermicro H13SRE-F in an AS-3015MR-H10TNR; AMI firmware 2.5a, dated 2025-12-08 |
| Processor | AMD EPYC 4244P: CPUID family 25 (19h), model 97 (61h), stepping 2; 12 logical processors. By ADR-0009 A2's rule, family 19h is older than Zen 5 |
| Firmware mode | UEFI, 64-bit. Secure Boot off |
| Boot entries | The firmware's order puts the installed system first, then UEFI PXE on each of the two network ports, then a built-in EFI shell. The running system had been started through a network-boot entry |
| Devices | Two Intel X550 Ethernet ports; two Micron 7500 PRO NVMe disks; an ASPEED BMC with its own display; the processor's integrated AMD graphics; two AMD USB controllers. 36 PCI functions in all |
| Display | Linux chose the ASPEED BMC display as the boot display, not the integrated graphics |

## The tables Origin reads

| Table | What it says | SHA-256 |
|---|---|---|
| SPCR | Revision 2. A full 16550 (interface type 0) in I/O space at `0x2F8`, 8-bit registers, 115 200 baud. Revision 2 carries no UART clock | `2f93c54c877ec205131b5e18fb4601c5806e98a05c1b4983ee0a5f0e2fc71df1` |
| IVRS | Revision 2. One IOMMU, the PCI function at 00:00.2, registers at `0xF7400000`, segment 0, described twice (blocks of type 10h and 11h). Special entries: an HPET and two I/O APICs (handles 32 and 33). **No IVMD block** | `7f8517f6c55cceb3f9b8335bfd831d7f20049d51cf089ccc57e6f8098fdfdbc3` |
| DMAR | Absent, as on any AMD machine | |
| Also present | APIC (MADT), HPET, MCFG, FACP, FACS, DSDT and nine SSDTs, TPM2, WSMT, FPDT, FIDT, SPMI, UEFI, VFCT, WDRT, and an OEM table | APIC `9b9e9388…2664ea`, HPET `a17e1b1e…cff3a9`, MCFG `d660f8c5…fcb013c` |

## What the processor reports, for ADR-0017

Read on processor 0 from CPUID, as ADR-0017 Z1 reads it:
- Automatic IBRS (Fn8000_0021 EAX bit 8): **1**. Z2 sets EFER.AIBRSE.
- Fn8000_0008 EBX bits 15 and 17, which Z5 follows for STIBP: **1 and 1**.
- `IA32_ARCH_CAPABILITIES` (CPUID 07H.0 EDX bit 29), `BHI_DIS_S` and `RRSBA_DIS_S`: 0. These are Intel's controls and do not apply here.

Whether Origin's "speculation controls" proof passes here is known only once Origin boots (rental B). The processor offers the controls that Z2 and Z5 ask of an AMD processor.

## What this means for rental B

1. **The serial console is COM2.** Latitude's serial-over-LAN carries the second UART, which Linux names ttyS1: a line written to it appeared on the console, and a line written to ttyS0 did not. The SPCR names the same port (`0x2F8`).
   - Origin's console starts on COM1 at `0x3F8` and moves to the SPCR's UART once it can read the ACPI tables, replaying the earlier lines (ADR-0010 E4).
   - Reasoning: on this console, Origin's first kernel lines will appear only after that move, all at once. The loader's lines go through the firmware's own console, which this machine redirects to the serial port, as the firmware's start-up text showed.
   - The SPCR is revision 2, so it gives no clock. E4 then leaves the divisor latch as the firmware set it.
2. **The console drops at every reset.** During the reboot, the firmware's start-up text reached the console ("Pre-memory NB Initialization"), and then the BMC closed the session ("SOL session closed by BMC"). The tool must open the console again right after each power cycle, or it misses Origin's output. Not yet built.
3. **No reserved-memory maps are asked for.** With no IVMD block, ADR-0007 A9's refusal would refuse no device on this machine. The identity-map deadline (items 8, 12 and 229) stands as decided. This only says that this machine would not exercise the unity-map code.
4. **The screen should survive the IOMMU.** The boot display is the BMC's, which shows the picture from the BMC's own memory. Reasoning: the framebuffer console (ADR-0010) does not depend on a reserved-memory region here, unlike T14's integrated-graphics case.
5. **UEFI and Secure Boot need no change.** Rental B needs no firmware setting changed (item 231).
6. **Boot selection.** The firmware has a built-in EFI shell entry and a short timeout (1 second). Picking the second disk once, from the firmware's menu over the console, is still the plan (item 231). Whether the menu shows that disk is for rental B to see.

## What this rental cannot show

- Anything about Origin. Origin did not run.
- Anything about the declared scope (ADR-0009 A1).
- Whether a later rental gets the same board and firmware. Each rental records its own.
