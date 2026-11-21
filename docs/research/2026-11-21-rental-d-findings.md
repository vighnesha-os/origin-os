# Rental D: the bench build's breadcrumbs on the rented server, and nothing after the loader's last line

| | |
|---|---|
| Date | 2026-11-21 |
| Kind | Research ledger (CLAUDE.md §16). Rental D of `docs/research/2026-11-12-first-bare-metal-boot.md`, after rental C (`docs/research/2026-11-21-rental-c-findings.md`) |
| Decided by | Items 224–236, 317 and 342; TASKS item 112 (the bench build); the owner approved this rental's cost in chat on 2026-11-21 |
| Ran | TASKS item 112's bench build at `0fc45e51`: the bench loader and the bench kernel, three boots |
| Tool | `developer-tools/bench/latitude.py rental-b`, at `0fc45e51` |
| Record | `docs/hardware/records/2026-11-21-latitude-m4-metal-small-d.md`, with Origin's serial log beside it |
| Raw data | Kept off the repository (item 232), under the owner's `~/.local/state/origin-bench/`. No IP address, MAC address, serial number, GUID or server ID is written here |
| Sources read | Origin's own code at `0fc45e51`. The machine's own output: its serial console, and from the private dump the provider's system's `dmesg`, serial-port list, DMI strings and ACPI tables (FADT, DSDT). The owner's copy of UEFI 2.11 (§3.1.2, §7.4, §7.5.1, §8.5.1.1), paraphrased. No operating system's source (CLAUDE.md §3) |

**The machine is bench equipment.** It is outside ADR-0009 Amendment 1's scope: its cores are Zen 4. It qualifies nothing (item 233), and no requirements row changes.

## The rental

| | |
|---|---|
| Provider, plan, site | Latitude.sh, `m4-metal-small`, Amsterdam (`AMS`), $0.81 an hour |
| Bought, deployed, deleted | Bought at 08:00:31 UTC, after a read-only price check at 08:00:05 matched. Deployed 08:00:36, deleted 08:25:51: 25 minutes 15 seconds, inside one clock hour. A status read afterwards listed no bench server, and no watchdog or tool process remained |
| Cost | One hour, **$0.81**, as the tool's ledger counts it (the bill itself was not read). The ledger now holds 4 rentals, **$3.24** of item 225's $10 cap |
| Install | The tool copied the two bench images to the ESP, checked their SHA-256 there, wrote no UEFI variable, and found no `BENCH-STALE-KEY` on the ESP |

## What the machine is

- A server ID that neither earlier server had.
- Firmware American Megatrends **2.4 (2025-04-29)**, rental B's revision, not rental C's 2.5a. Its DSDT, FIDT and UEFI tables are byte-identical to rental B's.
- The SPCR, FADT, MADT, IVRS, FPDT, MCFG, HPET and all nine SSDTs are byte-identical across rentals B, C and D.
- Otherwise as rentals B and C describe it: AMD EPYC 4244P (family 19h, model 61h, stepping 2), 12 logical processors, 64 GiB, COM1 at `0x3F8` and COM2 at `0x2F8`, the serial-over-LAN console on COM2.
- The provider's system calibrated the time-stamp counter at 3,792.7 to 3,792.9 MHz, as on rental C's machine.

So the stop has now been seen on both firmware revisions (2.4 on B and D, 2.5a on C), and on at least two machines.

## The console, boot by boot

One serial-over-LAN session lasted the whole rental. Each boot showed the same sequence:

1. The firmware's progress text, `Supermicro H13SRE-F BIOS Date:04/29/2025 Rev:2.4`, then `Ready to Boot`.
2. The bench loader, through the firmware's console:
   - `bench build: no BENCH-STALE-KEY: ExitBootServices gets the firmware's own key`
   - `kernel verified: 10 sections, boot info 0.8, entry RVA 0x32000`, and the generator's tests passed
   - `console: the SPCR names a UART at I/O port 0x2f8 (interface 0x0, 115200 baud)`
   - `console: the walk read 908 bytes of firmware memory in 31 reads`, in 16,264 to 16,834 time-stamp counter ticks
   - KASLR, a different placement each boot; `page tables built: 98 tables` (97 on boot 3), `64614 MiB direct map`
   - **`bench build: entry state: CR0 0x80010011, CR4 0x628, EFER 0xd00, PAT 0x7040600070406; shadow stacks: S_CET 0x0, PL0_SSP 0x0, SSP table 0x0 (CPUID 07H.0:ECX[7] = 1)`**
   - `exiting boot services; entering the Framekernel`
