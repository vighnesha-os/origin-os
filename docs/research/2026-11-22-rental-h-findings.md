# Rental H: the bisection mode, the same table, and a reset after both calls

| | |
|---|---|
| Date | 2026-11-22 |
| Kind | Research ledger (CLAUDE.md §16). Rental H of `docs/research/2026-11-12-first-bare-metal-boot.md`, after rental G (`docs/research/2026-11-22-rental-g-findings.md`) |
| Decided by | Items 224–236, 349, 350 and 352 (ADR-0002 Amendment 6, the bisection mode); TASKS item 131; the owner's "Rent it" on 2026-11-22, through the coordinating session |
| Ran | TASKS item 131's bisection-mode bench build at `4165ecfb`: the bisection loader, and the bench kernel, which it reads and verifies but never enters; six boots, twenty-two loader runs |
| Tool | `developer-tools/bench/latitude.py rental-b` at `c67b54b6` (its bytes checked against that commit), run by the coordinating session's guarded script |
| Record | `docs/hardware/records/2026-11-22-latitude-m4-metal-small-h.md`, with Origin's serial log beside it |
| Raw data | Kept off the repository (item 232), under the owner's `~/.local/state/origin-bench/`. No IP address, MAC address, serial number, GUID or server ID is written here |
| Sources read | Origin's own code at `4165ecfb`. The machine's own output: its serial console, the times its bytes arrived, the tool's restart times and the console's session events. From the private dump: the provider's system's DMI strings, PCI list, memory list and the ACPI tables' hashes. Rentals C's, F's and G's table hashes and server IDs, compared and not written here. The coordinating session's run log, for its times. `PMxC0`'s bits are read as the loader decodes them, after AMD's PPR 55901 as rental F's findings cite it; no vendor document was read anew. No operating system's or other project's source was read |

**The machine is bench equipment.** It is outside ADR-0009 Amendment 1's scope: its cores are Zen 4. It qualifies nothing (item 233), and no requirements row's state rests on it.

**In short.**
- **The first question: the copy is not the cause.** In all twenty-two runs the live boot services table was the loader's copy, byte for byte.
- **The second question: neither call hung.** The board reset 5.5 s and 6.1 s after `X`, the delay that follows every control's triple fault on this board. The firmware's start then had the shape it has after a shutdown.
  - So `ExitBootServices` most likely returned, and the triple fault behind it reset the machine (reading R).
  - Whether the firmware's exit itself shut the processor down (S) is still open: both readings are shutdowns, and the start that follows cannot tell them apart.
  - It ran on firmware 2.5a, rental C's revision. There the normal loader, with the OS-defined memory types, reached the call and nothing followed it for eight minutes.
  - So the full loader's work, with the boot info and kernel stack as `EfiLoaderData`, gets past the call where the loader with the types stopped. That points at the types, on two calls of one board.
- **Amendment 6's own confirmation did not come.** It asked for the run after each call to read a shutdown, bit 23. Here it read bit 19, as after most controls. On this revision the firmware resets the machine itself, twice, after most shutdowns, and the FCH then records its reset, not the shutdown. The reading rests on the start's shape instead (section "Why bit 19, and twice bit 23").
- **The cost of that:** twenty of the twenty-two runs took the control, and the four boots after the second call made no call at all.

**The coordinating session's first reading,** from the console, agrees on each point it made, which this reading checked one by one:
- firmware 2.5a, dated 12/08/2025;
- the table the same;
- the control looping on bit 19, with bit 23 twice, at console lines 159 and 250;
- `X` at lines 164 and 255, each followed directly by the firmware starting again;
- every map line with no region of an OS-defined type.

Three things are added here:
- the firmware's own double reset, which explains bit 19;
- the start's shape after each call;
- rental C's loader differs from this one in more than the types.

## The rental

