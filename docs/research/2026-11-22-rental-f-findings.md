# Rental F: the FCH's reset on the rented server, and the A-pattern again

| | |
|---|---|
| Date | 2026-11-22 |
| Kind | Research ledger (CLAUDE.md §16). Rental F of `docs/research/2026-11-12-first-bare-metal-boot.md`, after rental E (`docs/research/2026-11-21-rental-e-findings.md`) |
| Decided by | Items 224–236, 317, 342, 344, 345 and 346; TASKS item 121 (a bench reset with no firmware code, ADR-0002 Amendment 4); the owner's "Rent it" on 2026-11-22, through the coordinating session |
| Ran | TASKS item 121's bench build at `12a4e081`: the bench loader, and the bench kernel it checks but does not enter; three boots, five loader runs |
| Tool | `developer-tools/bench/latitude.py rental-b`, checked by the coordinating session's guarded script against `73df0f4e`'s bytes (records only since `12a4e081`), and run by that script |
| Record | `docs/hardware/records/2026-11-22-latitude-m4-metal-small-f.md`, with Origin's serial log beside it |
| Raw data | Kept off the repository (item 232), under the owner's `~/.local/state/origin-bench/`. No IP address, MAC address, serial number, GUID or server ID is written here |
| Sources read | Origin's own code at `12a4e081`. The machine's own output: its serial console and its arrival times, and from the private dump the provider's system's DMI strings, PCI list and the ACPI tables' hashes. The coordinating session's run log, for its times. AMD's PPR for Family 19h Model 11h B2 (55901, revision 0.47, 2024-08-28; its ZIP's SHA-256 `e47c7461141fe424567ac14edfcedc6e5e85c9d3e23491f77795c0fbea385e8e`, the bytes rental E's ledger read as its [C], and [C] here too), volume 5, read again on 2026-11-22 from AMD's own site and paraphrased. No operating system's or other project's source was read |

**The machine is bench equipment.** It is outside ADR-0009 Amendment 1's scope: its cores are Zen 4. It qualifies nothing (item 233), and no requirements row changes.

**In short.** The control worked: in both boots that ran it, `0x06` to `0xCF9` before the call reset the machine, and the next run found the reset recorded. The run after it made the call and was not heard of again, with no `0xCF9` reset and no triple fault after it. That is TASKS item 121's reading A again, now with a positive control in the same boot. One thing changes the reading's strength: this firmware has the FCH's own `0xCF9` reset turned off (`PMxD2` bit 6) and resets through code of its own when the port is written, so the first reset after the call was firmware code after all. Only the triple fault behind it was free of firmware code.

## The rental

| | |
|---|---|
| Provider, plan, site | Latitude.sh, `m4-metal-small`, Amsterdam (`AMS`), $0.81 an hour |
| Bought, deployed, deleted | A read-only price check at 04:00:06 UTC matched $0.81. Deployed 04:00:33, deleted 04:25:50: 25 minutes 17 seconds, inside one clock hour. The status read afterwards listed the server as deleted |
| Cost | One hour, **$0.81**, as the tool's ledger counts it (the bill itself was not read). The ledger now holds 6 rentals, **$4.86** of item 225's $10 cap |
| Install | The tool copied the two bench images to the ESP, checked their SHA-256 there, wrote no UEFI variable, and found no `BENCH-STALE-KEY`, `BENCH-AFTER` or `BENCH-FCH` on the ESP |

## What the machine is

- Firmware American Megatrends **2.5 (2025-11-18)**: a fourth revision, after rentals B's and D's 2.4 (2025-04-29), rental E's 2.4a (2025-08-29) and rental C's 2.5a (2025-12-08).
- Its DSDT is byte-identical to rental C's, and its UEFI table to rentals C's and E's. Its SSDT1 differs from rentals B to E's, and its FACS, FIDT and `XX00` table from all four. The SPCR, MADT, FADT, IVRS, FPDT, MCFG, HPET, SPMI, TPM2, VFCT, WDRT, WSMT and SSDT2 to SSDT9 are byte-identical across rentals B to F.
- Otherwise as before: AMD EPYC 4244P (family 19h, model 61h, stepping 2), 12 logical processors, 64 GiB, the serial-over-LAN console on COM2 (`0x2F8`). The FCH's PCI functions: SMBus `1022:790b` revision 71h and LPC bridge `1022:790e` revision 51h.

## The console, boot by boot