3. Then nothing, for the 8 minutes each boot was given, until the tool restarted the machine. In the raw console, the last line's carriage return and line feed are followed directly by the clear-screen sequence of the next firmware start. There is no loader line after `ExitBootServices`, no halting line, not one breadcrumb byte, and no stray byte. The tool's reading: `breadcrumbs: none`.

## What it proves

- **The processor state the kernel inherits on this firmware**, read for the first time on a physical machine. Against OVMF's under QEMU at the same commit (`boot-bench`'s first scenario):

  | Register | AMI 2.4, this machine | OVMF, QEMU | What it means |
  |---|---|---|---|
  | CR0 | `0x80010011`: PE, ET, WP, PG | `0x80010033`: also MP and NE | AMI leaves the x87 error reporting (NE) and MP clear |
  | CR4 | `0x628`: DE, PAE, OSFXSR, OSXMMEXCPT | `0x668`: also MCE | AMI leaves machine-check exceptions off. No LA57, PCIDE, SMEP, SMAP, PKE or CET bit on either |
  | EFER | `0xD00`: LME, LMA, NXE | the same | No-execute is already on |
  | PAT | `0x0007040600070406` | the same | The processor's power-on value (Intel SDM; AMD APM) |
  | S_CET, PL0_SSP, SSP table | 0, 0, 0 | not read: QEMU's software processor reports no shadow stacks | Supervisor shadow stacks are off |

- **Rental C's second cause, in its shadow-stack form, is excluded on this firmware.** Supervisor shadow stacks are off, so the entry stub's `call` cannot fault on one.
- **The three shadow-stack reads ran without a fault** on a real Zen 4 with CPUID's shadow-stack bit set: TASKS item 112's risk D1 did not happen.
- **AMI's three clear bits** (CR0.MP, CR0.NE, CR4.MCE) are ones the kernel sets for itself, in `framekernel/frame/src/cpu.rs`:
  - `init_cpu_state` sets CR4.MCE;
  - `enable_simd` sets CR0.MP and CR0.NE;
  - `cpu_state_holds` checks both MCE and NE.

  That is read in the code, not seen on this machine.
- **The bench loader's lines all ran three times** on firmware 2.4, as the normal loader's did on 2.5a.

## What it does not prove

- Whether `ExitBootServices` returned.
- Anything about Origin's kernel on this machine.
- Anything about the declared scope (ADR-0009 A1).

## Where it stops

### The steps after the loader's last line

In the bench build at `0fc45e51`, in order:

| # | Step | Where | What it writes straight to `0x2F8` |
|---|---|---|---|
| L1 | The panic console is given up. The memory map is read and translated, the kernel image carved out, and the boot info written | `boot/uefi/src/main.rs:615–657` | Nothing. A failure here, before the first call, is reported on the firmware's console, and none was seen |
| L2 | `ExitBootServices`, with the firmware's own key. One retry if the first call reports a changed key | `main.rs:665`, `boot/uefi/src/uefi.rs:377` | Nothing |
| L3a | It succeeded: `origin-boot: bench build: boot services exited on the first call` (or `… on the retry …`) | `main.rs:675` | The whole line. Each byte waits for room for at most 100,000 status reads; a byte that never finds room ends the line |
| L3b | It failed: decision item 342's line, `origin-boot: ExitBootServices failed (…): halting`, then the halt | `main.rs:701` | The whole line, the same way |
| L4 | The entry stub: `R` after the CR3 load, `S` after the stack switch and a write to the new stack | `main.rs:1142` | `R` and `S`, each written **even if** its wait found no room |
| K1–K8 | The kernel's steps to its first line | `framekernel/src/kernel.rs` | `1` to `8`. Each waits for room, and the kernel gives up a byte that finds none |

None of these bytes was seen, so one of two things happened:

- **(A)** the machine stopped before `ExitBootServices` returned; or
- **(B)** the call returned, either way, and nothing written straight to `0x2F8` after that reached the console.

### The readings

**A1. `ExitBootServices` does not return on this firmware**, when Origin's loader calls it:
- It may hang inside the call, or in a handler the firmware runs as boot services end.
- One cause explains rentals B, C and D: in each, the first byte that would have shown comes after the return.
- But the provider's own system ends boot services on this firmware and then uses COM2 (rental C's E1). So A1 needs something Origin's loader does before the call that the provider's loader does not do. Nothing in L1 or before is known to be such a thing.

