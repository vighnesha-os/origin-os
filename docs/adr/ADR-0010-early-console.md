# ADR-0010: An early console that does not need a 16550 at 0x3F8

| | |
|---|---|
| Status | **Accepted** by the owner on 2026-11-08, as proposed (E1–E6, and the recommended answer to each of the three questions below; decision sheet items 27–29). It changes the boot-info structure (CLAUDE.md §84). The code is built and boot-tested under QEMU, and was merged into `claude/interesting-carson-ae7c83` on 2026-11-09 on the owner's word. Nothing here has shipped. |
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

Every line goes to the framebuffer, then to the UART, under the existing console lock. `framekernel::fbconsole` draws text through an abstract pixel sink, in 8 by 16 cells scaled by a whole number on panels taller than 1439 rows. Text never scrolls and the framebuffer is never read: the next line goes to the row below, painted first as a bar, and the screen wraps at the bottom. Scrolling would read or rewrite the whole screen for each line, with interrupts off, on memory that may be uncached. This way a line costs at most two rows of writes: its own and the bar's.

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