Every run printed the same FCH lines but for `PMxC0`:
- `the FCH's power-management block at 0xfed80300 …: an AMD processor; the DSDT describes a system-memory region at 0xfed80300, 256 bytes, that holds it; the firmware's page tables map it to itself; the firmware's memory map: Mmio`;
- `the FCH's registers (read): PMx00 0xe3070bf7, PMx08 0x021c0514, PMx60 0x0800, PMx74 0x1e060117, PMxC0 …, PMxD2 0x40; 0xCF9 decoded (PMx00 bit 1): yes; its reset stopped (PMxD2 bit 6): yes; the shutdown message (PMx74 bit 17): yes; a shutdown as a PCI reset (PMx08 bit 20): yes; …`.

Times are when the bytes reached the tool, to the half second.

| Boot | Run | `PMxC0` | What the run did | Then |
|---|---|---|---|---|
| 1 | 1 (loader 04:02:34) | `0x00080800`: bit 19, a `0x06` to `0xCF9` | The provider's own system had just restarted the machine (`reboot: Restarting system` at 04:01:48, the firmware's first line 7.1 s later). No control in this boot, so the call: the straight line, the last line and `X` at 04:02:35 | Nothing for 7 min 38 s, until the tool's restart |
| 2 | 1 (04:10:51) | `0x00200800`: bit 21, a reset from a sleep state | The boot's first run, the control: `0x06` to `0xCF9` at 04:10:52, no call | The firmware's first line 6.6 s later; no line saying the write had not reset |
| 2 | 2 (04:11:39) | `0x00080800`: bit 19 | The call: `X` at 04:11:40 | Nothing for 6 min 34 s, until the tool's restart |
| 3 | 1 (04:18:53) | `0x00200800`: bit 21 | The control, at 04:18:53 | The firmware's first line 7.1 s later; no such line |
| 3 | 2 (04:19:40) | `0x00080800`: bit 19 | The call: `X` at 04:19:41 | Nothing for 6 min 9 s, until the rental ended |

No `Y` came in any run, and the serial-over-LAN session never dropped.

## What it proves

- **`PMxC0` survives a firmware start, and names each reset.** Rental E's ledger left this open. Here it read bit 19 after each control (twice), bit 21 after each of the tool's restarts (twice), and bit 19 after the provider's system's own restart. One cause at a time each time, as AMD's document says ([C] pp. 326–327). Bit 11, set in every value, is inside a reserved field, and the register's documented reset value is `0x00000800`.
- **The tool's restarts pass through a sleep state.** The FCH recorded them as bit 21, a reset from a power-saving mode or S3 to S5, not as the reset pin (bit 16), which rental E's ledger had inferred for a BMC's reset.
- **A `0x06` to `0xCF9` resets this machine before the call,** through the firmware's own code (the section on `PMxD2` below). Both controls were followed by the firmware's first line 6.6 and 7.1 s later. The provider's own restart took 7.1 s to the same line. Neither control printed the line it prints if the machine still runs 100 ms after the write, and each next run read bit 19.
- **The loader reached the call in all three runs that made it.** `X` is written after the last memory map and immediately before the call.
- **No error return,** as far as the console shows: a failed first call prints `Y` before the retry, and none came.
- **Nothing reset the machine after the call** for 6.2 to 7.6 minutes in any boot: not the `0xCF9` write due as the very first thing after a successful return, not the triple fault due 100 ms after it (armed in every run, with both of its bits set), and not a firmware watchdog.
- **The FCH's registers have the layout of AMD's documents on this processor.** The PM1 event block (`0x800`) is the FADT's, `PMxC0` named one cause at a time, and no 32-bit value read all ones. PPR 56713, this processor's own document, has no FCH chapter, so this is checked against [C], the Zen 4 server part's.

## What it does not prove

- **That a triple fault resets this board.** It was armed, but no run reached it. Under reading A no run could.
- **Which way the firmware's `0xCF9` trap goes** (the next section): the SMI block was not read.
- Anything about the kernel, which was not entered.

## `PMxD2` bit 6: the firmware traps `0xCF9` (the coordinating session's first point)

**What AMD's document says** ([C], volume 5):
- **`PMxD2` bit 6** (`cf9rstdisable`, reset value 0): when set, a write to `0xCF9` does not cause a reset. The document gives its purpose: to let the firmware trap the port (p. 330).
- **The trap itself** is in the FCH's SMI block at `0xFED80200`, which the DSDT's 4 KiB region at `0xFED80000` covers:
  - a status bit records that bit 2 of `0xCF9` was written as 1 (`SMIx90` bit 13, p. 99). `0x06` sets bit 2;
  - a two-bit control routes that event to nothing, an SMI, an NMI or IRQ13 (`SMIxC0` bits 27:26, p. 119).
- **The register of reset causes** (`PMxC0`) records bit 19 when the reset was caused by `0x06` written to `0xCF9` (p. 326).

