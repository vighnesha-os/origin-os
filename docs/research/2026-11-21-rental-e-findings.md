# Rental E: the reset diagnostic on the rented server, and the A-pattern

| | |
|---|---|
| Date | 2026-11-21 |
| Kind | Research ledger (CLAUDE.md §16). Rental E of `docs/research/2026-11-12-first-bare-metal-boot.md`, after rental D (`docs/research/2026-11-21-rental-d-findings.md`) |
| Decided by | Items 224–236, 317, 342 and 344; TASKS item 116 (the reset diagnostic); the owner's "Rent it" at 13:39 UTC on 2026-11-21, through the coordinating session |
| Ran | TASKS item 116's bench build at `8ff56bd9`: the bench loader, and the bench kernel it checks but does not enter; three boots |
| Tool | `developer-tools/bench/latitude.py rental-b`, at `bec3b2ab` (the same bytes as at `8ff56bd9`), run by the coordinating session's guarded script |
| Record | `docs/hardware/records/2026-11-21-latitude-m4-metal-small-e.md`, with Origin's serial log beside it |
| Raw data | Kept off the repository (item 232), under the owner's `~/.local/state/origin-bench/`. No IP address, MAC address, serial number, GUID or server ID is written here |
| Sources read | Origin's own code at `8ff56bd9`. The machine's own output: its serial console and its arrival times, and from the private dump the provider's system's DMI strings, PCI list and ACPI tables (FADT, DSDT, the SSDTs). The owner's copy of UEFI 2.11 (§3.1.2, §7.4, §7.5.1, §8.5.1.1), paraphrased. AMD's own documents for the FCH, listed where they are used. No operating system's or other project's source was read |

**The machine is bench equipment.** It is outside ADR-0009 Amendment 1's scope: its cores are Zen 4. It qualifies nothing (item 233), and no requirements row changes.

## The rental

| | |
|---|---|
| Provider, plan, site | Latitude.sh, `m4-metal-small`, Amsterdam (`AMS`), $0.81 an hour |
| Bought, deployed, deleted | A read-only price check at 14:00:08 UTC matched $0.81. Deployed 14:00:36, deleted 14:25:54: 25 minutes 18 seconds, inside one clock hour. The status read afterwards listed the server as deleted |
| Cost | One hour, **$0.81**, as the tool's ledger counts it (the bill itself was not read). The ledger now holds 5 rentals, **$4.05** of item 225's $10 cap |
| Install | The tool copied the two bench images to the ESP, checked their SHA-256 there, wrote no UEFI variable, and found neither `BENCH-STALE-KEY` nor `BENCH-AFTER` on the ESP |

## What the machine is

- Firmware American Megatrends **2.4a (2025-08-29)**: a third revision, after rentals B's and D's 2.4 and rental C's 2.5a.
- Its DSDT and FADT are byte-identical to rentals B's and D's, and its UEFI table to rental C's. Its FIDT, FACS and an `XX00` table differ from all three. The SPCR, MADT, IVRS, FPDT, MCFG, HPET, SPMI, TPM2, VFCT, WDRT, WSMT and all nine SSDTs are byte-identical across rentals B to E.
- Otherwise as before: AMD EPYC 4244P (family 19h, model 61h, stepping 2), 12 logical processors, 64 GiB, COM1 at `0x3F8` and COM2 at `0x2F8`, the serial-over-LAN console on COM2. The FCH's PCI functions: SMBus `1022:790b` revision 71h and LPC bridge `1022:790e` revision 51h.

## The console, boot by boot

The three boots were alike; only the counts and addresses that change from boot to boot differ.

