# Session handoff — 2026-11-07: local Claude Code session on the M1 Max

| | |
|---|---|
| From | Claude Code, local, in `~/work/origin-os` (follows `2026-11-07-handoff.md`) |
| Directive | `CLAUDE.md` v7.1 |
| Owner | Vighnesha |
| Branch | `m0/toolchain-verification` (local only; not pushed, not merged into `main`) |

## 1. Done in this session

| Task | Result | Evidence |
|---|---|---|
| T1–T3 toolchain | Rust 1.99.1 (`58cf8f9c3`, LLVM 23.1.1) installed through rustup. `verify.sh`: 18 PASS / 0 FAIL with `--install`, 17 / 0 in check mode. | `docs/toolchain/VERIFICATION.md` |
| T2 experiments | E1–E4 repeated on 1.99.1. E1–E3 unchanged. E4 confirmed, plus two new findings: the prebuilt `libcore` for the Linux triple needs `rust_eh_personality` even under `panic=abort`, and mem* definitions must use the exact C signatures. | `docs/research/2026-11-07-onbf.md` §2.1 |
| T7 QEMU/OVMF | QEMU 11.1.3 and edk2-stable202408 installed through Homebrew (owner approved). Firmware hashes are in VERIFICATION.md. | ledger §2.1 |
| T4 workspace | Virtual workspace, strict unsafe lints, `cargo xtask` (check / build / boot-test / run). | `Cargo.toml`, `developer-tools/xtask/` |
| T8 step 1 | `boot/uefi` (`origin-boot-uefi`): validates the system table, prints to ConOut, returns `EFI_UNSUPPORTED`. `boot-test` passes in 1.8 s, and OVMF confirms it received `Unsupported`. | `SOURCE_PROVENANCE.md` |
| T5 versioning | `origin-version.toml` plus the generated `origin-version` crate. The loader banner reads `Origin OS 0.1.0-dev UEFI loader (build 0)`. `xtask check` rejects a version in any Cargo manifest. | `SOURCE_PROVENANCE.md` |
| T9 part 1 | `runtime/onbf/format` (`onbf-format`), the container layer: header, section table, layout, Merkle cross-references and the content-digest input. 74-case malicious corpus plus truncation, padding and bit-flip sweeps. The owner decided C1–C14: C12 (uncompressed manifests), C13 (reject every unknown kind) and C14 (empty sections only for SEGMENT) changed the parser. | `docs/spec/ONBF-errata.md` |
| SHA-2 | `security/crypto` (`origin-crypto`): SHA-256 and SHA-512 from FIPS 180-4, with constants derived by script. Known-answer tests, 310 differential vectors (hashlib and shasum agree), streaming splits. Portable code runs at 285/462 MiB/s on the M1 host. The new `xtask check` no-panic step proves that neither freestanding library's release object references a panic routine. | `SOURCE_PROVENANCE.md` |
| Merkle | `runtime/onbf/merkle` (`onbf-merkle`): `root`, `build`, and `verify_block` with a trust cache, using G2 and G3. No 4 KiB buffers, a few hundred bytes of stack. It agrees with an independent Python reference on 17 sizes up to 3 levels, and on every block-swap, tamper and padding attack. Its root runs at 279 MiB/s, about 2% over plain SHA-256. It is panic-free in the kernel target. | `SOURCE_PROVENANCE.md`, errata R1–R3 |
| OCBOR-1 + KRN manifest | In `onbf-format`: the `ocbor` validator and `Cursor`, plus `KrnManifest::decode`/`check_against` and `Section::verify_sha256` for the ONBF-R1 order. RFC 8949 Appendix A examples, a manifest corpus, an end-to-end loader-order test, and the canonical-encoding property over every bit flip. Panic-free in the kernel target. | `SOURCE_PROVENANCE.md`, errata R4–R5 |

Commits: `b5a446a` (M0), `87f50fd` (workspace + loader), `fc4d99e` (T5, which added this file), `f04fc1b` (T9 part 1), `b0eb0df` (C1–C14), `d7ce514` (SHA-2), `75f5051` (G2–G5), `cfd0662` (Merkle), `2b33b08` (manifest keys). The owner approved each one. The OCBOR-1 work updates this file.

`cargo xtask check` and `cargo xtask boot-test` pass at the end of the session.

## 2. Decision taken: T9 comes before M1 step 2

KRN, the Framekernel image, is an ONBF profile (ONBF §6.5), and only the boot loader parses it, through `onbf-format` (ONBF §8.2). An ELF-loading interim loader would contradict ADR-0001 and would be thrown away. The order is therefore:

