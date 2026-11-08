# Session handoff — 2026-11-07: local Claude Code session on the M1 Max

| | |
|---|---|
| From | Claude Code, local, in `~/work/origin-os` (follows `2026-11-07-handoff.md`) |
| Directive | `CLAUDE.md` v7.1 |
| Owner | Vighnesha |
| Branch | `m0/toolchain-verification` — pushed and open for review as PR vighnesha-os/origin-os#1 (at the owner's request, 2026-11-08); **not merged into `main`** |

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
| `48e7f01` | Network packet layer: Ethernet/IPv4/ICMP/ARP and the Internet checksum |
| `2335c2d` | TCP: segments and a minimal connection state machine |
| `7750afb` | UDP datagrams and a monotonic timer queue |
| `1091e35` | `origin-sync`: a reader-writer spin lock, backing the PCIe device list |
| `ed14eac` | A live `/sys` tree in the VFS; an 8-CPU + mutation-soak verification pass |
| `f740193` | Seeded mutation tests for the net and TCP parsers |
| `b4c3fde` | I/O APIC interrupt routing, proven by routing the PIT's IRQ0 |
| `6ae2541` | ACPI PM timer: a second time-source cross-check |
| `c9e63d2` | CPU topology from CPUID leaf 0Bh |
| `fe6be25` | CPU cache hierarchy from CPUID leaf 04h |
| `e275027` | Records: STATUS milestone rows and this handoff's commit list |
| `dc4ba79` | OriginFS v0 read-only parser + image builder (ADR-0006, Proposed; host-only, not mounted — §84) |
| `3c038fc` | NUMA discovery from the ACPI SRAT (proven at boot with two `-numa` nodes) |
| `f95384e` | NUMA distances from the ACPI SLIT |
| `ec9c729` | NUMA locality map (CPU/address → node, nearest-node order, self-nearest) |
| `a017bc6` | Streaming UTF-8 decoder for the text engine (byte-exact vs `core::str` over 20k fuzzed inputs) |
| `a5b139b` | Gap buffer for editable text (50k-op model test) |
| `ca9d3ce` | Records: STATUS and handoff for the NUMA/OriginFS/text work |
| `ca44453` | Hardware entropy: RDSEED/RDRAND with bounded retries (boot-proven) |
| `36cfaf9` | Glob pattern matching for names and paths |
| `a34f0a9` | IPv4 longest-prefix-match routing table (bit trie, oracle-tested) |
| `54e2aea` | DNS message parsing with bounded name decompression |
| `32f3ae1` | Mutation soaks for SRAT/SLIT/DNS/OriginFS; cap `read_whole` |
| `5774b7f` | Fixed-size bit allocator (lowest-free, model-tested) |
| `34ec628` | Records: STATUS/handoff + the ~04:20 verification pass |
| `ff23487` | ustar (tar) archive reader |
| `7b7f71d` | UTF-16 transcoding for UEFI/CHAR16 interop |
| `e051b10` | Fixed-capacity ring buffer (input/audio/log) |
| `87bd46a` | Boot proof: extract an embedded ustar initramfs into a VFS (§21) |
| `5188d53` | DHCP client (build/parse + lease state machine) |
| `6a7a417` | DEFLATE (RFC 1951) decompression, all three block types |
| `5649c4f` | zlib and gzip wrappers over DEFLATE, with Adler-32 |
| `8e6bb84` | Records: STATUS/handoff for the ustar/deflate/dhcp batch |
| `e6145a5` | Base64 encode/decode (RFC 4648) |
| `b789c50` | JSON parser (RFC 8259), depth-limited |
| `4507507` | Records: base64/json/zlib-gzip + a 490-test checkpoint |
| `fd944de` | Semantic versioning: parse + precedence (semver.org) |
| `63ac05c` | String-keyed hash map (FNV-1a open addressing) |
| `6bb0885` | semver requirement matcher (^, ~, ranges) + prerelease gate |
| `b19dc37` | Bounded kernel log buffer (§69) |
| `f0a0972` | virtio split virtqueue ring transport |
| `4074b18` | virtio device negotiation + virtio-net structures |
| `c053f4c` | virtio::find_caps (walk the PCI capability list) |
| `9810c84` | virtio-net kernel driver: bring-up + TX (boot-proven) |
| `f7cce65` | virtio-net: RX datapath + a **live ARP exchange** over the NIC |
| `70d5a3c` | virtio-net: a **live DHCP lease** over the NIC — the OS joins the network |

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
proof and at least one negative control. By 02:46 IST, 163 host library tests pass (396 across the workspace); `cargo xtask check`
passes; the system boots on 1–8 processors. Everything is QEMU-only; no physical-hardware qualification (CLAUDE.md §74). The
ADR-0005 kernel system-call layer is still the one area left untouched (a safety classifier
stopped it twice earlier); the owner's non-design items (downloads, a licence, merge/push)
remain.

### Continued (2026-11-08, ~02:00–03:50 IST)

Six more features, each built → host-tested → committed, with boot proofs and negative
controls where they run in the kernel:

