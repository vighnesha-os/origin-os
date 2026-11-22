# Rental G: the minimal program, and a reset after every call

| | |
|---|---|
| Date | 2026-11-22 |
| Kind | Research ledger (CLAUDE.md §16). Rental G of `docs/research/2026-11-12-first-bare-metal-boot.md`, after rental F (`docs/research/2026-11-22-rental-f-findings.md`) |
| Decided by | Items 224–236, 347 (ADR-0002 Amendment 5, the minimal-program mode), 348, 349 and 350 ("One more, then pause"); TASKS item 128; the owner's "Rent it" on 2026-11-22, through the coordinating session |
| Ran | TASKS item 128's minimal-mode bench build at `32b6e043`: the minimal loader, and the bench kernel beside it, which it neither reads nor enters; six boots, thirteen loader runs |
| Tool | `developer-tools/bench/latitude.py rental-b` at `32b6e043`, run by the coordinating session's guarded script |
| Record | `docs/hardware/records/2026-11-22-latitude-m4-metal-small-g.md`, with Origin's serial log beside it |
| Raw data | Kept off the repository (item 232), under the owner's `~/.local/state/origin-bench/`. No IP address, MAC address, serial number, GUID or server ID is written here |
| Sources read | Origin's own code at `32b6e043`. The machine's own output: its serial console and the times its bytes arrived, and from the private dump the provider's system's DMI strings, PCI list, memory size and the ACPI tables' hashes. Rentals E's and F's raw consoles and arrival times, for comparison. The coordinating session's run log, for its times. AMD's PPR for Family 19h Model 11h B2 (55901, revision 0.47, 2024-08-28), volume 5, pp. 119 and 326–327: rental F's ledger's [C], the same bytes, read from AMD's own site on 2026-11-22, and paraphrased. UEFI 2.11, §3.1.2 (p. 70) and §7.5.1 (pp. 205–206), from the owner's local copy, which came from a third-party file store and was not compared with the publisher's file; paraphrased. No operating system's or other project's source was read |

**The machine is bench equipment.** It is outside ADR-0009 Amendment 1's scope: its cores are Zen 4. It qualifies nothing (item 233), and no requirements row's state rests on it.

**In short.** The control worked in all six boots: a triple fault reset this board, and the next run found a shutdown recorded. Then each call that ran its course was followed by the same reset: the firmware started again 48.8 to 48.9 s after `X`, the delay it showed after each triple fault and after the provider's own restart. The one run that came before the tool's next restart read a shutdown and made the call again. So the minimal program's `ExitBootServices` most likely returned, and the triple fault behind it reset the machine. That is Amendment 5's "if the call returns" branch, on the firmware revision where rental E's bench loader stopped at the call. The cause of the stop is then most likely in what Origin's loaders do before the call, and the list of it is bisected next.

**The coordinating session's first reading was the opposite**: that the minimal program's call does not return either. It came from the rental tool's summaries, which say "with no reset after the call" for five boots. Those summaries are wrong. The tool counts loader runs, not firmware starts, and its five-minute boot ended before the run that would have followed each call's reset (section "The tool's reading").

## The rental

| | |
|---|---|
| Provider, plan, site | Latitude.sh, `m4-metal-small`, Amsterdam (`AMS`), $0.81 an hour |
| Bought, deployed, deleted | A read-only price check at 09:00:09 UTC matched $0.81. Deployed 09:00:36, deleted 09:30:35: 29 minutes 59 seconds, inside one clock hour. The status read afterwards listed the server as deleted |
| Cost | One hour, **$0.81**, as the tool's ledger counts it (the bill itself was not read). The ledger now holds 7 rentals, **$5.67** of item 225's $10 cap |
| Install | The tool copied the two images to the ESP, checked their SHA-256 there, wrote no UEFI variable, and found no `BENCH-STALE-KEY`, `BENCH-AFTER` or `BENCH-FCH` on the ESP |
| Plan | Item 350's: six boots of up to five minutes, each ending early when a run after a reset reaches its own call, or three minutes after the last call with no loader since |

## What the machine is