**What follows on this board:**
- **The FCH did not reset on the control's write itself.** Bit 6 reads 1 in every run, so the write alone does nothing.
- **Firmware code reset the machine.** The reset came within 7 s, and the next run read bit 19: a reset the FCH counts as caused by `0x06` to `0xCF9`. The simplest path consistent with both is the one the document describes: the write raises an SMI, and the firmware's handler resets the machine, very likely by clearing bit 6 and writing `0x06` itself. The route is inferred, not read: `SMIxC0` was not read, so an NMI or IRQ13 route is not excluded, though no firmware handles a reset that way that we know of.
- **The provider's own restart ended in the same bit-19 reset.** Whatever path that system takes (the FADT's register is the SMI command port, `0xBE` to `0xB2`), the firmware's resets on this board end as `0x06` to `0xCF9`.
- **So on this board, Amendment 4's first reset after the call is firmware code,** most likely in SMM, and only the triple fault is free of it. TASKS item 121's hand-over read bit 6 as "`0xCF9` does not reset this board"; that was wrong, and the loader's words "its reset stopped" say too much. The bit stops the FCH's own reset, not the firmware's.

**Whether a trapped write behaves differently after the call.** An SMI handler belongs to the firmware's runtime, not to boot services: it stays installed after `ExitBootServices`, and nothing in [C] ties the trap to boot services. Whether this firmware's handler checks something that the exit changes is not known. Nothing seen shows it.

### Why both resets failing still reads as A

After a call that returns `EFI_SUCCESS`, the bench loader runs, in order: the status's compare, the `out` to `0xCF9`, `cli`, a 100 ms spin on the time-stamp counter (380 million ticks at the rate it measured), then a zero-limit IDT and `int3`. Only the `out` can reach firmware code.

- **The triple fault does not go through the firmware.** A shutdown sends a message to the FCH, which resets the system when `PMx74` bit 17 and `PMx08` bit 20 are set; both were. The FCH records that reset as bit 23 (pp. 25, 326).
- **So, for the call to have returned and no reset to follow, one of two things must hold:**
  - **A′:** the trapped write entered firmware code that neither reset the machine nor returned to the loader, after the same trapped write had reset the machine within 7 s before the call. One fault on rental F; but it cannot be the one cause of all five rentals. Rental C ran the normal loader, which enters the kernel after the call, and its kernel writes to `0x2F8` from its first line (rental C's findings). Under A′, rental C's silence needs the console's path to fail after the call as well: rental E's failure (i).
  - **2′:** the trapped write returned without a reset, and the triple fault did not reset either, against AMD's document for the two bits as read. Two independent failures, and for rental C a third, the console's.
- **A needs one fault for all five rentals** on four firmware revisions with both loaders: the call does not return.
- **A failed first call with `Y` lost** would need the console to fail after a failed call, then the retry to hang (A on the second call) or to fail as well, with Amendment 2's line lost and both of Amendment 3's resets failing. Weighed as negligible.

**The ranking:** A first, well ahead. A′ second: it needs the firmware's trap to act differently after the call, and a second fault for rental C. 2′ last.

**What would remove A′ and 2′:** after the call, the triple fault first, with no `0xCF9` write before it, and in each boot's first run a triple fault as the control (its record is bit 23). Then nothing the firmware wrote lies between the call and the reset, and the control shows the shutdown path resets this board. That changes Amendment 4's order, so it is an owner item.

**The watchdog,** as on rental E: no reset came within five minutes of the loader's start in any boot. Under A the firmware stopped its watchdog during its own exit handling, before the hang, or this firmware arms none. Nothing more is seen of it.

## The firmware revision (the second point)

Rental F ran on 2.5 (2025-11-18), a fourth revision. The stop is the same.

| Rental | Firmware | Loader | Stop |
|---|---|---|---|
| B | 2.4 (2025-04-29) | normal | the loader's last line |
| C | 2.5a (2025-12-08) | normal | the loader's last line |
| D | 2.4 | bench (TASKS item 112) | the loader's last line |
| E | 2.4a (2025-08-29) | bench (TASKS item 116) | `X`, then no reset |
| F | 2.5 (2025-11-18) | bench (TASKS item 121) | `X`, then no reset; the control before the call resets |

Rental F's DSDT is rental C's, and its SSDT1 is new. The normal loader reads the SPCR, the same on every rental, and none of the tables that differ; rentals B and C, with that loader, stopped as well. The bench loaders also read the FADT, the same on every rental, and rental F's scans the DSDT, which differs between 2.4 and 2.5 but holds the PM block's region in both.

## The UART's `LSR 0x00` (the third point)

**What was read.** In the three runs that made the call, as in rental E's three: `LSR 0x00`. A control run makes no call and reads no UART. The holding register (THRE, bit 5) and the transmitter (TEMT, bit 6) were both not empty, no byte had arrived, and no error was flagged.

**What it means.** The loader reads the registers just after the firmware's console has written the run's previous line, the FCH's decision line, 223 bytes. At 115,200 baud a byte takes 87 µs, and the firmware's console returns as soon as its last bytes are in the UART's FIFO. A busy transmitter at that instant is what a working UART shows.

**THRE does set.** The next line goes straight to the UART through Origin's own writer, which waits for THRE before every byte, for at most 100,000 status reads, and abandons the line if it never comes (`uart.rs`). Every byte of that line arrived, and so did every byte of `X`, written after the memory map and immediately before the call. So THRE set within that bound for every byte, right up to the call.

**Could the firmware's exit wait on THRE for ever?** Only if THRE stopped setting at the call. A handler in the exit (the console redirection's, say) would have to stop the transmitter first and then wait on it: turn the FIFO or the port's decode off, set loopback, or find the BMC's side no longer taking bytes. Nothing seen shows that. The provider's own system calls `ExitBootServices` through the same console redirection, and its call returns.