1. **T9 `onbf-format`** in `runtime/onbf/format/`.
   - **Done:** the container layer, the OCBOR-1 validator, and the KRN manifest decoder.
   - **Remaining:** the signature-block structure (§7.1).
   - **SHA-256 and SHA-512 are done** (`origin-crypto`). `Image::verify_content_digest(…)` can now be called with `origin_crypto::Sha512`.
2. **`onbf-merkle` is done**: Merkle-SHA256-4K (§4.6), with G2, G3 and R1–R3. **Still to do:** connect it to `onbf-format` sections, so that a section's `digest` is checked as its Merkle root against the MERKLE section.
3. **A conversion path for KRN**: an `onbf-pack` subset that turns the kernel's ELF linker output into KRN. G4 and G5 are decided and enforced by `onbf-format`. The KRN manifest keys are decided (`docs/spec/ONBF-manifest-keys.md`, K1–K4), and decoding is done. **Still needed:**
   - the host-side `onbf-pack` itself: ELF64 PIE input → KRN segments, MERKLE sections via `onbf-merkle`, and the OCBOR-1 manifest encoder;
   - the CPU-feature CPUID tests, verified against the Intel SDM and AMD APM.
4. **M1 step 2**:
   - a *Proposed* boot ADR covering the boot-info structure and its ABI version (add it to `origin-version.toml [compatibility]` then), KASLR and relocation, and the trust anchor;
   - a `framekernel` crate for `x86_64-unknown-none` that writes to COM1;
   - the loader steps: memory map, load KRN, `ExitBootServices` (clear `PANIC_CONSOLE` first), jump;
   - `boot-test` assertions on COM1 output, with an `isa-debug-exit` exit path.

## 3. Waiting on the owner

