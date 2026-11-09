# ADR-0010: An early console that does not need a 16550 at 0x3F8

| | |
|---|---|
| Status | **Accepted** by the owner on 2026-11-08, as proposed (E1–E6, and the recommended answer to each of the three questions below; decision sheet items 27–29). It changes the boot-info structure (CLAUDE.md §84). The code is built and boot-tested under QEMU, and was merged into `claude/interesting-carson-ae7c83` on 2026-11-09 on the owner's word. Nothing here has shipped. **Amendments 1 and 2 (below, to E3) are Proposed**, not accepted: they record changes waiting for the owner's word. |
| Requires | CLAUDE.md §13 (checked parsing), §19 (boot-info structure, early diagnostics), §51 (font provenance), §58 (threat-model privileged interfaces), §69 (diagnostics), §74, rules 14, 30 and 40 |
| Implements | ADR-0009 H6, third bullet: a console that does not depend on a 16550 at `0x3F8` |
| Code | `boot/info` (`Framebuffer`), `boot/uefi` (the Graphics Output Protocol hand-off), `framekernel/paging` (the mapping), `framekernel/src/{fbconsole,font,spcr,serial,console}.rs`, `developer-tools/xtask` |
| Evidence | `docs/engineering/SOURCE_PROVENANCE.md`, "The early console"; `docs/research/2026-11-08-early-console.md` |
| Related | ADR-0002 B4, B6; ADR-0004 H2; ADR-0007 (T4, T14); `docs/engineering/THREAT_MODEL_IOMMU.md` |

## Context

Until this ADR the kernel's only console was a 16550 at the fixed I/O port `0x3F8`. ADR-0009 chose first machines (an ASRock board, a Lenovo desktop) whose vendors do not document the COM port's address. A failed first boot on such a machine would show nothing. Firmware offers two standard descriptions of a console, and this ADR uses both: a linear framebuffer through UEFI's Graphics Output Protocol, and a UART through the ACPI SPCR table.

## Decisions

### E1. Boot info 0.4: a `Framebuffer` (the ABI decision; accepted by the owner on 2026-11-08)

`origin-boot-info` appends one 48-byte structure at offset 128 and `size_of::<BootInfo>()` becomes 176. The version in `origin-version.toml` becomes 0.4. The kernel accepts only its own major-0 version, and the KRN manifest records it (ADR-0002 B4).

| Offset | Field | Meaning |
|---|---|---|
| 0 | `base: u64` | physical address of the top-left pixel, 0 for none |
| 8 | `size: u64` | the firmware's `FrameBufferSize` |
| 16 | `virtual_base: u64` | where the loader mapped the first pixel in the first page tables (E2), 0 if it did not |
| 24, 28 | `width`, `height: u32` | visible pixels and rows |
| 32 | `stride: u32` | bytes from one row to the next |
| 36, 40, 44 | `red_mask`, `green_mask`, `blue_mask: u32` | the bits of a little-endian 32-bit pixel holding each channel |

The pixel format is carried as masks, so the kernel sees one representation instead of the firmware's enumeration. This follows ADR-0002 B4: "the loader translates the UEFI map so the kernel never sees UEFI structures". `origin_boot_info::Framebuffer::check` is the contract and runs in both the loader and the kernel. It requires whole 32-bit pixels, a stride of at least `width * 4` that is a multiple of 4, a size that covers the visible rows, an end below 2^52, and disjoint contiguous masks of 16 bits or fewer. The loader takes whichever mode the firmware is already displaying and never calls `SetMode`: it picks the first Graphics Output handle the firmware uses as a console, else the first usable one.

### E2. Where the framebuffer is mapped

The loader maps the framebuffer's visible rows in the first page tables (ADR-0004 H4) at `layout::framebuffer_mapping`: the first 2 GiB of the MMIO window (`0xFFFF_E000_0000_0000 + (base mod 2 MiB)`), uncached, read/write and never executable. The kernel's own page tables map it at the same address, so the console draws across the switch without a change. Device registers start 2 GiB higher now. A framebuffer that cannot be mapped is not an error in either place: `layout::map_framebuffer` runs after everything else and returns whether it did.

