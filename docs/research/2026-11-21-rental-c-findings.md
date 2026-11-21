# Rental C: boot info 0.8 on the rented server, and still no kernel line

| | |
|---|---|
| Date | 2026-11-21 |
| Kind | Research ledger (CLAUDE.md §16). Rental C of `docs/research/2026-11-12-first-bare-metal-boot.md`, after rental B (`docs/research/2026-11-20-rental-b-findings.md`) |
| Decided by | Items 224–236 and 317 (boot info 0.8, TASKS item 84); the owner approved this rental's cost in chat on 2026-11-20 |
| Ran | Origin's developer-mode loader and Framekernel at `1c9c7a96` (`main` after PR 47), three boots |
| Tool | `developer-tools/bench/latitude.py rental-b`, at `1c9c7a96` |
| Record | `docs/hardware/records/2026-11-21-latitude-m4-metal-small.md`, with Origin's serial log beside it |
| Raw data | Kept off the repository (item 232), under the owner's `~/.local/state/origin-bench/`. No IP address, MAC address, serial number, GUID or server ID is written here |
| Sources read | Origin's own code at `1c9c7a96`, and the machine's own output: its serial console and the provider's system's `dmesg`, serial-port list and ACPI tables from the private dump. No operating system's source (CLAUDE.md §3) |

**The machine is bench equipment.** It is outside ADR-0009 Amendment 1's scope: its cores are Zen 4. It qualifies nothing (item 233).

## The rental

| | |
|---|---|
| Provider, plan, site | Latitude.sh, `m4-metal-small`, Amsterdam (`AMS`), $0.81 an hour |
| Deployed, deleted | 00:00:36 and 00:25:52 UTC: 25 minutes 16 seconds, inside one clock hour. The deletion was confirmed by the API, and a preflight afterwards listed no bench server |
| Cost | One hour, **$0.81**, as the tool's ledger counts it (the bill itself was not read). With rentals A and B the ledger holds **$2.43** of item 225's $10 cap |
| Deploy | Instant: the server was on 21 seconds after the request and answered SSH 3 seconds later |
| The ledger | Latitude gave this rental the server ID it had given rental A, and the tool's ledger was keyed by server ID: rental C got no row, and rental A's row took C's watchdog and deletion time, so the cap counted A as nine days long ($175.77). That failed safe but was wrong. Each rental is now a row of its own, keyed by hostname and server ID, with offline tests of a reused ID; the ledger was repaired from the run logs, and a preflight reads 3 rentals, $2.43 |

## What the machine is