| | |
|---|---|
| Provider, plan, site | Latitude.sh, `m4-metal-small`, Amsterdam (`AMS`), $0.81 an hour |
| Bought, deployed, deleted | Renting began at 15:00:31 UTC. Deployed 15:00:36, deleted 15:42:03: 41 minutes 27 seconds, inside one clock hour |
| Cost | One hour, **$0.81**, as the tool's ledger counts it (the bill itself was not read). The ledger now holds 8 rentals, **$6.48** of item 225's $10 cap |
| Install | The tool copied the two images to the ESP, wrote no UEFI variable, and found no `BENCH-STALE-KEY`, `BENCH-AFTER` or `BENCH-FCH` on the ESP. The coordinating session's guarded script had checked the tool's bytes, the images' SHA-256s, the six boots and the price before it rented |
| Plan | TASKS item 130's and 131's: six boots, each ending at its answer, or at a window from the firmware's own measured delays and the loader's own work, 5.8 minutes at rental G's delays |

## What the machine is

- **The firmware:** American Megatrends **2.5a (2025-12-08)**, rental C's revision, on a server ID no earlier rental had.
  - Its ACPI tables are byte-identical to rental C's but for the FACS and the `XX00` table.
  - Against rental F's 2.5, the FACS, FIDT, SSDT1 and `XX00` differ; against rental G's 2.4a, the DSDT, FACS, FIDT and `XX00`.
  - The SPCR, MADT, FADT, IVRS, FPDT and UEFI table are the same on every rental.
- **The rest, as before:**
  - AMD EPYC 4244P (family 19h, model 61h, stepping 2), 12 logical processors;
  - 63.1 GiB of System RAM;
  - the serial-over-LAN console on COM2 (`0x2F8`);
  - the FCH's PCI functions: SMBus `1022:790b` revision 71h and LPC bridge `1022:790e` revision 51h.
- **What the loader read, in every run:**
  - the FADT's reset register: `0xbe` to I/O port `0xb2`;
  - `SMIxC0 0x04000000`: a write to `0xCF9` raises an SMI;
  - the time-stamp counter at 3,800,015 ticks a millisecond;
  - the FCH's registers, as rental F's: `PMx00 0xe3070bf7, PMx08 0x021c0514, PMx60 0x0800, PMx74 0x1e060117, PMxD2 0x40`.

  Unlike rental G's board, `PMxC0`'s bit 20 is clear here.

## The console, run by run