- **NUMA, end to end (§24, §27, §28, §66).** The ACPI **SRAT** (CPU and memory proximity
  domains) and **SLIT** (inter-node distance matrix) are parsed, and a pure **locality map**
  (`numa`) answers CPU/address → node, nearest-node order and the self-nearest invariant.
  QEMU is now booted with two `-numa` nodes and a cross-node distance, so the kernel reads a
  real firmware SRAT/SLIT: `numa: 2 nodes, N cpus, …; consistent true` and
  `slit: 2 localities, local 10 (true), …, from 0 [0, 1]; consistent true` at `-smp`
  1/2/4/8. Three negative controls (SRAT entry offset, SLIT matrix offset, inverted
  nearest-node comparator) each fail the boot and were restored.
- **OriginFS v0 (§39–§41, §84).** A read-only on-disk filesystem parser and image builder
  against the **Proposed** ADR-0006 (superblock, inode table, 12 direct + 1 single-indirect
  pointers, linked directory entries). Host-only, **not mounted** — the format decision
  awaits the owner. The kernel binary does not reference it, so the no-panic object is
  unaffected.
- **Text-engine groundwork (§51).** A streaming, error-locating **UTF-8 decoder** (`text`),
  byte-exact against `core::str::from_utf8` and `String::from_utf8_lossy` over 20 000 fuzzed
  inputs; and a scalar-indexed **gap buffer** (`editbuffer`) for editable text, checked by a
  50 000-operation model test against a `Vec<char>` oracle. Both host-only (no UI path yet).

By ~03:50 IST, **189 host library tests** pass; `cargo xtask check`, `boot-test` and
`boot-matrix` (1/2/4) pass; 8-CPU boots confirmed. The NUMA map is the decision layer — a
NUMA-aware frame allocator and scheduler placement (§66) are the deliberate next step, left
for a careful change rather than an unattended edit to the core allocator. ADR-0005
(syscalls) remains untouched; the owner's non-design items (downloads, a licence,
merge/push) remain.

### Continued (2026-11-08, ~03:50–04:20 IST)

Six more commits:

- `ca44453` **Hardware entropy (§59).** A kernel entropy source: a pure bounded-retry
  `draw`/`fill` layer (`entropy`) over RDSEED/RDRAND wrappers in `cpu.rs`. Boot proof: 16
  draws, all distinct and non-zero (`entropy: … consistent true`) at `-smp` 1/2/4/8.
  Negative control: a constant drawn value fails the boot. It is a true-random source, not
  a DRBG (that needs KATs and review, deferred).