- Merge `m0/toolchain-verification` into `main`, and push. Both are the owner's call; nothing has been pushed.
- A project licence. Every provenance entry says "not chosen yet".
- ONBF gap G1 (no section kind for SYM's DWARF data) in `docs/spec/ONBF-errata.md`. C1–C14, G2–G5 and the manifest-key policy K1–K4 were decided on 2026-11-07. Next decision on the critical path: the boot ADR (boot-info structure, KASLR, trust anchor), which is needed for M1 step 2.
- The boot trust anchor (Microsoft UEFI CA 2023, or Origin keys enrolled in firmware) for the boot ADR.
- Whether to install Rosetta 2 for the *executed* FFI-bool test (§10). Only the assembly-level check exists so far.
- The starting release values in `origin-version.toml` (`0.1.0-dev`, build 0, SVN 0) are a development default. Change them if you prefer others.

## 4. Environment facts worth keeping

- Running `rustup` inside the repository auto-installs the pinned toolchain. Use `RUSTUP_AUTO_INSTALL=0` to prevent that, which `verify.sh` does in check mode.
- QEMU ignores `SIGALRM`, so a test harness must kill it. `xtask boot-test` does this.
- The shell in the Claude Code desktop app is zsh, where `PIPESTATUS` is `pipestatus` and indexed from 1.
- OVMF's ConOut is mirrored to the serial port with ANSI sequences; `xtask/src/serial.rs` strips them.
- `-cpu max` under TCG on Apple Silicon. x86-64-v3 code in userspace will need the emulated CPU to expose AVX2: check before M6.

## 5. Later the same day: M1 step 2 and M2 (three-hour run, 17:56–20:56 IST)

Branch `m0/toolchain-verification`, local only (never pushed). Commits, oldest first:

| Commit | What |
|---|---|
| 78ea688 … be48ce8 | boot-info contract, loader step 2, ADR-0002, framekernel and SHA-3 merges, provenance |
| 91334ee | merge `onbf-pack` (agent) |
| a078db0 | merge Ed25519 (agent); crate docs reconciled (Ed25519 is variable time on public data) |
| 226eca1 | **first end-to-end boot**: loader → KRN → Framekernel; GDT/TSS/IDT, `int3`, frame allocator, paging library |
| ae035fa | boot info 0.2 (kernel segment table) and the kernel's own page tables; four #PF probes |
| 8aff1f3 | x2APIC, PIT-calibrated APIC timer, periodic interrupts |
| 0fcb16b | kernel heap, `GlobalAlloc`, frame runs, strict frees |

How to see it: `cargo xtask boot-test` (two scenarios, about 4 s) and `cargo xtask check`.

Lessons worth keeping:
- **Every boot proof needs a negative control.** Breaking the thing on purpose found two real bugs: a timer wait that halted (`hlt`) and so could never reach its own deadline, and a reordered failure message that no longer matched its xtask marker. xtask now matches failure markers in complete lines only, so the failing line's details always reach the log.
- **QEMU TCG coalesces late APIC timer expiries**: at 1 kHz with `hlt`, 100 interrupts took 125 ms. The proof runs at 100 Hz with an asymmetric bound (≥ 95 %, ≤ 150 %). Tighten it on hardware.
- **The frame allocator must refuse to free what it never owned.** A test showed `free` on a kernel-image address would have succeeded; fixed.
- The kernel library stays `forbid(unsafe_code)`: page tables are reached through a safe `TableMemory` that returns `None` for unknown addresses, the heap does address arithmetic only, and hardware access sits behind capability types in the binary (`X2Apic`, `LegacyPorts`).

Still running when this was written: the ML-DSA-65/87 verification agent (its own worktree; no NIST vectors without approval). Next: M3 (SMP and scheduling), starting with an ADR for AP start-up; see `TASKS.md`.

### Continued, 19:00–19:55 IST

| Commit | What |
|---|---|
| bbdc717 | merge ML-DSA-65/87 (agent) |
| 50c790c | M3 step 1: ACPI MADT discovery, INIT-SIPI-SIPI, per-CPU GDT/TSS |
| c41a39a | M3 step 2: preemptive kernel threads, per-CPU run queues |
| 02be5c3, 2e24da7, 1711675 | SMEP/SMAP; guard pages below every stack (thread, AP, boot) |
| 9332e52 | ADR-0003 (Proposed) |
| 44b3684 | reclaim loader and boot-services memory (40 MiB) |
| 9aac7da, bf79fce | thread lifecycle (yield, sleep, exit) and the stack reaper |
| be93105 | `origin-sync`: the spin lock as a host-tested crate |
| 5e3fbd3 | GS-based CPU-local area; switch cost measured (~18 % less) |
| a520df2, 398f3c5 | ring 3: one DPL-3 gate, crash isolation, RSP0 per switch |

Lessons: a negative control must be checked to have **rebuilt and booted** (two attempts here proved nothing: one did not compile, one used `timeout`, which macOS lacks). The boot flow runs with IF=0, so any wait in it must open interrupt windows, or time stops.

How to see it all: `cargo xtask boot-test` (default 2 CPUs; `ORIGIN_QEMU_SMP=1..8` also pass) and `cargo xtask check`.

### Continued, 19:55–20:05 IST

| Commit | What |
|---|---|
| 515868d | SYSCALL entry on every CPU, through the common dispatch path |
| 653c93a | NMI and #MC on their own IST stacks (self-NMI proof) |
| 8220b24, 64ac900 | `docs/STATUS.md`: verified status by milestone |
| 13c323a | `cargo xtask boot-matrix`: boot-test at 1, 2 and 4 CPUs |
| 19ccb14 | ADR-0004 (Proposed): higher-half layout and KASLR, four owner questions |
| ada5edb | heap and address-space locks held with interrupts disabled (found by review) |

Start here next time: `docs/STATUS.md`, then `docs/engineering/TASKS.md`. The next code step after the owner's answers is either the signature block (S1–S4) or the higher-half layout (ADR-0004).

### Continued, 20:05–20:20 IST

| Commit | What |
|---|---|
| 1bdd1a4 | S1–S4 decided by the owner (all recommended options) |
| 43a37ab | `onbf-format`: the signature-block parser |
| d89f5b3, e19dd36 | `onbf-verify`: entry verification, proven against reference-signed vectors |
| ab4aaa6 | validated, fault-safe copy from user memory; a write system call (closed a kernel-memory leak found by its own test) |

Final state at 20:20: `cargo xtask check` and `cargo xtask boot-matrix` pass; the working tree is clean; nothing pushed.

### After the owner accepted ADR-0004 (20:21–20:35 IST)

| Commit | What |
|---|---|
| 008008c | ADR-0004 accepted as proposed |
| 226346a | `origin-paging`: the page-table library shared by loader and kernel |
| ceccf62 | the higher-half kernel with KASLR (H1–H4), boot info 0.3 |

**The kernel no longer runs identity-mapped.** RAM is at `direct_map_base + physical` (use `phys::to_virt`/`to_phys`), the image at `kernel_virtual_base`; the lower half is empty and belongs to user space. Next: per-process address spaces sharing the kernel half (M4), then H5 (global pages, PCIDs).

M4 began with per-process address spaces (`process::Process`, CR3 per thread in the scheduler); the next M4 decisions (process ABI, handles, IPC) need an ADR and the owner's approval (§84).

## 6. The twelve-hour run (from 21:42 IST), under the owner's delegation

The owner asked for the design to be finalised with the recommended options, followed by twelve hours of work without asking. Commits so far, each with its proof and negative controls in its message and in `SOURCE_PROVENANCE.md`:

| Commit | What |
|---|---|
| `03379ff` | Every open design decision finalised: ADR-0002 with B7, ADR-0003, errata G1 (the SYM `DWARF` kind, implemented) and the readings |
| `2d08b37` | Clocks: a monotonic TSC clock whose rate is proven against one RTC second, a UTC wall clock from the CMOS RTC (century register from the FADT), and TSC consistency checked across CPUs |
| `b53a5c5` | A serialized console: whole lines from every CPU, checked over the whole boot log by xtask; `SpinLock::try_lock` |
| `115651d` | TLB shootdown IPIs and global kernel pages (ADR-0004 H5); closes a real gap, since guard pages were invalidated only locally |
| `a03df12` | Kernel wait queues with cross-CPU wake-ups by IPI; no lost wake-ups |
| `9b74be4` | PCIe enumeration through the MCFG: uncached ECAM, every function and BAR |
| `05dcde1` | A polled NVMe driver: Identify, admin and I/O queues, read, write, flush, against a GPT test disk xtask builds |
| `31e1c56` | GPT: both copies validated over NVMe, partitions listed |
| `e075ca4` | Interim records (this handoff and the backlog) |
| `742d1ef` | Seeded mutation tests for every firmware and device parser (§72) |
| `2e06794` | Timed waits: deadlines on the monotonic clock for blocked threads |
| `57a5f10` | MSI-X: interrupt-driven NVMe completions for a blocking thread |
| `a28e58c` | Per-CPU NVMe queues: every CPU does I/O at once through its own pair |
| `56996f6` | Stride scheduling: proportional shares; a true idle context on APs |
| `216b926` | Block layer: a 4 KiB page cache over the NVMe namespace |
| `a6a3cb4` | CPU feature discovery, and FPU/SIMD state across context switches |
| `61e3ad5` | Panic diagnostics: a frame-pointer backtrace |
| `885d764` | In-memory VFS: paths, directories, files |
| `816158c` | Software rasterizer: fills, blits, alpha blend (CRC cross-check) |
| `ab39160` | PS/2 keyboard: scancode decoding and i8042 controller bring-up |
| `0e42e20` | HPET: ACPI discovery and a counter-rate cross-check |

(The 2026-11-08 commits use the Claude Opus 4.8 attribution; the run spanned midnight.)

### Lessons from this run

- **Run the good path after adding boot-test markers, before any negative control.** A control fails on its failure string whatever the marker order. Wrongly ordered PCIe markers were therefore caught only by the boot matrix.
- **Firmware state can defeat a control.** OVMF had already set bus mastering, so a control that only stopped setting it proved nothing. OVMF also restored a corrupted primary GPT from the backup before the kernel ran (UEFI 2.11 §5.3.2 allows it). A control must set the state it tests explicitly, and must check that its fault reached the kernel.
- `bool::then_some(x)` evaluates `x` eagerly. A lock guard built that way and then dropped unlocks a lock that someone else holds. The host test caught it.
- Check an unimplemented BAR's mask before widening it: a zero mask widened to 64 bits reads as 4 GiB.
- `split_large` must carry the memory type (PCD, PWT, PAT) into the 4 KiB pieces.
- QEMU keeps stale TLB entries across vCPUs, so a missing shootdown is visible there. Its TCG TSC runs at about 1 GHz, is not reported as invariant, and has no CPUID 15h rate.
- The wake-up race between registering on a wait queue and blocking really happens even in QEMU: the transfer stalled at 6202 of 10 000 items without pending wake-ups.

## 7. Where the twelve-hour run stands (2026-11-08, ~02:00 IST)

The kernel now boots on 1/2/4 CPUs through one `boot-matrix` run with, in order: higher-half
load with KASLR, GDT/TSS/IDT and exceptions, a frame-pointer backtrace proof, CPU-feature
discovery with SSE/AVX enabled, the frame allocator and W^X tables, the heap, SMEP/SMAP,
the APIC timer, the monotonic/RTC clocks and the HPET cross-check, SMP bring-up, preemptive
stride scheduling with FPU-state save/restore, kernel wait queues (timed and cross-CPU),
a serialized console, TLB shootdown with global pages, PCIe enumeration, a polled and then
MSI-X interrupt-driven NVMe driver with per-CPU queues, GPT validation, a block cache, the
in-memory VFS, the software rasterizer and the i8042 keyboard bring-up — each with a boot
proof and at least one negative control. 143 host library tests pass; `cargo xtask check`
passes. Everything is QEMU-only; no physical-hardware qualification (CLAUDE.md §74). The
ADR-0005 kernel system-call layer is still the one area left untouched (a safety classifier
stopped it twice earlier); the owner's non-design items (downloads, a licence, merge/push)
remain.