Times are UTC on 2026-11-22. The tool restarted the machine at 15:01:52.2 (through the provider's system, which printed "Restarting system"), 15:07:14.8, 15:11:55.4, 15:19:07.1, 15:25:47.9 and 15:32:39.6. An "opening" is the firmware clearing the screen and printing "Pre-memory NB Initialization" a few seconds later, as every start of this firmware does. Its times are counted from the run's last byte: the decision line for a control, `X` for a call. The arrival times are about half a second apart.

| Run | Boot | Banner | `PMxC0` | Branch | The firmware's openings after the run's last byte (s) | The tool's restart (s after) |
|---|---|---|---|---|---|---|
| 1 | 1 | 15:02:50.9 | `0x00080800` (bit 19) | control | 5.6, 28.4, 51.7 | |
| 2 | 1 | 15:04:33.3 | `0x00080800` (bit 19) | control | 5.6 | |
| 3 | 1 | 15:05:27.6 | `0x00800800` (bit 23) | call, `X` | 5.5, 28.3, 51.7 | |
| 4 | 1 | 15:07:10.9 | `0x00080800` (bit 19) | control | 5.6, 40.5, 63.8 | +2.9 |
| 5 | 2 | 15:09:07.9 | `0x00080800` (bit 19) | control | 5.5 | |
| 6 | 2 | 15:10:03.7 | `0x00800800` (bit 23) | call, `X` | 6.1, 28.9, 52.1 | |
| 7 | 2 | 15:11:46.0 | `0x00080800` (bit 19) | control | 5.6, 46.1, 69.4 | +8.4 |
| 8 | 3 | 15:13:45.1 | `0x00080800` (bit 19) | control | 5.6, 28.4, 51.7 | |
| 9 | 3 | 15:15:25.9 | `0x00080800` (bit 19) | control | 5.5, 28.4, 51.7 | |
| 10 | 3 | 15:17:09.8 | `0x00080800` (bit 19) | control | 5.6, 28.4, 51.7 | |
| 11 | 3 | 15:18:53.7 | `0x00080800` (bit 19) | control | 5.6 (then the console's gap) | +12.4 |
| 12 | 4 | 15:20:23.8 | `0x00200800` (bit 21) | control | 5.6, 28.5, 51.8 | |
| 13 | 4 | 15:22:08.8 | `0x00080800` (bit 19) | control | 5.5, 28.3, 51.6 | |
| 14 | 4 | 15:23:54.1 | `0x00080800` (bit 19) | control | 5.5, 28.3, 51.2 | |
| 15 | 4 | 15:25:38.6 | `0x00080800` (bit 19) | control | 5.5, 69.9 (the console's gap between) | +8.3 |
| 16 | 5 | 15:29:24.1 | `0x00080800` (bit 19) | control | 5.6, 28.4, 51.7 | |
| 17 | 5 | 15:31:09.0 | `0x00080800` (bit 19) | control | 5.5, 28.3, 51.7, 114.4 | +89.6 |
| 18 | 6 | 15:33:55.6 | `0x00200800` (bit 21) | control | 5.6, 28.4, 51.7 | |
| 19 | 6 | 15:35:38.5 | `0x00080800` (bit 19) | control | 5.5, 28.4, 51.7 | |
| 20 | 6 | 15:37:23.4 | `0x00080800` (bit 19) | control | 5.6, 28.4, 51.2 | |
| 21 | 6 | 15:39:06.8 | `0x00080800` (bit 19) | control | 5.6, 28.3, 51.6 | |
| 22 | 6 | 15:40:50.1 | `0x00080800` (bit 19) | control | 5.6, 28.3, 51.7 | |

**In every run:**
- the bisection mode's first line, and the full loader's work: the kernel read and verified, the generator, the framebuffer, the page tables (96 tables, 97 in one run), the SPCR;
- the table line: the same as the copy;
- the map line, in 65 to 73 descriptors, none of an OS-defined type;
- the FCH's lines;
- then the decision.

The two calls then printed the UART's registers under the firmware and the line written straight to the UART, the last line, and `X`. The controls print neither, since they triple-fault at the decision (the full loader's order, TASKS item 131's R131-6).

Run 5's table line arrived with bytes lost and NULs between characters, and what is left of it reads the same as the others. No run printed `Y`, Amendment 2's line or a halting line.

## The boot services table (the first question)

All twenty-two lines say the system table names the boot services table at `0x98d3ca80`, and that it is "the same as the loader's copy, byte for byte over its 264 bytes". `ExitBootServices` is `0x98d26a20` in both, and the header's CRC32 `0xb4b91c57` in both. The firmware did not change its table after the copy, so on this revision a call through the live table would be the same call. Amendment 6's next step for a changed table, calling through the live one, is not needed.

## The calls (the second question)

Runs 3 and 6 read bit 23, printed the decision to call, the line before the call, `ExitBootServices next, through the loader's copy of the boot services table`, and `X`.

**After each, the machine reset:**
- the firmware's first byte came 5.5 s (run 3) and 6.1 s (run 6) after `X`. After the twenty controls' triple faults it came 5.5 to 5.6 s after their lines;
- the start that followed opened three times, at the spacing that follows thirteen of the fifteen undisturbed controls: 5.5, 28.3, 51.7 s and 6.1, 28.9, 52.1 s. The single opening after Linux's reboot and after the restart before run 18 is a different shape;
- no `Y` and no Amendment 2 line came, so the first call did not return an error.

**So:**
- **R:** the call returned `EFI_SUCCESS`, and item 349's triple fault reset the machine, or
- **S:** the firmware's own exit shut the processor down.

Either way the call did not hang. Rentals B to F saw nothing at all for minutes after the loader's last line before the call, and that did not happen here.

**R against S.** As on rental G, nothing distinguishes them, since by item 349 nothing is written after a successful call. Run 6's reset came 0.6 s later than run 3's, with every opening after it 0.6 s later too. The console's times are about half a second apart, so the difference is barely above their resolution and is not read.

**Against Amendment 6's outcomes.** The amendment reads "a reset after each call, with the next run reading bit 23" as the OS-defined types stopping the call. Both resets came. The next run read bit 19 after both, as after every three-opening start, so the bit-23 half of that outcome did not come on this revision. The start's shape stands in for it, which is weaker than the amendment asked.

## Why bit 19, and twice bit 23 (the coordinating session's question)

**What the starts show:**
- **After a shutdown, the firmware resets the machine itself, usually twice.**
  - After each of thirteen undisturbed controls the pattern is the same. The firmware opens 5.5 s after the triple fault and reaches "Pre-memory NB Initialization" about 3 s later. About 20 s after that it opens again, and once more 23.3 s later. Only then does it go on to "Connecting Drivers", and the banner comes about 96 s after the first opening.
  - The run that follows reads bit 19: "a `0x06` written to `0xCF9`".
- **Twice (after runs 2 and 5) it opened once** and went straight on, with the banner about 48 s after the opening. The next run read bit 23.
- **After the other resets seen, it opened once:**
  - Linux's own reboot before run 1, recorded as bit 19;
  - the tool's restart before run 18, recorded as bit 21.

  The tool's other four restarts landed while the firmware was starting after a triple fault, and are not read for this.

**So the FCH did record the shutdown, and the firmware then replaced it.** `PMxC0` holds the last reset's cause, beside bit 11, which is set in every reading here. Each of the firmware's own resets is a reset of its own, recorded as bit 19, so the shutdown's bit 23 survives only when the firmware does not reset itself.

The coordinating session asked whether the firmware turns a shutdown into its own `0xCF9` reset. In effect, yes:
- **what it does:** the shutdown does reset the board, at the same 5.5 s as every other reset; then, during its pre-memory initialization, the firmware resets the board twice more, and the record ends on that reset;
- **what is not shown:**
  - which of the firmware's code writes `0xCF9`, and why. A memory-training path after a warm reset would fit, but nothing read here says so;
  - why it skipped the two resets twice, both in the first two boots.

**What it means for the control rule on this revision.** The rule (Amendment 5's step 4, Amendment 6's step 3) takes "another cause" for the boot's first run and makes it the control. It takes "a shutdown" as the run after the control, and makes the call.
- **Here a run after the control usually reads "another cause"**, so it takes the control again: twenty of the twenty-two runs were controls. After run 6 no run read bit 23, so boots 3 to 6 made no call.
- **The rule failed safe:** it made no call it should not have. Of the twenty-two runs it made the call in two.
- **Bit 19 cannot cue a call on this revision.** It also follows Linux's reboot, before run 1, where no control had run. A rule that called on bit 19 would therefore make the boot's first run's call with no control behind it.
- **A cue that survives the firmware's resets** would be something the loader writes before the control: a UEFI variable, a file on the ESP or CMOS RAM. Each is a write the bench modes avoid today, so it is an owner item, and only worth one if another control-based run is made on this revision (next steps, rank 3).

## What it proves

- **On firmware 2.5a, the firmware's boot services table did not change after the loader copied it,** in twenty-two runs: the copied table is not a cause of the stop there.
- **A loader doing all the full bench loader's work before the call, with the boot info and kernel stack as `EfiLoaderData`, did not hang at the call.** In two of two calls the board reset within 6.1 s, with a shutdown's start.
- **A triple fault resets this board** (twenty controls, all followed by a reset). On this revision the FCH's record of it is usually replaced by the firmware's own resets.

## What it does not prove

- **That the call returned.** S, the firmware's exit shutting the processor down, gives the same console.
- **That the OS-defined types are the cause, as against everything else.** On 2.5a the comparison is rental C. Its normal loader at `1c9c7a96`, with the types, ran to its last line and then printed nothing for eight minutes, and did not restart. That loader differs from this one in:
  - the types;
  - what follows the call: it enters the kernel, which writes to `0x2F8` from its first line;
  - every change to the loader between `1c9c7a96` and `4165ecfb`.

  Rentals D to F, whose bench loaders kept the types, stopped at the call on 2.4, 2.4a and 2.5, each with its own caveat:
  - D's silence was A1 or B2 in its findings;
  - E's fits a call that returned and then two failures (its reading 2);
  - F's first reset after the call was firmware code, a trapped `0xCF9` write (its A′).

  No rental has run one loader both with and without the types on one board. So "the types stop the call" is the reading the evidence points to, not one it shows.
- **Two calls, on one board.** The firmware revision is whatever the provider gives, and the minimal program and this mode have each run on one revision only (2.4a and 2.5a).
- **What the firmware's own resets are for.**
- **Nothing here qualifies anything** (item 233).

## The tool's reading

The tool read the runs as the console does:
- its "Runs" row: "calls: 2: of the 2 judged, a reset after 2, 5.5 to 6.1 s after the mark; controls: 20, a reset after 19, 5.5 to 5.6 s after the line";
- run 4 called undetermined, its restart issued 2.9 s after the line;
- `reread` agreeing run for run.

TASKS item 130's fix held: no boot said "no reset" while a restart was in flight, and boots 1 and 2 ended at their answer.

**What it does not know is the firmware's own resets.** Its load figure, a start's first byte to the next banner, is 154.6 s. That holds the firmware's two resets and the starts the restarts disturbed, so its window grew to 9.2 minutes, and boots 3 to 6 each ran to their windows. That cost boots, never money: each boot's limit is still capped at the deadline less five minutes, and the rental was deleted at 41 minutes. Its loader figure, the work from banner to last byte, is 1.1 s, against the plan's 0.6 s, which the window took in.

## What the bisection clears from the list

The candidates of `docs/research/2026-11-21-loader-before-the-call.md`, under reading 1, on 2.5a:
- **the copied table (candidate 2):** cleared by the first question and by the reset after the call;
- **the generator (3), the ACPI walk's reads (4) and the stack's depth (5):** each was in this loader, which got past the call, so none of them alone stops it on this revision;
- **the OS-defined types (1):** the one candidate this loader left out, and the one the list has left.

The full bench loader's other additions are in this loader too, and so are cleared the same way: the FCH reads, `SMIxC0`, the table compare, the UART's registers, the `Stall` and the line before the call.

## The next step, ranked

1. **An owner decision on ADR-0002 B3 itself: the normal loader's memory types.** *(2026-11-22: accepted as owner item 353, ADR-0002 Amendment 7, and built by TASKS item 133 under QEMU. The confirming rental below, rental I, was cancelled by the owner before it ran, and no further rental is to be made (item 354).)*
   - **The change:** every loader would allocate the boot info and the kernel stack as `EfiLoaderData`, not as the OS-defined types `0x80000002` and `0x80000003`. Origin's own translated map would name them by address, as `memmap::carve` already names the kernel image, whose type cannot mark it (B3's own reason).
   - **The kernel would be unchanged:** it reads Origin's region kinds, not the firmware's types.
   - **What B3 gives up:** that the firmware's map identifies the two regions.
   - **What would confirm it:** one rental of a bench build of the normal loader with that change, entering the bench kernel with its breadcrumbs (TASKS item 112).
     - Breadcrumbs after the loader's last line (`R`, `S`, then the kernel's) would show that the call returned and the kernel started. That settles R against S too, and answers whether the change gets the normal path past the call: two questions in one rental (item 350).
     - The loader's last line and then nothing would put the stop after the call, in what the normal loader does next, not in the types.
     - On a revision where loaders with the types stopped (2.4, 2.4a or 2.5), the reading is cleaner still, but the revision cannot be chosen.
   - **A stronger version, if the owner wants causation shown by a pair:** a rental of this bisection mode with the types kept, TASKS item 131's planted control BC-1 made a mode. `X` and nothing more, on a revision where this mode reset, would show the types stop the call. That needs an amendment and a rental of its own.
   - **Before it is put:** the coordinating session should check it against the code. By reading, the loader's two allocations and one `carve` call per region are the change. It has not been built.
2. **The tool counts the firmware's openings (free).** On a firmware that resets itself after a shutdown, the tool's start delay should be the last opening's, not the first's, and its record should show the openings, so its windows are not stretched by the firmware's own resets.
3. **A control cue that survives the firmware's own resets (an owner item, only if another control-based run is made on 2.5a).** See "What it means for the control rule on this revision". A kernel-entering loader, rank 1's, needs no control.
4. **R against S by a mark after a successful call** (rental G's rank 4). It changes item 349, and rank 1's breadcrumbs answer the same question.

## What this rental cannot show

- Anything about Zen 5 or a 2026 platform: the board is outside the declared scope.
- Whether the normal loader gets past the call without the types. That is rank 1's rental.
- Why this firmware resets itself after a shutdown.