- Firmware American Megatrends **2.4a (2025-08-29)**: rental E's revision. Its ACPI tables are byte-identical to rental E's but for the FACS and the `XX00` table. Against rental F's 2.5, its DSDT, SSDT1, FACS, FIDT and `XX00` differ. The SPCR, MADT, FADT, IVRS, FPDT, UEFI table and the rest are the same on every rental.
- Otherwise as before: AMD EPYC 4244P (family 19h, model 61h, stepping 2), 12 logical processors, 64 GiB, the serial-over-LAN console on COM2 (`0x2F8`). The FCH's PCI functions: SMBus `1022:790b` revision 71h and LPC bridge `1022:790e` revision 51h.

## The console, run by run

Every run printed the same lines but for `PMxC0`:
- no stand-in table, so the FCH was read where the firmware's tables allow it;
- the PM block's checks passed, as on rental F;
- the FADT's reset register: `0xbe` to I/O port `0xb2`;
- `SMIxC0 0x04000000`: a write of bit 2 to `0xCF9` raises an SMI;
- `PMx00 0xe3070bf7, PMx08 0x021c0514, PMx60 0x0800, PMx74 0x1e060117, PMxD2 0x40`, rental F's values.

Times are when the bytes reached the tool, to the half second. "First byte" is the firmware's: a clear-screen, then its progress text. The tool's restarts are from the coordinating session's run log, to the second.

| Run | Tool's boot | Loader | `PMxC0` | Branch | Then |
|---|---|---|---|---|---|
| 1 | 1 | 09:03:32.0 | `0x00180800`: bit 19, beside bit 20 | control | Triple fault at 09:03:32.5; first byte 48.5 s later |
| 2 | 1 | 09:05:11.4 | `0x00900800`: bit 23, beside bit 20 | call | `X` at 09:05:11.9; first byte 48.8 s later, 47 s before the tool's restart |
| 3 | 2 | 09:06:53.1 | `0x00900800` | call | `X` at 09:06:53.6, 5.6 s after the tool's restart was issued; first byte 61.5 s later, 67.1 s after the restart |
| 4 | 2 | 09:08:47.5 | `0x00300800`: bit 21, beside bit 20 | control | Triple fault at 09:08:48.0; first byte 48.3 s later |
| 5 | 2 | 09:10:24.7 | `0x00900800` | call | `X` at 09:10:25.2. The tool ended its boot 2 on this second call and restarted 3.8 s after `X`; first byte 66.8 s after the restart |
| 6 | 3 | 09:12:30.2 | `0x00300800` | control | Triple fault at 09:12:30.7; first byte 48.3 s later |
| 7 | 3 | 09:14:11.4 | `0x00900800` | call | `X` at 09:14:11.9; first byte 48.9 s later, 29 s before the tool's restart |
| 8 | 4 | 09:17:32.4 | `0x00300800` | control | Triple fault at 09:17:32.4; first byte 48.8 s later |
| 9 | 4 | 09:19:09.6 | `0x00900800` | call | `X` at 09:19:09.6; first byte 48.8 s later, its banner at 09:20:18, then the tool's restart at 09:20:32 |
| 10 | 5 | 09:22:31.0 | `0x00300800` | control | Triple fault at 09:22:31.5; first byte 48.4 s later |
| 11 | 5 | 09:24:08.2 | `0x00900800` | call | `X` at 09:24:08.2; first byte 48.9 s later, its banner at 09:25:17, then the tool's restart at 09:25:32 |
| 12 | 6 | 09:27:32.3 | `0x00300800` | control | Triple fault at 09:27:32.3; first byte 48.9 s later |
| 13 | 6 | 09:29:11.7 | `0x00900800` | call | `X` at 09:29:11.7; first byte 48.8 s later; the rental ended at 09:30:32 |

No run wrote `Y` or Amendment 2's line. Between `X` and the firmware's next byte nothing arrived in any run. The serial-over-LAN session failed once, at boot 4's restart (09:15:39, 9 s after it was issued), while the firmware was starting after run 7's call; the tool reopened it at once, and the next start arrived whole.

### The firmware's own times on this revision

| From | To the firmware's first byte |
|---|---|
| The provider's system's last line ("Restarting system", boot 1) | 48.8 s |
| Each control's line, then a triple fault 50 ms later (six) | 48.3, 48.3, 48.4, 48.5, 48.8, 48.9 s |
| `X`, in the five calls the tool did not interrupt | 48.8, 48.8, 48.8, 48.9, 48.9 s |
| The tool's restart, as issued (five) | 66.8, 67.1, 67.1, 67.9, 68.0 s |