**A2. The loader stops in L1, before the call.**
- A panic or a fault in the loader's own code, silent because the panic console was given up just before.
- Unlikely: the same map reading and translation ran twice before in this boot (`main.rs:432` and `main.rs:453`), on a map that differs only by the allocations in between.
- A panic spins with interrupts as they were, under boot services, and the firmware's watchdog would then reset the machine. UEFI 2.11 §3.1.2 requires the boot manager to arm a 5-minute watchdog before it starts a boot option; §7.5.1 says an expired watchdog resets the platform; and Origin's loader never calls `SetWatchdogTimer`. No reset came in any of the six 8-minute windows (rentals C and D).
- A fault, taken by the firmware's exception handler with interrupts off, would get no such reset. So this is weighed down, not excluded.

**B1. The call returns, but the UART stops transmitting, or transmits unreadably, after the firmware's exit handling changes its registers.**
- The coordinating session's first reading was that this needs two conditions: it would also need the kernel to stop before K7 on rental C, because K7 programs the UART. That is true for any change K7 undoes: K7 sets the line control (8N1), the FIFO, the interrupt enable, and DTR, RTS and OUT2 (which also clears loopback).
- **It is not true for the divisor.** With the SPCR's revision 2 (no clock field) and a nonzero latch, `origin_spcr::line_setup` keeps the latch K7 finds (`firmware/acpi/spcr/src/lib.rs:173–187`, from `framekernel/src/console.rs:1158–1165`).
- So a divisor changed to another nonzero value as boot services end would have been kept by rental C's kernel too. That is **one cause** for rentals C and D.
- Weighing against it: a wrong rate on a physical UART usually shows as stray bytes, and none came. It shows nothing only if the console drops them, or if the console's UART is a virtual one, where the rate does not matter.

**B2. The call returns, but the port is switched off as boot services end:** its decode, or its path to the serial-over-LAN console.
- **One cause** for rentals C and D. Rental C's kernel probes the loader's UART at K7 and refuses it when the status reads as all ones (`framekernel/src/console.rs:1151`). It then goes on with COM1 alone, which nobody watches.
- For the provider's system, which uses COM2 after boot services end, the port would have to be switched back on.
- The DSDT shows the platform can do that: both COM ports are devices that the operating system can disable (`_DIS`) and configure and enable (`_SRS`), with `_STA`, `_CRS` and `_PRS`. That is the pattern of a configurable Super I/O logical device.
- Nothing shows the firmware switching it off at `ExitBootServices`, and Origin evaluates no device's `_SRS`.

### The ranking

**A1 and B (B1 with the divisor, or B2) cannot be ranked from what was seen.** Each is one cause for rentals B, C and D, each is silent, and the watchdog does not separate them.
- The watchdog weighs only against stopping inside boot services with the timer still running.
- A1 needs Origin's call to differ from the provider's in a way no one has found.
- B needs the firmware to touch the console UART, or its path, as it exits. That is the kind of thing a firmware that redirects its console to a UART does.

So I lean slightly to B, but do not rank it first. A2 comes last.

### The walk's three times longer figure

- The walk read the same 908 bytes in the same 31 reads on rentals C and D, through byte-identical tables. Rental C took 5,396 to 5,814 ticks; rental D took 16,264 to 16,834.
- At the 3,792.7 to 3,792.9 MHz both machines' counters run at, that is **1.4 to 1.5 µs on rental C and 4.3 to 4.4 µs on rental D**.
- The two machines differ in firmware revision (2.5a and 2.4). Where the firmware places its tables, and the core's clock while the loader runs, are the plausible causes. Neither was measured.
- **It has no bearing on the stop:** the walk finished, and every later loader line printed.
- For cost ledger row CL46 it is a second physical figure. Both are below 5 µs, once per boot. QEMU's software processor takes 0.79 to 1.37 ms for its smaller walk.

## The next diagnostic, ranked

Each needs one rental: **$0.81** if it starts in the first minutes of a clock hour, **$1.62** at the tool's planned worst case. The ledger holds $3.24 of $10, so at most **$4.86** after it, with $5.14 left.

### Rank 1. A bench loader that separates A from B in one rental, with the kernel left out

The bench loader only; the bench kernel is unchanged and not entered. Every boot alike:

