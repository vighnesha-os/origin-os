# Rental B: Origin's loader on a rented bare-metal server, and a kernel the console did not show

| | |
|---|---|
| Date | 2026-11-20 |
| Kind | Research ledger (CLAUDE.md §16). Rental B of `docs/research/2026-11-12-first-bare-metal-boot.md` |
| Decided by | Items 224–236; the owner approved this rental's cost in chat on 2026-11-20 ("Rent it") |
| Ran | Origin's developer-mode loader and Framekernel at `69ce9ecd` (`main` `ea2293ca` and the rental tool, which changes no image input), three boots |
| Tool | `developer-tools/bench/latitude.py rental-b`, at `69ce9ecd` |
| Record | `docs/hardware/records/2026-11-20-latitude-m4-metal-small.md`, with Origin's serial log beside it |
| Raw data | Kept off the repository (item 232), under the owner's `~/.local/state/origin-bench/`. No IP address, MAC address, serial number, GUID or server ID is written here |

**The machine is bench equipment.** It is outside ADR-0009 Amendment 1's scope: its cores are Zen 4. It qualifies nothing (item 233).

## The rental

| | |
|---|---|
| Provider, plan, site | Latitude.sh, `m4-metal-small`, Amsterdam (`AMS`), $0.81 an hour |
| Deployed, deleted | 06:00:36 and 06:25:54 UTC: 25 minutes 18 seconds, inside one clock hour. The deletion was confirmed by the API, and `status` afterwards listed no bench server |
| Cost | One hour, **$0.81**, as the tool's ledger counts it (the bill itself was not read). The ledger holds $1.62 of item 225's $10 cap with rental A |
| Deploy | Instant: the server was on 22 seconds after the request, answered SSH 2 seconds later, and its serial console was captured 30 seconds after that |

## What the machine is

The same model as rental A's, with older firmware:

