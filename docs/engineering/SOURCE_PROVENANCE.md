# Source provenance

| | |
|---|---|
| Required by | CLAUDE.md §6 |
| Rule | One entry per original module. Automated similarity scanning cannot prove independent authorship, so this file records how each module was produced. |

**Open item for the owner:** the repository has no LICENSE file yet. Every entry below therefore reads "project licence not chosen yet".

## `rust-toolchain.toml`

- **Content:** copied verbatim from CLAUDE.md §8 (v7.1).
- **Author / date:** Claude (cloud session), 2026-11-07.
- **Licensing:** project licence not chosen yet.

## `developer-tools/toolchain/verify.sh`

- **What:** a host-side verification tool for CLAUDE.md §8–§10. It is not part of the shipping OS.
- **Author / date:** Claude (cloud session), 2026-11-07, written for this project.
- **Specifications:**
  - CLAUDE.md §8–§10.
  - PE/COFF image layout: DOS `MZ` header, `PE\0\0` signature, machine 0x8664, PE32+ magic 0x20B, subsystem 10 (EFI application).
  - ELF gABI: `e_ident`, `e_type`, and `e_machine` 62 (x86-64).
  - The regression conditions in rust-lang/rust issues #163614, #163911 and #163947.
- **Architecture decisions:** CLAUDE.md §9 (results go to `docs/toolchain/VERIFICATION.md`) and ADR-0001 (toolchain targets).
- **Dependencies:**
  - rustup, rustc, cargo, rust-lld and llvm-tools (`llvm-nm`, `llvm-objdump`). These are permitted toolchain infrastructure under CLAUDE.md §4.
  - POSIX utilities: `od`, `grep`, `sed`, `sort`, `mktemp`, `date`, `head`, plus `shasum` (macOS) or `sha256sum` (Linux) for firmware hashes.
- **Toolchain:** bash 3.2 or later (the macOS default), and Rust 1.99.1 per `rust-toolchain.toml`.
- **Review:** not yet independently reviewed.
- **Licensing:** project licence not chosen yet.
- **Test evidence.** Run on 2026-11-07 in the cloud sandbox: Linux x86-64, rustc 1.97.0, no network.
  - The syntax check passes.
  - Failure paths are reported correctly, with the expected exit codes:
    - the pinned toolchain is missing
    - rustup is not on PATH
    - `--install` with downloads blocked (rustup accepted the arguments and failed at the download)
    - an unknown argument (exit 2)
  - With a 1.97.0 override, the checks that can run there behave as expected: edition 2024, running rust-lld directly, FFI bool on `x86_64-unknown-linux-gnu`, hardware-float code generation, and the unstable hardening flags.
  - The FFI-bool check **fails** on the exact miscompiled body from #163911 (`movl %edi,%eax; retq`) and passes on the correct body. Both were assembled with llvm-mc.
  - The PE header checks agree with `llvm-objdump -p` on a real EFI application linked by rust-lld.
  - **First run on the M1 Max, 2026-11-07** (Claude Code, local), rustc 1.99.1 `58cf8f9c3`, macOS 27.0.1:
    - `--install`: 18 PASS, 0 FAIL.
    - Check mode: 17 PASS, 0 FAIL (the install step is skipped).
    - The output is in `docs/toolchain/VERIFICATION.md`.
    - The `wcslen` PASS was cross-checked with a linker map: `wcslen` resolves to the weak definition in `compiler_builtins`.
- **Change, 2026-11-07** (Claude Code, local). The QEMU section now:
  - lists each firmware file once, by its resolved path;
  - includes the `edk2-i386-vars.fd` NVRAM template;
  - records a SHA-256 of each file.

  Re-verified with `bash -n` and a full check-mode run.

## `tests/toolchain/`

Author, date, toolchain, review and licensing are as for `verify.sh`.