### E3. The console: framebuffer first, then the UART

Every line goes to the framebuffer, then to the UART, under the existing console lock. (Amendments 1 and 2, below, propose changing this order and sending a line a section at a time.) `framekernel::fbconsole` draws text through an abstract pixel sink, in 8 by 16 cells scaled by a whole number on panels taller than 1439 rows. Text never scrolls and the framebuffer is never read: the next line goes to the row below, painted first as a bar, and the screen wraps at the bottom. Scrolling would read or rewrite the whole screen for each line, with interrupts off, on memory that may be uncached. This way a line costs at most two rows of writes: its own and the bar's.

### E4. The UART: from the SPCR when the firmware names one

The console starts on COM1, because the ACPI tables cannot be mapped until the kernel's own page tables are live. Then `console::adopt_firmware_uart` reads the SPCR (`framekernel::spcr`, parsed as untrusted input), and if the firmware names a UART this kernel can drive, moves the console there and sends it everything printed so far (kept in an 8 KiB log), so its log is complete. COM1 is only used when the firmware gives nothing better: no table, a table it refuses, a port the table names where nothing answers (a floating bus reads 0xFF), registers the memory map says are RAM, or a UART that answers but will not take the replayed log (then COM1 stays).

The UART's speed: if the table states both a rate and the UART's clock (revision 3 and later), that rate is programmed. Otherwise a divisor latch the firmware programmed is left alone, because the table's rate is "the baud rate the BIOS used", so a programmed latch already holds it, whatever the clock behind it. A latch of zero (never programmed) gets the table's rate on the PC clock if the UART is in I/O space, and is left alone in memory space, where the clock could be anything. COM1's settings are read at entry before the console programs it, so "as is" for the COM1 port means what the firmware left, not the kernel's 115 200 baud; a latch of zero is never restored.

### E5. A missing or unusable console never stops the boot

No framebuffer (no GOP, Blt-only, an unusable or unmapped one) leaves the UART alone. No UART still draws. A framebuffer that cannot be mapped costs only itself: it is mapped after everything else in both the loader's and the kernel's tables. A UART that stops taking bytes is given up on after one bounded wait (previously the driver waited once per byte on every line) and said so on the screen. Nothing in the console paths can panic, and everything derived from firmware is bounds-checked before use.

### E6. The font

`framekernel::font` is an original 8 by 16 ASCII bitmap font, drawn glyph by glyph for this project and committed as source art. No existing font's bitmap data was consulted or copied (§3, §51); `SOURCE_PROVENANCE.md` records this.

## Alternatives

| Alternative | For | Against |
|---|---|---|
| Keep only COM1, and fix the machines | Nothing to change | ADR-0009 H6: a failed boot shows nothing on the chosen machines |
| Program a framebuffer through the VGA registers or a GPU driver | No firmware dependence | Needs a driver per GPU. Far out of scope; GOP exists for this |
| E1 as a pointer to a separate structure, or a tag list | Extensible without moving fields | More validation for one fixed structure. A later console can bump the version |
| Pixel format as UEFI's enum in boot info | Field for field with UEFI | Puts UEFI's model in the kernel's contract, which B4 avoids; masks cover RGB, BGR and bit-mask modes uniformly |
| E2: the loader maps nothing, the kernel maps the framebuffer itself | One less loader step | The screen would start only after the kernel's page tables, losing the earliest lines |
| E2: the framebuffer in the direct map | One address, no new window | ADR-0004 keeps MMIO out of the direct map, and KASLR's slide would have to allow for a framebuffer at any address |
| E3: scrolling | Newest text always at the bottom | Each line would read or rewrite the whole screen, with interrupts off |
| E4: read the SPCR in the loader and pass it in boot info | The right UART from the first line | A parser for untrusted data in the loader, and more boot-info ABI; early lines are replayed anyway, and the screen shows them meanwhile |
| E4: wait for the SPCR before writing anything to any UART | 0x3F8 is never touched when the firmware names something else | A hang before `init_paging` would show nothing on serial, a regression on every machine with a real COM1 |

## Evidence

