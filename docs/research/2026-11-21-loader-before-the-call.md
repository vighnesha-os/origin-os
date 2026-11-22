# What Origin's loader does before `ExitBootServices` that a minimal UEFI program does not

| | |
|---|---|
| Date | 2026-11-21 |
| Kind | Research ledger (CLAUDE.md §16). Rental E's findings, rank 2 (`docs/research/2026-11-21-rental-e-findings.md`); TASKS item 121 |
| Read | Origin's own code only, at `c8ecfc9d`: `boot/uefi/src/` (`main.rs`, `uefi.rs`, `console.rs`, `random.rs`, `memmap.rs`, `spcr.rs`, `bench.rs`, `uart.rs`, `protocols.rs`), and `security/random/src/` where the generator's start-up tests are. UEFI 2.11 §7.2.1 (memory types), paraphrased. No other operating system's or project's source |
| By | An AI reader of the code, and the TASKS item 112, 116 and 121 session checking its line references; no person |

**Why.** On rentals B to E the loader always stops at the call, with the normal loader (B, C) and the bench loaders (D, E). The provider's own system starts from the same firmware and its call returns. Under rental E's reading 1, the call does not return for Origin's loader, so something Origin's loader does, and that system does not, is the likely cause. This list is where to look. It decides nothing by itself.

**The minimal program** compared against: it is entered, calls `GetMemoryMap`, and calls `ExitBootServices` with that map's key, once more after a fresh map if the first call fails, and nothing else.

## What both builds do, in order

Line numbers are `boot/uefi/src/main.rs` unless named otherwise.