1. **Under the firmware**, before its last line, the loader reads the UART at the SPCR's port: IER, LCR, MCR, LSR, MSR, the scratch register, and the divisor (DLAB set for the two reads, then LCR put back). It prints them **through the firmware's console**, which is known to work.
2. **One line straight to the UART**, as the firmware left it, still before `ExitBootServices`. This is the coordinating session's marker, made a whole line.
3. **One byte straight to the UART just before each call** (`X` before the first, `Y` before the retry). It shows that L1 finished and the call is being made. It writes nothing to memory, so the map key stays valid.
4. **After the call returns:**
   - first, the existing line, unchanged, to the UART as it is;
   - then the UART's registers are read again;
   - then the values read in step 1 are written back: divisor, LCR, IER, MCR, and the FIFO enabled and cleared, since FCR cannot be read;
   - then a second line, which **names the registers as read after the call**.
5. **Then a reset, about 5 seconds later, whatever was seen.** First the runtime service `ResetSystem` with a cold reset (UEFI 2.11 §8.5.1.1), which stays callable after `ExitBootServices`. If the machine is still running 2 seconds later, the FADT's reset register (on this board, the value `0xBE` to I/O port `0xB2`, the firmware's SMI command port). Then a halt.

   A reset shows as a second firmware start inside the tool's 8-minute window, and then as a loop. Rentals C and D never showed one.

| What the console shows | What it means | What follows |
|---|---|---|
| No line from step 2 | Raw writes to `0x2F8` do not reach the console even while the firmware owns the UART | Step 1's values say what the firmware does differently; the resets still say whether the call returns |
| Step 2's line and `X`; nothing more; **no reset** | **A1**: `ExitBootServices` does not return | Free work first: list everything Origin's loader does before the call that a minimal UEFI program does not. Then a rental with a bench loader that calls `ExitBootServices` as early as it can, and halves the difference |
| Step 2's line and `X`; nothing more; **resets** | **B2**, or a change that putting the firmware's values back does not undo | Read the device's ACPI objects (`_STA`, `_SRS`) with Origin's AML interpreter, offline; the next bench loader evaluates them after the call |
| Step 4's second line only, then resets | **B1**: the firmware's exit handling changed the UART's registers, and the line says which | A design decision for ADR-0010: the kernel must not keep a latch it finds after `ExitBootServices`, for example by taking the loader's value through boot info. Then a rental with the kernel |
| Both of step 4's lines, then resets | Raw writes work after the call; rental D's silence did not repeat | A rental with the kernel and its breadcrumbs |

- **Why not the coordinating session's "program it as K7 does"?** K7's rule keeps whatever divisor it finds, which is exactly what is in question. Putting the firmware's own values back, and printing what was found after the call, settles B1 and names the change.
- **Why reset, and leave the kernel out?** Without a channel that does not depend on COM2, silence means A1 or B2, as it did on this rental. The kernel's breadcrumbs are worth reading only once the UART carries bytes after the call, and every outcome above says when that is.
- **Free first, under QEMU:** each reset method shown to work (QEMU with `-no-reboot` exits on a reset; OVMF implements both methods), and the new lines checked by `boot-bench`'s scenarios. Controls: the reset silenced, and the registers left unrestored.
- **A ruling it needs:** calling `ResetSystem` after `ExitBootServices` is a bench-only exception to ADR-0002 B5, which keeps every loader from firmware services after the call. The FADT's register alone avoids that exception, at the cost of a FADT read in the loader's walk. On this board it reaches the firmware too, through an SMI.

### Rank 2. As rank 1, but keeping the kernel, and resetting only when the UART reads all ones after the call

- It saves a rental in B1's case, since the kernel's breadcrumbs would follow the restored line.
- It loses the separation between A1 and any form of B that the UART's status does not show.

### Rank 3. The coordinating session's test as given

A raw marker before the call, then K7's programming and a line after it. Rank 1 does all of it except K7's programming, which rank 1 replaces with the firmware's own values.

## Follow-ups (records, no rental)

- **Cost ledger row CL46** still says "unmeasured on a physical machine". It can take rentals C's and D's figures: 1.4 to 4.4 µs at 3.79 GHz, once per boot.
- **If B1 is confirmed, ADR-0010's latch rule** (TASKS item 84: the kernel keeps a nonzero divisor it finds) is the thing to change. That is the owner's decision, through the coordinating session.

## What this rental cannot show

- Whether `ExitBootServices` returned.
- Anything about Origin's kernel on this machine.