| File | Purpose | Origin |
|---|---|---|
| `uefi_smoke.rs` | UEFI release link test, and the `wcslen` regression (#163614) | Original |
| `none_smoke.rs` | Freestanding x86-64 link test; also the direct-`rustc` run for #163947 | Original |
| `ffi_bool.rs` | FFI `bool` regression (#163911) | The function body is the public three-line reproducer from that issue, kept verbatim so the test matches the reported bug |
| `float_abi.rs` | Float-ABI probe (research experiment E2; CLAUDE.md §11) | Original |

## `Cargo.toml`, `.cargo/config.toml` (workspace)

- **What:** the virtual workspace (`resolver = "3"`), with shared lints (`unsafe_op_in_unsafe_fn`, `clippy::undocumented_unsafe_blocks`, `missing_safety_doc` and `multiple_unsafe_ops_per_block` all deny), a release profile with `codegen-units = 1`, the `cargo xtask` alias, and `-C force-frame-pointers=yes` for `x86_64-unknown-uefi` (CLAUDE.md §14).
- **Author / date:** Claude Code (local, M1 Max), 2026-11-07. Handoff task T4.
- **Architecture decisions:**
  - `default-members` is the host `xtask` only. Freestanding crates are built for their own target through the xtask, because per-package targets are unstable in Cargo.
  - No `version` field: generated version metadata comes in T5 (CLAUDE.md §62).
- **Dependencies:** none. `Cargo.lock` lists only workspace crates.
- **Licensing:** project licence not chosen yet.

## `boot/uefi/` (`origin-boot-uefi`), the UEFI loader, M1 steps 1 and 2

*Step 1 (the entry below, kept as written) printed a banner and returned. Step 2 follows it.*

- **What:** an EFI application (`origin-boot.efi`, 8.7 KB PE32+). It:
  - validates the system table: null, alignment, signature, revision ≥ 2.0, and `HeaderSize` covering every field read;
  - copies the table once;
  - prints a banner, the UEFI revision and the firmware vendor and revision through `ConOut`;
  - returns `EFI_UNSUPPORTED`, because Framekernel loading is not implemented yet.

  It does not load, verify or jump to anything, and it does not call `ExitBootServices`.
- **Author / date:** Claude Code (local, M1 Max), 2026-11-07. Original code written for this project.
- **Specifications:** UEFI 2.11:
  - §4.1, the image entry point;
  - §4.2, `EFI_TABLE_HEADER` and how a revision is displayed;
  - §4.3, `EFI_SYSTEM_TABLE`;
  - §12.4, `EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL.OutputString`;
  - Appendix D, status codes.

  It also follows the Microsoft x64 calling convention, through Rust's `extern "efiapi"`. Fields the loader does not use are opaque pointers, not guessed signatures.
- **Architecture decisions:**
  - CLAUDE.md §10: a `BOOLEAN` is a `u8`; none crosses the boundary yet.
  - CLAUDE.md §19.
  - No third-party crates (CLAUDE.md §5; handoff §3).
- **Dependencies:** `core`, and the first-party `origin-version` crate for the banner's release identity (since T5).
- **Toolchain:** Rust 1.99.1, `x86_64-unknown-uefi`, rust-lld (msvc flavour).
- **Unsafe inventory, reviewed 2026-11-07 by the author (not independently):**
  - Four `unsafe fn` with `# Safety` contracts: `efi_main`, `uefi::read_system_table`, `uefi::copy_char16_string`, `console::ConOut::new`.
  - Eight one-operation `unsafe` blocks, each with a SAFETY comment. Clippy enforces both rules.
  - No Rust references to firmware memory: the system table is copied after validation, and the protocol is used through raw pointers only.
  - `ConOut` is `!Send` and `!Sync`.
  - The panic handler can call `OutputString` only from Rust code, never re-entrantly from inside a firmware call.
  - Open for step 2: clear `PANIC_CONSOLE` before `ExitBootServices`.
- **Test evidence (2026-11-07):**
  - 20 host unit tests (`cargo test -p origin-boot-uefi --lib`):
    - UCS-2 chunking, CR/LF, replacement characters;
    - revision display;
    - header acceptance and rejection;
    - null and misaligned pointers;
    - bounded and unaligned `CHAR16` copy.
  - Compile-time layout assertions for the x86-64 offsets of the three structures.
  - Clippy at `-D warnings`, on the host and on `x86_64-unknown-uefi`.
  - `cargo xtask boot-test` passed in 1.8 s on QEMU 11.1.3 + edk2-stable202408 (q35, TCG):
    - The loader printed `UEFI 2.7 system table` and `firmware EDK II, revision 0x00010000`.
    - OVMF reported `failed to start Boot0001 …: Unsupported`, so the returned status reached the firmware intact.
  - After T5 the banner reads `Origin OS 0.1.0-dev UEFI loader (build 0)`. `boot-test` builds the expected banner from the same generated constants, and it passes (1.8 s).
  - QEMU only. No physical hardware yet (CLAUDE.md §74).
- **Licensing:** project licence not chosen yet.

### Step 2: read, verify, place and enter the Framekernel (ADR-0002, Proposed)

- **What (2026-11-07):**
  - `protocols`: the UEFI 2.11 boot-services table (offsets of the functions used are pinned), `EFI_LOADED_IMAGE_PROTOCOL`, `EFI_SIMPLE_FILE_SYSTEM_PROTOCOL`, `EFI_FILE_PROTOCOL`, `EFI_FILE_INFO`, the configuration table and the ACPI 2.0 GUID.
  - `main`: reads `\EFI\ORIGIN\KERNEL.ONBF` from the loader's own volume (B1), verifies it, allocates `EfiLoaderCode` pages for the span and places it, allocates a 64 KiB stack and the boot-info pages (OS types 0x8000_0003 and 0x8000_0002), finds the RSDP, clears the panic console, then `GetMemoryMap` → translate → `ExitBootServices` with one retry (B5), and enters the kernel with `call` on the new stack, RDI = boot info (B6).
  - `kernel::verify` (B2): parse, KRN profile, no signature block yet (refused unless `developer-mode`), SHA-512 content digest, manifest SHA-256 and decode, `boot_info_abi` equal to the loader's, `cpu_features` = 0, every section's digest or Merkle root, no compression.
  - `place`: copies SEGMENTs, zero-fills tails, applies RELR with every slot checked to lie in a writable, non-executable segment's content; `kernel_segments` builds the boot-info 0.2 segment table (RELRO drops W).
  - `memmap`: UEFI → Origin region kinds, sort, overlap check, merge, and `carve` to mark the kernel image by address.
- **Specifications:** UEFI 2.11 §4.4 (boot services), §7.2 (memory allocation, `GetMemoryMap`), §7.4 (`ExitBootServices`), §9.1 (loaded image), §13.4–§13.5 (simple file system, file protocol), §4.6 (configuration table); ONBF §3–§6, §8.1; boot-info 0.2.
- **Unsafe inventory (counted by grep, 2026-11-07; reviewed by the author, not independently):** `main.rs` 34 `unsafe` blocks and 2 `unsafe fn`, `uefi.rs` 10 and 2, `console.rs` 2 and 1. `kernel`, `place`, `memmap`, `protocols` and `text` have none. Clippy enforces one operation per block and a SAFETY comment on each.
- **Test evidence (2026-11-07):**
  - 29 host unit tests (`cargo test -p origin-boot-uefi --lib`), including RELR decoding and slot checks, the memory-map translation and carve, and the segment-access mapping.
  - Probe boots: no kernel file → "no Framekernel image" and OVMF reports Not Found; a garbage file → "kernel rejected: … (BadMagic)" and Security Violation.
  - `cargo xtask boot-test`, both scenarios, QEMU 11.1.3 + OVMF (q35, TCG, `-cpu max`): the loader reads, verifies and places the real KRN (7 sections, 3 segments, 128 relocations at the time of writing) and enters the kernel; a KRN with one byte of its first SEGMENT flipped is rejected with "section 1 does not match its digest" and the kernel never runs.
  - QEMU only. No physical hardware yet (CLAUDE.md §74).

## `boot/info/` (`origin-boot-info`), the boot-info contract v0.3 (Proposed)

- **What:** the one structure the loader hands the Framekernel (ADR-0002 B4):
  - `BootInfo`: `repr(C)`, 80 bytes, every field explicitly sized, offsets pinned at compile time;
  - `MemoryRegion`: 24 bytes;
  - `RegionKind`: 13 Origin memory kinds, so the kernel never sees UEFI structures;
  - `BootInfo::check`: validates magic, version, size and region count before anything else is trusted.

  The version is `[compatibility] boot-info` in `origin-version.toml`. Major 0 means unstable until ADR-0002 is accepted.
- **0.2 (2026-11-07):** `KernelSegment` (rva, pages, R/W/X access, 24 bytes; at most 16) appended to `BootInfo`, now 96 bytes; `check` bounds the count; `check_segments` enforces aligned, non-empty, sorted, disjoint, inside the span, readable, never W and X, no unknown bits, `reserved` zero. 6 unit tests.
- **Author / date:** Claude Code (local, M1 Max), 2026-11-07. Original code.
- **Dependencies:** `core` and `origin-version`. `no_std`, `forbid(unsafe_code)`.
- **Test evidence (2026-11-07):**
  - 4 unit tests: magic, header acceptance and rejection, region-kind round trip, version source.
  - Clippy on the host, UEFI and kernel targets.
  - **No panic paths:** the kernel-target object references 0 external symbols.
- **Licensing:** project licence not chosen yet.

## `framekernel/` (`framekernel`), the Framekernel: M1 entry and M2

- **What:** a `no_std`, `no_main` ELF for `x86_64-unknown-none` (24 KB static PIE). The UEFI loader jumps to it after `ExitBootServices`. `_start` (`extern "sysv64"`, boot info in `rdi`):
  - programs COM1 (0x3F8) as a polled 16550 at 115200 8N1, after a bounded wait for the transmitter to drain;
  - checks that the loader applied the image's relocations, using a static pointer read volatile and compared with its RIP-relative target;
  - validates the boot info: pointer, `BootInfo::check`, and a region array that is aligned, sorted and non-overlapping;
  - prints the banner, the boot-info version, the region count, usable MiB, the load address and span, and the ACPI RSDP;
  - halts with `cli; hlt`.

  The panic handler prints to COM1 and halts. The kernel installs no page tables, GDT or IDT yet (M2).
- **Author / date:** Claude Code agent (local, M1 Max), 2026-11-07, in its own worktree, merged by the coordinator. Original code.
- **Specifications:**
  - PC16550D UART datasheet: registers and divisor;
  - the COM1 base, 0x3F8;
  - System V x86-64 psABI;
  - Intel SDM: `IN`, `OUT`, `CLI`, `HLT`;
  - ELF gABI and x86-64 psABI;
  - ONBF §6.1, §6.1.2 and §6.5;
  - boot-info v0.1.
- **Architecture decisions:**
  - lib + bin split, with the host-testable logic in the library (`forbid(unsafe_code)`);
  - `build.rs` passes rust-lld `--entry=_start --no-dynamic-linker -ztext -zseparate-loadable-segments -zmax-page-size=4096 -zcommon-page-size=4096 -znorelro -znoexecstack` for the kernel target only;
  - frame pointers through `.cargo/config.toml` (CLAUDE.md §14).
- **Dependencies:** `core`, `origin-boot-info` and `origin-version`.
- **Toolchain:** Rust 1.99.1, `x86_64-unknown-none` (soft-float, kernel code model, PIE), rust-lld (gnu flavour).
- **Unsafe inventory, reviewed by the author (not independently):**
  - Five `unsafe fn`, each with a `# Safety` contract: `_start`, `read_boot_info`, `Com1::new`, `outb`, `inb`.
  - Eleven one-operation `unsafe` blocks.
  - Port I/O touches only 0x3F8–0x3FD, confirmed in the disassembly.
- **Test evidence (2026-11-07):**
  - 20 host unit tests (`cargo test -p framekernel --lib`): boot-info and region validation, usable-MiB rounding, the exact report lines, and the UART init sequence and bounded polling.
  - Clippy on the host and on the kernel target.
  - The release ELF, checked with `llvm-readobj --elf-output-style=GNU`:
    - ET_DYN, with its entry in the RX segment;
    - PT_LOADs R, RX and RW, all page-aligned;
    - no INTERP, TLS or GNU_RELRO;
    - 38 RELATIVE relocations.
  - Two clean builds in the same path were byte-identical.
  - Boot evidence is in the `boot/uefi` step 2 entry.
- **Licensing:** project licence not chosen yet.

### Higher half and KASLR (ADR-0004, accepted and implemented 2026-11-07)

- **What:** `origin-paging` (the page-table library, now a crate shared with the loader) gains `layout`: the ADR-0004 windows, the KASLR slot arithmetic and the builder (direct map at `base + physical`, the kernel image at its virtual base with its segment permissions, no alias, nothing in the lower half). Boot info 0.3 carries the loader's table root, the direct-map base, the kernel's virtual base and a KASLR flag. The loader takes 64 random bits from RDSEED (else RDRAND; else a fixed placement, reported), relocates the kernel for its virtual base, builds the first tables from a pre-final memory map, and enters through a naked stub (EFER.NXE, CR0.WP, CR3, direct-map stack). The kernel converts every physical address through `phys` (the heap's bookkeeping stays physical and crosses at `GlobalAlloc`), builds its own tables with the same builder, maps the AP trampoline at its identity address only while APs start, and checks at run time that the kernel image has no identity mapping and no direct-map alias.
- **Test evidence (2026-11-07):** 4 layout tests (the high image with exact access and no lower-half mapping, the direct map's coverage, KASLR windows and alignment over 10,000 draws with more than 9,900 distinct placements, and a segment over the direct map is an error). `cargo xtask boot-matrix` passes at 1, 2 and 4 CPUs with every earlier proof. Three boots chose three different placements (RDSEED); a forced no-entropy boot placed the kernel at 0xffffffff80000000 and the direct map at 0xffff800000000000 and passed. Negative control: an identity mapping of the kernel image left in place gives "higher half: kernel image identity address mapped true ... higher half failed".

### M2: descriptors, frames, paging, interrupts, heap (2026-11-07)

- **What:** after the M1 report, `_start` now:
  - loads its own GDT (null, code 0x08, data 0x10, TSS 0x18), reloads CS by far return and the data segments, loads a TSS with a 16 KiB IST1 stack for #DF, and an IDT of 32 naked exception stubs plus the timer (0x20) and spurious (0xFF) vectors; proves it with `int3`, which is reported and resumed;
  - builds the frame allocator (bitmap) and a 256-frame page-table pool in the first fitting `Usable` region;
  - builds its own address space (`kernel_space`): each kernel segment with its declared access from boot info 0.2, a RW+NX identity direct map of `Usable`, `LoaderReclaimable`, `BootInfo`, `KernelStack` and `AcpiReclaimable` RAM, nothing else (no page 0, no MMIO, no alias of the kernel image); sets EFER.NXE and CR0.WP and loads CR3;
  - proves the protections with four probes, each a single deliberate access whose #PF at the exact armed RIP is recorded and resumed: write to code (0x3), write to read-only data (0x3), fetch from writable data (0x11), read of page 0 (0x0);
  - re-proves the frame allocator through the new direct map, then starts the heap (`heap` slabs of 16–2048 bytes plus frame runs, `memory` `GlobalAlloc` behind `sync::SpinLock`) and proves it with a `Vec`, 2000 `Box`es, a `BTreeMap` and a `String`, requiring the heap and frame counts to return exactly to their earlier values;
  - masks the 8259s, enables x2APIC through the legal transitions, calibrates the APIC timer and the TSC over a 10 ms PIT channel-2 one-shot, takes 50 periodic interrupts at 100 Hz (polling with one-instruction interrupt windows, never `hlt`, so a lost interrupt cannot hang the deadline), checks the time against the TSC (≥ 95 %, ≤ 150 % of nominal), and masks the timer.
- **Library modules (host-tested, `forbid(unsafe_code)`):** `descriptors`, `frames` (with runs, and frees refused for frames never allocatable), `paging` (4-level, 4 KiB and 2 MiB leaves, structural W^X, atomic entries, safe `TableMemory`/`TablePool`), `kernel_space`, `apic` (x2APIC and PIT encodings, calibration arithmetic), `heap`.
- **Specifications:** Intel SDM Vol. 3A: segment and system descriptors, the 64-bit TSS, IDT gates and Table 6-1, 4-level paging and the #PF error code, EFER, CR0.WP, CPUID leaves 1 and 8000_0001h, the x2APIC MSR map, LVT timer, divide configuration and SVR, and IA32_APIC_BASE transitions; AMD64 APM Vol. 2 §5.3; Intel 82C54 data sheet (mode 0, 1.193182 MHz on the PC platform).
- **Unsafe inventory (counted by grep, 2026-11-07; reviewed by the author, not independently):** `main.rs` 23 blocks and 5 `unsafe fn`; `cpu.rs` 18 and 9 (including the naked stubs and probes); `timer.rs` 8 and 2 (capability types `X2Apic` and `LegacyPorts`: the safe methods reach only the ID, version, timer and SVR registers, never the ICR, and only the PIC masks, PIT channel 2 and port 0x61); `memory.rs` 1 block, 2 `unsafe fn` (the `GlobalAlloc` methods) and 1 `unsafe impl`; `sync.rs` 2 blocks and 1 `unsafe impl`. The library has none.
- **Test evidence (2026-11-07):**
  - 53 host unit tests (`cargo test -p framekernel --lib`).
  - `cargo xtask boot-test` passes (QEMU 11.1.3 TCG, `-cpu max`, OVMF): segments R/RX/RW; 13 tables, 505 MiB direct map, 246 2 MiB leaves; the four probes fault with the expected codes at the probed addresses; heap peak 899 KiB in 27 slabs and 200 run pages, then everything freed; x2APIC ID 0, version 0x14; APIC timer about 62.6 MHz at divide 16, TSC about 1 GHz; 50 interrupts in 501–504 ms over three runs.
  - Negative controls, each restored byte-identical afterwards, each making the boot test fail: read-only segments mapped writable ("page protection violated"); no EOI ("1 interrupts in 2000027 us"); timer at divide 1 after calibrating at 16 ("50 interrupts in 63686 us"); one leaked `Box` ("heap leaked: … 16 bytes, 1 slab").
  - At 1 kHz with `hlt`, 100 interrupts took 125 ms: QEMU TCG coalesces late expiries. Hence 100 Hz and the asymmetric bound; physical hardware will tighten it.
  - QEMU only. No physical hardware yet (CLAUDE.md §74). Not done: Loom/Kani for the spin lock and the allocators, an independent review.

### M3: processor start-up and preemptive kernel threads (2026-11-07)

- **What:**
  - `acpi` (library): RSDP, table and XSDT checks (signatures, exact lengths, checksums) and MADT processors (types 0 and 9, each once). Firmware tables are untrusted input.
  - `smp`: processors from RSDP → XSDT → MADT; only enabled ones are started. A trampoline (AT&T `.code16`/`.code32`/`.code64`, constants only, no relocations) is copied to the reserved frame 0x8000, which is remapped read/execute while APs start and restored to read/write, never executable, afterwards; its GDT (descriptors pre-marked accessed), CR3, stack, entry and argument are in the separate RW frame 0x9000. Each AP in turn: INIT, 10 ms, start-up IPI, 200 µs, a second one if needed, up to 200 ms for the handshake. The AP enters long mode on the kernel's own tables (PAE, CR3, EFER.LME|NXE, CR0.PG|WP with CD/NW cleared), loads its own GDT and TSS (heap `CpuTables`) and the shared IDT, switches its APIC to x2APIC, and answers with its own APIC ID.
  - `runqueue` (library) and `sched`: a thread's context is its interrupt frame on its own stack; `dispatch` returns the frame to resume and the common stub switches stacks. One round-robin queue per CPU behind its own lock, taken with interrupts disabled outside the timer handler. New threads get a built frame on a 16 KiB heap stack. Each AP runs its own timer and idle loop.
  - `vm`: the live address space behind a lock (read-only mapping of firmware tables on demand; per-page access changes with `invlpg`).
- **Specifications:** ACPI 6.6 §5.2.5, §5.2.6, §5.2.8, §5.2.12; Intel SDM Vol. 3A, the MP initialization protocol and "Initializing IA-32e Mode"; the x2APIC ICR.
- **Unsafe additions (reviewed by the author, not independently):** `smp.rs` (the trampoline's `global_asm!`, the trampoline copy, data-page writes, firmware-table slices, the IPIs, the AP entry), `sched.rs` (RFLAGS save/restore, `sti; hlt` idles, the APIC ID read), `vm.rs` (`invlpg`), and `X2Apic::send_ipi`, kept apart from the safe register methods so ordinary APIC use cannot reach the ICR.
- **Test evidence (2026-11-07):**
  - 61 host unit tests in the library (acpi 4, runqueue 4 among them).
  - `cargo xtask boot-test` (default `-smp 2`) and `ORIGIN_QEMU_SMP=4` and `=8` pass: every enabled processor online 10.0–10.5 ms after its INIT.
  - Scheduler: on every CPU two never-yielding workers ran there for 500 ms with shares within 3 % of each other and 51 context switches (2 and 4 CPUs).
  - Negative controls, each restored byte-identical and each failing the boot test: the trampoline left NX (the AP faults at its first fetch with paging on and triple-faults; QEMU stops after the MADT line), and a scheduler that never switches ("counted 0 and 0, 0 context switches").
  - QEMU only.

### Hardening and lifecycle after ADR-0003 (2026-11-07)

- **SMEP and SMAP** on every processor (CR4 bits 20 and 21, CPUID 7.0), proven on the boot processor with a temporary user page at 0x7fffffffe000: a supervisor fetch faults with #PF 0x11 and a supervisor read with #PF 0x1. Control: CR4 bits not set → "page protection violated: SMEP … Completed".
- **Guard pages** below every kernel thread stack and AP stack (`sched::guarded_stack`, `vm::make_guard`, with the library's new `split_large` for 2 MiB leaves; each AP reloads CR3 when its scheduler starts) and below the boot processor's stack (the loader allocates one extra `KernelStack` page; `kernel_space` leaves it unmapped). Probes: both guards fault with #PF 0x0. Control: guard left mapped → "page protection violated: stack guard page … Completed".
- **Reclaim** of `LoaderReclaimable` memory once every enabled processor is online: 40 MiB (463 → 504 MiB free); `frames::reclaim`, with frees outside owned regions still refused.
- **Thread lifecycle:** run-queue slots Free/Ready/Sleeping; software interrupts 0x81 (yield), 0x82 (sleep), 0x83 (exit) share the timer's switch path; workers exit after the preemption window. Proof: "yield alternated 100 of 100 turns; sleep(5) took 5 ticks; exited threads freed their slots (slot Ok(1) reused)". The proof found a real bug (the boot flow runs with IF=0, so yielding only to itself froze time; fixed with an interrupt window). Control: a yield that never switches → "alternated 2 of 100 turns". A first attempt at that control did not compile and so proved nothing; it was redone and checked to have booted.
- **Reaper:** each CPU records its threads' stacks; an exited thread's stack becomes a zombie, freed only by the CPU it died on and only from thread context (its exit handler ran on that stack), with the guard page remapped first. Proof: the heap returns exactly to its pre-spawn size (0, 88 and 264 KiB at 1, 2 and 4 CPUs; 88 KiB per AP stays). Control: a reaper that never frees fails the boot test.
- **CPU-local area:** IA32_GS_BASE on each CPU points at its own 64-byte `CpuLocal` (keyed by the CPUID x2APIC ID, set right after the segment reload); `cpu_id()` is one GS-relative load, checked against each CPU's local APIC ID at start-up.
- **Measured** (yield ping-pong of three contexts, 2000 rounds, every boot; QEMU TCG, so only the comparison means anything): RDMSR-based CPU lookup 716 / 659 / 693 TSC cycles per switch over three boots; GS-based 553 / 567 / 612 (about 18 % less at the median).
- **Evidence:** `cargo xtask boot-test` passes with `ORIGIN_QEMU_SMP` = 1, 2 and 4; `cargo xtask check` passes (65 library tests). QEMU only. Not done: priorities, load balancing, Loom models.

### M4 groundwork: ring 3 and crash isolation (2026-11-07)

- **What:** user data and 64-bit user code segments (GDT entries 5 and 6); one DPL-3 gate, `int 0x80` (RAX 0 exit, 1 report; everything else returns `u64::MAX`), every other gate DPL 0; `enter_user` (DPL-3 selectors in DS/ES/FS/GS so the return to CPL 3 nulls none, the kernel GS base put back, every general-purpose register zeroed, IF set, IOPL 0, no I/O bitmap); TSS.RSP0 recorded per CPU and moved by the scheduler on every switch to the resumed thread's kernel stack; an exception taken in user mode ends that thread and the kernel carries on.
- **Proof** (boot-test, every boot): a good program reports 42 and exits 0; a program that reads a kernel address takes #PF 0x5 (present, read, user) and is ended; a program that executes `hlt` takes #GP(0) and is ended; two compute-bound user programs run at once, preempted many times, and both report correctly with their RSP intact; afterwards every thread stack is reaped.
- **Controls:** the system-call gate at DPL 0 turns the good program's `int 0x80` into #GP 0x402 (IDT entry 0x80); RSP0 left frozen makes one of the two concurrent threads lose its frames ("reports 0b10"). Both fail the boot test.
- **Specifications:** Intel SDM Vol. 3A: privilege levels and gates (DPL checks for `int n`), the 64-bit TSS (RSP0), IRET to a lower privilege level (segment-register nulling), #PF and #GP error codes.
- **SYSCALL entry** (later the same day): EFER.SCE, STAR, LSTAR and FMASK (TF, IF, DF, NT, AC) on every CPU; `syscall_entry` switches to the running thread's kernel stack (kept in the CPU-local area beside TSS.RSP0) and builds the same frame an interrupt from user mode pushes, so `syscall` and `int 0x80` share one dispatch path and return with IRETQ. Proof: a program reports 77 and exits with `syscall`. Control: SCE clear → #UD, the thread ends unreported.
- **IST stacks for NMI and #MC** (IST2, IST3; 16 KiB each per CPU), so an NMI right after `syscall` never lands on the user stack. Proof: a self-NMI's frame lies inside the boot processor's NMI stack. Control: NMI without IST → the frame is on the interrupted stack.
- **Copy from user memory** (system call 2, write): the buffer must lie in the user half and every page must be a user page (a table walk), and the copy runs in a STAC/CLAC window as a probe-armed `rep movsb`, so a fault mid-copy returns EFAULT. The first version checked only the range and leaked eight bytes of kernel memory to a user program, because the kernel is identity-mapped in the lower half; the page-table check fixed it, and that run is the negative control. Proof: "valid buffer -> 17, kernel address -> EFAULT, unmapped user address -> EFAULT".
- **Not done:** per-process address spaces (the user pages live in the kernel's tables; ADR-0004 comes first), `swapgs`, SYSRET, process objects and capabilities (M4).

### M4: process address spaces (2026-11-07)

- **What:** `origin-paging` gains `upper_half`/`set_upper_half` and `map_user` (lower half only). `process::Process` owns a PML4 whose kernel half (entries 256–511) is copied from the kernel's tables (ADR-0004 H2) and whose lower half holds only its own pages; its page tables come from the frame allocator through a `TableMemory` that answers only for frames it allocated itself; `destroy` frees them. The scheduler records each thread's address space and, on every switch, loads its CR3 (the kernel's for kernel threads) when it differs, next to TSS.RSP0; `spawn_in` starts a thread in a given address space.
- **Proof** (every boot): two processes run the same program at the same virtual addresses (code 0x400000, data 0x500000, stack 0x600000) at the same time, each storing its own value (11, 22), being preempted while the other stores its own at the same address, then reading back: the reports are exactly bits 11 and 22; the kernel's tables do not map 0x500000; the two PML4s differ. Both are destroyed afterwards and their frames freed.
- **Control:** both threads in the first address space read back 11 (reports 0x800), and the boot test fails.
- **Test evidence:** 15 `origin-paging` tests (one for the shared kernel half and per-space user frames); `cargo xtask boot-matrix` passes at 1, 2 and 4 CPUs; `cargo xtask check` passes.
- **Not done:** process objects with handles and capabilities, a process's user copies checked against its own tables (`copy_from_user` still checks the kernel's), more than 32 table frames per process, PCIDs (H5), loading programs from `.ose` images.

### Clocks: monotonic TSC clock and RTC wall clock (CLAUDE.md §68, 2026-11-07)

- **What:**
  - `clock` (library): `Scale` turns ticks into nanoseconds with one 64×64→128-bit multiply and a shift. The multiplier is rounded up and keeps at least 63 significant bits, so a reading is the exact value rounded either way (exact whenever that is a whole number of nanoseconds) and never decreases. Also `ticks_for` (deadlines, rounded up), `SharedScale` (published once, read without a lock) and the CPUID 15h TSC rate.
  - `rtc` (library): CMOS RTC decoding (BCD or binary; 12- or 24-hour; the FADT's century register, else 20xx), with every field range-checked and invalid BCD digits refused; Unix time in both directions (proleptic Gregorian, no leap seconds); ISO 8601 display.
  - `acpi::parse_fadt`: the century register index (accepted only in 0Eh–7Fh) and the IAPC_BOOT_ARCH "CMOS RTC not present" flag.
  - `firmware` (binary): ACPI table lookup, moved out of `smp` and now shared by SMP and the clocks.
  - `time` (binary):
    - The monotonic clock's zero is the TSC at `_start`.
    - At boot, one RTC second is timed on the TSC between the ends of two update cycles.
    - The TSC rate is CPUID 15h's if enumerated and within 1 % of the PIT calibration; else the RTC second's, if within 1 %; else the PIT's.
    - The RTC is read just after the first edge, as two identical consecutive readings, so the wall clock is right to the polling latency rather than to a second.
    - A cross-CPU check: for 100 ms every online processor takes ordered TSC readings (RDTSCP, else LFENCE then RDTSC) in turn under one lock. No reading may be below the one before it, and the processors must have handed over at least 100 times.
- **Specifications:**
  - Intel SDM Vol. 2A, CPUID leaves 15h and 8000_0007h; Vol. 2B, RDTSC and RDTSCP;
  - the MC146818-compatible RTC as Intel PCH datasheets describe it: registers 00h–0Bh, register A's update-in-progress bit, register B's binary and 24-hour bits;
  - ACPI 6.6 §5.2.9 (FADT: `CENTURY` at offset 108, `IAPC_BOOT_ARCH` at 109, bit 5).
- **Unsafe additions, reviewed by the author (not independently):**
  - `time.rs`: `Cmos::new` (an `unsafe fn` owning ports 70h–71h, with NMIs left unmasked), two port-I/O blocks, and the two ordered-TSC `asm!` blocks;
  - `firmware.rs`: the firmware-table slice, moved from `smp.rs`.
- **Test evidence (2026-11-07):**
  - 77 library tests:
    - the conversion against exact 128-bit arithmetic at nine rates from 1 Hz to 2^64−1 Hz over about 7,000 tick counts, with monotonicity checked;
    - Unix time against Python's `datetime` at nine dates from 1970 to 2400 (leap, century and Y2038 cases);
    - every day from 1970 to 2500 round-trips;
    - BCD, binary and 12-hour decoding, and twelve malformed-register cases;
    - the FADT century register's accepted range.
  - `cargo xtask boot-test` and `boot-matrix` (1, 2 and 4 CPUs) pass. QEMU's RTC starts at 2030-06-15T12:00:00 (`-rtc base=`), and the kernel reports 2030-06-15T12:00:04Z.
  - Across boots, one RTC second lasted 999 930 to 1 001 231 µs on the PIT-calibrated TSC, so the RTC second set the rate (999 999 to 1 000 003 kHz; QEMU's TCG TSC runs at about 1 GHz). TCG reports no invariant TSC and no CPUID 15h rate.
  - Cross-CPU check: 0 backwards readings with 12 291 handoffs on 2 CPUs and 8 658 handoffs on 4.
  - The check costs up to two seconds of boot (two RTC update edges); boot to halt is 4.5 s.
  - Negative controls, each restored afterwards and each failing the boot test:
    - AP readings skewed by 5000 ticks → "3862 backwards (worst 5000 ticks): clock failed";
    - the calibration 3 % off → "one RTC second lasted 968002 us … clock failed";
    - BCD read as binary. This control found a real gap: the year became 3248, beyond the 64-bit nanosecond range, and was shown as 2554 without failing. Such a year is now refused ("the RTC says 3248-06-21T18:00:03Z, beyond the clock's range: clock failed").
  - QEMU only.
- **Not done:** HPET or ACPI PM timer fallbacks; resynchronising TSCs (IA32_TSC_ADJUST) when the check fails; TSC-deadline timers; time zones and network time; the user-visible clock calls.

### Serialized kernel console (2026-11-07)

- **What:**
  - `console` (binary): a `Console` collects one line (160 bytes) and sends it to COM1 under one spin lock, with interrupts disabled. Lines from different processors, or from an interrupt handler on the same one, never interleave; longer lines go out in whole 160-byte pieces.
  - Exception reports and the panic handler use `Console::urgent`. It tries the lock 2^22 times (on the order of 100 ms, longer than one line takes), then writes without it, so a processor that faulted inside the console, or waits on one that died holding the lock, still reports.
  - Every boot proof, the exception handler, the panic handler and the write system call now print through it. `origin-sync` gains `SpinLock::try_lock`.
- **A bug found on the way:** the first `try_lock` built its guard with `bool::then_some`, which evaluates its argument eagerly. A failed attempt therefore built a guard and dropped it, unlocking a lock that someone else held. The new host test (a held lock must refuse `try_lock` from another thread) caught it before the kernel used it; the guard is now built only on success.
- **Proof** (every boot): every online CPU prints 20 lines at once. Each line is `origin-kernel: console: CPU {id} line {k} of 20: ` followed by 60 copies of the letter `a + id`. xtask then checks the whole log:
  - every proof line arrived whole and in order, from every CPU;
  - no line of the log holds two `origin-kernel:` prefixes.

  Two new xtask unit tests cover mixed, lost, joined and over-long lines. In the boot log the two CPUs' lines alternate in runs (8, 1, 7, 1, 1, 1, 3, 6, …), so the printing really overlapped.
- **Control:** with the lock bypassed, two CPUs' bytes mix mid-line, and the boot test fails with "two kernel lines ran together".
- **Evidence:** `cargo xtask check` and `boot-matrix` (1, 2 and 4 CPUs) pass; boot to halt takes 4.8, 5.4 and 7.1 s.
- **Limitation:** interrupts stay off while one line is sent. In QEMU that is microseconds; at 115 200 baud on real hardware it is about 9 ms per 100 bytes. Production logging will go to a memory ring drained by a thread (CLAUDE.md §69).

### TLB shootdown and global kernel pages (ADR-0004 H5; CLAUDE.md §28, 2026-11-07)

- **The gap closed:** before this, a guard page made while application processors ran (every `spawn` makes one) was invalidated only on the processor that made it. Another processor could keep the old translation, a 2 MiB one included, and run through the guard without a fault. The negative control below shows QEMU really keeps such stale entries.
- **What:**
  - `origin-paging`: `Attributes::kernel_global`, `Translation::global`, and `AddressSpace::leaf_census` (present leaves by half and by global bit). The layout builder marks the whole kernel half (direct map and image) global.
  - `cpu`: `enable_global_pages` (CR4.PGE, cleared then set, which also drops every translation cached under the loader's tables), `flush_tlb_all` (toggles CR4.PGE when it is set, since a CR3 load keeps global entries; else reloads CR3), `invlpg`, and the gate for vector 0xF0.
  - `tlb` (binary), the shootdown:
    - one request at a time, under an initiator lock; a processor waiting for the lock serves the request in progress, so two initiators never deadlock;
    - each request has a generation; every processor records the last one it served, and the initiator waits for every target's record (panicking after about a second, because carrying on would not be memory-safe);
    - targets are the processors whose schedulers run, each sent a fixed IPI by its x2APIC ID. A processor marks itself online and then flushes everything, so no request falls between the two.
  - `vm`: `unmap` and `make_guard` shoot down once the address-space lock is released; `make_guards` unmaps several pages with one full flush. `map_firmware` and `unguard` map global pages.
- **Specifications:** Intel SDM Vol. 3A §4.10.2.4 (global pages), §4.10.4 (invalidation: INVLPG, CR3 and CR4.PGE writes), §4.10.5 (propagation to multiple processors), the x2APIC ICR; Vol. 2A, INVLPG.
- **Proof** (every boot):
  - Every processor turned on global pages, and the kernel's tables hold 4269 to 4296 kernel-half leaves, all global, and none in the lower half.
  - Every application processor reads three heap pages, then stops touching them. The boot processor unmaps one with a single-page shootdown and two with one full flush. Every processor's next read of each page must fault: 3 of 3 at 2 CPUs, 9 of 9 at 4.
  - Measured in QEMU TCG (comparisons only): a single-page shootdown took 11 000 to 48 000 TSC cycles at 2 to 4 CPUs, a full flush 92 000 to 113 000.
- **Negative controls**, each restored and each failing the boot test:
  - No IPIs sent: "0 of their 3 reads faulted and 3 saw a stale translation".
  - The direct map left non-global: "upper_local: 4199 … TLB failed".
  - An earlier attempt at that control changed `map_firmware`. It passed, because this boot maps no new firmware page, and so proved nothing; it was redone on the layout builder.
- **Test evidence:** 16 `origin-paging` tests (the census, and global bits surviving a large-page split); `cargo xtask check`; `boot-matrix` at 1, 2 and 4 CPUs.
- **Not done:** PCIDs for user address spaces (every CR3 switch still drops all non-global entries); shootdowns for user mappings of a process running on several processors (processes are single-threaded so far); INVPCID.

### Kernel wait queues and cross-CPU wake-ups (CLAUDE.md §25, §26, §30; 2026-11-07)

- **What:**
  - `runqueue` (library): a `Blocked` state; a per-slot generation, bumped by every `push`, so a wake-up names its thread exactly and a stale one never wakes the slot's next thread; and a pending-wake flag, so a wake-up that arrives before its thread has blocked makes the next `block` return at once.
  - `sched`: `block` (software interrupt 0x84), `current_thread`, `ThreadRef`, and `wake`. Waking a thread blocked on another processor sends that processor a reschedule IPI (vector 0xF1), whose handler acknowledges and switches.
  - `wait` (binary): `WaitQueue::wait_until(condition)` checks the condition under the queue's lock and registers the thread there before blocking; `wake_one` takes a waiter under the lock, releases it, then wakes it. Lock order: a wait queue's lock, then what its condition reads. These queues are kernel-internal.
- **Proof** (every boot):
  - A producer on CPU 1 sends 10 000 numbers through an 8-slot ring to a consumer on CPU 0, each blocking when the ring is full or empty. All arrive in order, in 19–32 ms.
  - Two threads on CPUs 0 and 1 pass a turn back and forth 1000 times: 44–73 µs per round trip, woken by IPI, against a bound of 2 ms.
  - On one CPU the producer and consumer alternate exactly (1250 blocks each), and a round trip takes 4 µs.
- **Negative controls**, each failing the boot test:
  - No reschedule IPI: the transfer slows from 19 to 180 ms, and the ping-pong does not finish within 5 s (a woken thread waits for its processor's next 10 ms tick).
  - No pending wake-ups: a wake-up is lost and the transfer stalls at 6202 of 10 000 items. So the race between registering and blocking really happens even in QEMU, and the pending-wake rule is what closes it.
- **Test evidence:** 2 new run-queue tests (blocking and waking; a stale generation and the boot context refused); `cargo xtask check`; `boot-matrix` at 1, 2 and 4 CPUs.
- **Timed waits (added later the same day):**
  - A blocked slot may carry a deadline tick, and the tick that reaches it makes the thread ready. `sched::block_until` converts a monotonic-clock deadline into ticks (RDI of the block request), and `WaitQueue::wait_until_deadline` returns `TimedOut` once the deadline passes with the condition still false.
  - Proof: an unsignalled 50 ms wait timed out after 57–58 ms, and a 2 s wait signalled after about 20 ms returned in 8–19 ms, not timed out (1, 2 and 4 CPUs).
  - Control: deadlines ignored by the run queue → the 50 ms wait never returns → "wait queues failed".
  - One new run-queue test covers a deadline, a wake-up before it, a late wake-up kept, and the one-tick minimum.
- **Not done:** sub-tick deadlines (TSC-deadline one-shot timers), priority inheritance, the user-visible waiting of ADR-0005 P9, and Loom models of the protocol.

### PCI Express enumeration (M5 groundwork; CLAUDE.md §32; 2026-11-07)

- **What:**
  - `origin-paging`: a `Cache` memory type (write-back at PAT entry 0, or uncached at entry 3 through PCD and PWT); `Attributes::mmio`; `Translation::uncached`. `split_large` now keeps PCD, PWT and the PAT bit, which moves from bit 12 to bit 7. Before, it dropped PCD and PWT, which would silently have made split device mappings write-back.
  - `cpu::check_pat`: every processor's IA32_PAT must hold write-back at entry 0 and uncached at entry 3 (the reset values). The kernel then refuses to map device registers rather than reprogramming caches behind the firmware.
  - `vm::map_mmio`: a bump allocator in the MMIO window (ADR-0004 H2), with 2 MiB pages where the physical alignment allows.
  - `pci` (library), over an abstract `ConfigSpace`: MCFG allocations, checked (1 MiB-aligned base, no wrap, end bus not below start); ECAM offsets, refused outside the window or misaligned; header decoding; enumeration (functions 1–7 only on multi-function devices, nothing when function 0 is absent); BAR sizing for 32- and 64-bit memory and I/O BARs, including 16-bit I/O decoders. Sizing turns decoding off meanwhile, then restores every BAR and the command register, writing zeros to the status register's write-one-to-clear bits.
  - `pcie` (binary): MCFG, then segment 0's window (refused if it overlaps RAM in the boot map), mapped uncached; every function listed with its sized BARs; afterwards each BAR and the command register is read back and compared.
- **Bugs found by the new tests before they reached the kernel:**
  - An unimplemented BAR (mask 0) was reported as 4 GiB, because the mask was widened to 64 bits before the zero check. The same flaw was in the I/O path.
  - The PAT bit and PCD/PWT were lost on a split (above).
- **Specifications:** PCI Firmware Specification 3.3 §4.1.2 (MCFG); PCI Express Base 6.x §7.2.2 (ECAM) and §7.5.1 (configuration headers, BAR sizing); Intel SDM Vol. 3A §11.12 (PAT) and §4.9 (paging memory types).
- **Proof** (every boot, QEMU q35 under OVMF):
  - The ECAM is at 0xe0000000 (256 MiB, buses 0–255), mapped at 0xffffe00000000000 with 2 MiB uncached pages; IA32_PAT is 0x0007040600070406 on every processor.
  - The 5 functions found: host bridge 8086:29c0; VGA 1234:1111 (BAR0 prefetchable 16 MiB, BAR2 4 KiB); ISA bridge 8086:2918; AHCI 8086:2922 (BAR4 I/O 32 B, BAR5 4 KiB); SMBus 8086:2930 (BAR4 I/O 64 B).
  - Every BAR and command register read back unchanged.
- **Negative controls**, each failing the boot test:
  - BARs not restored after sizing → "restored: false".
  - Uncached mappings encoded without PCD and PWT → "uncached false".

  Whether sizing turns decoding off is invisible in QEMU; a host test checks it on a device model that records any probe made while decoding is on.
- **A mistake in the boot test, caught by the matrix:** the PCIe markers were first listed before the scheduler's last line, which the boot prints earlier, so every boot would have failed. The controls had failed on their own failure strings, so they did not show it. The markers now follow the boot order. Lesson: run the good path after adding markers, before running controls.
- **Test evidence:** 4 `pci` tests and 1 more `origin-paging` test; 83 library tests in all; `cargo xtask check`; `boot-matrix` at 1, 2 and 4 CPUs.
- **Not done:** bridges' bus numbers and hot-plug, MSI/MSI-X, PCIe capabilities, AER, an IOMMU (no DMA confinement yet), and segments other than 0.

### NVMe driver (M5; CLAUDE.md §33; 2026-11-07)

- **What:**
  - `nvme` (library): CAP decoding, the version register, doorbell offsets, CC and AQA encodings, submission entries (Identify, Create I/O Completion and Submission Queue, Read, Write, Flush), PRP entries for transfers of up to two pages, completion decoding with the status field, the queue indexes (one slot always empty, the phase flipping at each wrap), and Identify Controller and Namespace parsing (bounds-checked; an inactive namespace, an out-of-range format index or a block size outside 512 B–64 KiB is refused).
  - `storage` (binary), a first-party polled driver (ADR-0001 D2):
    - binds to the first NVMe controller PCIe listed; turns on memory decoding and bus mastering; maps BAR 0 uncached;
    - resets the controller and awaits CSTS.RDY within CAP.TO (failing on the fatal-status bit); creates 32-entry admin queues and one 64-entry I/O queue pair;
    - masks the pin interrupt and polls each completion: the phase dword first, a deadline of 2 s, and a check that the command ID is its own;
    - DMA pages are zeroed heap pages that are never freed; a fence orders writes before every doorbell; no reference is ever made into memory the device may write.
  - xtask `disk`: a 16 MiB GPT test disk built independently of the kernel (protective MBR, primary and backup headers and entry arrays with bitwise CRC-32, two partitions, and a per-LBA pattern), attached as `-device nvme,serial=ORIGIN0001`. The image is fresh for every boot. The two Origin partition-type GUIDs are provisional, for this disk only (the M9 partition-layout decision fixes the real ones).
- **Specifications:** NVM Express Base 2.x (§3.1.4 registers, §4.2 and §4.6 queue entries and the phase tag, §5.2 admin commands); NVM Command Set 1.x (Read, Write, Flush, Identify Namespace); NVMe over PCIe Transport 1.x (doorbells, PRPs); UEFI 2.11 §5.3 (GPT, for the test disk).
- **Proof** (every boot):
  - Controller: 00:02.0 1b36:0010, NVMe 1.4.0, with a 64-bit BAR 0 that OVMF placed at 0xc000000000 (above 4 GiB); model "QEMU NVMe Ctrl", serial "ORIGIN0001".
  - Namespace 1: 32 768 blocks of 512 B.
  - Through I/O queue 1: LBA 0 holds the MBR signature, LBA 1 "EFI PART", and 8 of 8 blocks at LBA 2048 match the pattern xtask wrote.
  - LBA 32704 was written, flushed and read back unchanged; a read at LBA 32768 (past the end) completed with status type 0, code 80h (LBA out of range).
- **Negative controls**, each failing the boot test:
  - Reading from LBA 2049 instead of 2048 → "0 of 8 blocks … match".
  - Bus mastering really off → "a command timed out". The driver reports the timeout; it does not hang.
  - A first version of that control only left out the bus-mastering bit. OVMF had already set it, so the control passed and proved nothing; it was redone so that it clears the bit.
- **Test evidence:** 7 `nvme` tests (91 library tests in all) and 3 xtask `disk` tests (CRC-32 check value, GUID byte order, a self-consistent GPT); `cargo xtask check`; `boot-matrix` at 1, 2 and 4 CPUs.
- **Not done:** MSI-X interrupts, several I/O queues (one per CPU), PRP lists for transfers over 8 KiB, controller-reset recovery, power states, health logs (SMART), an IOMMU, and a block layer above the driver.

### GUID Partition Tables (CLAUDE.md §39; UEFI 2.11 §5.3; 2026-11-07)

- **What:**
  - `gpt` (library): a table-driven CRC-32, independent of xtask's bitwise one, with incremental updates; GUIDs in their mixed-endian form, shown in canonical text.
  - `parse_header` follows UEFI 2.11 §5.3.2: signature, revision 1.0, header size 92 bytes to one block, the header CRC (the CRC field zeroed), and MyLBA. Also a layout check: the usable range lies inside the disk, after the primary structures and before the backup header; the entry array lies inside the disk, outside the usable range, and over neither header; entries are at least 128 bytes and a power of two; the array is at most 64 KiB.
  - `parse_entries`: the array CRC; every used entry inside the usable range and not ending before it starts; no two entries overlapping; names read as UTF-16LE.
  - `Header::agrees_with`: the two copies must describe the same disk, entries and CRC, and point at each other.
  - `storage`: reads both copies over NVMe and checks each and their agreement, then lists the partitions. With only one valid copy, the disk is reported from it and the proof fails.
- **Proof** (every boot):
  - Both copies are valid (the backup at LBA 32767) and agree.
  - The disk is 28a71590-fa80-422c-a428-8dff96984d99, usable blocks 34–32734.
  - Partition 1 is "Origin system A", type ab846206-03ad-4cf6-b80b-5da764646190, blocks 2048–10239. Partition 2 is "Origin data", type 1a0f3667-a459-431f-b1fb-c1d11e0d699a, blocks 10240–32734.
  - All of this is exactly what xtask wrote, so two independent GUID and CRC implementations agree.
- **Firmware finding:** the first control flipped one bit in the primary entry array. It passed, because OVMF had restored the primary copy from the backup before the kernel ran, as UEFI 2.11 §5.3.2 allows. The test image is fresh for every boot, so the repair does not persist. Keep this in mind for the installer and recovery (M9, M12): firmware may rewrite GPT structures on its own.
- **Negative control** (failing the boot test): both entry arrays corrupted, which the firmware cannot repair → "primary copy invalid (ArrayCrc), backup copy … invalid (ArrayCrc) … GPT failed".
- **Test evidence:** 5 `gpt` tests: the CRC check value and incremental updates; GUID text; a valid disk with agreeing copies; header checks (signature, revision, size, the CRC and the bytes it does not cover, MyLBA, entry size and count, layout); entry checks (ranges, overlap, capacity, a flipped array bit, a short array). Also `cargo xtask check` and `boot-matrix` at 1, 2 and 4 CPUs.
- **Not done:** restoring a corrupt copy (the kernel only reports it), MBR-only disks, a block layer and volumes above partitions, and a filesystem.

### Seeded mutation tests for the firmware and device parsers (CLAUDE.md §72; 2026-11-07)

- **What:** `testing` (test-only) holds xorshift64\* and a mutator: bit flips, random and boundary bytes, truncation, extension and chunk copies, one to four per case. Each parser gets a test that it never panics on mutated valid input and that anything it accepts keeps its invariants:
  - **GPT:** the header CRC really matches; the usable range lies inside the disk; partitions lie inside it and never overlap. Half the cases re-seal both CRCs after mutating, so the semantic checks behind them are reached: over 500 headers and 300 tables were accepted per default run.
  - **ACPI:** an accepted RSDP sums to zero over both checksum ranges; MADT processors are each listed once (half the cases re-sealed); an accepted FADT's century register is in the standard bank.
  - **PCI:** accepted MCFG windows are 1 MiB-aligned and do not wrap; random sizing masks give only powers of two.
  - **NVMe:** accepted Identify Namespace data has 512 B–64 KiB blocks and at least one block; Identify text fields are printable; any completion decodes.
  - **RTC:** every date from 1970 to 2099 round-trips through each register encoding (BCD or binary, 12- or 24-hour, with or without a century register). A single corrupted register decodes only to a real date, and both outcomes are reached over 10 000 times.
- **Scale:** the default run is about 20 000 parser cases plus 100 000 RTC cases, in 0.8 s with all 101 library tests. `ORIGIN_MUTATION_SCALE=n` multiplies the counts. A 50× release run (about a million parser cases and 5 million RTC cases) passed in 9 s.
- **Result:** no parser bug was found. The two first-run failures were in the tests themselves (shared Identify data, and random registers that are almost never a date) and were redesigned.
- **Not done:** coverage-guided fuzzing (cargo-fuzz needs nightly, against CLAUDE.md §12), and Kani proofs of the same properties (§73).

### MSI-X and interrupt-driven NVMe completions (CLAUDE.md §31 interrupt management, §33; 2026-11-07)

- **What:**
  - `pci` (library): the capability-list walk. It needs the status bit, starts from the pointer at 34h, stops at a loop, a pointer into the header or 48 entries, and reads bytes through dword accesses. Also the MSI-X capability (entries, table and PBA BARs and offsets, with BIRs above 5 refused) and the x86 MSI message: address `FEEh` with the destination APIC ID (IDs above 255 refused until interrupt remapping exists), and fixed edge-triggered data for vectors 16–254.
  - `nvme`: Create I/O CQ with interrupts (PC, IEN and the vector in CDW11).
  - `pcie::enable_msix`: sets the enable bit and clears the function mask; `pcie::msix`.
  - `cpu`: `DEVICE_VECTOR` 0x40. Its handler acknowledges, calls `storage::on_interrupt` (count, record the CPU, wake the wait queue), then reschedules.
  - `storage`:
    - `run` split into `submit` and `poll`, so a thread can block between them;
    - the controller kept in a lock once bound;
    - `setup_interrupts`: every MSI-X entry masked, then entry 1 routed to the target CPU and unmasked; MSI-X on; I/O queue pair 2 created with interrupts on vector 1;
    - the I/O thread submits under the driver lock and blocks with `wait_until_deadline`. Its condition is the completion queue itself, so a completion that lands before the thread blocks is never missed.
  - `sched::stop_boot_processor`: the boot processor's scheduler now stays active through the storage proofs.
- **Specifications:** PCI Express Base 6.x §7.5.1.1 (capability pointer), §7.7.2 (MSI-X); Intel SDM Vol. 3A §11.11 (message address and data); NVM Express Base 2.x §5.2.1 (Create I/O CQ: PC, IEN, IV).
- **Proof** (every boot): q35's NVMe has 65 MSI-X vectors, with the table in BAR 0 at offset 2000h. A thread on CPU 1 (CPU 0 with one processor) made 64 reads of 8 blocks through queue 2, each blocking until its interrupt: exactly 64 interrupts, all on the target CPU, and 512 of 512 blocks matched the pattern.
- **Measured in QEMU:** 89 µs per read on one CPU, about 375 µs when the thread waits on an idle AP. The difference is probably QEMU waking a halted vCPU, not the driver.
- **Negative control:** vector 1 left masked → no interrupt arrives, and the first wait times out → "0 reads … none arrived … NVMe failed".
- **Process note:** the MSI-X markers were first placed before the GPT markers, which the boot prints earlier. This time the good-path run made before the controls caught it, as the lesson from the PCIe step requires.
- **Test evidence:** 1 more `pci` test (capability walk, loops, header pointers, bad BIRs, messages) and 1 more `nvme` assertion; `cargo xtask check`; `boot-matrix` at 1, 2 and 4 CPUs.
- **Not done:** per-CPU queues with a vector each, interrupt remapping (x2APIC IDs above 255), MSI (non-X) devices, and moving the admin queue to interrupts.

### Per-CPU NVMe queues and concurrent I/O (CLAUDE.md §33 multiqueue, §66 per-CPU queues; 2026-11-07)

- **What:**
  - `nvme`: Set Features, Number of Queues (feature 07h), and decoding of the allocation in its completion.
  - `storage`:
    - before any I/O queue exists, the driver asks for queue pairs 1 (polled), 2 (the MSI-X proof) and one per processor, and fails if the controller allocates fewer;
    - `submit_on` and `poll_on` work on one queue pair, so a processor's own queue needs no global lock;
    - per processor `k`, MSI-X entry `2 + k` is sent to that processor and I/O queue pair `3 + k` completes to it;
    - the completion interrupt wakes the wait queue of the processor it arrived on, and counts per processor.
  - Each processor's thread submits under its own queue's lock and blocks until its interrupt; the wait's condition is its completion queue.
- **Proof** (every boot):
  - Asked for 4 queue pairs (6 at 4 CPUs); QEMU allocated 64.
  - Every online CPU at once: 32 reads of 8 pattern blocks through its own queue pair, all matching, and one block of its own written, flushed, cleared and read back. Its 35 interrupts (32 reads, write, flush, read-back) all arrived on that CPU.
  - Totals: 1 CPU 32 reads in 4 ms; 2 CPUs 64 in 52 ms; 4 CPUs 128 in 54 ms. Twice the work in the same time at 4 CPUs, so the queues run concurrently (QEMU throughput figures are only comparisons).
- **Negative control:** every per-CPU vector sent to CPU 0 → CPU 1's thread is never woken: "0 reads … 0 interrupts on CPU 1 … NVMe failed".
- **Test evidence:** 1 more `nvme` assertion group (Set Features encoding and allocation decoding); `cargo xtask check`; `boot-matrix` at 1, 2 and 4 CPUs.
- **Not done:** a block layer that routes requests to the submitting CPU's queue, PRP lists and larger transfers, command timeouts with abort and controller reset, and NUMA placement of queue memory.

### Weighted fair scheduling: stride scheduling and an idle context (CLAUDE.md §26; 2026-11-07)

- **What:** `runqueue` (library) picks the next context by stride scheduling, a deterministic proportional-share policy (Waldspurger and Weihl, MIT LCS/TM-528, 1995; the published algorithm, not anyone's code):
  - every context has a weight (1–1024, default 8) and a pass value; each quantum it uses (a tick or a yield) advances the pass by 2^20 / weight;
  - the ready context with the lowest pass runs next, ties going round-robin from the current one, so equal weights are exactly round-robin;
  - a context that becomes ready (new, woken, or due after a sleep) starts at no less than the lowest pass among the ready ones: sleeping earns no credit;
  - slot 0 can be the processor's idle context, which runs only when nothing else is ready; each AP's idle loop now is one. Leaving idle mode admits slot 0 at the floor.
  - `sched::spawn_weighted` sets a thread's weight.
- **Bugs found by the new host tests:**
  - A new thread's starting pass was computed with the new slot already counted as ready. The floor then included that slot's stale pass of 0, so the newcomer started 70 quanta behind and took 180 of 300 quanta.
  - The idle context, which is charged only for quanta nobody else wanted, monopolized the processor when it stopped being idle (20 of 20 ticks).

  Both are fixed. Three test expectations were wrong instead and were corrected: an off-by-one tick, the share arithmetic, and an old test that encoded pure round-robin order.
- **Proof** (every boot): three never-yielding workers with weights 1, 4 and 16 share one CPU for 2 s. Their shares are 45–50, 185–191 and 758–763 per mille, against ideal 47, 190 and 761 (1, 2 and 4 CPUs). Every earlier scheduler proof still passes.
- **Measured:** with an idle context on each AP, two workers there now get about 1.6 M iterations each in the 500 ms window, against 1.0 M before. The idle loop no longer takes a third of the processor.
- **Negative control:** weights ignored (one stride for all) → shares 341, 327 and 331 → "scheduler failed".
- **Test evidence:** 3 new run-queue tests (exact shares 100 : 400 : 1600 over 2100 quanta, and after a weight change; the idle context; no credit for sleeping and new threads starting level); 106 library tests; `cargo xtask check`; `boot-matrix` at 1, 2 and 4 CPUs.
- **Not done:** interactive and background classes on top of the weights, load balancing between processors, priority inheritance, and an EEVDF evaluation (§26).

### Block layer: a page cache over NVMe (CLAUDE.md §39, §40; 2026-11-08)

- **What:**
  - `blockcache` (library): the cache's index — a fixed-capacity map from a page number to a slot, with LRU order and write-back bookkeeping. `access` returns Hit, Fill (an empty slot), Replace (evict a clean slot) or Evict (write back a dirty slot first); `mark_dirty`, `mark_clean`, `dirty_slots` (oldest first) and hit/miss counts. No data, no `unsafe`.
  - `block` (binary): each cache slot is a 4 KiB DMA page (8 × 512 B device blocks). A read returns the cached page, fetching it on a miss after writing back the page it evicts; a write does a read-modify-write and dirties the page; a flush writes every dirty page back and flushes the controller. `storage` gained the polled device access it needs (`geometry`, `dma_page`, `transfer`, `flush_device`, `dma_read`/`dma_write`).
- **Bugs found by the library's model test before the kernel ran:** none in the cache. Three first-run failures were all in the test model (a second stray `access` per write inflated the counts, the mirror was sized to the unclamped capacity, and one stats expectation was miscounted) and were fixed.
- **Proof** (every boot): a 4-slot cache over the NVMe disk. Reading all 32 pattern blocks of 4 pages does 4 device reads (one per page) and every block matches; re-reading the 4 resident pages does no device I/O (32 hits). A 5th page evicts the LRU clean page with one read and no write-back. A block written through the cache, then flushed (one device write), is read back changed after its page is evicted.
- **Negative controls**, each failing the boot test:
  - cache hits still fetch from the device → cold reads jump from 4 to 32 and re-reads do 4 more;
  - written pages never marked dirty → "flushed (0 device writes) ... read back changed: false".
- **Test evidence:** 6 `blockcache` tests, including a 20 000-operation model check over a 20-block working set in 8 slots (every slot and dirty bit mirrored, no block cached twice, the hit rate sane); 112 library tests in all; `cargo xtask check`; `boot-matrix` at 1, 2 and 4 CPUs.
- **Limitation and not done:** the block layer holds its lock across polled I/O and is driven from the boot flow; a production one routes each request to the submitting processor's queue and blocks instead of polling. Also not done: readahead, a writeback thread on a timer, per-file or per-inode caching, and a filesystem above it.

### CPU feature discovery and FPU/SIMD state across context switches (CLAUDE.md §14, §24; 2026-11-08)

- **The correctness gap closed:** the scheduler saved only general-purpose registers in the interrupt frame, never the FPU/SSE/AVX state. Two threads both using SIMD would corrupt each other's registers across a preemption. The negative control shows this happens on almost two thirds of checks when the state is not restored.
- **What:**
  - `features` (library): decodes CPUID leaves 1, 7, 0Dh and 8000_0001h into a `Features` set, grades the x86-64 microarchitecture level (v1–v3, from the psABI supplement), gives the XCR0 to set and the XSAVE area size. Host-tested against a synthetic v3 CPU and degraded ones.
  - `cpu`: `gather_cpuid`; `enable_simd` (CR0.EM clear, MP set; CR4.OSFXSR, OSXMMEXCPT and, where XSAVE exists, OSXSAVE; XCR0 ← x87|SSE|AVX via XSETBV); `fpu_save`/`fpu_restore` (XSAVE/XRSTOR with the full mask, else FXSAVE/FXRSTOR) and `fpu_init_area` (a clean FCW and MXCSR). The kernel stays soft-float; this only adds a capability.
  - `sched`: each slot has a 64-byte-aligned FPU area, allocated at spawn and initialized clean; a switch saves the outgoing context's state and restores the incoming one's; the area is freed with the stack when the thread is reaped. Each processor's boot/idle context gets an area before the heap baseline is measured, so the leak check stays fair.
- **Proof** (every boot): the feature report with the x86-64 level, then three SIMD threads on one processor, each holding four values in spread-out XMM registers (0, 3, 7, 15), spin checking them for 300 ms across 100–307 context switches: 1.3–1.6 M checks, 0 corruptions.
- **QEMU note:** QEMU 11's TCG `-cpu max` presents x86-64 level 2 (not every v3 feature), so the baseline reads false under emulation; on the supported 2026 hardware it is v3+. The level is reported, not enforced at boot. XCR0 is 0x7 (x87, SSE, AVX) and the XSAVE area 2696 bytes.
- **Negative control** (failing the boot test): the incoming context's FPU state not restored on a switch → 812 443 of 1 318 469 checks saw another thread's XMM values → "scheduler failed".
- **Test evidence:** 4 `features` tests (a v3 CPU decoded and graded, levels 1 and 2, absent leaves, the XSAVE-size sanity check); 116 library tests; `cargo xtask check`; `boot-matrix` at 1, 2 and 4 CPUs.
- **Not done:** lazy FPU (deliberately — eager save/restore is used), the AVX upper halves tested separately from the full XSAVE image, AVX-512 (XCR0 leaves it off), and a per-CPU "FPU used" optimization to skip saving an untouched state.

### Panic diagnostics: a frame-pointer backtrace (CLAUDE.md §69; 2026-11-08)

- **What:**
  - `unwind` (library): `walk` follows the frame-pointer chain (each frame is `[RBP]` = the caller's RBP, `[RBP + 8]` = the return address), through a caller-supplied word accessor that returns `None` to stop, and halts at a null, unaligned or non-increasing RBP or one more than a bound above the last. Pure, host-tested.
  - `cpu::capture_backtrace`: reads the current RBP and walks it with an accessor bounded to a 256 KiB window above the starting frame, so a corrupt chain cannot fault.
  - `_start` is now a tiny naked shim that zeroes RBP, aligns the stack and calls `kernel_main`. So every backtrace (a panic's included) terminates cleanly at `kernel_main` instead of reading the loader's leftover frame.
  - The panic handler prints the kernel base and the backtrace addresses, which the KASLR base turns into image offsets offline.
- **Proof** (every boot): a three-deep `#[inline(never)]` call chain whose innermost frame captures a backtrace yields at least four return addresses, every one at or above the kernel image's virtual base.
- **A bug found and fixed on the way:** the first proof required every captured address to be in a `base .. base + 2 GiB` window, but that upper bound wrapped past the top of the address space for a KASLR base near `0xFFFFFFFF80000000`, making the window empty; and the walk stepped into `_start`'s frame and recorded the loader's leftover word (`0x1e093037`) as a return address. The naked `_start` fixed the second (the chain now ends at `kernel_main`), and the check became "at or above the kernel base".
- **Negative control** (failing the boot test): the walk's frame-step bound set to 0 so it cannot move between frames → only 1 frame → "backtrace failed".
- **Test evidence:** 5 `unwind` tests (a clean chain, the chain end, the capacity bound, a wild frame pointer, an unreadable word); 121 library tests; `cargo xtask check`; `boot-matrix` at 1, 2 and 4 CPUs.
- **Not done:** symbol names (addresses are printed for offline symbolization), DWARF `.eh_frame` unwinding for frame-pointer-less code, and a full crash dump (registers plus stack) to storage.

### In-memory virtual filesystem (CLAUDE.md §39; M8/M9 groundwork; 2026-11-08)

- **What:** `vfs` (library): a tree of directories and files in RAM, with the path handling and node operations the runtime and services will build on. Pure, no `unsafe`.
  - `components` parses an absolute, `/`-separated, UTF-8 path, skipping empty parts, with `.`/`..` and a per-component length limit.
  - `create`, `mkdir`, `mkdir_all`, `write` (grow and zero-fill a gap), `read` (offset and partial), `truncate`, `stat`, `list` (sorted), `remove` (a file or an empty directory; never the root). `resolve` follows `.` and `..` (the root's parent is itself). Distinct `FsError`s; a node and file-size limit; freed slots are reused and a failed create leaks nothing.
  - No on-disk format is decided here: an on-disk filesystem (its own format ADR) will implement the same operations later.
- **Proof** (every boot, on the heap): builds `/etc/origin/hostname`, writes and reads it back through a `..` path, lists the directory, stats it, removes everything, and checks the node count returns to the root alone (no leak).
- **Negative control** (failing the boot test): a stray node left after the removals → "removed all back to 2 node: vfs failed".
- **Test evidence:** 6 `vfs` tests, including path parsing at the length limit, offset/gap read-write, the error taxonomy, the node limit with slot reuse, and a 4000-operation deterministic workload kept consistent; 127 library tests; `cargo xtask check`; `boot-matrix` at 1, 2 and 4 CPUs.
- **Not done:** an on-disk filesystem and its format ADR, open-file handles and a current-directory, permissions and ownership, symbolic links, mmap, and wiring the VFS to system calls (ADR-0005) and the block cache.

### Software rasterizer (CLAUDE.md §50 Origin UI, §52 the compositor's software fallback; 2026-11-08)

- **What:** `raster` (library): a 2D rasterizer over a borrowed 32-bit `0xAARRGGBB` surface with a stride (a view into a larger framebuffer works). `clear`, `fill_rect`, `blend_rect` (straight-alpha "over"), `stroke_rect`, `blit`, with every operation clipped to the surface, so out-of-bounds coordinates are safe. `blend` rounds `(a·s + (255−a)·d + 127)/255` per channel. Pure, no `unsafe`.
- **Cross-check:** `demo_scene` draws a fixed scene (clear, three fills that run off the edges, a frame, a translucent band, a blitted checker sprite). The host test renders it to a 160×48 surface, checks it is deterministic, and proves its CRC-32 (`0x912df877`); the kernel renders the same scene on the heap and checks its CRC equals that constant, so the two renderers agree bit for bit.
- **Proof** (every boot): "raster: 160x48 scene rendered, CRC 0x912df877 (expected 0x912df877)". The value is identical on every machine (pure computation, no timing or KASLR), so the boot test matches it exactly.
- **Negative control** (failing the boot test): one fill's blue channel changed by 1 → CRC `0x53d95223` → "raster failed".
- **Test evidence:** 5 `raster` tests (fills and clipping at every edge, the blend arithmetic, a clipped blit, surface-construction validation, the deterministic demo and its CRC); 132 library tests; `cargo xtask check`; `boot-matrix` at 1, 2 and 4 CPUs.
- **Not done:** a text/font engine (CLAUDE.md §51), drawing to a real framebuffer (no GOP framebuffer in boot info yet), damage tracking and the compositor (§52), and hardware-accelerated rendering.

### PS/2 keyboard: scancode decoding and i8042 bring-up (CLAUDE.md §56; 2026-11-08)

- **What:**
  - `ps2` (library): scan-code set 1 decoding. A `Decoder` turns bytes into key events (make/break, the `0xE0` extended prefix handled across calls, the `0xE1` pause sequence ignored), a US QWERTY `to_char` with a `Modifiers` state machine (Shift, Ctrl, Alt, Caps Lock, with Caps Lock affecting only letters and Shift the symbols). The tables are authored from the published set-1 assignments, not copied.
  - `i8042` (binary): controller bring-up after `ExitBootServices` — disable both ports, flush, read and rewrite the config byte (clearing the IRQ and translation bits for now), the controller self-test (0xAA → 0x55), the keyboard-port test (0xAB → 0x00), then enable the keyboard port. Every status-register wait is bounded (a million polls), so a missing controller cannot hang the kernel.
- **Proof** (every boot): "i8042: config 0x77; self-test 0x55 (want 0x55), keyboard port test 0x00 (want 0x00)". QEMU's emulated controller answers deterministically.
- **Negative control** (failing the boot test): the self-test command byte changed from 0xAA to 0xAB, so the controller returns the port-test result 0x00 instead of 0x55 → "i8042 failed".
- **Test evidence:** 7 `ps2` tests (lower case with releases, Shift capitals and symbols, Caps Lock on letters only, Ctrl typing nothing, the extended keys, make/break and function keys, and typing the whole string "Origin OS!"); 139 library tests; `cargo xtask check`; `boot-matrix` at 1, 2 and 4 CPUs.
- **Not done:** live keystrokes need the keyboard IRQ (vector routing through the I/O APIC, not wired yet), the keyboard device reset and LED/typematic control, scancode set 2 without translation, USB HID keyboards, and other layouts.

### The HPET: discovery and a rate cross-check (CLAUDE.md §68; 2026-11-08)

- **What:**
  - `hpet` (library): parses the ACPI HPET table (the MMIO base from the GAS at offset 44, with the address-space and alignment checked) and the main-counter arithmetic (the period in femtoseconds from the capabilities register, period → frequency, a counter delta → nanoseconds, rate agreement). Pure, host-tested.
  - `time::prove_hpet`: finds the HPET through ACPI, maps its registers uncached, enables the main counter, and measures a counter delta against a TSC-timed ~20 ms window, requiring the HPET-measured and TSC-measured elapsed times to agree within 2 %.
- **Proof** (every boot): "HPET: at 0xfed00000, 100 MHz counter (period 10000000 fs); 20 ms elapsed by the HPET, 20 ms by the TSC". QEMU's HPET is a 100 MHz counter; the two clocks agree.
- **A boot-test lesson:** mapping the HPET first took the MMIO window's first slot (0xffffe00000000000), so the PCIe ECAM then mapped one slot higher and a marker that had hard-coded the ECAM's virtual address stopped matching. The virtual address is an allocation detail, so that marker now matches "uncached true, 2 MiB pages" without it. (The debug that found this — printing the first unmatched marker on a timeout — was removed.)
- **Negative control** (failing the boot test): the second counter read forced equal to the first (a frozen counter) → "0 ms elapsed by the HPET, 20 ms by the TSC: HPET failed".
- **Test evidence:** 4 `hpet` tests (a valid table, bad addresses and short tables, the counter arithmetic at 100 MHz and 14.318 MHz, rate agreement); 143 library tests; `cargo xtask check`; `boot-matrix` at 1, 2 and 4 CPUs.
- **Not done:** HPET comparators as timer interrupt sources (one-shot and periodic), using the HPET as the wall-clock reference, and the ACPI PM timer as a further fallback.

### Network packet layer (CLAUDE.md §54; 2026-11-08)

- **What:** `net` (library): header parsing and building for Ethernet II, IPv4 (no options), ICMP echo and ARP (IPv4 over Ethernet), and the Internet checksum (RFC 1071), including a streaming `checksum_parts` for a header plus payload without copying. Every parser is bounds-checked, requires a correct checksum where one applies, and never panics on a short or malformed buffer. Pure, no `unsafe`.
- **Self-validating proofs:** the checksum is anchored by its defining property — a block carrying its own checksum sums to zero — and by small hand-verifiable cases (one word 0x0001 → 0xFFFE, odd-byte padding), with the streaming form matching the contiguous one across an odd split. IPv4, ICMP and ARP round-trip (build → parse) with their checksums, and corrupting any checksummed byte makes parsing reject. No external vectors are needed.
- **Kernel proof** (every boot, on the heap): a 65-byte Ethernet/IPv4/ICMP echo request is built from host A to host B, B parses it and builds a reply, and A matches the reply to its request with the 23-byte payload intact and every checksum valid.
- **Negative control** (failing the boot test): the ICMP checksum left unwritten → the echo request fails its checksum on parse → "payload and checksums intact false: net failed".
- **Test evidence:** 5 `net` tests (the checksum and its streaming form, an IPv4 round trip with a corrupted-byte rejection and an options rejection, an ICMP echo build/parse/reply, ARP request/reply, and a full Ethernet/IPv4/ICMP loopback echo); 148 library tests; `cargo xtask check`; `boot-matrix` at 1, 2 and 4 CPUs.
- **Not done:** a NIC driver (so nothing is on a wire yet), TCP and UDP, IP fragmentation and options, routing, DHCP and DNS, and IPv6 (CLAUDE.md §54).

### TCP: segments and a minimal connection state machine (CLAUDE.md §54; 2026-11-08)

- **What:** `tcp` (library): TCP header parse/build with the IPv4 pseudo-header checksum, and a `Tcb` connection state machine (RFC 793 states). It drives the three-way handshake, one in-order data exchange and the FIN teardown, returning explicit outgoing segments (owning the send-sequence advance) and what to deliver. Pure, no `unsafe`.
- **Scope, stated honestly:** the connection core only — no retransmission, congestion control, out-of-order reassembly, window scaling or options beyond ignoring them on receive. It establishes, carries ordered data and closes.
- **Self-validating proofs:** a segment round-trips through build/parse with the pseudo-header checksum (which binds the IP addresses — parsing with the wrong addresses is rejected, as is a corrupted byte); `seq_len` counts SYN and FIN. `loopback_demo` connects two TCBs through an in-memory pipe: handshake to ESTABLISHED, A sends a payload B receives in full, A closes and B follows, ending with B CLOSED and A in teardown. No external vectors needed.
- **Kernel proof** (every boot, on the heap): "tcp: loopback connection established, 30-byte payload delivered, and closed: true".
- **Negative control** (failing the boot test): `connect` sends no SYN flag → the handshake never completes → "closed: false: tcp failed".
- **Test evidence:** 4 `tcp` tests (segment round trip with checksum and address binding, `seq_len`, `loopback_demo` over several payload sizes, and the full establish/data/close loopback); 152 library tests; `cargo xtask check`; `boot-matrix` at 1, 2 and 4 CPUs.
- **Not done:** retransmission and timers, flow and congestion control, reassembly, the socket API and port demultiplexing, and moving segments over a real NIC.

### UDP, and a monotonic timer queue (CLAUDE.md §54, §68; 2026-11-08)

- **UDP** (`net`): datagram parse/build with the IPv4 pseudo-header checksum and the length field, a zero stored checksum accepted (RFC 768), a computed zero sent as all ones. Self-validating: round-trip with address binding and corrupted-byte and bad-length rejections. The kernel's net proof now also round-trips a UDP datagram.
- **Timer queue** (`timers`): a deadline-ordered set of `(id, deadline)` timers — `arm` (re-arming moves a timer), `cancel`, `next_deadline`, and `expire(now)` which removes and returns the due ids in deadline order. Kept sorted (O(1) peek, O(n) arm/cancel; a heap is a later optimization). Pure, no `unsafe`.
  - Host tests include a 5 000-operation workload checked against a `BTreeMap` model after every step (length, next deadline and the exact fired set).
  - Kernel proof (every boot): three timers armed at 5/10/15 ms on the monotonic clock, plus one cancelled, fire in order [10, 20, 30] with the cancelled one never firing; worst lateness 0 µs (the poll loop reads the clock tightly).
- **Negative controls**, each failing the boot test: a UDP checksum left unwritten is rejected on parse (the net control already covers the ICMP path; UDP's is covered by host tests); `cancel` made a no-op so the cancelled timer fires → order [10, 20, 99, 30] → "timers failed".
- **Test evidence:** 6 `net` tests (UDP among them) and 4 `timers` tests; 157 library tests; `cargo xtask check`; `boot-matrix` at 1, 2 and 4 CPUs.
- **Not done:** wiring the timer queue under the scheduler's sleeps and socket timeouts (it stands alone so far), a binary-heap implementation, and UDP/TCP port demultiplexing to sockets.

### Integration: a `/sys` info tree in the VFS, and a verification pass (2026-11-08)

- **What:** a boot step builds a read-only `/sys`-style tree in the in-memory VFS from facts several subsystems discovered — `/sys/mem/free_mib` (the frame allocator), `/sys/cpu/tsc_mhz` (the clock), `/sys/bus/pci/count` (`pcie::device_count`) and `/sys/block/nvme0/size_mib` (`storage::geometry`) — then reads a value back and checks it parses to the discovered number. It shows the VFS carrying a live system namespace built from real hardware state, tying the memory, clock, PCIe, NVMe and VFS subsystems together.
- **Proof** (every boot): "sysfs: built /sys with 4 facts (4 subtrees): mem/free_mib=503, cpu/tsc_mhz=999, bus/pci/count=6, block/nvme0/size_mib=16; read back pci count Some(6)". The PCIe count (6) and disk size (16 MiB) are deterministic in QEMU; free MiB and the TSC MHz vary and are reported, not asserted.
- **Negative control** (failing the boot test): a wrong value stored for the PCIe count → the read-back (7) no longer matches the discovered count (6) → "sysfs failed".
- **Verification pass (2026-11-08):**
  - The whole workspace: 27 test suites, 396 tests, 0 failures (1 ignored).
  - An 8-processor boot (`ORIGIN_QEMU_SMP=8`): all 8 online, the scheduler on every one, the clock consistent across 8 (0 backwards), and 8 per-CPU NVMe queues doing I/O at once.
  - A 40x parser-mutation soak (`ORIGIN_MUTATION_SCALE=40`, release): ~800,000 parser cases and ~4,000,000 RTC cases over the firmware/device parsers, 0 failures, in about 7 s.

### I/O APIC interrupt routing (CLAUDE.md §24, §31; 2026-11-08)

- **What:**
  - `acpi`: MADT parsing extended to I/O APIC structures (type 1: id, MMIO base, GSI base) and interrupt source overrides (type 2), with `gsi_for_irq` resolving a legacy ISA IRQ to its global system interrupt and flags (an override, else identity).
  - `ioapic` (library): the redirection-table register indexes, the version register's max-entry field, MPS INTI flag decoding (polarity and trigger), and the redirection-entry bit layout (vector, fixed delivery, physical destination APIC id, polarity, trigger, mask). Host-tested.
  - `irq` (binary): finds the I/O APIC via the MADT, maps its registers uncached, programs a redirection entry through IOREGSEL/IOWIN (high dword first), and the 8254 PIT channel 0 as a 100 Hz rate generator.
- **Proof** (every boot): the I/O APIC is id 0 with 24 entries at 0xfec00000; the PIT's IRQ0, remapped to GSI 2 by a MADT override, is routed to vector 0x41 on the boot processor, and 20 periodic interrupts arrive in ~204 ms (polling with interrupt windows, never halting). The entry is then masked and the PIT stopped; nothing schedules on it yet.
- **Negative control** (failing the boot test): the redirection entry left masked → 0 interrupts in 2 s → "I/O APIC failed".
- **Test evidence:** 7 `acpi` tests (I/O APIC and overrides among them) and 3 `ioapic` tests; 163 library tests; `cargo xtask check`; `boot-matrix` at 1, 2 and 4 CPUs.
- **Not done:** routing the keyboard IRQ1 to a handler for live keystrokes (the i8042 bring-up is done; this is the remaining link), level-triggered PCI INTx routing, per-CPU interrupt affinity and balancing, and an EOI path for the I/O APIC's own remote-IRR on level interrupts.

## `framekernel/sync/` (`origin-sync`), kernel synchronization primitives

**RwLock (2026-11-08):** a reader-writer spin lock added beside `SpinLock`, for read-mostly kernel state: many readers or one writer, writers waiting for the readers to drain (writer starvation is accepted for rarely-written state). It does not disable interrupts, the same contract as `SpinLock`. Host stress test: 4 writers keep a protected pair equal while 8 readers (which overlap — a max-concurrency counter proves it) check 500,000 times that they never see a half-updated pair; 0 torn reads. Two guard types with documented `unsafe` `Deref`/`DerefMut`. Now backs the kernel's PCIe device list (`pcie::DEVICES`), written once at enumeration and read by driver binding. 6 `origin-sync` tests in all; `cargo xtask check`; boots with the NVMe driver finding its controller through the read lock.


- **What:** `SpinLock<T>` (Acquire on lock, Release on unlock; `SpinGuard` with `Deref`/`DerefMut`; `Debug` that never reads the value), moved out of the kernel binary on 2026-11-07 so that it can be tested. Interrupt rule: no handler may take a lock that interrupted code can hold; the kernel holds locks it shares with handlers with interrupts disabled.
- **Author / date:** Claude Code (local, M1 Max), 2026-11-07. Original code.
- **Dependencies:** `core` only. `no_std`; the no-panic lints are denied outside tests. One `unsafe impl Sync` and two `unsafe` blocks (the guard's `Deref`/`DerefMut`), each with a SAFETY argument.
- **Test evidence (2026-11-07):** 3 host tests with real threads: 8 × 20,000 non-atomic increments under the lock (exact total), a writer/reader pair that never sees a torn two-field update, re-locking after a drop. Clippy on the host, UEFI and kernel targets; the no-panic object check references 0 external symbols.
- **Not done:** Loom models (the `loom` crate needs the owner's approval to download), a fairness (ticket or MCS) lock, interrupt-disabling guards as a type.
- **Licensing:** project licence not chosen yet.

## `origin-version.toml` and `runtime/version/` (`origin-version`), T5

- **What:**
  - `origin-version.toml` is the canonical release identity (CLAUDE.md §62–§63) and the only place a version is written by hand. It holds:
    - the release version, SemVer 2.0 without build metadata (ONBF §6.2);
    - a monotonic `build` number;
    - the anti-rollback `security-version` (ONBF §6.5, §7.6);
    - the ONBF container version, `1.0` (ONBF §4.2).
  - These numbers are independent of each other (ONBF-R8). No other ABI identifiers exist yet; each is added when its interface first exists.
  - `origin-version` is a `no_std`, `forbid(unsafe_code)` crate. Its build script parses the file with a strict TOML-subset parser (`manifest.rs`) and generates the constants: `VERSION`, its parts, `BUILD`, `SECURITY_VERSION`, `ONBF_FORMAT_MAJOR` and `ONBF_FORMAT_MINOR`. `PRODUCT` is a plain constant.
- **Author / date:** Claude Code (local, M1 Max), 2026-11-07. Original code. The SemVer grammar follows semver.org 2.0.0.
- **Architecture decisions:**
  - The generated output depends only on `origin-version.toml`: no timestamps, host paths or git state (ONBF-R9, CLAUDE.md §75).
  - Strict parsing: unknown sections or keys, duplicates, missing keys, escapes, leading zeros and trailing text are all errors. One file has one reading (ONBF-R6).
  - The starting values (`0.1.0-dev`, build 0, security version 0) are a development default. The owner may change them.
- **Dependencies:** `core` in the library; `std` in the build script (host only).
- **Test evidence (2026-11-07):**
  - 7 unit tests, covering 33 cases. One of them checks that the generated constants equal a fresh parse of the repository file.
  - Negative checks by hand, with the files restored and compared with `cmp` afterwards:
    - changing the version to `0.2.0-rc.1` reaches the rebuilt `.efi`, which proves the build script's `rerun-if-changed` works;
    - `version = "0.2"` fails the build with `origin-version.toml:16: … expected MAJOR.MINOR.PATCH`;
    - a hand-written `version` in a crate manifest fails `xtask check`, naming the file and line.
- **Licensing:** project licence not chosen yet.

## `runtime/onbf/format/` (`onbf-format`), the ONBF parser: container, OCBOR-1, KRN manifest

- **What:**
  - **Container layer** (T9 part 1). It validates:
    - the header (§4.2), the section table (§4.3) and the kind/profile table (§4.4);
    - the file layout (§4.1): canonical section offsets, zero padding, and the signature block at EOF;
    - the Merkle cross-references (sizes and exactly one referrer).

    It exposes the §4.5 content-digest input, a digest check that takes a caller-supplied SHA-512, and `Section::verify_sha256` for SHA-256-protected sections. `Image::parse` never interprets section content (ONBF-R1).
  - **OCBOR-1 (§5), `ocbor` module.** `validate` checks a whole document in one pass with an explicit 16-frame stack and no recursion (ONBF-R10). It enforces:
    - allowed types only;
    - shortest integers and lengths;
    - unsigned, strictly ascending map keys;
    - depth, item, text and size limits;
    - strict UTF-8;
    - no trailing data.

    `Cursor` reads a validated document item by item.
  - **KRN manifest** (`KrnManifest`), from `docs/spec/ONBF-manifest-keys.md` (K1–K4) and errata R4–R5:
    - all seven keys are required;
    - unknown keys below 1000 are rejected, and extensions are skipped, including in nested maps;
    - field types, lengths and ranges are checked;
    - reserved feature bits and IBT are rejected;
    - `check_against(&Image)` confirms the profile is KRN and that `entry` lies in the content of an RX segment.
  - **Not yet included:** the signature block's contents (§7.1), Merkle verification of sections (crate `onbf-merkle`), and the other profiles' manifests.
- **Author / date:** Claude Code (local, M1 Max), 2026-11-07. Original code written from `docs/spec/ONBF.md` v1.0.
- **Architecture decisions:**
  - ADR-0001.
  - The ONBF §12 parser contract: `no_std`, no allocation, `forbid(unsafe_code)`, typed errors, no partial results. In non-test builds `clippy::{panic, unwrap_used, expect_used, indexing_slicing, arithmetic_side_effects, unreachable, todo, unimplemented, cast_possible_truncation}` are denied. A planted index and a planted unchecked addition were rejected, which confirms the lints are active.
  - Where §4 allowed more than one reading, the parser implements the owner's decisions C1–C14 in `docs/spec/ONBF-errata.md` (2026-11-07):
    - 12 keep the reading first implemented;
    - C12 makes manifests uncompressed only;
    - C14 allows empty sections only for SEGMENT;
    - C13 rejects every unknown kind. That was the owner's choice, against the recommendation to skip optional ones.
  - The owner's G4 and G5 decisions are enforced here:
    - segments are page-sized, in ascending memory order, disjoint, and end within the 2 GiB span, with at most 16 in EXE/LIB;
    - REQUIRED is set on every section.

    G2 and G3, the Merkle level numbering, bind `onbf-merkle`. MERKLE section sizes already follow G3.
  - A compile-time assertion ties the accepted `format_major` to `origin-version.toml`.
- **Dependencies:** `core`, and the first-party `origin-version` crate.
- **Toolchain:** Rust 1.99.1. Clippy-clean on the host, `x86_64-unknown-uefi` and `x86_64-unknown-none`.
- **Test evidence (2026-11-07, after the C1–C14 and G2–G5 decisions):** 17 tests.
  - Valid images: a KRN image with Merkle and bss segments; a minimal image in every profile; unsigned images in developer mode and nested; `format_minor` > 0; the digest plumbing; 16 segments in EXE and LIB; 17 in KRN; a guard gap between segments.
  - Rejected by design: unknown kinds with or without REQUIRED (C13); empty sections other than bss segments (C14).
  - **Malicious corpus, 81 cases** (ONBF §13 item 4, container part), each rejected with the exact expected error. It covers:
    - overlapping, misaligned and misplaced sections;
    - counts beyond limits, integer overflow, truncation and trailing data;
    - W+X and reserved fields;
    - Merkle mis-references and padding;
    - compressed manifests and empty sections;
    - REQUIRED not set (G5);
    - segment size, order, overlap and span, and 17 segments in an EXE (G4).
  - Sweeps:
    - every truncation of a 20,648-byte image is rejected;
    - every one of 10,732 padding bytes is checked;
    - all 5,120 single-bit flips of the header and table are rejected, except in fields with other valid values, all of which are digest-covered (errata, "Observed by the bit-flip sweep").
  - Since the SHA-2 work, content digests in the tests are real SHA-512 from `origin-crypto`, a dev-dependency only.
  - **OCBOR-1 and KRN manifest** (15 more tests, 32 in all, in `tests_ocbor.rs` plus two in `tests.rs`):
    - **RFC 8949 Appendix A:** 34 examples OCBOR-1 accepts decode to the right items. 13 that the profile excludes (floats, tags including a bignum, `undefined`, simple values, indefinite lengths, text keys) are rejected for the right reason.
    - **§13 item 4 manifest corpus:**
      - 10 non-shortest encodings;
      - 15 malformed items (reserved additional information, a stray break, truncations, trailing data, an overlong NUL, a surrogate);
      - 6 key-order cases (descending, duplicate, negative, bytes and bool keys, a nested map);
      - limits at their exact boundaries: depth 16 and 17, 65,536 and 65,537 items, 64 KiB and 64 KiB + 1 of text, 1 MiB + 1 of document.
    - **KRN schema:** every required key missing; unknown keys 6, 23, 25 and 999; extensions skipped at the top level and in nested maps; 20 invalid-field cases.
    - **Loader order:** parse, content digest, manifest SHA-256, decode, entry check, end to end with real SHA-256 and SHA-512. A tampered manifest fails its digest before decoding. The entry check is tested at 7 boundaries, plus a KRN manifest inside an EXE image.
    - **Canonical-encoding property (ONBF-R6):** all 1,208 single-bit flips of a KRN manifest were tried. Every flip the decoder accepts re-encodes to exactly the flipped bytes, so no accepted document has a second reading. Every truncation is rejected.
  - **No panic paths:** the release object for `x86_64-unknown-none` references only `memset` and `core::str::from_utf8` (`xtask check`, no-panic step). The latter was added to the allowlist with a justification: it is total and RFC 3629-strict, and under `forbid(unsafe_code)` it is the only way to turn bytes into `&str`.
  - Not done yet: fuzzing (§13 item 3), Kani bounds (item 5) and the N-version check (item 6).
- **Licensing:** project licence not chosen yet.

### Signature-block parser (ONBF §7.1–§7.2, encodings S1–S4), added 2026-11-07

- **What:** `signature::SignatureBlock::parse`: "ONBFSIG1", version 1, 1–8 length-prefixed entries; each entry a canonical OCBOR-1 map with exactly keys 1–7 (key 8, the transparency-log proof, is refused in v1); role 1–5 (S1) with its FIPS 204 context string from a table; suite 1–2 (S2), fixed by the role (kernel and release need OSS-1R); 32-byte key ID; the certificate array kept raw for the chain verifier; the statement as a map with exactly keys 0–6 (S3), whose exact encoded bytes are returned for the signatures; 64-byte Ed25519 and suite-length ML-DSA signatures; the statement's role must equal the entry's; no (role, key) twice. Verification (digest, both signatures, chain) is not wired yet.
- **Author / date:** Claude Code (local, M1 Max), 2026-11-07, right after the owner decided S1–S4. Original code.
- **Test evidence:** 6 tests: a two-entry block with the exact statement bytes; framing (magic, version, counts 0 and 9, truncation, trailing data, every strict prefix); each entry rule; each statement rule; duplicates and a non-minimal integer head; every single-bit flip of a valid block is refused or reads a different entry, and nothing panics. The no-panic object check still passes (4 allowed external symbols).

## `runtime/onbf/verify/` (`onbf-verify`), signature-entry verification

- **What:** `verify_entry(entry, key, content_digest, profile)`, in order: the key ID is SHA-256 of the key's canonical encoding (the OCBOR-1 `keys` map, S4); the ML-DSA key length fits the suite; the statement's digest and profile match the image; Ed25519 (strict) over the exact statement bytes; ML-DSA-65 or -87 (strict) over the same bytes with the role's context string. Trust decisions (which keys, certificate chains) are left to the trust store (B7).
- **Author / date:** Claude Code (local, M1 Max), 2026-11-07. Original code. `no_std`, no allocation, `forbid(unsafe_code)`, no-panic lints; depends only on `onbf-format` and `origin-crypto`.
- **Test evidence:** `tools/make_fixture.py` builds a Kernel-role (OSS-1R) block signed by the independent pure-Python references (Ed25519 RFC 8032 and ML-DSA-87 FIPS 204, deterministic keys; byte-identical on every run, SHA-256 `aa9e76c0…2e72`). 3 tests: the block verifies under the Rust verifiers (two implementations agreeing end to end); a wrong digest, profile or key and a flipped byte in either signature fail with their own errors; every single-bit flip inside the statement fails. The no-panic object check passes (6 allowed external symbols).
- **Not done:** wiring into the loader (B2 step 3), the trust store and OKC chain verification, the host signer, vectors from an independent author.
- **Licensing:** project licence not chosen yet.

## `runtime/onbf/merkle/` (`onbf-merkle`), Merkle-SHA256-4K

- **What:** the ONBF §4.6 trees, with the owner's G2 and G3 decisions and readings R1–R3 (`docs/spec/ONBF-errata.md`):
  - `Shape`: leaves, levels and MERKLE-section positions, highest level first;
  - `root`: one streaming pass, for whole-section verification such as the boot loader checking KRN segments;
  - `build`: writes the MERKLE section, for `onbf-pack`;
  - `verify_block`: checks one 4 KiB block by walking one hash block per level, stopping at the first hash block a caller-supplied `TrustCache` already trusts. This is the ONBF-R2 page-fault path.
- **Design:**
  - Child hashes stream into one SHA-256 state per level (at most 4 levels for 1 TiB), so there are **no 4 KiB buffers**: the stack use is a few hundred bytes, which suits kernel stacks.
  - Iterative, with no recursion (ONBF-R10).
  - Hash blocks are marked trusted only after the walk reaches the root or a trusted ancestor.
- **Author / date:** Claude Code (local, M1 Max), 2026-11-07. Original code written from ONBF §4.6. The design approach is credited in the spec to Fuchsia blobfs and fs-verity; no code from either.
- **Architecture decisions:** the ONBF §12 contract (`no_std`, no alloc, `forbid(unsafe_code)`, no-panic lints) and ADR-0001.
- **Dependencies:**
  - `core` and the first-party `origin-crypto` (SHA-256).
  - The first-party `onbf-format` as a dev-dependency, to cross-check MERKLE section sizes.
- **Toolchain:** Rust 1.99.1. Clippy-clean on the host, `x86_64-unknown-uefi` and `x86_64-unknown-none`. `[profile.dev.package.*] opt-level = 3` for `origin-crypto` and `onbf-merkle`, so that the tests (about 200 MB hashed) take 1.1 s instead of 16.7 s.
- **Test evidence (2026-11-07):** 12 tests, plus 1 ignored benchmark.
  - **Independent reference:** `tools/merkle_reference.py` implements §4.6 differently (whole levels as lists, materialized blocks) over Python `hashlib` (OpenSSL). For 17 sizes up to 64 MiB + 1 (3 levels), `root`, `build` and `Shape` agree with it on every root and on the SHA-256 of every MERKLE section's bytes.
  - **Shape:** equals `onbf_format::merkle_hash_blocks` for 606 sizes up to 1 TiB, which has 4 levels. Larger content is rejected.
  - **Every block verifies:** for 6 sizes across 1–2 levels, and for all 16,385 blocks of a 3-level tree with a warm cache, which ends up trusting all 132 hash blocks exactly.
  - **Attacks (§13 item 4, "Merkle block swap"):** each fails with the exact expected error.
    - Tampered data at 0, 1 and 2 levels.
    - Swapped data blocks (each leaf is bound to its index).
    - Swapped hash blocks.
    - Tampered hash-block padding, an upper-level slot and an unused slot.
    - A wrong root, or the same bytes under a different `content_size`.
    - A bad index, a bad block length and a bad section length.
  - **Cache:** after the first verification, a sibling block stops after one lookup. A tampered block fails against a warm cache and adds nothing to it.
  - **The root binds `content_size`:** a trailing zero byte inside the last block changes the root.
  - **No panic paths:** the release object for `x86_64-unknown-none` references only `memcpy`, `memset` and `origin_crypto`'s `Sha256::update`/`finalize`, which are themselves checked. A planted latent `split_at` passed Clippy and the tests, and was caught only by the no-panic step (`core::panicking::panic_fmt`).
  - **Throughput on the M1 Max host, portable SHA-256, release:** `root` 279 MiB/s, against 285 MiB/s for plain SHA-256. That is about 2% tree overhead, as expected from 1 extra hash per 128 blocks.
  - Not done: a second Merkle implementation by a different author (§13 item 6), integration of `verify_block` with `onbf-format` sections, and Kani bounds.
- **Licensing:** project licence not chosen yet.

## `security/crypto/` (`origin-crypto`), SHA-2, SHA-3, SHAKE, Ed25519 and ML-DSA

- **What:** streaming and one-shot SHA-256 and SHA-512 (FIPS 180-4 §6.2, §6.4), with the §5.1 padding shared through a generic block buffer. The schedule is a 16-word ring.
  - These are the hashes ONBF needs: SHA-512 for content digests, SHA-256 for section digests and Merkle trees.
  - Portable Rust only. SHA-NI with CPUID detection is planned (ONBF §4.6, CLAUDE.md §24).
- **Author / date:** Claude Code (local, M1 Max), 2026-11-07. Original code written from FIPS 180-4.
- **Constants:**
  - Not typed from memory. They were generated by `security/crypto/tools/derive_sha2_constants.py` from their definition: the fractional bits of the square and cube roots of the first 80 primes, computed with exact integer roots.
  - The script asserts that each 32-bit table is the high half of the 64-bit one.
  - The tests re-derive the SHA-256 tables independently in Rust (`u128` integer roots).
- **Architecture decisions:**
  - The ONBF §12 contract: `no_std`, no allocation, `forbid(unsafe_code)`. The same no-panic Clippy lints as `onbf-format` are denied in non-test builds.
  - Verification only, with no signing (ADR-0001 D3).
  - `Debug` prints no internal state (CLAUDE.md §61, secret-safe logging).
- **Side-channel assessment (CLAUDE.md §59):**
  - No data-dependent branches or memory indices. Round constants are indexed by round number, and branches depend only on public lengths.
  - In Origin's verification paths SHA-2 hashes public data.
  - **Not done:** zeroizing the state on drop, which needs volatile writes and therefore a reviewed `unsafe`. Add it before any secret is hashed (for example HMAC keys or password hashing, §57).
- **Dependencies:** `core` only.
- **Toolchain:** Rust 1.99.1. Clippy-clean on the host, `x86_64-unknown-uefi` and `x86_64-unknown-none`.
- **Test evidence (2026-11-07):** 8 tests, plus 1 ignored benchmark.
  - **Known-answer tests:** the FIPS 180-4 example messages, five per algorithm (empty, "abc", 448-bit, 896-bit, one million "a"). Each expected digest was confirmed beforehand against Python `hashlib` (OpenSSL) and macOS `shasum` (Perl Digest::SHA).
  - **Differential tests (ONBF §13 item 2):** 310 lengths (0–300 plus 511–1,000,003) in `tests/vectors/sha2-differential.txt`. The file was generated by `tools/gen_sha2_vectors.py`, which writes a line only where `hashlib` and `shasum` agree.
  - **Streaming:** every two-piece split, and 1-, 7-, 64- and 129-byte pieces, of 15 boundary lengths match the one-shot digest.
  - **Other:** clone forks the stream, and `Debug` hides the state.
  - **No panic paths:** the release object for `x86_64-unknown-none` references only `memcpy`, `memset` and `Formatter::write_str`.
    - The first version referenced `copy_from_slice`'s length-mismatch panic, which no lint flags. It was replaced with a non-panicking copy.
    - Putting it back makes `xtask check` fail and name the symbol.
  - **Throughput on the M1 Max host (aarch64, portable code, release, single thread):** SHA-256 285 MiB/s, SHA-512 462 MiB/s. This is indicative only: the target is x86-64, and SHA-NI is not used yet.
  - Not done: NIST CAVP/ACVP vector files (downloading them needs the owner's approval), SHA-NI, and an independent review.
- **Licensing:** project licence not chosen yet.

### SHA-3 and SHAKE (FIPS 202)

- **What:**
  - Streaming SHA3-256 and SHA3-512.
  - SHAKE128 and SHAKE256, with `finalize_xof() -> ShakeReader`, whose `squeeze` can be called repeatedly.
  - Keccak-f[1600] on `[u64; 25]`.
  - The sponge reuses the SHA-2 block buffer, which gains the FIPS 202 byte-oriented padding: suffix 0x06 or 0x1F, then the last byte `|= 0x80`.

  ML-DSA (FIPS 204), half of every ONBF signature suite, is built on SHAKE (ONBF §7.2).
- **Author / date:** Claude Code agent (local, M1 Max), 2026-11-07, in its own worktree, merged by the coordinator. Original code from FIPS 202.
- **Constants:**
  - None are typed in. Const fns compute the round constants (Algorithm 5 LFSR, placed per Algorithm 6), the ρ offsets (the §3.2.2 walk) and the π mapping (Algorithm 3).
  - The tests re-derive all three another way:
    - the round constants as x^t mod x^8+x^6+x^5+x^4+1 over GF(2);
    - the ρ positions from the matrix power [[0,1],[2,3]]^t mod 5;
    - π in the forward form.
- **Side channels:** the permutation uses fixed lane positions only (ρ compiles to immediate rotates, π to no table loads). Branches depend only on public lengths. State is not zeroized on drop yet.
- **Test evidence (2026-11-07):** 11 tests, plus 1 ignored benchmark.
  - **Known answers:** "" and "abc", and SHAKE at 32, 64 and 1000 bytes.
  - **Differential tests:** 304 lengths, written only where hashlib's OpenSSL 3.5.7 and HACL* backends agree.
  - **Streaming:** splits around every rate, and SHAKE squeezed in pieces.
  - **Mutation check:** five planted faults, one at a time; all were caught.
  - **No panic paths:** the kernel-target object references only `memcpy`, `memset` and `Formatter::write_str`.
  - **Throughput:** SHA3-256 581 MiB/s and SHAKE256 about 580 MiB/s on the M1 host (aarch64, portable code). Indicative only.
  - Not done: NIST CAVP/ACVP files, and an independent review.

### Ed25519 verification (RFC 8032 §5.1, pure variant; ONBF §7.1), added 2026-11-07

- **What:** `ed25519_verify(public_key, message, signature) -> Result<(), Ed25519Error>`, verification only, strict. In order: A canonical (y < p, a root exists, not x = 0 with the sign bit set) and not small order; the same for R; S < L; then the cofactorless [S]B = R + [k]A, compared projectively. Mixed-order points are accepted. Each failure has its own error variant.
- **Author / date:** Claude Code agent (local, M1 Max), 2026-11-07, in its own worktree, merged by the coordinator (a078db0). Original code from the RFC 8032 math: GF(2^255−19) in 5×51-bit limbs with `u128` products; extended twisted Edwards coordinates with the §5.1.4 complete formulas; §5.1.3 square roots by a (p−5)/8 addition chain; bit-serial reduction mod L; a Straus–Shamir double-scalar loop. SHA-512 is the crate's own.
- **Constants:** not typed from memory. `tools/ed25519_reference.py constants` generates d, 2d, sqrt(−1), B, 4p and L from their definitions, asserting p and L probably prime, d a non-square, and B on the curve with [L]B = identity. `src/tests_ed25519.rs` re-derives every value in Rust.
- **Side channels:** variable time by design: verification handles only public data (ONBF §7.2). There are no secrets to zeroize.
- **Test oracle:** `tools/ed25519_reference.py`, an independent pure-Python RFC 8032 implementation (affine big-integer arithmetic, Euler's-criterion square roots, hashlib SHA-512). It shares no formulas with the Rust code, but has the same author.
- **Test evidence (2026-11-07):** 13 tests, plus 1 ignored benchmark.
  - **Known answers:** RFC 8032 §7.1 TEST 1, 2, 3 and SHA(abc), each embedded only after the reference reproduced its public key and signature from the seed and message.
  - **Differential:** 300 reference signatures (`tests/vectors/ed25519-differential.txt`).
  - **Negative corpus:** `tests/vectors/ed25519-negative.txt`, 1,608 single-bit flips and 157 constructed cases (S + L, S = L, non-canonical y as A and R, the 8 small-order points, check-order cases, a mixed-order key), each with the exact expected error.
  - **Fault injection:** six deliberate faults, each caught.
  - **No panic paths:** the `x86_64-unknown-none` release object references only `memcpy`, `memset` and `Formatter::write_str`.
  - **Throughput:** 16,625 verifications/s on the M1 Max host (aarch64, release, single thread). Indicative only; the target is x86-64.
  - **Not done:** Wycheproof vectors (downloading them needs the owner's approval); an N-version check by a different author (ONBF §13 item 6); Kani and fuzzing; an x86-64 benchmark; independent review.

### ML-DSA verification (FIPS 204), added 2026-11-07

- **What:** strict `mldsa65_verify` and `mldsa87_verify` (FIPS 204 Algorithm 3, pure ML-DSA with a context string, and Algorithm 8) for the ONBF suites OSS-1 and OSS-1R (ONBF §7.1–§7.2), with `MlDsaError` and the Table 2 lengths. HashML-DSA and ML-DSA-44 are not provided.
  - Strict semantics, first failure wins: `ContextTooLong` (|ctx| > 255); HintBitUnpack (Algorithm 21) in its own order, `HintCountOutOfRange` and `HintIndexNotIncreasing` per polynomial, then `HintPaddingNonZero`; `ZOutOfRange` (‖z‖∞ ≥ γ1 − β); `ChallengeMismatch`. SampleInBall is seeded with all λ/4 bytes of c̃ (the final standard); the ExpandA nonce is ρ ‖ column ‖ row; rejection loops are unbounded, as written (errata M1, pending).
  - Â is never materialized: each entry is regenerated by RejNTTPoly and multiplied into its row as it is sampled, and each row of w′1 is fed straight into the SHAKE256 state for c̃′. Peak stack in release: ML-DSA-87 about 15.4 KiB measured on the host and 15,064 bytes from the x86-64 kernel object's frames; ML-DSA-65 about 12.7 KiB and 12,952 bytes (budget 24 KiB; the kernel stack is 64 KiB).
  - Arithmetic mod q is canonical throughout, with Barrett reduction by ⌊2^64/q⌋ (the bound is argued in `field.rs`).
- **Author / date:** Claude Code agent (local, M1 Max), 2026-11-07, in its own worktree, merged by the coordinator (bbdc717). Original code from FIPS 204 (August 2024).
- **Constants:** only the Table 1 parameters are typed in. The zeta table (ζ^BitRev8(m), ζ = 1753) and 256^−1 are computed at compile time; the tests re-derive them and check ζ^256 = −1 and ζ^512 = 1. Const assertions tie the Table 2 lengths and the encodings to each parameter set.
- **Side channels:** variable time by design: verification handles only public data (ONBF §7.2), and nothing secret is held.
- **Oracle:** `tools/mldsa_reference.py`, an independent pure-Python FIPS 204 (KeyGen, deterministic and hedged Sign, Verify) using only hashlib's SHAKE, with a recursive CRT NTT checked against the evaluation definition, big-integer packing and a materialized Â. As a development check only, its `crosscheck` command matched the locally installed OpenSSL 4.1.0 (already on the machine; nothing was downloaded) byte for byte: 26 keys from their seeds and 104 deterministic and fixed-entropy signatures, all of which OpenSSL verified, and it rejected 12 mutations. Nothing in the build or the tests depends on OpenSSL.
- **Test evidence (2026-11-07):** 19 tests, plus 2 ignored (a calibrated stack measurement and a benchmark).
  - **Differential:** 104 signatures (52 per set; messages of 0–4,096 bytes; contexts of 0–255 bytes, including the five ONBF role contexts; deterministic and hedged), regenerated byte-identically.
  - **Negative corpus:** 2,631 cases with exact expected errors: every bit of c̃ and of the hint encoding, z bits, malformed hints of every kind and the order of their checks, the z bound at γ1 − β − 1 and γ1 − β, contexts (256 bytes included), messages, and keys of the other parameter set.
  - **Properties and internals:** 1,040 random mutations never verify; Decompose and UseHint exhaustively over Z_q; the NTT against its definition, its inverse and schoolbook negacyclic multiplication; Barrett against `%`; SampleInBall; the streamed matrix entries against RejNTTPoly.
  - **Mutation check:** 13 planted faults, one at a time; 12 caught, and the 13th (accepting ω + 1 hints) cannot change a verdict because the position-slice bound returns the same error.
  - **No panic paths:** the kernel-target object references only `memcpy`, `memset` and `Formatter::write_str`.
  - **Throughput:** ML-DSA-65 about 14,000 and ML-DSA-87 about 8,400 verifications/s on the M1 host (aarch64, portable code). Indicative only.
  - **Not done:** NIST ACVP vectors and the FIPS 204 errata sheet (downloading needs the owner's approval), an AVX2 path, an independent review. The oracle has the same author as the code.

## `developer-tools/xtask/` (`xtask`)

- **What:** host-side build orchestration, with four commands:
  - `check`:
    - rustfmt, clippy and tests per crate and target. The freestanding libraries (`onbf-format`, `onbf-merkle`, `origin-crypto`) are checked on the host, `x86_64-unknown-uefi` and `x86_64-unknown-none`;
    - the UEFI release build;
    - a **no-panic step**: each freestanding library is rebuilt for the kernel target in release mode. Its object may reference only:
      - `memcpy`, `memmove`, `memset` or `memcmp`;
      - the total `core` functions `Formatter::write_str` and `str::from_utf8`;
      - symbols *defined by another freestanding library's object*, since those pass the same check;
    - two lints:
      - CLAUDE.md §11: no `target_os = "linux"` in any Rust file outside `developer-tools/`;
      - CLAUDE.md §62: no `version` in the `[package]` or `[workspace.package]` section of any Cargo manifest.
  - `build`.
  - `boot-test`: stages a fresh ESP (`\EFI\BOOT\BOOTX64.EFI`, UEFI 2.11 §3.5.1.1) and a fresh NVRAM copy; boots QEMU with serial on a pipe; requires the loader's lines and OVMF's status line in order; then stops QEMU.
    - *Since M1 step 2:* builds the Framekernel, packs it with `onbf-pack krn` into `\EFI\ORIGIN\KERNEL.ONBF`, and runs two scenarios: the real kernel (every loader and kernel milestone line in order; any failure marker, matched in complete lines only, fails at once) and a tampered kernel (one SEGMENT byte flipped: the loader must reject it and the kernel must never run). `ORIGIN_QEMU_DEBUG=1` adds QEMU's interrupt log.
    - `check` also covers the Framekernel (host and kernel-target clippy, tests, release build), the developer-mode loader, and `onbf-pack` (clippy and tests).
  - `run`.

  It is not part of the shipping OS.
- **Author / date:** Claude Code (local, M1 Max), 2026-11-07. Original code.
- **Dependencies:** Rust `std` (CLAUDE.md §4) and the first-party `origin-version` crate; no third-party crates. `#![forbid(unsafe_code)]`. At run time it needs QEMU and the EDK2 firmware, found next to `qemu-system-x86_64` or through `ORIGIN_OVMF_CODE` and `ORIGIN_OVMF_VARS`.
- **Test evidence (2026-11-07):**
  - 3 unit tests: escape-sequence stripping and in-order matching.
  - Failure paths checked by hand:
    - a timeout below the boot time fails with exit 1 and QEMU is killed;
    - missing firmware is reported by path;
    - an unknown command exits 2.
- **Licensing:** project licence not chosen yet.

## `developer-tools/onbf-pack/` (`onbf-pack`), ELF64 PIE → KRN

- **What:** the host tool that converts the Framekernel's `rust-lld` output into an unsigned (developer-mode) KRN image (ONBF §6.1.2, §6.5; D1):
  - `elf`: an ELF64 x86-64 `ET_DYN` parser for the conversion contract. Every field is decoded from bytes, and each hard error has its own typed error. ELFCLASS32, EM_386 and x32 are rejected explicitly (ONBF-R11). PT_GNU_EH_FRAME is accepted and ignored (coordinator amendment, 2026-11-07);
  - `relr`: the SHT_RELR encoder (greedy, shortest) and decoder;
  - `ocbor`: a canonical OCBOR-1 encoder (shortest heads, `BTreeMap` keys);
  - `krn`: MANIFEST (with `boot_info_abi` from `origin-version`), one SEGMENT per PT_LOAD with RELA addends written in, RELOCS, MERKLE; the canonical container (C1–C4, C7, C10, C12, C14, G4, G5); and `verify`, which re-checks the finished image with `onbf-format` and `onbf-merkle` before anything is written;
  - CLI `onbf-pack krn`: exit 0, 1 or 2; output is written through a temporary file and a rename.
- **Author / date:** Claude Code agent (local, M1 Max), 2026-11-07, in its own worktree, merged by the coordinator (91334ee). Original code, written from the ELF gABI (ELF header, program headers, dynamic section, `Elf64_Rela`, SHT_RELR), the x86-64 psABI (`EM_X86_64`, `R_X86_64_RELATIVE`), RFC 8949 §4.2.1, ONBF §3–§6 and the errata.
- **Architecture decisions:** ADR-0001 D1; errata C1–C14, G2–G5 and R1–R5; manifest keys K1–K4. Its readings of the conversion contract (program-header policy, segment order by page-rounded end, `DT_INIT`/`DT_FINI` rejected, relocation-tag completeness, wrapping addends, `flags_digest` over the exact `--flags` bytes, and others) are listed in its report and await the owner in the errata.
- **Dependencies:** Rust `std` and the first-party `onbf-format`, `onbf-merkle`, `origin-crypto` and `origin-version`; no third-party crates. `#![forbid(unsafe_code)]`. The library denies the no-panic Clippy lints in non-test builds. At run time it uses `rustc -vV`, unless all three toolchain values are given.
- **Toolchain:** Rust 1.99.1; clippy-clean on the host.
- **Test evidence (2026-11-07):** 57 unit tests and 4 integration tests, now part of `cargo xtask check`.
  - **Negative tests:** each hard error of the contract is rejected with its own error; every strict prefix of a valid ELF is rejected as truncated.
  - **Valid conversions:** with and without relocations, RELA, RELR and both merged, a bss-only segment, one- and two-level MERKLE sections, and a fixture of the Framekernel's shape, each checked independently with `onbf-format`, `onbf-merkle` and the RELR decoder.
  - **RELR:** round trips over 2000 generated sets, and a brute-force check that the encoding is minimal on 3000 small sets.
  - **Integrity and determinism:** flipping any byte of a packed image makes `verify` fail; packing twice, in one process or two, gives identical bytes.
  - **Integration:** a `no_std` program linked with the Framekernel's flags by the pinned rustc and rust-lld 23.1.1, packed with DT_RELA and with `-zpack-relative-relocs`, and relocated at two bases by a simulated loader.
  - **Real kernel:** `cargo xtask boot-test` packs the real Framekernel ELF (7 sections, 3 segments, 128 relocations at the time of writing), which the loader verifies and boots.
  - **Not done:** fuzzing, Kani bounds, an independent second implementation (§13), signatures, EXE/LIB profiles, UNWIND/DEBUG_LINK sections.
- **Review:** not yet independently reviewed.
- **Licensing:** project licence not chosen yet.