1. **The system table** is checked and copied (`read_system_table`, `uefi.rs` 267–282; line 75). Firmware memory is only read.
2. **The console:** every line goes out through `OutputString`, in chunks of 128 characters (`console.rs` 28–40; `text.rs` 7). `Reset`, `SetMode`, `SetAttribute` and `ClearScreen` are never called.
3. **The boot services table is copied** (`boot_services`, 833–849). Every later service, `GetMemoryMap` and `ExitBootServices` among them, is called through the copy. If the firmware changed a pointer in its own table after the loader copied it, the loader would still call the old one.
4. **The kernel file** (`read_kernel`, 886–978): `HandleProtocol` (loaded image), `HandleProtocol` (simple file system, on the image's device), `OpenVolume`, `Open`, `GetInfo`, `AllocatePages` (`EfiLoaderData`), a `Read` loop, `Close` on the file and on the volume. No protocol is closed with `CloseProtocol`, which `HandleProtocol` does not need.
5. **The kernel is verified:** SHA-512, Ed25519 and ML-DSA-87, and SHA-256 Merkle checks of each section (`kernel.rs` 133–185). This uses only the processor, on the firmware's own stack. **How deep that stack goes has not been measured,** for ML-DSA-87 in particular.
6. **Pages allocated:**
   - `EfiLoaderCode` for the kernel (233);
   - `EfiLoaderData`, 15 pages, for the random-number generator (252);
   - **OS-defined types:** the kernel stack as `0x80000003` (328) and the boot info as `0x80000002` (342). UEFI 2.11 §7.2.1 reserves `0x80000000` to `0xFFFFFFFF` for OS loaders, so both are allowed, and a minimal program uses neither;
   - `EfiLoaderData` for the memory map's buffer (346) and the page-table pool (457–475), the last allocation.
   All are `AllocatePages` with `AllocateAnyPages`, zeroed by the loader, and none is freed. `AllocatePool` is never called.
7. **The random-number generator** (`random.rs` 231–258; `security/random/src/drive.rs` 269–330):
   - a known-answer test, and start-up tests of 1,024 `RDSEED` and 1,024 `RDRAND` draws;
   - **each draw is one step on the generator's own stack** (`on_own_stack`, `random.rs` 126–178): `pushfq` and `cli`, RSP moved into the generator's pages, `RDTSC` and the draw, registers and XMM0–15 cleared, RSP restored, and `popfq`, which restores the interrupt flag. That is thousands of such steps while boot services run;
   - the kernel's slides are two more steps (283, 500);
   - at the end the generator zeroes its 15 pages with volatile writes (611–623; `random.rs` 279–295).
8. **The kernel is placed:** copied and relocated into its pages (286).
9. **The memory map is read five times before the first call:** a size probe with no buffer (331, 981–1002), then three reads with `translate` and `carve` (417, 457, 478). The fifth read is inside the call's preparation. A minimal program reads it once.
10. **The ACPI walk to the SPCR** (`spcr::find`, `spcr.rs` 172–350; 417–427): volatile byte reads of the RSDP, the XSDT (up to 64 KiB, checksummed), each listed table's 36-byte header up to the SPCR, and the SPCR (up to 1 KiB).
    - The walk reads only memory the firmware's map describes. It refuses memory-mapped I/O, unaccepted memory and page 0, and does read reserved, ACPI NVS and runtime memory.
    - `RDTSC` on each side.
11. **The framebuffer** (`find_framebuffer`, 1048–1130): `LocateHandle` for the graphics output protocol into a 32-entry array; for each handle, `HandleProtocol` for it, its `Mode` and `Info` read, and `HandleProtocol` for the console-output device. `QueryMode`, `SetMode` and `Blt` are never called.
12. **Processor state read:** CR4 (393), CPUID leaves `0x80000000` and `0x80000001` (404). Nothing is written.
13. **The page tables are built** in the pool, in memory only, and not loaded (520–550).
14. **The last line** is written through the firmware's console, and the panic console is cleared (640–644).
15. **The call** (`uefi::exit_boot_services`, `uefi.rs` 377–396; 647–710):
    - the preparation reads the memory map into the buffer allocated earlier, translates and carves it, and writes the boot info into the loader's own pages, calling no firmware and allocating nothing;
    - then `ExitBootServices`, through the copied table;
    - on `EFI_INVALID_PARAMETER` only, the preparation once more and a second call.

    Everything runs on the firmware's entry stack.

## What only bench builds add

Rentals B and C ran the normal loader and stopped in the same place, so none of these can be the cause on its own.

- **Each switch file** is read with the same calls as the kernel file (`read_bench_file`, 1369–1422): 16 bytes into the stack at `c8ecfc9d` (32 since TASKS item 121), both handles closed, nothing allocated. On a rental the `Open` fails.
- **TASKS item 116's walk to the FADT** (`bench::find_reset`, `bench.rs` 499–580), as item 10's.
- **The entry state** (1324–1339): CPUID leaves 0, 1 and 7; CR0 and CR4; `RDMSR` of `0xC0000080` and `0x277`, and of `0x6A2`, `0x6A4` and `0x6A8` when CET's shadow stacks are reported. Reads only.
- **Before the call** (`bench_before_call`, 1457–1503):
  - `Stall` for 20 ms with `RDTSC` on each side;
  - the runtime services table's header and `ResetSystem`'s pointer read, not called;
  - the UART's registers read with interrupts masked, toggling DLAB in LCR and setting it back (`bench.rs` 292–314);
  - a line written straight to the UART, then up to 100,000 status reads while it empties.
- **Between the last memory map and the call,** in bench builds only, the mark (`X`, or `Y` before a retry) goes straight to the UART, with up to 100,000 status reads a byte (694–703; `uart.rs` 70–83).

## What TASKS item 121 adds before the call, in rental F's loader

The list above is the loader at `c8ecfc9d`, rental E's. Rental F's bench loader (TASKS item 121) also does these before the call (TASKS item 121's review, R121-11):

- a third switch file, `BENCH-FCH`, opened as the others;
- CPUID leaf 0, for the vendor;
- a third walk from the RSDP through the XSDT to the FADT (`bench::with_fadt`, as TASKS item 116's), then the DSDT's scan: about 470 KB on the rented board, read 4 KiB at a time into a stack buffer that sits inside the walk's 1 KiB FADT buffer's frame, so the stack is deeper there than anywhere in rental E's loader;
- CR3 read, and up to eight page-table entries read from the firmware's tables;
- the six loads from the FCH's power-management block, only where the checks allow them;
- in a boot's first run, the control: `0x06` to port `0xCF9`, after the UART has emptied, and no call.

So rental F's loader is not rental E's plus a reset alone. If rental F shows reading 1 again, these are on the list too, and candidate 2 below, the stack's depth, weighs more.

## What is left as the firmware left it

- **The interrupt flag:** as at entry. Every `cli` is paired with a `popfq` or a conditional `sti`.
- **The task priority level:** never raised.
- **The watchdog:** `SetWatchdogTimer` is never called, so any watchdog the boot manager armed still runs at the call.
- **Events, timers and variables:** none created, signalled or written.
- **Control registers, MSRs, GDT, IDT, page tables:** unchanged. CR3, CR0.WP and EFER.NXE change only on entry to the kernel, after the call (1144–1169).
- **The console's mode and the display's mode:** unchanged.
- **File handles:** closed.

## The candidates, under reading 1

Ranked by how directly each could make the firmware's own exit handling hang, and so how soon a bench loader that leaves it out should be tried:

1. **The two OS-defined memory types** (`0x80000002`, `0x80000003`). They are allowed, but the firmware's exit handling walks its memory map, and a minimal program never gives it these types. A bench loader that allocates both as `EfiLoaderData` tests it; the kernel's boot info would then name them by address, as the kernel image already is.
2. **The stack's depth on the firmware's stack.** UEFI guarantees an application at least 128 KiB. ML-DSA-87's verification and the walks' 1 KiB buffers run on that stack, and the depth has not been measured. An overflow would corrupt firmware memory silently, and the exit handling could be the first code to trip on it. A free measurement: under QEMU, paint the stack below RSP before verification and find the deepest change after the call's preparation.
3. **The copied boot services table.** It holds only if the firmware's table was not changed after the copy, for example by a driver loaded during the loader's run. Comparing the copy's `ExitBootServices` with the live table's just before the call is a read.
4. **The generator's thousands of steps on its own stack,** with interrupts masked inside each. They end before the call, and nothing seen points at them.
5. **The ACPI walk's reads of reserved and NVS memory.** Reads only, of tables the firmware lists.

## What the code does not settle

- What the firmware does inside `OutputString`, `HandleProtocol`, `LocateHandle`, the file reads and `Stall`.
- How deep the stack goes (candidate 2), and how long the loader takes from entry to the call.
- Whether the boot manager armed a watchdog. The interrupt flag's value at entry.
- Whether a driver changed the boot services table after the copy (candidate 3).