- Boot: `cargo xtask boot-test`'s kernel scenario reads a QEMU screen dump back cell by cell and checks it against the rows the serial log implies, in the specification's colors, computed independently of the kernel's drawing code; `cargo xtask boot-console` covers a UART named by an SPCR, COM1 named, refused tables, a UART that takes no bytes, no display and no UART. `boot-matrix` runs both. See `SOURCE_PROVENANCE.md`, "The early console", for the runs and negative controls.
- Host: `framekernel::spcr` (revisions 1 to 4, every refusal, a 200,000-case mutation soak on `crate::testing`), `framekernel::fbconsole`, the font's table tests, the boot-info check, and `xtask`'s screen comparison with its own negative cases.

## Trade-offs

The ABI grows by one structure, and a loader built for 0.3 is incompatible, which the manifest's boot-info version already makes a build-time matter. Uncached mapping costs bandwidth on real panels; write-combining would need a PAT entry the kernel does not program yet. Text wraps rather than scrolls, so the newest line is above the bar, not at the bottom of the screen.

## Migration consequences

Boot info is at major 0, so no compatibility is promised, and every boot-info consumer is built from this tree. Nothing on disk depends on it. Accepting this ADR needs only this status line changed; a different E1 layout means updating `origin-boot-info`, the loader and the kernel in one commit.

## Threat model (§58)

| Input | What could be wrong with it | What happens |
|---|---|---|
| GOP mode (`describe`, `Framebuffer::check`) | Blt-only, zero base, sizes that wrap or undersize the visible rows, bad masks, overlap with RAM in the memory map | Refused with a named reason in the loader; the kernel re-checks before drawing and refuses the same way; the boot goes on without the screen |
| SPCR table (`spcr::parse`) | Any field, by bugs or corruption. A valid checksum proves nothing about intent | Parsed from a checked length, every field checked; I/O ports below `0x1000` other than the four standard COM blocks refused (system ports, the PCI configuration and reset ports, typical ACPI power-management blocks); alignment and range checked; registers overlapping RAM refused at use. A wrong port above `0x1000` can still reach another device's registers: firmware is trusted for that, as for all of its ACPI data |
| A UART the table names | Absent, wedged, or lying about its address | 0xFF status refused; transmit waits are bounded; a UART that answers but will not take the replayed log is given up on and COM1 stays; a UART that dies later costs one bounded wait and is dropped |
| Firmware running with kernel privileges | A malicious firmware controls the platform anyway | Out of scope here, as for ACPI generally (ADR-0007 T21) |

The framebuffer mapping is kernel-only, writable and never executable, in the kernel half. No Rust reference is ever made to video memory.

## What this does not prove

- Physical hardware (§74, rule 30). Every result here is QEMU 11.1 with OVMF.
- **The display after the IOMMU comes up.** A real integrated GPU scans out of memory by DMA through the remapping unit. `iommu::init` then blocks devices without a domain, and RMRR identity maps do not exist yet (T14, deadline "before real hardware"). The T4 bus-mastering work on its branch can do the same to the GPU's bus mastering. QEMU's VGA does neither, so this boot cannot see it. On a real machine the framebuffer console may go dark at `iommu::init` until T14 is done; H6's list asks for T14 to be closed or accepted before a first boot.
- A UART in memory space: QEMU's x86 machines have no memory-mapped 16550, so that path is tested with mocks on the build host only (register stride, access width, RAM overlap at the SPCR level), not on a live device.
- A really wedged UART. The boot shows the give-up path with a port that answers the status register with 0 (`isa-debugcon` set to read back 0) rather than a device that genuinely stops; the mechanism is the same bounded poll.

## Questions for the owner (answered 2026-11-08: yes to all three)

1. Accept boot info 0.4 as in E1 (recommended)? This is the §84 decision. **Yes (2026-11-08).**
2. Accept reserving the first 2 GiB of the MMIO window for the framebuffer (E2)? **Yes (2026-11-08).**
3. Accept the UART policy: COM1 until the ACPI tables can be read, then the SPCR's UART with the earlier output replayed, 0x3F8 only when firmware gives nothing better (E4)? **Yes (2026-11-08).**