- `36cfaf9` **Glob matching (§39).** `glob::matches` (`*`, `?`, `[...]` with ranges/
  negation, `\` escapes; wildcards never cross `/`), single-star-pointer backtracking that
  terminates on adversarial patterns. Host-tested.
- `a34f0a9` **IPv4 LPM routing (§54).** `route::Table<V>`, a bit trie; `lookup` oracle-
  tested against a brute-force longest-match scan over 2 000 random tables.
- `54e2aea` **DNS parsing (§54).** `dns::parse` with RFC 1035 name compression, bounded so
  a decompression bomb is a clean `None`. The round-trip test caught a real `rdlength`
  offset bug. Host-tested.
- `32f3ae1` **Mutation soaks + a fix.** "Never panics on mutated input" soaks for SRAT/
  SLIT/DNS/OriginFS; the soak found that `OriginFS::read_whole` allocated `inode.size`
  unbounded, now capped to v0's addressing (~2 MiB).
- `5774b7f` **Bit allocator (§40, §29).** `bitmap::Bitmap` (lowest-free alloc, exact free
  count), 60 000-op model test; the primitive for an FS free-space map and id pools.

**Verification pass (~04:20 IST):** the full workspace is **456 tests, 0 failures** (6
ignored = the download-gated crypto vectors); the 40× mutation soak runs 9 parser suites
with 0 failures (~31 s); an 8-CPU boot comes up with NUMA, SLIT and entropy all consistent;
`cargo xtask check` passes. **215 framekernel host library tests.** Still QEMU-only (§74);
ADR-0005 and the owner's non-design items remain.

### Continued (2026-11-08, ~04:20–04:55 IST)

Six more commits, filling out formats, text and networking:

- **ustar + initramfs (§21).** A read-only tar reader (`ustar`) and a loader (`initramfs`)
  that extracts an archive into the VFS, creating implicit parents and refusing `..`/
  absolute names (the tar path-traversal defense). **Boot-proven**: the kernel embeds a
  small committed `assets/initramfs.tar` and extracts it into a VFS at boot
  (`initramfs: 4 entries … consistent true`), with a negative control. This is the first
  end-to-end §21 step — an initial filesystem image loaded at boot.
- **DEFLATE (§17, §46).** `deflate::inflate` decodes raw DEFLATE — stored, fixed- and
  dynamic-Huffman blocks — validated against zlib-generated known-answer vectors (a verified
  dynamic block among them), with a decompression-bomb output cap. Real decompression for
  compressed packages/images.
- **UTF-16 (`utf16`), a ring buffer (`ring`), a DHCP client (`dhcp`).** The UTF-16 codec
  (byte-exact vs std) completes the scalar↔bytes codecs; the ring buffer is the bounded
  input/audio/log queue; the DHCP client is the full DISCOVER→OFFER→REQUEST→ACK lease
  machine.

By ~04:55 IST, **238 framekernel host library tests** pass; `cargo xtask check`, `boot-test`
and `boot-matrix` (1/2/4) pass. The library now carries 46 host-tested modules. Still
QEMU-only (§74); ADR-0005 (syscalls) and the owner's non-design items (downloads, a licence,
merge/push) remain.

### Continued (2026-11-08, ~04:55–05:10 IST), and a verification checkpoint

- **Compression stack.** The zlib (RFC 1950, Adler-32) and gzip (RFC 1952, CRC-32 reusing
  `gpt::crc32`) wrappers now sit over the DEFLATE decoder, so real `.gz`/zlib data decodes;
  `inflate` reports bytes consumed so the trailer can be located and verified.
- **Data-format parsers.** `base64` (RFC 4648, whitespace-tolerant, RFC vectors) and `json`
  (RFC 8259, recursive descent, depth-limited against stack overflow, numbers kept as source
  text since `core` has no `no_std` float parser) — the config/manifest/SDK and PEM/token
  paths now have their codecs.

**Verification checkpoint (~05:10 IST):** the full workspace is **490 tests, 0 failures**;
the 40× mutation soak runs **12 parser suites** with 0 failures (~32 s); `cargo xtask check`,
`boot-test`, and `boot-matrix` (1/2/4) all pass, with the NUMA, entropy and initramfs boot
proofs consistent; an 8-CPU boot was confirmed earlier in the run. **249 framekernel host
library tests across 49 modules.** Everything remains QEMU-only (§74); ADR-0005 and the
owner's non-design items (downloads, a licence, merge/push) remain.

Two more followed (`fd944de`, `63ac05c`, `6bb0885`): semantic-version parse/precedence
with a requirement matcher (§46 dependency comparison) and a string-keyed FNV-1a hash map
(§29/§30 name tables). **Final checkpoint (~05:25 IST): 501 workspace tests across 27
suites, 0 failures; 260 framekernel host library tests across 49 modules; `cargo xtask
check` passes.** The full run report is `docs/engineering/handoffs/2026-11-08-run-report.md`.

### Continued (2026-11-08, ~05:25–05:45 IST): a live NIC

The headline result of the later run: **the networking stack is live.** Bottom-up, each
layer host-tested and committed, then the kernel driver boot-proven:

- `klog` — a bounded kernel log ring (§69 groundwork).
- `virtqueue` — the split-virtqueue ring transport (descriptors, available/used rings),
  over plain byte buffers so the kernel drives DMA and the tests drive a `Vec<u8>`.
- `virtio` — device status/feature negotiation, the virtio-net config/header, and
  `find_caps` (walk the PCI capability list for the modern virtio structures).
- `virtionet` (kernel) — brings up a modern **virtio-net NIC** (PCI capability discovery,
  BAR mapping, the feature handshake to `DRIVER_OK`, RX/TX virtqueues on DMA pages), reads
  the device MAC, then **takes the OS onto the network**: it ARP-resolves QEMU's gateway
  (`ARP 10.0.2.2 -> 52:55:0a:00:02:02`) and runs a full **DHCP exchange** (DISCOVER → OFFER
  → REQUEST → ACK) with QEMU's built-in server, obtaining the lease **10.0.2.15** — each
  message wrapped by `net` in UDP/IPv4/Ethernet with correct checksums, transmitted, and
  each reply received and parsed back up into the `dhcp` client. Three negative controls
  (wrong MAC; ARP a nonexistent host; DHCP to the wrong port) each fail the boot and were
  restored. **The whole stack is live** — NIC TX+RX, Ethernet, ARP, IPv4, UDP and DHCP.

**Checkpoint (~06:00 IST): 273 framekernel host library tests across 52 modules; the
workspace passes `cargo xtask check`, `boot-test` and `boot-matrix` (1/2/4), with the NIC
coming up and the OS obtaining its DHCP lease at every `-smp`.** The reusable DMA/MMIO
patterns (NVMe's) and the bottom-up host-tested libraries (`virtqueue`, `virtio`, `net`,
`dhcp`) made the driver tractable. Next (left for careful continued work): make the NIC
interrupt-driven (MSI-X), add a socket API, and route real traffic on the lease. QEMU-only
(§74).

### Continued (2026-11-08, ~07:50–09:30 IST): the network path, interrupt-driven and recovering

Owner prompts in this stretch: "Keep building" (repeatedly), a request to open a pull request
(done: **vighnesha-os/origin-os#1**, ready for review, pushed and its description refreshed after
every commit — **not merged**; merging stays the owner's), and a request for this handoff.

Each commit below was built, passed `cargo xtask check`, was boot-proven at `-smp` 1/2/4, and
has at least one negative control that failed the boot and was restored (details in each commit
message and in `SOURCE_PROVENANCE.md`). Co-authored `Claude Opus 4.8` for `321b1a7`,
`Claude Opus 5.5` from `87b8f93`.

| Commit | What |
|---|---|
| `321b1a7` | MSI-X on the NIC's receive queue (IDT vector `0x42`) |
| `87b8f93` | an interrupt-driven **receive thread** on another CPU: it sleeps for each frame and the NIC interrupt wakes it |
| `1ab5405` | a **recycled TX ring** (4 descriptors, buffers allocated once), batched kicks, TX-completion interrupts (`0x43`); `virtqueue::avail_push` gained the index-publication barrier |
| `2326561` | **ping the gateway** from the leased address, unicast to the ARP-resolved MAC |
| `4293998` | `tftp` (RFC 1350 reader, host-tested); **fetch a file by TFTP**, byte-identical to the embedded image; receive-buffer recycling |
| `d54cc16` | **unpack the fetched image** into a fresh VFS (checks shared with the embedded proof) |
| `ba2de8d` | **reply timeouts**: a lost reply fails its own step by name and the datapath carries on |
| `3f415b2` | **TFTP retransmission**, with the first read request dropped on purpose every boot |
| `cfbbd0f` | **DHCP retransmission**, with the first DISCOVER dropped on purpose every boot |

What every boot now shows (all `consistent true`, at 1, 2 and 4 CPUs; the excerpt is the
2-CPU boot — across the three, the receive thread's sleeps (13/8/9) and the ping round trips
vary, and at one CPU the thread runs on CPU 0; the frame counts, the TX ring's figures, the
retransmissions and the fetched file are identical):

```
virtio-net: … DHCP lease 10.0.2.15 (1 retransmitted after a 500 ms timeout, the first DISCOVER dropped on purpose); receive thread on CPU 1: slept 8, woken by the NIC's interrupt 8 (of 17 MSI-X interrupts, the last on CPU 1)
virtio-net TX: 25 frames through 4 descriptors (buffers allocated once), 25 completions reclaimed, 4 of 4 free at the end; the ring was full 1 times and senders slept 2 times, woken by the TX-completion interrupt 2 (of 22)
virtio-net ping: 10.0.2.15 -> 10.0.2.2 (52:55:0a:00:02:02): 4 of 4 echo replies (identifier, sequence and data matched)
virtio-net TFTP: initramfs.tar from 10.0.2.2: 4608 bytes in 10 blocks (the last empty), identical to the embedded image; 1 retransmitted after a 500 ms timeout (the first read request is dropped on purpose); the receive ring's 8 buffers carried 17 frames (16 recycled)
virtio-net initramfs: the fetched image extracted into a fresh VFS: 4 entries; /etc/hostname true, implicit /boot dir true
```

#### Lessons from this stretch

- **A passing check can hide a path that never ran.** Three times a mechanism existed but QEMU's
  timing never exercised it: the receive thread "slept 0 times" (slirp's replies beat it to its
  wait), the TX ring was "full 0 times", and slirp never loses a packet. Each time the fix was to
  *make the condition happen deterministically* and assert that the path ran: an armed handshake
  (the boot flow sends the ARP request only once the thread is asleep), batched kicks with the
  kick inside the wait condition, and losses injected on purpose (a corrupted checksum the server
  discards). Proofs now require the path to have run (`slept > 0`, `woken > 0`, exactly one
  retransmission), so they cannot pass on a lucky fast path.
- **The boot thread runs with interrupts off.** An MSI-X message routed to it waits in the local
  APIC (several arrivals coalesce into one IRR bit) until an interrupt window. Reading the x2APIC
  IRR told "pending" from "never delivered" when the interrupt count was 0.
- **To never miss a completion, kick inside the wait condition and route the interrupt to the
  waiter's CPU:** the condition runs with interrupts off before the waiter registers, so the
  completion's interrupt can only arrive once it is registered — and a batch's sleep becomes
  deterministic.
- **More traffic finds latent bugs.** Adding traffic exposed three: the receive ring never
  handed buffers back (7 of 8 were already used); the transmit path reused descriptor 0 and
  leaked a page per frame, and stopped waiting for completions after the first frame; and
  `avail_push` published the ring index without a barrier.
- **`xtask` stops QEMU at the first failure marker,** so later lines are lost. Put the reason in
  the failing step's own line (the timeout notes do), and expect controls to show only the first
  failing line.
- **Re-run a control when a change touches its premise.** With a receive deadline, "no wake-up"
  changed from a permanent sleep to a crawl (`woken … 0`) that misses the boot flow's bound —
  still a failure, for a different reason, and recorded as such.
- **QEMU user networking (slirp) facts:** it answers pings to its own addresses itself
  (`icmp_reflect`, no host socket — an earlier note here that its ICMP might need host
  privileges applies only to outside hosts); it verifies ICMP and UDP checksums; with
  `tftp=<dir>` it serves that directory read-only from 10.0.2.2:69 with no host network; and it
  never drops packets, so recovery paths need injected losses.

**Checkpoint (~09:30 IST): 522 workspace tests across 27 suites, 0 failures (6 ignored: the
download-gated crypto vectors); 281 framekernel host library tests across 53 modules;
`cargo xtask check`, `boot-test` and `boot-matrix` (1/2/4) pass.** QEMU-only (§74): nothing is
qualified on physical hardware.

**Next, for the following session:** the owner's non-design items still stand (a licence, the
NIST/Wycheproof/`loom` downloads, ADR-0006, and merging PR #1). In networking: answer ARP and
echo requests addressed to us, RFC 2131's backoff and lease renewal, retransmitting echo
requests, a socket API with a long-lived receive service over the two rings, and mounting
fetched content rather than only checking it. Outside networking: NUMA-aware frame allocation
(the `numa` map is the decision layer), the keyboard IRQ → `ring` → `editbuffer` input path, and
teeing the console into `klog`. The run report is `docs/engineering/handoffs/2026-11-08-run-report.md`.

### Audit against CLAUDE.md's 2026+ hardware target (2026-11-08, ~09:45 IST)

The owner asked whether the work targets the latest (2026+) hardware for production grade, per
CLAUDE.md (header: "Intel and AMD platforms introduced in 2026 or later"; §14 x86-64-v3 baseline
and CET shadow stacks; §17 PCIe Gen 5/6, NVMe 2.4, USB4 v2; §24 capability discovery, hybrid
cores; §74/§76 physical qualification of a declared scope). **Answer: the direction is aligned,
but nothing is production-grade or tested on 2026+ hardware.** Findings:

- **Fixed now:** the x86-64-v3 baseline was misread (every v3 CPU graded v2, an OSXSAVE-ordering
  bug — see `SOURCE_PROVENANCE.md`, CPU feature discovery) and only reported. It is now detected
  correctly and **enforced**: a CPU below v3 is refused at boot with the missing features named
  (`ORIGIN_QEMU_CPU=max,-avx2` shows it). An earlier note blaming QEMU for "level 2" was wrong.
- **Missing 2026+-platform requirements (in priority order for the next session):** CET shadow
  stacks (§14 says enforce; there is no CET code; QEMU's TCG cannot model CET, so it needs KVM on
  CET hardware or bare metal); the **IOMMU** (Intel VT-d / AMD-Vi) with **interrupt remapping**
  (§32 — DMA protection, and MSI to APIC IDs above 255 is refused without it; QEMU emulates both
  IOMMUs, so this can proceed in CI); **hybrid/extended topology** (only CPUID leaf 0x0B is read —
  no 0x1F, no core type 0x1A, no AMD 0x80000026; §24, §27); a **UEFI GOP display fallback** (§36);
  **xHCI/USB HID** (the only keyboard path is legacy PS/2), then USB4/Thunderbolt (§34–35);
  Secure Boot enforcement in the loader (signatures are parsed, not required); power (§67).
- **Verification platform:** QEMU q35 emulates a 2007-era Intel ICH9 chipset; the NIC is
  virtio-net (virtual-machine-only — a test vehicle; the protocol and ring code above it carry
  over, the driver does not); the disk is QEMU's NVMe 1.4 (baseline: NVMe 2.4); the emulated CPU
  topology is homogeneous (no SMT, no hybrid). No `docs/hardware` compatibility database (§79), no
  physical test matrix (§74), no declared supported scope (§76, rule 40).
- **Owner decisions needed:** which 2026+ Intel and AMD machines are the declared targets, and
  physical x86 test hardware (the M1 Max host cannot run x86 natively). Before relying on specific
  2026 platform features (e.g. FRED, AVX10, APX), research them per §16 rather than from memory.

### Continued (2026-11-08, ~10:20–11:50 IST): Intel VT-d DMA and interrupt remapping

The audit above put the IOMMU with interrupt remapping first among the missing 2026+ pieces that
QEMU can test. This stretch built it. Another session started it in its own worktree,
**branch `claude/interesting-carson-ae7c83`**. That branch began at `main`'s initial commit and
was fast-forwarded to `540fc2c` (the `m0/toolchain-verification` tip) before any work. Its
commits are **local and not pushed**; PR #1 is untouched.

| Commit | What |
|---|---|
| `6a39520` | research (`docs/research/2026-11-08-vt-d-iommu.md`); `dmar`, the ACPI DMAR parser; `vtd`, the VT-d encodings and second-level walker (17 host tests, a mutation soak) |
| `38c288e` | the kernel's `iommu` and `dma`; NVMe and virtio-net through their own domains, with MSI-X through IRTEs; xtask's `intel-iommu` and markers; ADR-0007 (Proposed) |
| (records) | provenance, STATUS, this section |

What every boot now shows (at `-smp` 2; the matrix agrees at 1 and 4, with 4 and 7 MSI-X entries
remapped):

```
IOMMU: unit 0 at 0xfed90000 (segment 0, 4 KiB of registers): 7 PCI scope(s), I/O APIC 0 (source ID 0xff00)
IOMMU: unit 0: … interrupt remapping on (256 entries, 32-bit x2APIC destinations, compatibility format blocked); translation on (GSTS 0xc7000000): every device's DMA faults until its driver attaches it
IOMMU: translated DMA: the NVMe controller (00:02.0, source ID 0x0010) read LBA 2048 into IOVA 0x40012000, which its domain maps to physical 0x75000 (not the same address): 8 of 8 blocks landed in that physical page; consistent true
IOMMU: blocked DMA: IOVA 0x40012000 unmapped (IOTLB invalidated page-selectively, reads and writes drained), the same read completed with status type 0 code 0x0, and the page kept its canary true; fault recorded: source ID 0x0010, write at 0x40012000, reason 0x5 (write to a page not mapped writable); consistent true
IOMMU: interrupt remapping: 5 of 5 MSI-X entries (NVMe 3, virtio-net 2) hold only a remappable handle with data 0 …; APIC ID 300 accepted (its IRTE reads back destination 300), which compatibility format refuses: true; consistent true
IOMMU: blocked interrupt: NVMe MSI-X vector 1 pointed at IRTE 5, left not present: … 0 interrupts arrived; fault recorded: source ID 0x0010, interrupt index 5, reason 0x22 (interrupt table entry not present); vector 1 restored; consistent true
```

Negative controls, each failing `cargo xtask boot-test` and then restored from git:
- **C1**, no IOTLB invalidation on unmap: the canary is overwritten and no fault is recorded.
- **C3**, IOVAs never mapped: an NVMe admin command times out.
- **C4**, virtio-net without a context entry: no ARP, no lease.
- **C5**, IRE off: no NVMe interrupt arrives.
- **C6**, a wrong source ID in the IRTEs: no NVMe interrupt arrives.
- **C7**, the probe IRTE left present: 1 interrupt arrives and no fault is recorded.

One probe **passed**, and is recorded as a QEMU blind spot. C8, with no interrupt-entry-cache
invalidation, boots fine: QEMU reads IRTEs from memory on every interrupt. CFI blocking cannot
be shown under QEMU either.

**Checkpoint (~11:50 IST):**
- `cargo xtask check` passes: 542 tests, 0 failures, 6 ignored (the download-gated vectors);
  301 framekernel host library tests, 17 of them new.
- `boot-test` and `boot-matrix` (1/2/4) pass on the committed code; the kernel boots in
  13–22 s.
- QEMU-only (§74): nothing is qualified on physical hardware.

#### Lessons from this stretch

- **Make translation visible.** IOVAs are never physical addresses, and under QEMU's 512 MiB the
  IOVA window [1 GiB, 2 GiB) has no RAM behind it. A DMA that skipped translation would land
  nowhere, so "the data arrived in that physical page" proves translation. With identity mapping,
  a translation that never happened would look exactly like one that did.
- **The unmap proof works only because QEMU caches translations.** Its IOTLB keeps a translation
  until it is invalidated, so control C1 (no invalidation) lets the "blocked" DMA through. The
  proof therefore really tests the invalidation path.
- **QEMU's device model does not report a blocked DMA to the device.** The NVMe read to an
  unmapped IOVA "completes" with status 0, through QEMU's bounce path. A proof of blocking must
  check memory and the fault record, never the command status. On hardware a blocked posted
  write is silent to the device too.
- **QEMU facts:**
  - `eim=auto` is off under TCG, so set `eim=on`;
  - virtio devices bypass the IOMMU unless `iommu_platform=on`, and then the driver must accept
    `VIRTIO_F_ACCESS_PLATFORM`;
  - repeated faults from one source ID collapse while a record is pending, so clear records
    first;
  - compatibility-format MSIs pass through even with IR on (CFI is not enforced);
  - a vector-0 MSI is silently dropped.
- **This worktree started on `main`'s initial commit,** not on the code. Check `git log` before
  building anything in a new worktree.

#### Next, for the following session

- **Interrupts:**
  - I/O APIC entries in remappable format, with IRTEs for the I/O APIC's source ID 0xff00. This
    is needed before any I/O APIC interrupt (the keyboard IRQ) is used with IR on: hardware
    blocks compatibility format.
  - Decouple the CPU index from the APIC ID (per-CPU arrays are indexed by APIC ID, `MAX_CPUS`
    64). Then QEMU can boot a CPU with an APIC ID above 255 and prove delivery to it live.
  - A fault-event interrupt feeding `klog`.
- **DMA:**
  - RMRR identity maps (devices that need them are refused today);
  - AMD-Vi (the IVRS table, device table, command buffer), as the separate step the task planned;
  - scalable mode, after checking from VT-d Rev 5.20 itself (download approval) whether 2026+
    platforms keep legacy mode.
- **The owner:** ADR-0007's five questions; approval to download the VT-d PDF.
- **The rest of the audit list:** CET shadow stacks (needs KVM on CET hardware or bare metal),
  hybrid/extended topology, a GOP display fallback, xHCI/USB HID, Secure Boot enforcement.

### Continued (2026-11-08, ~11:15–12:15 IST): processors above x2APIC ID 255

The checkpoint above listed "decouple the CPU index from the APIC ID" under *Next*. This stretch did it, in another session's worktree: **branch `claude/eloquent-kapitsa-840594`**. That branch was fast-forwarded to the IOMMU branch tip (`1c790fe`) before any work. Its commits are **local and not pushed**.

| Commit | What |
|---|---|
| `351e630` | `framekernel::cpumap` (8 host tests); `CpuIndex` and per-CPU state by index in `cpu`, `smp`, `sched`, `tlb`, `storage`, `virtionet`, `wait`, `console`, `time`, `user`; MADT and SRAT lists sized from the table; the x2APIC proof; xtask `boot-x2apic`, `ORIGIN_QEMU_TOPOLOGY`, `ORIGIN_QEMU_IOMMU`; research ledger `docs/research/2026-11-08-x2apic-above-255.md`; ADR-0008 (Proposed) |
| (records) | provenance, STATUS, TASKS, this section |

The machine: `-smp 1,maxcpus=150,sockets=2,dies=3,modules=5,cores=5,threads=1` plus `-device max-x86_64-cpu,socket-id=1,die-id=0,module-id=0,core-id=0,thread-id=0`. Dies take 2 bits of the APIC ID and modules and cores 3 each, so package 1 starts at APIC ID 256. `cargo xtask boot-x2apic` shows:

```
ACPI MADT: 150 processors, 2 enabled (largest APIC ID listed 420); CPU index 0 for the boot processor (APIC ID 0), 1 for the other enabled processor
CPU 1 (APIC ID 256) online after 10641 us: own GDT, TSS and IDT, x2APIC
SMP: 2 of 2 enabled processors online; trampoline identity pages removed true
topology: 2 logical, 2 cores, 2 packages (1 threads/core, 256 logical/package); consistent true
numa: 2 nodes, 150 cpus, 3 memory ranges, 511 MiB; consistent true
scheduler: CPU 1: workers 2 and 3 counted 1157688 and 1265397, 51 context switches
TLB: 1 other processors cached 3 pages; after one single-page shootdown (9000 TSC cycles) and one full flush of two pages (82000 cycles), 3 of their 3 reads faulted and 0 saw a stale translation
wait queues: ping-pong between CPU 0 and CPU 1: 1000 round trips, 39 us each, woken by IPI
NVMe: MSI-X: 65 vectors, table in BAR0 at offset 0x2000; I/O queue 2 completes to vector 1, sent to CPU 1 (APIC ID 256) as IDT vector 0x40
NVMe: interrupt-driven: 64 reads of 8 blocks by a thread on CPU 1 (APIC ID 256), each blocking until its completion interrupt: 64 interrupts (the last on CPU 1 (APIC ID 256)), 512 of 512 blocks match, 81 us per read
NVMe: CPU 1 (APIC ID 256): 32 reads through its own queue pair, 256 of 256 blocks match, its write read back true, 35 interrupts on CPU 1
x2APIC: CPU 1 (APIC ID 256) is above 255, which a compatibility-format MSI address cannot carry (refused true); its NVMe MSI-X entry 3 holds only remappable handle 2, whose IRTE sends vector 0x40 to destination 256 after checking source ID 0x0010: true; 99 NVMe interrupts arrived there, and its threads blocked 98 times for a completion and were woken 98 times by an interrupt taken there; consistent true
… receive thread on CPU 1 (APIC ID 256): slept 11, woken by the NIC's interrupt 11 (of 17 MSI-X interrupts, the last on CPU 1 (APIC ID 256)); consistent true
```

Negative controls, each failing `boot-x2apic` and then restored:
- **X1**, an 8-bit IRTE destination: the interrupts land on `CPU 0 (APIC ID 0)`, and the thread on APIC ID 256 is never woken.
- **X2**, `intremap=off`: the kernel refuses to program APIC ID 256 (`APIC IDs above 255 need interrupt remapping`).
- **X3**, a wrong APIC ID on record: the AP refuses its index and halts (`a different processor answered`).

`boot-matrix` (1, 2, 4) and `boot-test` pass. `cargo xtask check` passes: 550 tests, 0 failures, 6 ignored.

#### Lessons

- **QEMU 11.1 under TCG does not require interrupt remapping for APIC IDs above 255.** The check in `x86_cpus_init` is KVM-only; only the x2APIC CPU feature is enforced. A QEMU machine can therefore have processors that no MSI can reach on hardware. The kernel's refusal (compatibility format, 8 bits) is what stops it, and control X2 proves that refusal.
- **The MADT and SRAT list every *possible* processor**, here 150 for 2 present. Any fixed-size discovery buffer breaks on a sparse or hot-pluggable machine, so the lists are now sized from the table. QEMU never sets "online capable" and flags every SRAT entry enabled.
- **Prove delivery by the wake-ups, not only by the counts.** The x2APIC proof requires every block of a thread on the processor to have been ended by an interrupt taken there (blocks = wake-ups). A misrouted interrupt is still counted somewhere, but it wakes nobody.
- **A newtype found every call site.** Replacing `cpu_id()` (an APIC ID used as an index) with `this_cpu() -> CpuIndex` made the compiler list every place that mixed the two. The refactor compiled, and passed `boot-x2apic` and the matrix, on its first run.

#### Next, for the following session

- **Interrupts:** I/O APIC entries in remappable format, with destinations by APIC ID (still 8-bit physical today). This is in progress in a sibling session.
- **Per-CPU state:** sized at discovery instead of 64 static slots (ADR-0008 C6); CPU hot-plug (MADT "online capable"); CPUID leaf 1Fh topology for §27.
- **AP start-up, an older hazard noticed here:** an AP that answers only after its 200 ms timeout would run on the trampoline data the boot processor has already rewritten for the next AP (stack, entry, argument). The new identity check makes such an AP refuse and halt, but only after it has run on the shared stack. Send INIT to a silent AP before the page is reused, or give each AP its own trampoline data.
- **The owner:** ADR-0008's three questions; ADR-0007's five; whether `boot-x2apic` joins `boot-matrix`.

### Continued (2026-11-08, ~12:00–12:30 IST): parking an AP that misses its handshake

The section above listed an older AP start-up hazard under *Next*: an AP that misses its 200 ms handshake may only be slow, and could later run the trampoline with the data the boot processor had already rewritten for the next AP. This stretch fixed it, in another session's worktree: **branch `claude/priceless-cartwright-2c1803`**, fast-forwarded to `claude/eloquent-kapitsa-840594` (`91688a1`) before any work. Its commits are **local and not pushed**.

| Commit | What |
|---|---|
| `e430ee5` | `framekernel::apstart` (7 host tests, including the race on real threads); `smp`: `wake`, `park`, the settled handshake in `start_one` and `ap_entry`, and the park drill; xtask: the drill's markers; ADR-0008 C7 (Proposed) |
| (records) | provenance, STATUS, this section |

**The choice.** INIT-park was chosen over per-AP trampoline data. A timed-out AP must be stopped in any case: otherwise it runs on after the trampoline's identity mapping is gone and triple-faults. INIT is the MP protocol's own way to stop it. Per-AP data stays with parallel start-up, and would also close the residual risk (an SMI longer than 10 ms on a timed-out AP). ADR-0003 K6's design is unchanged; ADR-0008 C7 records the failure path and asks the owner.

On every boot with an AP (here `boot-x2apic`):

```
SMP: park drill: CPU 1 (APIC ID 256) ran a counting loop from the trampoline (count 252 at INIT); 10 ms after INIT, its count moved 0 in 1000 us: parked true
CPU 1 (APIC ID 256) online after 10677 us: own GDT, TSS and IDT, x2APIC
```

The test-only slow AP (APIC ID 2 waits in the trampoline until its data page is rewritten for another AP; 4 CPUs) shows the hazard and the fix:

```
before (91688a1):
CPU 2 (APIC ID 2): no answer within 200 ms: AP start-up failed
CPU 3 (APIC ID 3): a different processor answered: AP start-up failed
SMP: control S: the slow processor (APIC ID 2) read the data page after it was rewritten 1 times, and went on with the start record of CPU 3 (APIC ID 3)
after:
CPU 2 (APIC ID 2): no answer within 200 ms; parked by INIT: AP start-up failed
CPU 3 (APIC ID 3) online after 10114 us: own GDT, TSS and IDT, x2APIC
SMP: control S: the slow processor (APIC ID 2) read the data page after it was rewritten 0 times
```

Controls: N2 (`park` without INIT) fails the drill: `its count moved 5189 in 1000 us: parked false; … no AP is started`. N3 (no park after a missed handshake) brings the hazard back. The hook, the capture script and the logs are in the shared notes folder of this group of sessions, not in the repository.

`cargo xtask check` passes: 557 tests, 0 failures, 6 ignored. So do `boot-test`, `boot-matrix` (1, 2, 4) and `boot-x2apic`.

#### Lessons

- **A race test must show it raced.** The first version released both threads from a `Barrier`, and the AP thread won 1994–2000 of 2000 rounds: the last thread to arrive runs on while the other one is woken. Spinning both threads to the start, with random jitter, split the rounds about 52/48. The test now asserts that both kinds of winner occur.
- **Make the slow AP wait for an event, not for a time.** It stalls until the boot processor rewrites its argument, the exact moment of the hazard. So the control is deterministic under TCG, whatever the host's load.
- **`boot-test` stops at the first failure line.** A control whose interesting lines come later needs the whole log. `cargo xtask run`, with the serial output redirected and QEMU stopped by a script, gives that.

#### Next, for the following session

- **AP start-up:** per-AP trampoline data together with parallel start-up (ADR-0003 K6); move the PAT and global-page counts after the handshake, so an AP parked after reaching them is not counted (seen in N1 and N3); optionally retry a parked AP once after the others; and decide whether loader memory may be reclaimed when the only processors not online are parked (`online.complete` stays false today).
- **The owner:** ADR-0008's four questions (C7 is the fourth).