1. The firmware's start, about 50 s, then Origin's loader's lines through the firmware's console, among them:
   - `the FADT's reset register: write 0xbe to I/O port 0xb2` (the board's SMI command port, as on rental D);
   - `ResetSystem is at 0x985969e8; the time-stamp counter counted, over the firmware's Stall, 3800015 ticks a millisecond` (3,800,013 in boot 2);
   - the entry state, the same as rental D's;
   - `the UART at I/O port 0x2f8 under the firmware: IER 0x00, LCR 0x03, MCR 0x03, LSR 0x00, MSR 0xb0, scratch 0xaa, divisor 0x0001`.
2. `origin-boot: bench build: this line went straight to the UART, before ExitBootServices`: written with `in` and `out` to the port, not through the firmware.
3. `origin-boot: exiting boot services; entering the Framekernel`, the loader's last line through the firmware.
4. `X`, written to the port after the memory map was read and immediately before the call, in the same half second as line 3.
5. Nothing more within the boot: no line after the call, no `Y` (no retry), and no new firmware start.

| Boot | Loader runs | `X` at (UTC) | Next byte | What ended the boot |
|---|--:|---|---|---|
| 1 | 1 | 14:03:32 | 7 min 25 s later | the tool's restart |
| 2 | 1 | 14:11:49 | 7 min 8 s later | the tool's restart |
| 3 | 1 | 14:19:48 | none from the machine | the rental's end, 6 min 6 s after `X` |

**Boot 3's gap:** the BMC's serial-over-LAN session dropped at 14:21:04, 76 s after `X` ("No response to keepalive"), and reopened at 14:21:15; the console client printed its banner at 14:21:19. Bytes sent in those 15 s may be lost. The loader's reset was due 5 s after the call, 71 s before the gap, and a firmware start writes for about 50 s before the loader's lines, so the gap cannot hide one. Boots 1 and 2 had no drop.

## What it proves

- **The stop is the same on a third firmware revision.** Rentals B, C, D and E, on firmware 2.4, 2.5a, 2.4 and 2.4a, with the normal loader (B, C) and the bench loaders (D, E), all end at the loader's last line.
- **Raw writes to `0x2F8` reach the console while the firmware owns the UART.** The line straight to the UART and `X` both arrived, in order among the firmware's lines. That retires the first row of TASKS item 116's reading table.
- **The loader reached the call.** `X` is written after `GetMemoryMap` returned and the boot info was written, with nothing between it and `ExitBootServices`.
- **No error return.** A failed first call would have printed `Y` before the retry. None came.
- **No reset of any kind** for 6 to 7.4 minutes after `X`: neither the loader's, due 5 s after the call if it returned, nor a firmware watchdog's.
- **The UART as the firmware leaves it:** 115,200 baud, 8 bits, no parity, one stop bit (divisor 1, LCR `0x03`); DTR and RTS on, OUT2 off (MCR `0x03`); its interrupts off; `0xAA` in the scratch register, left there by the firmware. LSR `0x00` says the transmitter was still busy at that instant, just after the firmware's last line.
- **ResetSystem is in the runtime services table**, and the counter runs at 3.800 GHz by the firmware's `Stall`, 0.2% above the provider's system's calibration (3,792.7 to 3,792.9 MHz on rentals C and D).

## What it does not prove

- **That `ExitBootServices` did not return.** The two-failure reading below fits too.
- **Which part of the console's path would fail** under that reading.
- Anything about the kernel, which was not entered.

## The readings

**Reading 1 (A): `ExitBootServices` does not return** on this firmware, for Origin's loader.

**Reading 2, the two-failure reading (R116-1): the call returns, and then two things fail.**
- **(i)** No byte written to `0x2F8` after the call reaches the console, although the loader writes all of the 16550's registers back to the values it read under the firmware, and the same path carried `X` half a second earlier.
- **(ii)** No reset follows. Either `ResetSystem` neither resets nor returns, or it returns and the FADT's register, a write to the SMI command port, does not reset either.

A variant of reading 2, where the call returns and the loader stops before its first byte, is weighed as negligible. Between the call and that byte the loader returns from a closure, masks interrupts, and starts a raw write bounded at 100,000 status reads a byte. It touches no memory the call gives back and calls no firmware.

### What weighs for reading 1

- **One cause covers all four rentals,** across three firmware revisions and both loaders.
- **Reading 2 needs two independent failures** on one machine after a call that returned:
  - a console path that worked half a second before, with the UART's registers written back;
  - and a runtime service that the UEFI specification defines for use after `ExitBootServices` (§8.5.1.1) neither resetting nor returning, or else both reset methods failing.
- **Failure (i) would have to lie outside the 16550's eight registers:** in the I/O decode that carries `0x2F8` to the BMC, or in the BMC itself. Nothing seen shows such a change.

### What weighs for reading 2

- **The watchdog.** The rule:
  - UEFI 2.11 §3.1.2 says the boot manager must arm a five-minute watchdog before it starts a boot option.
  - §7.5.1 says the watchdog's expiry must eventually reset the platform, and that it is disabled when `ExitBootServices` succeeds.
  - Origin's loader never touches it.
- **What was seen:** no reset came within five minutes of the loader's start in any of rental E's three boots, nor in rentals B to D's eight-minute boots.
  - Under reading 2 that is what the specification says.
  - Under reading 1 it needs one of two things: the hang comes after the firmware has stopped the watchdog within its own exit handling, or this firmware arms no watchdog (§7.5.1 allows a system without one).
- This is the one observation that leans away from reading 1, and it rests on the firmware arming a watchdog, which nothing shows.

### Neutral

- **The provider's own system starts from this firmware and writes to this UART after its own `ExitBootServices`:** its shutdown messages open each rental's raw log. So the call returns for that program, and the UART can carry bytes after it, given whatever that system does to it.
- Under either reading, Origin's loader differs in something from a program for which this firmware works. That is what candidate (a) below lists.

### The ranking

1. **Reading 1 (A)**, first: one fault against two.
2. **Reading 2**, second, closer than one fault against two would make it, because of the watchdog's silence.

The next step should separate them without depending on either the console or the firmware's reset.

### The walk's figure

The loader's walk read the same 908 bytes in 31 reads in 5,548, 6,536 and 5,206 ticks: 1.4 to 1.7 µs at the counter's measured rate, on firmware 2.4a. Rental C (2.5a) took 1.4 to 1.5 µs; rental D (2.4) took 4.3 to 4.4 µs. Rental D is the outlier, and why was not measured. Cost ledger row CL46 holds the three figures.

## Resets with no firmware code on this FCH: what the documents and the dump say

The coordinating session's candidate (b) resets the machine as the very first thing after the call returns: a write of `0x06` to I/O port `0xCF9`, then, with no reset within 100 ms by the counter, a triple fault (a zero-limit IDT and `int3`). These are what AMD's documents say about both, read on 2026-11-21 from AMD's own site, paraphrased.

**Which documents:**

| | Document | Revision |
|---|---|---|
| [A] | PPR for Family 19h Model 61h B1 (56713), this processor's own | 3.05, 2023-03-08 |
| [B] | AMD64 Architecture Programmer's Manual, Volume 2 (24593) | 3.46, October 2026 |
| [C] | PPR for Family 19h Model 11h B2 (55901), the Zen 4 server part | 0.47, 2024-08-28 |
| [D] | PPR for Family 1Ah Models 00h–0Fh C1 (57238) | 0.49, 2026-03-06 |
| [E] | PPR for Family 1Ah Model 70h A0 (57930), a client part with an integrated FCH | 3.00, 2025-04-30 |

- **[A] has no FCH chapter.** It says nothing about `0xCF9`, the FCH's power-management registers or the cause of a reset. So everything below about the FCH comes from [C] and [D], which agree at every offset checked, and from [E] where it covers the same ground.
- **None of [C] to [E] is this processor's own document.** Each register below has to be read on the machine before anything rests on it.
- **The SHA-256 of each document's bytes read:**
  - [A]'s ZIP `a91f641b43a556bc2bbf4d6eb4042ef0145dd19fa1594afa090bdcd1fa959db8`.
  - [B] `e4130d5aa26227b6a116d75af9e0f6e5adb4c034ff0d49a3c0fe73c3f49794cf`.
  - [C]'s ZIP `e47c7461141fe424567ac14edfcedc6e5e85c9d3e23491f77795c0fbea385e8e`.
  - [D]'s ZIP `e91d9fe3ae32df3d6c88012cdac261440cc06e089a0d83d191247a06c8c91e99`.
  - [E]'s ZIP `6eb92cbcb64a41dc10d0ec3252b088768fc31e8c6a64907dc23e60ca707b0c52`.
- AMD publishes no revision guide for models 60h–6Fh, so this processor's stepping 2 is not confirmed to be [A]'s revision B1.

**Port `0xCF9`** ([C] volume 5 p. 43, [D] volume 6 p. 43, [E] p. 579):
- **The register:** the FCH's reset control register, reset value 0.
- **Its command field is bits 3:1,** with three defined values: `0x04` sends an INIT, `0x06` is a warm reset, and `0x0E` a cold reset that holds the system off for 3 to 5 s. Every other value is reserved.
- **Whether the port is decoded at all** is a bit in the PM block's decode-enable register (`PMx00`, bit 1), whose reset value is 0 ([C] p. 297, [D] p. 332).
- **A separate bit stops a write there from resetting,** so that firmware can trap it (`PMxD2`, bit 6, reset 0; [C] p. 330).
- **The PM block** is reached by memory at `0xFED80300` (the FCH's ACPI MMIO base `0xFED80000` plus `0x300`; [C] p. 296, p. 298).

**A triple fault:**
- **The processor:** [B] §8.2.9 (PDF p. 318) says the processor shuts down, and only NMI, RESET or INIT restart it.
- **The FCH:** [C] p. 25 and [E] p. 562 say a shutdown broadcasts a message on which the FCH resets the system, warm or cold as configured. Two bits gate it, both with reset value 0:
  - one enables the shutdown message (`PMx74`, bit 17; [C] p. 313);
  - one chooses between an INIT sent upstream and a PCI reset (`PMx08`, bit 20; [C] p. 299).
- **What the documents leave open:** what happens with the message disabled.

**The cause of the last reset** (`PMxC0`, [C] pp. 326–327, [D] p. 362):
- **What it records:** each reset sets one bit and clears the others' bits, so the register names the latest cause. All its bits are cleared by writing 1.
- **The bits it has:** among them, one for each of the three `0xCF9` commands (`0x06` is bit 19), the shutdown message (bit 23, recorded only when both bits above are set), the reset pin (bit 16), and the FCH's watchdog (bit 25).
- **Whether it survives a firmware start** is not settled: [C]'s table of sticky bits says bits 29:0 are sticky only under a debug setting. No document says what a BMC's reset or a power-on records. That a BMC's reset sets the reset pin's bit is an inference.

**What the dump shows:**
- **No ACPI table of the machine names port `0xCF9`.** No `0x0CF9` word appears in the DSDT or any SSDT. The FADT's reset register is the SMI command port (`0xBE` to `0xB2`), with `RESET_REG_SUP` set.
- **The DSDT addresses the PM block where the documents put it:** a 256-byte region at `0xFED80300`, and a 4 KiB one at `0xFED80000`.
- **Its fields in the PM block** are at offsets `0x00` (bit 6), `0x60`, `0x80` and `0xF0`: none of the bits above.
- The provider's system's PCI list shows the FCH as SMBus `1022:790b` revision 71h and LPC bridge `1022:790e` revision 51h.
- Nothing in the dump gives a register's value.

**So whether `0xCF9` resets this machine is not established.** It depends on two bits the firmware sets, whose values nothing has read. Whether a triple fault resets it depends on two more. A bench loader can read all four before the call, from the PM block at `0xFED80300`, and print them.

## The next step, ranked

### Rank 1. Candidate (b), with the FCH's own record of each reset as the positive control (an owner item; then about $0.81)

**What it is:** the coordinating session's candidate (b), and two additions.

**Addition 1: before the call, in every run,** the bench loader reads and prints:
- the PM block's decode-enable register (`0xCF9` decoded or not);
- the bit that stops a `0xCF9` reset;
- the two shutdown bits;
- the cause of the last reset (`PMxC0`).

These are reads only, from memory the firmware's own DSDT describes, before `ExitBootServices`, like TASKS item 116's UART registers.

**Addition 2: the cause of the last reset is the positive control, and it needs no switch, file or variable.**
- **If the last reset was not the loader's own `0xCF9` reset,** the run is the boot's first: the tool's restart or a power-on. As soon as it has printed, the run resets through `0xCF9` before the call, as the control.
- **If the last reset was the loader's `0xCF9` reset,** the run makes the call, and resets through `0xCF9` as the very first thing after it returns, before any line, register read or write-back. The triple fault follows if no reset comes within 100 ms.

**What a boot then shows:**

| Runs in the boot | Reading |
|---|---|
| One, whose last line says it is resetting through `0xCF9` before the call | `0xCF9` does not reset this machine, and the four bits it printed say why. The rental separates nothing, and costs one rental |
| Two: the control, then one that made the call and was not heard of again | **A**, with neither the console nor any firmware reset in the way. The second run's printout shows the control's reset recorded |
| Three or more, each after the first showing a `0xCF9` reset recorded | **The call returns.** Reading 2 holds, and its two parts become separate questions: the console's path after the call, and why `ResetSystem` did not reset |

- **Caveats:**
  - The control logic depends on `PMxC0` surviving the firmware's start. The documents leave that open, so the loader prints the raw value, and a value that never names a `0xCF9` reset reduces the rental to counting runs.
  - None of the registers is in this processor's own document. A value that contradicts the documents' layout (for example a decode-enable register that reads all ones) makes the loader say so and make the call with no control.
  - Under QEMU, `0xCF9` and the triple fault both show as QEMU exiting under `-no-reboot`. QEMU has no AMD FCH, so the PM block reads as absent, and the "no record" path is what QEMU shows.
- **Why it changes item 344:** ADR-0002 Amendment 3 allows `ResetSystem` and the FADT's register only. A `0xCF9` write and a triple fault after the call are new exceptions to B5. So rank 1 needs an owner item and an amendment before it is built. The reads before the call need none.
- **Cost:**
  - The build is a few hundred lines in `bench.rs` and `main.rs`, with host tests and QEMU scenarios, in one side session, plus its controls and gates.
  - One rental: $0.81 expected, $1.62 worst under the cap. The ledger holds $4.05, so that is at most $5.67 of $10.

### Rank 2. Candidate (a): list what Origin's loader does before the call that a minimal UEFI program does not (free, alongside rank 1)

- **What it does:** a reading of Origin's own code, with no rental. It lists each thing the loader does between `StartImage` and the call that a minimal program (load, read the memory map, call `ExitBootServices`) does not. The provider's own system's start shows the call returns on this firmware for some program.
- **What a first look shows, before the list:**
  - The loader writes no model-specific or control register before the call; those writes come only in its entry to the kernel, after the call.
  - It changes no display mode and no task priority level.
  - It asks the firmware only for:
    - protocols by handle: the loaded image, the file system (to read its two files), and the graphics outputs, found by a handle search;
    - pages, allocated and freed;
    - its console output;
    - the memory map;
    - `Stall` (bench builds only).
  - Between the map and the call it writes only the boot info, into memory it owns, and, in bench builds, the mark.
  - Since rentals B and C ran the normal loader, nothing that only bench builds do can be the cause.
- **What it gives:** under reading 1, the candidates for the cause, each one testable by a bench loader that leaves it out. It separates nothing by itself.
- **Cost:** nothing; about an hour of one session.

### Rank 3. Candidate (e): read the COM device's ACPI objects with Origin's AML interpreter (free)

- **What it does:** offline, from the private dump, reads `_STA`, `_CRS`, `_SRS` and `_DIS` of the device that owns `0x2F8`. It tells whether the firmware describes a step that turns the port's decode on, which reading 2's first failure would need.
- **What it costs:** nothing.
- **What it gives:** something only if rank 1 shows the call returns.

### Rank 4. The FADT's register first, then `ResetSystem` (R116-1's other option; about $0.81)

- **What it does:** within item 344's two methods, swaps their order, so a hang in `ResetSystem` no longer hides the FADT's reset.
- **Why it ranks below rank 1:** the FADT's register is the SMI command port, so firmware code still stands between the call and the reset. The console still decides how a run ends. A run that ends silently still reads two ways.
- **What it costs:** a smaller change than rank 1, and a rental. It changes item 344's order, so it too goes to the owner.

### Not ranked: a watchdog control

- **What it would be:** a bench build that waits six minutes inside boot services before the call. It would show whether this firmware arms the watchdog, and with it how much the watchdog's silence weighs.
- **Why it is left out:** it costs a rental for a datum that only shifts the weight between the readings. Rank 1 decides between them.

## What this rental cannot show

- Whether the firmware arms a watchdog: nothing in the boot waited five minutes inside boot services.
- Whether the line after the call was written and lost, or never written.
- Anything on a processor in the declared scope (Zen 5 or newer): this machine is Zen 4.