From the first byte to the loader's first line took 48.3 to 54.4 s. A boot of item 350's plan on this revision therefore runs: the tool's restart, the first run about 120 s later (119.0 to 122.4 s), the control's reset, the call about 99 s after the control (96.7 to 101.2 s), and, if the call is followed by a reset, the next run about 101 s after `X` (run 3: 101.2 s), so about 320 s after the restart.

On rental F's revision 2.5 the firmware's first line came 6.6 to 7.1 s after a reset. On 2.4a it comes after about 49 s, for every reset of the processor seen here.

## Reading the calls

**A reset followed each of the five calls the tool did not interrupt** (runs 2, 7, 9, 11 and 13). The firmware started again, and in runs 9 and 11 got as far as its banner before the tool's restart. These were not the tool's restarts: each came 29 to 47 s before the next one, and the tool's restarts take about 67 s to show a first byte, not 49.

**Each reset came within about a second of `X`.** From `X` to the first byte took 48.8 to 48.9 s. From the control's line to the first byte took 48.3 to 48.9 s, and the control triple-faults 50 ms after its line. The capture's times are to the half second, so the two sets cannot be told apart. Whatever ended each call ended it within about a second.

**What the one following run read.** Run 3 came after run 2's reset, before the tool's restart took effect, and read `PMxC0` bit 23: a reset caused by a shutdown, with the bits of every other cause but bit 20 cleared (AMD's document, [C] p. 326). It then made the call itself.

**The readings that fit:**
- **R: the call returned `EFI_SUCCESS`, and item 349's triple fault reset the machine.** Every observation fits: the delay of a triple fault, a shutdown recorded, no `Y` and no line. This is the designed path, and the one Amendment 5 tests.
- **S: the firmware's own exit shut the processor down,** within a second, five times out of five. It also fits every observation, since after `EFI_SUCCESS` the mode writes nothing before its triple fault (item 349). But during the exit the firmware's own exception handlers are still installed, so a fault inside it reaches a handler rather than shutting the processor down. And on the same revision, rental E's bench loader made the same call and nothing at all followed for over seven minutes. Under S, this firmware's exit would fail in two different ways for two programs.

**The readings that do not fit:**
- **A hang**, rentals E's and F's stop: no reset follows. Rentals E and F, re-read for this ledger, show no byte from the machine between `X` and the tool's next restart, or the rental's end, in any boot. Rental E's third boot had a 15-second gap in the session 76 s after `X`, too short to hide a firmware start, which takes about 50 s of output before the loader's line on this revision.
- **An error return:** the retry would have written `Y`, and a second error Amendment 2's line. Neither came in any run.
- **A watchdog** (below): a reset at a fixed time after the program starts, recorded with a cause of its own.

**The ranking:** R first, well ahead; S second. Only a mark written after a successful call, before the triple fault, would remove S, and item 349 rules that out for this mode; changing it is an owner item (next steps, rank 4).

**Under R, Amendment 5's question is answered:** the call returns on this firmware for a program from this ESP, started by `Boot0002`, so the stop is most likely in something Origin's loaders do and the minimal program does not. And the firmware's exit does not turn off the shutdown message, which Amendment 5's "What must hold on the machine" left open.

## Boot 2, and the 114 s (the coordinating session's question)

The tool's summary for boot 2 says the machine reset itself after the first call, and the next loader came 113.9 s later. It mixes two boots:
- **Boot 2's first run was boot 1's sequel.** Run 2, boot 1's call, was followed by a reset 48.8 s later. Boot 1's five minutes ended at 09:06:47 and the tool issued its restart at 09:06:48. The firmware, already restarting, reached the loader at 09:06:53: run 3. It read bit 23, boot 1's call's reset, and made the call.
- **The reset after run 3 was the tool's restart,** issued 5.6 s before run 3's `X`. Its first byte came 67.1 s after it was issued, as every restart's did (66.8 to 68.0 s), and the next run read bit 21, the record of a restart.
- **The 113.9 s** is from run 3's `X` to run 4's loader line, across the tool's restart. So `PMxC0` did not read `0x00900800` before and after it: run 4, after the restart, read `0x00300800`.
- **Whether run 3's own call returned** cannot be told: the tool's restart came within seconds of it either way, and its record overwrote the cause.

Nothing in the arrival times around the 114 s points elsewhere: no byte between run 3's `X` and the restart's first byte, and the session stayed open.

## What a watchdog would look like on this board

- **The boot manager's watchdog** (UEFI 2.11 §3.1.2 and §7.5.1): the firmware's boot manager must arm a five-minute watchdog before it starts a boot option, and a successful `ExitBootServices` turns it off. On expiry the firmware resets the platform, through `ResetSystem` or a platform action. A reset five minutes after the loader started, not one second after `X`.
- **The FCH's own record** ([C] p. 326) gives each timer its own cause: bit 24 for AMD's fail-boot timer, bit 25 for the watchdog timer the FCH provides for the operating system, and bit 29 for MP1's watchdog, which no other reset clears. A reset the firmware makes in its own code ends on this board as a write to `0xCF9` (rental F), bit 19. A reset by the BMC is the reset pin (bit 16), or, as the tool's restarts show on rentals F and G, a power transition (bit 21).
- **None of these** was read after a call. The only record after a call that ran its course is bit 23, a shutdown. Bits 24, 25 and 29 appear in none of the thirteen reads.

So no watchdog shows on this rental, as none showed on rentals E and F, where nothing reset the machine for over six minutes after `X`.

## `SMIxC0`: the firmware's `0xCF9` trap is an SMI

All thirteen runs read `SMIxC0` as `0x04000000`. Bits 27:26 are `01`, which AMD's document gives as an SMI for a write of bit 2 to `0xCF9` ([C] p. 119). So on this board, firmware 2.4a, a write that would reset through `0xCF9` goes to the firmware's SMM code: rental F's inference, now read, though on the other revision.

## `PMxC0`'s two bits (the coordinating session's third point)

Every read on rental G held bit 20 beside exactly one other cause: bit 19 after the provider's restart, bit 23 after each triple fault, bit 21 after each of the tool's restarts. On rental F's machine bit 20 was never set.

AMD's document explains it ([C] p. 326):
- every reset clears the cause bits in 31 and 28:16 but its own, **except bit 20**, a reset by `0x0E` written to `0xCF9`, which hardware never clears; software clears it by writing it;
- a `0x0E` reset is followed by a reset from a sleep state, which sets bit 21 as well.

So bit 20 is the record of some earlier full reset that nothing on this machine has cleared since. It was there at the first read, after the provider's own restart, so its origin is not in this rental. Rental F's "one cause at a time" still holds for every cause the hardware clears, which is all of them but bit 20. One consequence: on this machine a tool's restart (bits 21 and 20) cannot be told from a fresh `0x0E` reset, which sets the same two. Neither is a shutdown, so no run's branch depends on it.

It changes no run's reading. The loader's layout check accepts bit 20 beside one cause, as the code was written to (TASKS item 121's review, R121-10), and the minimal decision keys on bit 23 alone.

## What it proves

- **A triple fault resets this board,** and the FCH records it as a shutdown: six controls out of six. Rental F could not show this, since no run reached its triple fault.
- **The firmware's exit leaves the shutdown path working,** under R: a reset followed each call within about a second.
- **The minimal program reached the call in all seven runs that made it,** with `X` written straight to the UART immediately before it, and no error came back.
- **`PMxC0` survives a firmware start and names each reset,** as on rental F, with bit 20 kept beside it as AMD's document says.
- **The firmware's `0xCF9` trap is an SMI** on this board, firmware 2.4a.

## What it does not prove

- **That the call returned `EFI_SUCCESS`.** By item 349 nothing is written after a successful call, so R rests on the reset's timing and one read of `PMxC0`, and S is not excluded.
- **That the same holds on another revision:** rental F's 2.5 and rental C's 2.5a were not run with this program.
- **Which of the loaders' additions stops their call.** The minimal mode makes no copy of the boot services table, so it does not read whether the firmware changed the table after a copy.
- Anything about the kernel, which was not entered, and anything on a processor in the declared scope (Zen 5 or newer).

## The tool's reading (owner item 350's change)

The tool's summaries are wrong for this image, and the change that made them is owner item 350's, built in TASKS item 128:
- **It counts loader runs, not firmware starts.** A minimal boot "ends at its answer" only when a run after a reset reaches its own call, or three minutes after the last call with no loader since. A reset after the call shows first as the firmware's bytes, about 49 s after it on this revision, and the next loader comes about 52 s after those.
- **The plan was set from rental F's revision,** where the firmware's first line came 7 s after a reset. On 2.4a it comes after about 49 s, so the call came about 220 s into each boot and the run after its reset would have come at about 320 s: past the five-minute end by 6 to about 25 s. Neither early end fired except in boot 2, whose window held boot 1's sequel and so a second call.
- **"After the loader's last line the next byte came after 0.0 s"** measures from the call's line to `X`, which arrives with it. Measured from `X`, it would have read 48.8 s, and shown the reset.

A fix is free, and is the first of the next steps.

## What the minimal program clears from the list

The list of what Origin's loader does before the call (`docs/research/2026-11-21-loader-before-the-call.md`) holds the candidates. Under R, whatever the minimal program also does is not, alone, the cause on this firmware:
- the system table's checks, and the lines through the firmware's console (items 1, 2 and 14);
- the ACPI walks: the SPCR, the FADT and the DSDT's scan of about 470 KB (item 10; candidate 4);
- the FCH's reads, `CR3` and the firmware's page-table entries, and CPUID leaf 0 (rental F's loader's additions);
- the memory map read twice, into a static buffer;
- the mark written straight to the UART immediately before the call, with the transmitter still busy: rental F's "UART at the exit", its rank 5.

What the minimal program leaves out, and so what is left:
1. **The copied boot services table** (item 3; candidate 2). Every loader that stopped called `GetMemoryMap` and `ExitBootServices` through its copy; the minimal program called through the live table. The full bench loader at `32b6e043` already compares the two before the call (Amendment 4's addition), and has not run on a machine.
2. **Every allocation,** with Origin's OS-defined memory types first (item 6; candidate 1), as Amendment 5 names.
3. **The file-system calls:** the kernel's read, and the bench loaders' switch files (item 4).
4. **The framebuffer search:** `LocateHandle` and `HandleProtocol` for the graphics output protocol (item 11).
5. **The random-number generator's** thousands of steps on its own stack, interrupts masked in each (item 7; candidate 3).
6. **Work on the processor alone:** the kernel's verification and placement, the page tables built in memory, the memory map's other reads, and `CR4` and two CPUID leaves (items 5, 8, 9, 12 and 13); the stack's depth was measured and ranked last (TASKS item 127).

The bench loaders' own additions (`Stall`, the UART's registers, the runtime table's header, the entry state's reads) are not in rentals B and C's normal loader, which stopped as well, so none of them is the cause alone.

## The next step, ranked

Item 350's pause holds: no further rental is put to the owner unless two questions batch, or a machine in the declared scope appears.

1. **The tool's reading of a minimal image (free; now). *Done by TASKS item 130 the same day: its replay of rental G's console reads a reset after 5 of the 5 calls, with runs 3 and 5 overtaken.*** Measure the next byte from the mark, not from the call's line; count the firmware's bytes after a call's mark as that call's reset; and end a minimal boot when they come, or give it time for this revision's cycle (a run about 101 s after `X`). Test it on a fixture shaped like rental G's console and times, with no identifier in it, and re-read rental G's raw data with the fixed tool: it should report a reset after runs 2, 7, 9, 11 and 13.
2. **Stage the full bench image of `32b6e043` for a rental (free). *Staged by TASKS item 130.*** It holds Amendment 4's addition: the live table compared with the copy, both `ExitBootServices` pointers and both CRCs printed, before a call made through the copy.
3. **The next rental, two questions in one (when the pause lifts):** the full bench loader with the table's compare, and Origin's OS-defined memory types allocated as `EfiLoaderData` (an owner item and an amendment).
   - **If the call returns:** the memory types were the cause, and the table's line is read besides.
   - **If it hangs and the table differs:** the memory types are cleared, and the stale copy is the first suspect, tested next by calling through the live table.
   - **If it hangs and the table is the same:** both are cleared, and the bisection goes on with the file-system calls and the framebuffer search, then the generator.
   Whether this is the two batched questions item 350 asks for is the coordinating session's and the owner's call.
4. **A mark after a successful call,** one byte straight to the UART before the triple fault (an owner item: it changes item 349's "nothing between them"). It removes reading S. Free to build, it rides only with a later minimal-mode rental, and ranks last while R is well ahead.

## What this rental cannot show

- Whether the call returned `EFI_SUCCESS`, rather than the firmware's exit shutting the processor down (S).
- Whether the firmware changes its boot services table after a loader copies it.
- The same program on firmware 2.5 or 2.5a.
- Anything on a processor in the declared scope (Zen 5 or newer): this machine is Zen 4.