Very likely **rental A's machine** again: the same server ID, the same firmware (American Megatrends **2.5a, 2025-12-08**), and a DSDT byte-identical to rental A's (rental B's machine had 2.4 and another DSDT). The SPCR, IVRS, MADT, FADT and FPDT are byte-identical across all three rentals. Otherwise as rental B describes it: AMD EPYC 4244P (family 19h, model 61h), 12 logical processors, 64 GiB mostly above 4 GiB, COM1 at `0x3F8` and COM2 at `0x2F8`, the serial-over-LAN console on COM2, and the firmware's framebuffer on the BMC's graphics device (ASPEED), 1024×768 at `0xF4000000`.

The SPCR: revision 2, 80 bytes, a 16550 (interface 0) at I/O port `0x2F8` with 8-bit registers, IRQ 3, 115 200 baud, 8N1, no flow control, an ANSI terminal; no clock field (that comes with revision 3).

## The console, boot by boot

One serial-over-LAN session lasted the whole rental; the BMC kept it open through all three resets. Each boot showed the same sequence (boot 1 began with the provider's system shutting down):

1. The firmware's progress text, "Supermicro H13SRE-F BIOS Date:12/08/2025 Rev:2.5a", "Ready to Boot".
2. Origin's loader, through the firmware's console, as on rental B and now with boot info 0.8:
   - `kernel verified: 10 sections, boot info 0.8, entry RVA 0x32000`
   - the generator's known-answer test and RDSEED and RDRAND health tests passed
   - `kernel placed at 0x7e061000`, at a different virtual address each boot (KASLR)
   - **`console: the SPCR names a UART at I/O port 0x2f8 (interface 0x0, 115200 baud): the kernel writes to it from its first line`**
   - **`console: the walk read 908 bytes of firmware memory in 31 reads`**, in 5,396 to 5,814 time-stamp counter ticks (cost ledger row CL46): the first figure for the walk on a physical machine
   - `framebuffer: 1024x768 at 0xf4000000`; `page tables built: 97 tables, 64614 MiB direct map`
   - `exiting boot services; entering the Framekernel`
3. Then nothing, for the 8 minutes each boot was given, until the tool restarted the machine. In the raw console each boot's window holds one firmware start and one loader run, and **not one byte after the loader's last line**: the next firmware start is the tool's restart.

## What it proves

- **TASKS item 84's loader half works on this firmware.** The loader's walk found the SPCR among AMI's tables through the UEFI configuration table and the memory map, three times, and named the UART the firmware's console uses.
- **The loader ran to its last line three more times**, on newer firmware than rental B's.
- **The kernel wrote nothing to `0x2F8`.** The UART works for an operating system after boot services end, the table and the speed are handled as intended, and nothing else writes to that port after the loader's last line; see below.

## What it does not prove

- Whether the kernel was entered at all, or how far it ran. No boot proof ran that anyone saw.
- Anything about the declared scope (ADR-0009 A1): bench equipment, item 233. No requirements row changes.

## Where it stops

### What excludes the console

- **E1. The UART carries an operating system's writes after boot services end.** Minutes before boot 1, the provider's system had its console on that UART (kernel command line `console=ttyS1,115200n8`; its serial driver found a 16550A at I/O `0x2F8` and had sent 28,856 bytes through it), and its shutdown messages, up to its last line before the reset, are in the raw console. Its framebuffer driver also used the firmware's framebuffer after boot services ended.
- **E2. The table and the speed.** The same images, booted under QEMU with this board's own SPCR bytes given to the firmware as an extra table (2026-11-21, local, under the turn lock; the table stays in the private folder): the loader names `0x2f8`; the kernel says `the loader named the SPCR's UART at I/O port 0x2f8 (interface 0x0): written to from the first line, beside COM1, as the firmware set it`, so it keeps the divisor the firmware left, the one that carried the loader's lines; and 387 kernel lines reach that UART, to `boot verdict: degraded: 70 of 71 proofs passed; failed: speculation controls`. The kernel programs it plainly: interrupts off, FIFOs on, DTR, RTS and OUT2, 8N1, no automatic flow control.
- **E3. No reset reached the firmware** in 8 minutes, three times: no second firmware start in any boot's window.
- **E4. Nothing else writes there.** The loader prints only through the firmware's console, before `ExitBootServices`; after its last line the first direct write to `0x2F8` is meant to be the kernel's.

So the kernel never wrote its first line to `0x2F8`. The only other way to that silence, the kernel attaching nothing at `0x2F8` and its later SPCR move failing too, needs the port claim or the status probe to fail on a UART that E1 shows answering; it is not credible.

**This also settles rental B's ranking.** Rental B's first three causes (the SPCR move, the frame allocator and kernel tables over AMI's map, the speculation controls' first writes) all lie after the kernel's first line, which since boot info 0.8 goes to `0x2F8`. The stop is earlier.

### The steps from the loader's last line to the kernel's first byte on `0x2F8`

At `1c9c7a96`, in order. Nothing in this span prints anything this console shows.

| # | Step | Where | How it could end silently here |
|---|---|---|---|
| L1 | The memory map read into the boot info's regions (`read_memory_map`, then the kernel image carved out), the boot info written, `ExitBootServices` called | `boot/uefi/src/main.rs:583` | A failure before the first call is reported on the firmware's console (not seen) |
| L2 | If the first call fails with a changed map key, one retry (UEFI 2.11 §7.4): the map read again into the same buffer (32 spare descriptors) and the call made again. Any other failure of the first call, and any failure of the retry, ends in `halt()`: `cli; hlt`, nothing printed | `boot/uefi/src/uefi.rs:377`, `main.rs:641`, `main.rs:709` | **A silent halt, by design**: after a failed `ExitBootServices` the loader may not use the firmware's console |
| L3 | `enter_kernel`: interrupts off, EFER.NXE and CR0.WP set, CR3 loaded with the loader's tables, the kernel stack, `call` the entry with the boot info | `main.rs:637`, `main.rs:1027` | A fault from the CR3 load on runs through the firmware's IDT, which the new tables do not map: a triple fault |
| K1 | `_start`: RBP cleared, the stack aligned, `call kernel_main` (the first write to the kernel stack, in the direct map) | `framekernel/frame/entry/main.rs` | A fault: a triple fault |
| K2 | `time::mark_entry` (RDTSC) | `framekernel/src/kernel.rs:38` | — |
| K3 | COM1 (`0x3F8`, a real 16550A here): its latch read, then programmed after a wait for the transmitter of at most 100,000 status reads | `kernel.rs:41–43`, `serial.rs:64` | Bounded: a fraction of a second to about a second on this bus |
| K4 | The relocation probe | `kernel.rs:45` | A refusal goes to COM1 only, then a halt |
| K5 | The console's state set up | `kernel.rs:52` | — |
| K6 | The boot info read and checked: header, version 0.8, regions sorted, disjoint and page-aligned, kernel segments, the seed's place | `kernel.rs:57`, `framekernel/frame/src/early.rs`, `boot.rs` | A refusal goes to COM1 only, then a halt |
| K7 | The loader's UART: the structure checked, ports `0x2F8` claimed, its status probed, its latch kept (E2), programmed after the same bounded wait, made the second sink | `kernel.rs:75`, `console.rs:1101` | Excluded by E1 and E2 |
| K8 | The framebuffer checked, then the whole 1024×768 screen cleared: 786,432 uncached 4-byte stores to the BMC's graphics memory through the loader's mapping | `kernel.rs:76`, `console.rs:947` | Slow is possible; seconds at most, not 8 minutes |
| K9 | The framebuffer's line queued, and COM1's and `0x2F8`'s first sections written in turn: **the first byte on `0x2F8`** | `console.rs` (`send_and_draw`) | — |

### The causes, ranked (Reasoning, from the code and the machine's output)

1. **`ExitBootServices` on AMI's firmware (L2).** The loader's one silent end is here, and the only path that OVMF, in every QEMU and CI boot, may never have taken: the loader says nothing about whether its first call failed or retried. A first call refused for a changed key can happen on any firmware, since an event between the map and the call can allocate (UEFI 2.11 §7.4 provides the retry for it); the retry then reads the map again into a buffer with 32 spare descriptors, and fails silently if the firmware's partial shutdown left more changes than that, or refuses the second call. Fits everything seen: no line, no reset, three times alike, on two firmware versions (rentals B and C).
2. **A fault from L3 to K8, if this board does not reset on a triple fault.** Rental B's ledger assumed a triple fault would reset the machine and show the firmware's start again; that is not established for this board. If it stops in a processor shutdown instead, every fault in this span looks exactly like what was seen. What real firmware could leave that OVMF does not is processor state the kernel inherits: CR4 bits (a supervisor shadow stack left on would fault at `enter_kernel`'s `call` into the kernel, its shadow stack no longer mapped), PAT, MTRRs. None is known to be set; none was read.
3. **A refusal printed on COM1 only (K4, K6).** The loader built the boot info with the same crate the kernel checks it with, so a refusal needs the two to disagree about AMI's map; nothing points to it, and QEMU's boots with 64 GiB pass.
4. **A slow step (K3, K8).** Bounded at seconds.

Excluded: the UART, the table and the speed (E1, E2); a reset loop (E3); everything after the kernel's first line, rental B's causes 1 to 3; the network-boot entry (every reset started Origin's loader).

## The next diagnostic, ranked

Each needs one rental: **$0.81** if it starts at the top of a clock hour, **$1.62** at the tool's planned worst case. The ledger holds $2.43 of $10.

| Rank | Way | What it changes | Cost | What it shows |
|---|---|---|---|---|
| 1 | **Breadcrumbs on the SPCR's UART, loader and kernel, in a bench build.** The loader prints the processor state the kernel inherits (CR0, CR4, EFER, PAT and, if CPUID reports CET, the supervisor shadow-stack MSRs) through the firmware's console before it leaves; after `ExitBootServices` returns it writes straight to the UART it named which call succeeded (first or retry), and before its silent halt why it halts. `enter_kernel` writes one byte after the CR3 load and one after the stack switch; the kernel one byte after each of K1 to K8 | The loader and the entry stub (the port passed in a spare register), a few writes in `kernel_main`; a QEMU scenario that checks the bytes in order, and a check that a release image has none | One task, then one rental | **The step it stops at, whichever cause it is**, in one rental |
| 2 | The loader half of 1 alone: the state line, the line after `ExitBootServices`, the line before the halt | Loader only | One small task, then one rental | Settles cause 1 and gives cause 2's processor state; if the loader hands over, the kernel step stays unknown and may need a second rental |
| 3 | With 1, a switch that faults on purpose after the last byte on the third boot | Bench build only | No extra rental | Whether this board resets on a triple fault, which decides how every later silent boot is read |
| 4 | Free, before any rental: a development hook that makes the loader's first `ExitBootServices` fail under OVMF (a stale map key), so the retry path runs on real firmware code at least once | Loader, development builds | No rental | Whether the retry path works at all; not AMI's behaviour |
| — | Port 0x80 codes | — | — | Not reachable as far as known: the board's code display is physical, and neither the console the tool reads nor the provider's API calls it makes show the BMC's capture of it |

A line that says why the loader halts after a failed `ExitBootServices` would be worth keeping beyond the bench build: it is the loader's only silent end. Writing to the SPCR's UART after boot services end uses no boot service. That is a decision for ADR-0002 and ADR-0010, through the coordinating session.

## What this rental cannot show

- Anything about Origin's kernel on this machine.
- Whether the stop is in the loader or the kernel: no byte separates them.