**One difference remains.** Origin's loaders make the call while the UART still holds bytes: `X` and its line end, written straight (bench builds), or, very likely, the end of the last line through the firmware (rentals B and C's normal loader: a 16-byte FIFO takes 1.4 ms to empty). Whether the provider's system makes the call with an empty transmitter is not known.

**Its rank:** below the stack's depth, the copied table and a minimal program, and above nothing. The evidence leans against it, and the next step's minimal program tests it for free if it writes its mark straight to the UART before the call, as Origin's loaders do.

## The next step, ranked

The list of what Origin's loader does before the call that a minimal program does not (`docs/research/2026-11-21-loader-before-the-call.md`) names the candidates under A. Ranked with this rental's two new points:

1. **The stack's depth on the firmware's stack, measured under QEMU (that list's candidate 2; free). Done by TASKS item 127 the same day, and ranked last since** (`docs/research/2026-11-22-loader-stack-depth.md`).
   - **The rentals' path** (an unsigned kernel): at most 10,312 bytes of the 128 KiB UEFI promises, 7.9%.
   - **The shipping path** (a signed kernel): at most 23,240 bytes, 17.7%.
   - OVMF's own frames are in both figures, and the firmware's would need over 100 KiB more to matter. So the stack drops to the bottom of the candidates (the coordinating session's ruling, 2026-11-22). Ranks 2 and 3 below are now first and second.
2. **Reads before the call in the next bench build (free, riding with any rental):**
   - the live boot services table's `ExitBootServices` pointer and the table's CRC32, compared with the loader's copy, just before the call (that list's candidate 3);
   - `SMIxC0` bits 27:26, the route of the `0xCF9` trap, through the same checks as the PM block (the DSDT's region at `0xFED80000` covers it).
   Both are reads before the call; whether they need an owner item is the coordinating session's call, as for the PM block's reads.
3. **The next rental: a minimal program, with the triple fault first after the call** (an owner item and an amendment; about $0.81, the ledger then $5.67 of $10).
   - **What it is:** a bench mode that does only what a minimal program does (the memory map, then the call), plus the FCH's reads its control needs, and writes `X` straight to the UART before the call as the loaders do. After a successful call: the triple fault, with no `0xCF9` write. The boot's first run triple-faults as the control.
   - **If it returns** (a third run, and the shutdown path shown to reset by its control): the call returns on this firmware for a program from this ESP, and the cause is something Origin's loader adds; the list is then bisected, a rental each, starting with its candidate 1, the OS-defined memory types.
   - **If it hangs too:** nothing on the list is the cause. What is left is how the firmware starts a program from `Boot0002` on this ESP, and the console's state at the call.
   - Either way it rules out A′ and 2′ for that program, and it tests the UART's busy transmitter at the call for free.
4. **The OS-defined memory types** (that list's candidate 1; a rental): only after 3 shows the minimal program returns.
5. **The UART at the exit** (the third point): last. Covered by 3 as long as the minimal program writes its mark straight before the call.

The line that says "its reset stopped (`PMxD2` bit 6)" should say instead that the FCH's own `0xCF9` reset is off, so that the firmware can trap the write. That is a one-line change, for the next bench build.

## What this rental cannot show

- Whether a triple fault resets this board: no run reached one.
- Where the firmware's `0xCF9` trap goes, and whether it acts the same after the call.
- Whether the firmware arms a watchdog.
- Anything on a processor in the declared scope (Zen 5 or newer): this machine is Zen 4.