## Amendment 1 (Proposed, 2026-11-09): the UART first, then the screen a section at a time

**Status: Proposed, not accepted (branch `claude/epic-joliot-bff82c`, local, not pushed).** The code on that branch already behaves as below, because the question only arose while building it; the owner can accept or reject it when the branch comes to be merged, and E3 as accepted above stands until then.

**Why.** Under KVM on a real AMD EPYC 7763 (free GitHub runner, Actions run 42547942738) one console line took 4.3 to 4.6 ms on average and up to 10 ms at worst, with interrupts off. TCG, QEMU's software CPU, had hidden it (about 0.2 to 0.6 ms). The line is about 40,960 pixels of uncached memory, and one line can cost a whole 10 ms timer tick. E3's text ("Every line goes to the framebuffer, then to the UART, under the existing console lock") is what made a line one critical section.

**The change to E3.** A line is handed over in one critical section under the console lock: it goes out on the UART and joins a bounded backlog of text for the screen, so both sinks get lines in one order (E3's guarantee). The pixels are then written after that hand-over, a section at a time: each section holds the console lock with interrupts off for at most 256 pixels (1 KiB of uncached memory), and nothing is held between sections, so interrupts are taken between them, and no preempted holder can stand in anyone's way. Any processor can paint the next section. A writer that can wait draws through its own line before it returns. A writer with interrupts off for its own reasons (an interrupt handler) hands its line over and leaves. An exception or panic report draws the whole backlog before its own line and leaves nothing behind.

**Why the UART goes first.** Keeping the screen first would keep each line's UART send behind its pixels, as now, and would add to that everything that can now happen between sections: interrupts, preemption, a turn at the lock. The UART is the log that has to survive a bad machine, so a line must not wait for pixels. So the line is on the wire before any pixel of it is drawn, whatever happens next. E3's stated reason for the opposite order, "a UART that holds a line up cannot delay what reaches the screen", weakens to this: a UART delays the screen by at most that one line's send, and a UART that stops taking bytes is given up on after one bounded wait, as E5 requires, so the delay happens once.

**What it changes.** `framekernel/src/console.rs`, `framekernel/src/fbconsole.rs` (`Backlog`, resumable drawing), one line each in `framekernel/src/cpu.rs` and `framekernel/src/main.rs`, and `developer-tools/xtask` (its check of the screen against the log waits a moment for the screen to catch up; a frame that never matches still fails).

**Alternatives.**

| | Gain | Why not |
|---|---|---|
| Keep E3's order: draw each line in sections, then send it | No change to E3 | The UART waits behind the pixels and everything between the sections, which is what this work must avoid; the UART is the record of last resort |
| Draw a whole line with interrupts on, one drawer at a time, nobody waiting | No resumable drawing | A drawer preempted while holding the role can block every other writer: the boot flow, which only runs when nothing else is ready, would hold it while threads wait for it |
| A write-combining mapping for the framebuffer | Much cheaper pixels on real hardware | No bound: a large display still takes as long as it takes; touches paging; independent of this and worth doing later |
| Send the UART through the same backlog | One queue | A line is no longer on the wire when `flush` returns, so a crash can lose it |
| Spend fewer pixels on a line (a thinner bar) | Less to do | No bound either, and it changes what E3 and the screen check specify |

**Evidence.** `docs/engineering/SOURCE_PROVENANCE.md`, "The console: a line drawn a section at a time": under KVM on an AMD EPYC 9V74 runner (Actions run 42561010289) one section, the most that holds interrupts off for pixels, took 23.4 to 24.6 us on average, where a whole line took 4.3 to 4.6 ms on the EPYC 7763 that found the problem. Those are different processor models; no real machine has been tested.

## Amendment 2 (Proposed, 2026-11-09): the UART a section at a time

**Status: Proposed, not accepted (branch `claude/uart-bounded-send`, on top of Amendment 1's branch, local, not pushed).** The code on that branch already behaves as below. E3 as accepted, and Amendment 1 until the owner decides on it, stand until then.

**Why.** Amendment 1 made the pixels of a line go out a section at a time and left the UART in one critical section: the line's bytes, polled one at a time, with interrupts off and the console lock held. At 115 200 baud a byte takes 87 µs on the wire (10 bits), so 100 bytes take 8.7 ms, as long as the 10 ms timer tick. QEMU's UART takes a byte instantly, so no QEMU boot showed it.

**The change.** A line's hand-over no longer touches the UART: it joins a bounded queue of text for the UART (4 KiB) in the same critical section as the screen's backlog, so the two sinks still get lines in one order. Then the UART is worked at a section at a time, like the screen:

- Each section holds the console lock with interrupts off, reads the line status once, and, if the transmitter reports empty, writes one load: at most 16 bytes, the 16550's transmit FIFO, or 1 byte on a UART with no working FIFO. **The bound is 1 status read and 16 data writes, 17 register accesses. It does not depend on the line's speed**, because the 16 bytes need no further look at the status: `LSR.THRE` means the FIFO is empty, and the CPU never waits for the wire inside a section.
- A section that finds the transmitter busy writes nothing and ends. Waiting for a load to leave (1.4 ms at 115 200 baud) is done between sections, with the lock free and interrupts as the writer has them (on for a thread; a window between sections for the boot flow once interrupts can be taken).
- Whoever holds the lock takes the next section. Positions count bytes in the queue, as for the screen: a writer goes on when its own line's last byte has been written to the UART, whichever processor sent it.
- A writer that cannot wait (an interrupt handler) hands its line over, sends one section and leaves. The rest of its line goes out with the next writer that can wait, or the next report.
- A report (`Console::urgent`) keeps interrupts off from start to finish and takes the lock a section at a time like everyone else, so two reports at once queue behind each other instead of waiting each other out. It goes on until everything queued before it, and the report itself, are on the UART and on the screen. On a working UART its wait is bounded by the queue's size (355 ms at 115 200 baud); a UART that stops is given the one wait below.
- A UART that stops taking bytes is given up on after one bounded wait, however many sections it is spread over: `TX_POLL_LIMIT` polls in a row, per byte of the last load (the wait a transmitter was allowed when polled byte by byte). It is reported on the screen, as E5 requires, and the writer that found it out goes on only when that line has been drawn, even if nothing is printed again.
- Amendment 1 says a line is on the wire before any pixel of it is drawn. That is now true of the writer that can wait: it sends its line before it draws. One writer's last section of drawing can run on into the line queued behind its own, so a little of that later line may be drawn before it has gone out.
- The replay of the early log (E4) is not sent in sections: it runs once, before the timer starts and before any other processor does.

**What it changes.** `framekernel/src/console.rs`, `framekernel/src/serial.rs` (`TxQueue`, `Uart16550::detect_fifo`), `framekernel/src/linequeue.rs` (the ring both sinks use, taken out of `fbconsole::Backlog`), `framekernel/frame/src/timer.rs` (the proof's self-interrupt, a safe call in the framework crate) and `developer-tools/xtask`.

**Alternatives.**

| | Gain | Why not |
|---|---|---|
| Wait for the FIFO inside the section | A section is one load and one wait, and no queue is needed for the wait | The section lasts 1.4 ms at 115 200 baud (the wait), 16 ms at 9600, all of it with interrupts off: a shorter version of the same problem |
| Sections of less than 16 bytes | Shorter still | A section is already 17 register accesses; below that the lock, the status read and the interrupt toggle cost about as much as the bytes |
| One writer owns the UART across its line, nobody else may touch it | No queue | A preempted owner blocks every other writer; an interrupt handler cannot wait for it |
| UART interrupts feed the FIFO | No polling, and the writer sleeps | Needs an interrupt line and routing for every UART the SPCR can name, and makes the development console depend on interrupts working; §69 says production logging will be a ring drained by a thread, which is a different design |
| One queue for both sinks | Less memory | A slow screen would hold the UART back and the other way round; today each sink can fall behind on its own |

**Evidence.** `docs/engineering/SOURCE_PROVENANCE.md`, "The console: the UART a section at a time". QEMU's UART is effectively instant: what the boot proofs show is the bound in bytes, the windows between sections and the order on the wire, not the real time a section takes or the time a real line takes to drain.