| | |
|---|---|
| Board, firmware | Supermicro H13SRE-F in an AS-3015MR-H10TNR; American Megatrends firmware **2.4, dated 2025-04-29** (rental A's machine had 2.5a, 2025-12-08); UEFI 2.9 |
| Processor | AMD EPYC 4244P: CPUID family 19h, model 61h, stepping 2; 12 logical processors. CPUID reports no x2APIC, no hypervisor, automatic IBRS, STIBP (with "leave it on"), IBPB with return clearing, SSBD, and LFENCE always serializing |
| Memory | 64 GiB, most of it above 4 GiB: RAM up to physical `0x103DEFFFFF`, firmware-reserved ranges above it up to `0xFFFFFFFFFF` |
| Serial ports | Two 16550A UARTs: COM1 at `0x3F8` and COM2 at `0x2F8`. The serial-over-LAN console carries COM2, which the SPCR names |
| Display | The firmware's framebuffer is the BMC's (ASPEED), 1024×768 at physical `0xF4000000`, an address the memory map does not list |
| ACPI | SPCR, IVRS and MADT byte-identical to rental A's; the DSDT differs (the firmware version). The RSDP, XSDT, FACS and one other table lie in ACPI NVS memory, the other tables in ACPI data memory. The FPDT's two records point into firmware-reserved memory, and the FADT names the FACS only through its 32-bit field |

## Boot entries

The firmware's order puts `Boot0002` first, the disk entry that starts `\EFI\ubuntu\shimx64.efi`. The provider's running system had been started by `Boot0004`, a network-boot entry, as on rental A. The tool put Origin's loader in `Boot0002`'s file (item 236), and **every one of the three resets started `Boot0002`**: Origin's loader announced itself after the firmware's "Ready to Boot" each time. So the network-boot start was a one-time choice of the provider's, or a disk that was empty then; it did not repeat. No UEFI variable was written and no firmware setting changed (item 231).

## The console, boot by boot

One serial-over-LAN session lasted the whole rental. **The BMC did not close it at any of the three resets**, unlike rental A's, which closed it during the firmware's start; the tool's reopening was never needed. (Different firmware, or a different BMC state; not known which.)

Each boot showed the same sequence (boot 1 began with the provider's system shutting down):

1. The firmware's progress text: "Pre-memory NB Initialization", USB and PCI steps, "Supermicro H13SRE-F BIOS Date:04/29/2025 Rev:2.4", the processor, "Ready to Boot".
2. Origin's loader, through the firmware's console:
   - `Origin OS 0.1.0-dev UEFI loader (build 0)`; `UEFI 2.9 system table`; `firmware American Megatrends, revision 0x00050023`
   - `read \EFI\ORIGIN\KERNEL.ONBF (1925512 bytes)`; developer mode and the TEST trust store announced
   - `kernel verified: 10 sections, boot info 0.7, entry RVA 0x31000`
   - `random: known-answer test passed; RDSEED healthy, RDRAND healthy; the loader's generator is seeded`
   - `kernel placed at 0x7c89b000`, at a different virtual address each boot (KASLR), with a different direct-map base each boot
   - `framebuffer: 1024x768 at 0xf4000000, stride 4096`
   - `page tables built: 97 tables, 64614 MiB direct map` (98 tables on boot 3)
   - `exiting boot services; entering the Framekernel`
3. Then nothing, for the 8 minutes each boot was given, until the tool restarted the machine. **No kernel line, and no restart of the machine's own**: a triple fault would have reset it, and the firmware's text would have reappeared.

## What it proves

- **Origin's own code ran on a physical machine for the first time.** The UEFI loader ran three times, on the provider's firmware, to its hand-over: it read the kernel from the EFI system partition, verified the ONBF image, ran its generator's known-answer test and the health tests of RDSEED and RDRAND on a real processor, drew KASLR placements (different on each boot), built page tables over 64 GiB, and exited boot services.
- **Item 236's method works on this board**: replacing the first boot entry's file starts Origin at every reset, with no variable written.
- **The serial console carries the firmware's console**, so whatever the loader prints is seen.

## What it does not prove

- **Anything about the kernel.** Whether it ran, how far, and why it printed nothing on COM2 are not known: see below. No boot proof ran that anyone saw, so no speculation control, IOMMU or processor-start result exists.
- Anything about the declared scope (ADR-0009 A1): bench equipment, item 233. I2 stays Partly, E12 Open, and no requirements row becomes done.
- Whether a later rental gets the same firmware: rentals A and B had two versions.

## Why no kernel line reached the console

The kernel writes to COM1 (`0x3F8`) and to the framebuffer from its first line. It moves to the UART the SPCR names, and sends it everything printed so far, only once its own page tables are loaded (ADR-0010 E4; `framekernel/src/kernel.rs`, `console::adopt_firmware_uart`). On this machine COM1 is a real UART that nobody watched, and the screen was not captured. So **every way the kernel could stop before that move is silent here, and so is a move that fails, after which the whole boot stays on COM1.**

What runs from `_start` to the first byte the kernel can send to `0x2F8`, at `69ce9ecd`, and how each step could end silently on this machine:

| # | Step | How it could end silently here | New on this machine? |
|---|---|---|---|
| 1 | `_start`: clears RBP, aligns the stack, calls `kernel_main` | — | no |
| 2 | `time::mark_entry` (RDTSC) | — | no |
| 3 | COM1 programmed, `0x3F8` | — (a real UART here: lines go out, unwatched) | COM1 present |
| 4 | Relocations checked | refusal on COM1, halt | no |
| 5 | Boot info read and checked (`Entry::read_boot_info`): regions, segments, boot seed | refusal on COM1, halt | **a 64 GiB map, RAM above 4 GiB, AMI's region shapes** |
| 6 | The framebuffer attached; the banner, boot info and segment lines | a hang drawing on the ASPEED's memory | **the BMC's framebuffer, unlisted MMIO** |
| 7 | GDT, TSS, IDT (`cpu::init`), CPU-local area, CR4.MCE, DR7, SYSENTER_CS, then the speculation controls: `SPEC_CTRL` = STIBP and `EFER.AIBRSE` written and read back (ADR-0017 Z2, Z5) | a #GP on a write: two lines on COM1, halt | **the first write of these controls on a processor that has them** (KVM offered neither) |
| 8 | A breakpoint, handled and resumed | — | no |
| 9 | CPUID, XSAVE enabled with this processor's AVX-512 and PKU state, CPU and identity lines, the x86-64-v3 check | baseline refusal on COM1, halt | AVX-512 state on bare metal |
| 10 | Backtrace proof | — | no |
| 11 | Frame allocator (`Early::init_memory`): bitmap over the usable memory's end, page-table pool sized from the map, both placed in the first usable region that holds them | refusal on COM1, halt | **memory above 4 GiB, a bitmap of about 2 MiB** |
| 12 | The kernel's own tables (`build_kernel_tables`, `map_framebuffer`, `load_kernel_tables`): the direct map up to the highest RAM, the kernel half's tables, the stacks' region, the switch, the deliberate-fault proofs of W^X | a fault at the switch (a triple fault would reset: not seen), or a refusal: the move below then cannot map the tables, the console stays on COM1, and the refusal goes there | **a direct map over 64 GiB, directories beyond the first 4 GiB** |
| 13 | The SPCR move (`adopt_firmware_uart`): the first ACPI lookup locates the FACS and the FPDT's records by copy (TASKS item 71), then reads the RSDP and XSDT from ACPI NVS and the SPCR from ACPI data; claims ports `0x2F8`; checks something answers; replays the early log to COM2 | any refusal keeps the console on COM1: **the rest of the boot, verdict included, is invisible here** | **real firmware tables in NVS, an FPDT, a 32-bit FACS pointer** |

**No Origin boot before this one had more than 512 MiB of RAM.** Every QEMU machine in `xtask` has `-m 512M`, all of it below 4 GiB, and the KVM boots in CI use the same machine. Steps 5, 11 and 12 had never handled RAM above 4 GiB.

**So the same images were booted once more under QEMU, with 8 GiB** (2026-11-20, `cargo xtask run` with its memory size raised for that one boot in a local, uncommitted change, under the turn lock). They boot as with 512 MiB: `frame allocator: 2082257 free frames (8133 MiB), bitmap of 80 pages`, `page tables loaded: 411 tables, 8183 MiB direct map`, and `boot verdict: degraded: 70 of 71 proofs passed; failed: speculation controls`, the QEMU verdict decision item 154 keeps. Memory above 4 GiB as such is not the cause, as far as QEMU's map can show. Its size here (64 GiB) and the shape of AMI's map (many more regions, reserved and device ranges above the RAM up to 1 TiB, runtime regions) were not tried.

### The causes, ranked (Reasoning, from the code and the dump)

1. **The SPCR move itself (step 13).** It reads tables QEMU never had: an RSDP and XSDT in NVS, an FPDT whose records point into reserved memory, a FACS named only by the 32-bit field. Any refusal keeps the whole boot on COM1, which fits what was seen best: no reset, and a kernel that may have run to its verdict unseen. The SPCR itself is the one the QEMU scenario `spcr_uart_scenario` boots (revision 2, a 16550 at `0x2F8`), byte-identical to rental A's, and Origin programs COM2 plainly (DTR, RTS and OUT2, no automatic flow control), so the parse and the takeover are the less likely parts of it.
2. **AMI's memory map (steps 5, 11, 12).** Not memory above 4 GiB (the 8 GiB boot), but this map's size and shape, which no boot has had. A refusal there is silent here, and if the kernel's own tables are not loaded, the move cannot read the tables either.
3. **The speculation controls' first writes (step 7).** Both are enumerated by CPUID, so a #GP would be a processor or firmware surprise; possible, not likely.
4. **The framebuffer (step 6).** Each line is drawn on the BMC's memory through the loader's mapping; slow is possible, a hang unlikely.

Ruled out: the network-boot entry (every reset started Origin's loader); a reset loop or triple fault (none seen in 8 minutes, three times); the identity-map refusal of ADR-0007 A9 (no IVMD, and it comes after the move); the DE_CFG write, never made here (CPUID reports LFENCE serializing); the processor baseline (the provider's Linux reports AVX2, BMI2 and the rest of x86-64-v3).

### Making the next rental show where it stops

| Way | What it changes | Cost | What it shows |
|---|---|---|---|
| A. The loader hands the SPCR's UART to the kernel in the boot info (ADR-0010 E4's alternative) | Boot info 0.8 (an owner decision); the loader reads the SPCR through the UEFI configuration table before `ExitBootServices`; the SPCR parser moves to a crate both use; the kernel writes to that UART, and COM1, from its first line | One task; QEMU's SPCR scenarios extended to require the kernel's first line on `0x2F8` | Every kernel line on the operator's console from `kernel_main`, on every machine whose firmware has an SPCR |
| B. The kernel writes its early lines to every standard COM port that answers (`0x3F8`, `0x2F8`, `0x3E8`, `0x2E8`) until the SPCR settles it | An ADR-0010 E4 amendment; no ABI change | Small | The same here; on other machines, bytes on ports that may not be consoles |
| C. QEMU machines with more than 4 GiB, and with this machine's SPCR and FPDT given to QEMU as extra tables | `xtask` only | Small, no rental. Its first step, an 8 GiB boot, ran for this ledger and passed | Faults in steps 5, 11, 12 and 13 before any money is spent; not this machine's firmware or processor |
| D. The BMC's screen captured after each boot (Latitude's remote console) | The tool, and an IPMI session's credentials from the API | Unknown: the provider says some models lack it | The framebuffer, which has every kernel line from step 6, as pictures |

The recommendation goes to the coordinating session: a boot-info change is the owner's to decide.

## What this rental cannot show

- Anything about Origin's kernel on this machine.
- Whether rental A's machine, with its newer firmware, would behave the same.
