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
| `2ca1af2` | records: provenance, STATUS, this section |
| `7a1200c` | the PAT and global-page checks count online processors only: one flag per CPU index (`cpu`, `tlb`, `pcie`, `main`); controls C1–C3 |
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

**Counts of processors.** `ap_entry` added to two global counters, PAT-ready and global pages, before its handshake. So an AP parked after reaching them was still counted, and both checks failed. Control C1, with an AP stalled after both checks: `global pages on 4 of 3` and `PAT entry 3 is uncached on only 4 of 3 processors: PCIe failed`, so no NVMe or network ran. Each is now one flag per CPU index, counted over the online processors (C2: `3 of 3`, and the boot carries on). C3 (APs never mark) still fails both checks.

#### Lessons

- **A race test must show it raced.** The first version released both threads from a `Barrier`, and the AP thread won 1994–2000 of 2000 rounds: the last thread to arrive runs on while the other one is woken. Spinning both threads to the start, with random jitter, split the rounds about 52/48. The test now asserts that both kinds of winner occur.
- **Make the slow AP wait for an event, not for a time.** It stalls until the boot processor rewrites its argument, the exact moment of the hazard. So the control is deterministic under TCG, whatever the host's load.
- **Count per-CPU facts per CPU index, over the online set.** A global counter of processors that did X counts every processor that got that far, online or not. A flag per index, read for the online indexes, cannot.
- **`boot-test` stops at the first failure line.** A control whose interesting lines come later needs the whole log. `cargo xtask run`, with the serial output redirected and QEMU stopped by a script, gives that.

#### Next, for the following session

- **AP start-up:** per-AP trampoline data together with parallel start-up (ADR-0003 K6); optionally retry a parked AP once after the others; and decide whether loader memory may be reclaimed when the only processors not online are parked (`online.complete` stays false today).
- **The owner:** ADR-0008's four questions (C7 is the fourth).

### Continued (2026-11-08, ~11:15–12:05 IST): I/O APIC interrupts through VT-d interrupt remapping

The VT-d stretch above left one hole: with interrupt remapping on and compatibility format
blocked, hardware drops every I/O APIC interrupt whose redirection entry is in compatibility
format, and the only I/O APIC proof ran before the IOMMU. The IOMMU session started this one in
its own worktree, **branch `claude/bold-williams-a8e3ff`**. The worktree began at `main`'s
initial commit and was fast-forwarded to `1c790fe` (the VT-d branch tip) before any work.
Commits are **local and not pushed**.

| Commit | What |
|---|---|
| `cf0dee8` | `ioapic::RemappableEntry` (the remappable-format redirection entry), `Irte.level`, `Dmar::ioapic`; `iommu::route_ioapic_pin` (+ read-back, rewrite, release); `irq::prove_remapped`; a per-CPU count of the I/O APIC vector; xtask markers |
| (records) | provenance, ADR-0007 I8, the research ledger §2.5, STATUS, TASKS, this section |

What every boot now shows after the IOMMU proofs (at `-smp` 2; at 1 the IRTE is 4 and the target
CPU 0, at 4 the IRTE is 7 and the target CPU 1):

```
I/O APIC: remapped: the PIT's IRQ0 (GSI 2, edge-triggered, active-high) through IRTE 5: the redirection entry reads back in remappable format with that index, vector 0x41 and mask clear: true; the IRTE reads back vector 0x41 to CPU 1, edge-triggered like the pin, checking source ID 0xff00 (I/O APIC 0's, from the DMAR): true; the two agree: true; 0 earlier fault(s) cleared
I/O APIC: remapped: 20 PIT interrupts arrived on CPU 1 through IRTE 5 in 200 ms, 0 on other CPUs; fault recorded: none; consistent true
I/O APIC: IRTE 5 not present: 0 interrupts arrived in 100 ms; fault recorded: source ID 0xffff, interrupt index 5, reason 0x22 (interrupt table entry not present), from a message without a requester ID (the DMAR names 0xff00); IRTE restored, 5 interrupts arrived on CPU 1 again: true; consistent true
I/O APIC: IRTE 5 checking source ID 0xff01 instead of 0xff00: 10 interrupts arrived in 100 ms; fault recorded: none; the source-ID check cannot be shown on this platform: … (hardware blocks them, reason 0x26); IRTE restored, 5 interrupts arrived on CPU 1 again: true; consistent true
I/O APIC: remapped: the entry masked, the PIT stopped and IRTE 5 released: true; 0 fault(s) recorded after
```

Negative controls, each failing `cargo xtask boot-test` and then restored from git:
- **N1**, the entry left in compatibility format: the entry read-back fails.
- **N1b**, the same with the read-backs disabled: QEMU delivers all 20 interrupts to CPU 1 anyway,
  and the not-present probe catches it (10 arrive with the IRTE cleared, no fault).
- **N2**, the pin's IRTE never written: the IRTE read-back fails.
- **N2b**, the same with that read-back disabled: 0 interrupts in 2 s, and the unit records
  reason 0x22 for index 5 — with source ID 0xffff, not 0xff00 (below).
- **N3**, the not-present probe leaves the IRTE present: 10 interrupts arrive, no fault.
- **N4**, the IRTE checks source ID 0xff01: the IRTE read-back against the DMAR fails.
- **N5**, the IRTE level-triggered for an edge pin: the IRTE read-back and the agreement fail.

One variant **passed**, and is recorded as a QEMU blind spot: **N4b**, the wrong source ID with
the read-back disabled. All 20 interrupts arrive through an IRTE that checks 0xff01, because
QEMU's I/O APIC sends no source ID to check. Reason 0x26 needs hardware.

**Checkpoint (~12:05 IST):**
- `cargo xtask check` passes: 549 tests, 0 failures, 6 ignored (the download-gated vectors);
  308 framekernel host library tests, 7 of them new.
- `boot-test` and `boot-matrix` (1/2/4) pass on the committed code; the kernel boots in
  15.0 s, 16.6 s and 20.6 s.
- QEMU-only (§74): nothing is qualified on physical hardware.

#### Lessons from this stretch

- **QEMU's I/O APIC sends no requester ID.** It writes its messages with
  `MEMTXATTRS_UNSPECIFIED` (`hw/intc/ioapic.c`), so QEMU's VT-d model sees source ID 0xffff:
  it records I/O APIC faults as 0xffff and skips the IRTE's source-ID check for them
  (`hw/i386/intel_iommu.c`, `vtd_irte_get`). Reason 0x26 and the DMAR's 0xff00 in a fault
  cannot be shown for I/O APIC interrupts under QEMU. The task as given expected both; this is
  the third VT-d check that needs hardware, after CFI blocking and the IEC invalidation.
  Read the device model before promising a control.
- **A read-back is not a delivery proof; a blocked probe is.** Under QEMU a
  compatibility-format entry delivers whatever the IRTEs say, and an encoding slip in the
  remappable format could leave a compatibility-format message that still arrives (on CPU 0 if
  the index bits land in the destination). Clearing the IRTE and seeing the interrupts stop,
  then resume when it is restored, is what shows they went through it. Targeting another CPU
  than the boot processor guards the same way at `-smp` ≥ 2.
- **xtask stops at the first failure marker,** so a control that breaks an early check never
  reaches the later ones. To show what a later check catches on its own, disable the earlier
  check in the same control (N1b, N2b, N4b).
- **Coordinate per-CPU state with the CPU-index work.** The x2APIC session (branch
  `claude/eloquent-kapitsa-840594`) replaces `cpu_id()` with a dense `CpuIndex`. The new
  `IOAPIC_IRQS_ON` count is indexed by x2APIC ID and must follow that change when the branches
  meet (noted in the shared session notes).

#### Next, for the following session

- **The keyboard:** route IRQ1 through `iommu::route_ioapic_pin` and a remappable entry (or a
  compatibility-format one when interrupts are not remapped), and make the i8042 driver
  interrupt-driven: live keystrokes.
- **A level-triggered pin:** PCI INTx through the I/O APIC (active-low, level), with the EOI
  path that clears the pin's remote IRR (the entry's vector equals the IRTE's; host-tested).
- **More I/O APICs and HPET MSIs** (the DMAR's HPET scopes).
- **Hardware (§74):** the source-ID check on I/O APIC interrupts (reason 0x26, source ID from
  the DMAR), CFI blocking, and the IEC invalidation.
- Everything else in the VT-d stretch's list above still stands (AMD-Vi, RMRR identity maps,
  scalable mode, the CPU-index work, a fault-event interrupt).

### Continued (2026-11-08, afternoon IST): AMD-Vi DMA and interrupt remapping

The VT-d session named AMD-Vi as the next, separate step (§32 asks for both IOMMUs), and started
this work in its own worktree, **branch `claude/upbeat-dijkstra-0d25fe`**. That branch began at
`main`'s initial commit and was fast-forwarded to the VT-d branch's tip, `1c790fe`, before any
work. Its commits are **local and not pushed**.

| Commit | What |
|---|---|
| `313fa35` | research (`docs/research/2026-11-08-amd-vi-iommu.md`); `ivrs`, the ACPI IVRS parser; `amdvi`, the AMD-Vi encodings and host-table walker (14 host tests, a mutation soak); PCI class 08:06 named |
| `6e2426a` | `iommu::amd`, behind the existing `iommu` entry points; the neutral `iommu::Fault` and `iommu::route` for the proofs; xtask `ORIGIN_QEMU_IOMMU=amd`, its markers, QEMU-stderr checks, and `boot-matrix` over both IOMMUs |
| (records) | ADR-0007's AMD-Vi section (Proposed), provenance, STATUS, TASKS, this section |

What the AMD-Vi boot shows (at `-smp` 2):

```
IOMMU: IVRS revision 1, IVinfo 0x2801 (...); 1 IOMMU(s) in 2 IVHD(s), 0 memory definition(s)
IOMMU: unit 0 at 0xfed80000 (segment 0, IVHD type 0x11 of 2, IOMMU function 00:05.0, capability at 0x40): 9 device entries up to DeviceID 0x00fb, I/O APIC 0 (DeviceID 0x00a0)
IOMMU: unit 0: device table of 256 entries at 0x…, every entry blocking DMA and fixed interrupts; …; interrupt remapping per device (128-bit entries, 32-bit x2APIC destinations); translation on (control 0x4000000021007, status 0x18): …
IOMMU: translated DMA: the NVMe controller (00:02.0, source ID 0x0010) read LBA 2048 into IOVA 0x40012000, which its domain maps to physical 0x76000 (not the same address): 8 of 8 blocks landed in that physical page; consistent true
IOMMU: blocked DMA: IOVA 0x40012000 unmapped (…), … the page kept its canary true; fault recorded: DeviceID 0x0010, access at 0x0, domain 0, event 0x2 (I/O page fault: page not present); consistent true
IOMMU: interrupt remapping: 5 of 5 MSI-X entries (NVMe 3, virtio-net 2) hold only an index into their device's own interrupt remapping table, …; APIC ID 300 accepted (its IRTE reads back destination 300), …; consistent true
IOMMU: blocked interrupt: NVMe MSI-X vector 1 pointed at IRTE 3, left not present: … 0 interrupts arrived; fault recorded: none; vector 1 restored; consistent true
```

Negative controls, each failing `ORIGIN_QEMU_IOMMU=amd cargo xtask boot-test`, then restored from
git: N1 no page invalidation on unmap (the canary is overwritten, no fault); N2 IOVAs never
mapped (an NVMe command times out); N3 virtio-net never attached (no ARP, no lease); N4 the
DTE never routes interrupts to the device's table (no NVMe interrupt; QEMU reports the refusal);
N5 every IRTE's vector one too high (no NVMe interrupt); N6 the probe IRTE left present (1
interrupt arrives); N7 no x2APIC mode (32-bit IRTEs carry every interrupt, but APIC ID 300 is
refused). Probes N8 and N9 are in the provenance entry.

#### Lessons from this stretch

- **QEMU 11.1's `amd-iommu` translates nothing unless `dma-remap=on`.** The first boot brought the
  IOMMU up perfectly and then timed out the NVMe: its DMA went untranslated to unbacked IOVAs.
  Making translation visible (the VT-d lesson) is what exposed it at once. And even with
  `dma-remap=on`, QEMU puts a device under translation only on an INVALIDATE_DEVTAB_ENTRY for
  it, so the kernel invalidates every entry at bring-up.
- **Read the device model's events, do not assume them.** QEMU writes every event with
  address 0 (undefined behaviour in its encoder, fixed only after 11.1) and sets the
  "interrupt" flag on DMA faults; it logs no interrupt-remapping fault at all. The kernel
  decodes by the specification, handles each deviation narrowly, and the boot test pins
  QEMU's behaviour so a change is noticed.
- **Prefer the encoding both the specification and the model enforce.** Blocking with Mode 0
  and IR=IW=0 is correct by the specification but QEMU passes it through; V=1, TV=0 blocks in
  both.
- **Pin the IOMMU's PCI function** (`-device AMDVI-PCI,…,addr=05.0` with `pci-id`): unpinned it
  takes 00:02.0 and moves every device after it.
- **A second agent driving the device model directly** (`-accel qtest` with test DMA masters)
  confirmed QEMU's behaviour independently of the kernel.

#### Next, for the following session

- **I/O APIC under AMD-Vi:** its DTE (DeviceID 0x00a0, from the IVRS special entry) blocks fixed
  interrupts. Before an I/O APIC line is used with the IOMMU on, give that DeviceID an
  interrupt table (the redirection entry's vector field then carries the IRTE index); under
  QEMU a level-triggered line also needs an explicit I/O APIC EOI.
- IVMD unity maps and exclusion ranges (and RMRR identity maps for VT-d).
- **The owner:** ADR-0007's eight questions; compare the AMD specification's hash with AMD's copy.

### Continued (2026-11-08, afternoon IST): parser hardening (TASKS item 3)

Backlog item 3, on branch **`claude/gifted-maxwell-7e8a3f`** (based on
`claude/interesting-carson-ae7c83` at `0b9c3e0`; **not pushed**). Every parser in the
host-tested library that reads firmware or device data now has a deterministic, seeded
mutation soak on `crate::testing`. The evidence is `SOURCE_PROVENANCE.md`, "Parser hardening":
the inventory, the per-parser results and the bugs.

**Result:** 55 new tests. The workspace went from 579 to 634 tests (0 failed, 6 ignored); the
`framekernel` library runs in 1.0 s (0.92 s before). A default run is about 1.3 million mutated
inputs; `ORIGIN_MUTATION_SCALE=n` multiplies it.

**Gates on the final tree:** `cargo xtask check`; `cargo xtask boot-test` (both IOMMUs); `cargo
xtask boot-matrix` (Intel VT-d and AMD-Vi at 1, 2 and 4 CPUs). `ORIGIN_MUTATION_SCALE=10` (debug,
overflow checks on) and `ORIGIN_MUTATION_SCALE=100` (release) both pass in seconds.

**The 13 library bugs** (each with a regression test shown to fail without its fix):
`cache::decode` (`sub[2] + 1` wrapped at `ECX = FFFFFFFFh`); `originfs` (`inode_table_start +
table_blocks`, behind the CRC that kept the old soak out); `virtqueue::free_chain` and
`FreeList` (a device-reported id panicked or double-freed); `virtio::find_caps` (read past 100h,
took the last instance, took short capabilities and reserved BARs); `nvme::identify_namespace`
(a byte size over 64 bits); `pci::msix` (a capability at F8h or FCh); `gpt` (a header inside the
usable range); `ustar` (a directory's size swallowed the next header); `dns` (a name past its
RDLENGTH); `net::checksum` (a `u32` sum); `amdvi::Irte::decode_*` (vectors below 16);
`numa::self_is_nearest` (more than 256 localities); `acpi::table_length` (no upper bound).

**The kernel side** (fixed through the same library checks): a virtio structure or MSI-X table
outside its memory BAR, or in an I/O BAR; a notification register outside its structure; a queue
size that is not a power of two, or too short for the receive buffers; NVMe doorbells past BAR 0;
blocks larger than a page.

#### What to know

- `virtio::find_caps` now takes a second argument, `usable`, a predicate on each structure's
  location (the kernel passes "inside a memory BAR"): a device may list an I/O-BAR instance
  first (virtio 1.1 §4.1.4.1).
- `virtqueue::FreeList::free` returns `bool` and `free_chain` returns the count freed; a
  repeated or out-of-range index is refused rather than stored.
- `pci::Msix::table_address`, `virtio::CfgLocation::address_in` and `notify_offset`,
  `virtqueue::queue_size`, `nvme::doorbells_fit` and `NamespaceInfo::block_fits_page` are the
  checks a future driver must use: every value they take is device-supplied.
- `acpi::table_length` refuses anything over `MAX_TABLE_LENGTH` (16 MiB).
- When adding a parser: re-seal any checksum after mutating (a checksummed structure otherwise
  hides what is behind it from the soak) and, for a device model, assert the access bounds in
  the model itself. `crate::testing` has `Rng::edge_u64` and `mutate_window` for this.

#### Not done

- Coverage-guided fuzzing and Kani bounds (nightly, §12, §73); a multi-fault soak over the ONBF
  parsers (they have exhaustive single-fault sweeps, and the brief said not to redo them).
- The malformed-device paths in the kernel binary are host-tested through the library
  checks but can only be shown live on a fault-injecting device model or hardware (§74): QEMU
  will not present a capability outside its BAR or a queue size of 3.

#### Next

Backlog item 4 (M3 scheduling proper). The owner: the questions that were already open in this
file (ADR-0007, ADR-0008, the VT-d PDF, the AMD specification's hash) are unchanged.

### Continued (2026-11-08, afternoon IST): a multi-fault soak over the ONBF parsers

The follow-up to parser hardening, on branch **`claude/heuristic-chandrasekhar-6a46a1`** (on
`claude/gifted-maxwell-7e8a3f` at `62dc630`; **not pushed**). The ONBF parsers (`onbf-format`,
`onbf-merkle`, `onbf-verify`) now have seeded multi-fault soaks on a test-only helper,
`runtime/onbf/format/src/testing.rs`, which the other two crates include by path.

Evidence: `SOURCE_PROVENANCE.md`, "Seeded multi-fault mutation soaks over the ONBF parsers":
the inventory, the five bugs, 30 negative controls.

**Result:** 30 new tests (workspace 634 → 664, 0 failed, 6 ignored). A default run adds about 1 s
to `cargo xtask check`. Scale runs: 30x debug and 100x release, 0 failures.

**The five bugs** (each with a regression test shown to fail with its fix reverted):
- a MAPPABLE MERKLE section accepted (errata C8);
- a MAPPABLE DWARF section accepted (errata G1);
- a certificate chain of more than four accepted (Appendix A);
- a statement `version` that is not SemVer 2.0 accepted (S3);
- a block over 1 MiB accepted by `SignatureBlock::parse` when called directly (Appendix A).

None was a panic or an out-of-bounds read: every one of these crates is panic-free by lint and
by the object check, so what a soak finds here is a rule of the specification not enforced.

#### What to know

- `SectionError` gained `MappableNotAllowed`; `SignatureError` gained `TooLarge`;
  `signature::MAX_CHAIN` is new. `TrustCache`'s documentation now says an entry belongs to one
  tree (its MERKLE bytes and content size): the soak showed a warm cache given another tree's
  shape can accept, which is the caller's contract, not a forgery path.
- When adding an ONBF check, name a refusal in the soak's `*_REFUSALS` list and its model, and the
  floor keeps the check reached. To see what each soak reached, run with
  `ORIGIN_MUTATION_REPORT=1 ... -- --nocapture`.
- A multi-fault soak needs layouts rebuilt after each fault, and sometimes composite faults: a
  flipped MAPPABLE bit only counts once the layout is canonical again, and the DWARF rule needed
  two fields changed together, which did not happen by chance in 24,000 cases.

#### Open for the owner

- Errata **O1**: a statement's `id` must be NFC, and the parser has no normalization tables
  (R4 avoided them for the manifest with ASCII-only text). Today `id` is checked only as non-empty
  valid UTF-8. Decide: ASCII only, or tables in the parser.

#### Not done

- Coverage-guided fuzzing, Kani bounds and the N-version second parser (§13 items 3, 5, 6).

#### Next

Backlog item 4 (M3 scheduling proper). The owner's questions from earlier sections stand.

### Continued (2026-11-08, ~12:25–12:45 IST): the IOMMU follow-ups merged

The owner asked the VT-d session to merge its four follow-up branches into its own branch,
**`claude/interesting-carson-ae7c83`**, and run every gate. **Nothing is pushed.**

| Commit | What |
|---|---|
| fast-forward to `db16a9c` | the CPU-index work (`claude/eloquent-kapitsa-840594`) and AP parking (`claude/priceless-cartwright-2c1803`) |
| `56c6c90` | merge of the I/O APIC branch (`claude/bold-williams-a8e3ff`), ported to CPU indexes |
| `3aa34e4` | merge of AMD-Vi (`claude/upbeat-dijkstra-0d25fe`): the I/O APIC proof on the neutral fault record and skipped under AMD-Vi; `ORIGIN_QEMU_IOMMU` reconciled |
| (records) | provenance ("Integration: the IOMMU follow-ups merged"), STATUS, this section |

Gates on the final tree (`3aa34e4`):
- `cargo xtask check`: 579 tests, 0 failures, 6 ignored.
- `boot-matrix`: Intel VT-d and AMD-Vi, each at 1, 2 and 4 CPUs.
- `boot-x2apic`: CPU 1, at APIC ID 256, now also takes the I/O APIC's IRQ0 through its IRTE (20 interrupts in 201 ms, none elsewhere).
- Controls: `ORIGIN_QEMU_IOMMU=intel-iommu,intremap=off` still fails `boot-x2apic`, and a `virtio-iommu-pci` value is refused before QEMU starts.

After the first merge, `check` failed once on formatting: the call to `route_ioapic_pin` fit on one line once it took `apic`. The merged AMD-Vi marker test then failed once on a wrong count in my own new assertion. Both are fixed, and nothing else failed.

#### What to know

- **`ORIGIN_QEMU_IOMMU`:** `intel` (default), `amd`, or an `intel-iommu` `-device` value booted
  with the VT-d markers (for negative controls); anything else is refused.
- **I/O APIC pins:** routed through IRTEs under VT-d only. Under AMD-Vi
  `iommu::routes_ioapic_pins()` is false, `irq::prove_remapped` says so, and the pins stay
  masked. The keyboard IRQ1 must check `routes_ioapic_pins()` first.
- **Under `boot-x2apic`** the I/O APIC proof targets CPU 1, at APIC ID 256.
- The four branches stay as they were. This branch contains all of them.

#### Next

- An interrupt table for the I/O APIC's DeviceID under AMD-Vi (the AMD-Vi section above has the
  recipe), then the keyboard IRQ1 through `iommu::route_ioapic_pin` under both IOMMUs.
- **The owner:** ADR-0007's eight questions and ADR-0008's four; Intel's VT-d PDF download;
  a check of the AMD specification's hash against AMD's copy.


### Continued (2026-11-08, ~13:45 IST): CPU topology from CPUID (dies, modules, core types)

The next gap in the 2026+ hardware audit above: only CPUID leaf 0Bh was read. This branch, **`claude/zealous-lovelace-301ef1`**, was fast-forwarded from `claude/interesting-carson-ae7c83` (`0b9c3e0`) and is **not pushed**.

| Commit | What |
|---|---|
| `d1984cd` | the decoder and its host tests, per-CPU records by `CpuIndex`, the boot proof with its xtask markers, the research ledger |
| (the commit after it) | records: provenance, STATUS, TASKS, ADR-0008's consequence, this section |

What it adds:
- `framekernel::topology`: `CpuTopology::detect` reads leaves 80000026h, 1Fh and 0Bh (each only below its maximum) into one `Layout`, from thread to package, with core types as raw codes. Pure, host-tested (19 tests).
- Every processor records itself on itself: `cpu::record_topology`, read back by `cpu::topology_of(CpuIndex)`.
- `prove_topology` reports the layout, each processor's package, die, module, core and thread, the census of everything the MADT lists, and the checks against the MADT.

What to know:
- **Leaf 1Fh is not reachable under the default `-cpu max`.** QEMU's TCG vendor there is AMD, and it raises the CPUID maximum to 1Fh only for an Intel model. `boot-x2apic` therefore boots twice: `-cpu max,vendor=GenuineIntel` (leaf 1Fh) and `-cpu max` (leaf 0Bh). The `boot-x2apic` machine also has a new third processor at APIC ID 412 (package 1, die 2, module 3, core 4), so a die/module/core confusion shows.
- **A CPUID read above the maximum returns the highest basic leaf's data, not zeros** (Intel documents this; QEMU does it). So leaf 80000026h on the Intel-vendor model reads as a valid-looking copy of leaf 1Fh, with a different meaning of the shift. The decoder gates every leaf; a host test reproduces it.
- **QEMU 11.1 cannot show:** leaf 80000026h, leaf 1Ah and the hybrid flag (no model has them), mixed SMT widths, tiles or die groups. The boot prints `hybrid false` under QEMU, which says nothing about hybrid processors. AMD's leaf and core types are host-tested only (§74). Details: `docs/research/2026-11-08-cpu-topology-leaf-1fh.md`.

Gates on the final tree: `cargo xtask check`: 596 tests, 0 failures, 6 ignored (579 before); `boot-test`; `boot-matrix` (Intel VT-d and AMD-Vi, each at 1, 2 and 4 CPUs); `boot-x2apic` on both CPU models. Negative controls C1–C10 each fail the boot and were restored (`SOURCE_PROVENANCE.md`, "CPU topology from CPUID leaves 1Fh, 0Bh and 80000026h").

#### Next

- §27 proper: use the topology for placement (SMT siblings, shared caches, dies), and hybrid-core *capacity*, which no leaf read here gives (Intel's CPUID.06H feedback interface and ACPI CPPC are the candidates; AMD's efficiency ranking is decoded already).
- Hardware qualification (§74): a real multi-die, hybrid and AMD machine must confirm leaf 1Fh, 1Ah and 80000026h, including whether the thread field really differs between core types.
- **The owner:** compare the AMD manual's SHA-256 (in the ledger) with AMD's copy.

### Continued (2026-11-08, ~14:15–14:25 IST): parser hardening, the ONBF soak and CPU topology merged

The owner asked the VT-d session to merge the three newest branches into its own,
**`claude/interesting-carson-ae7c83`**, and run every gate. **Nothing is pushed.**

| Commit | What |
|---|---|
| fast-forward to `6a50bf5` | parser hardening (`claude/gifted-maxwell-7e8a3f`) and the ONBF multi-fault soak (`claude/heuristic-chandrasekhar-6a46a1`) |
| `cdcdde7` | merge of CPU topology (`claude/zealous-lovelace-301ef1`), with parser hardening's topology soak ported to the new decoder |
| (records) | provenance ("Integration: parser hardening, the ONBF soak and CPU topology merged"), STATUS, this section |

Gates on the final tree:
- `cargo xtask check`: 681 tests, 0 failures, 6 ignored.
- `boot-matrix`: Intel VT-d and AMD-Vi, each at 1, 2 and 4 CPUs.
- `boot-x2apic`: processors at APIC IDs 256 and 412, under leaves 1Fh and 0Bh.
- Controls: `ORIGIN_QEMU_IOMMU=intel-iommu,intremap=off` still fails `boot-x2apic`, and a `virtio-iommu-pci` value is refused before QEMU starts.

#### What to know

- **The textual merge was not enough.** Git merged `topology.rs` without a conflict, but the soak it kept called a decoder the other branch had removed. It is now `hostile_cpuid_always_gives_a_layout_that_partitions_ids`, over `CpuTopology::detect`, and eight negative controls show it catches a broken decoder.
- The branches named above stay as they were. This branch contains all of them.

#### Next

- **The owner:** ADR-0007's eight questions and ADR-0008's four; errata O1 (the signature statement's `id`: ASCII or NFC tables); Intel's VT-d PDF download; the two AMD manuals' hashes against AMD's copies.

### Continued (2026-11-08, ~15:00 IST): CLAUDE.md requirements for the Proposed ADRs (documents only)

The owner asked whether the decision sheet's recommendations had been checked against CLAUDE.md. They had not, fully, so the sheet (`docs/engineering/OWNER_DECISIONS.md`) was corrected, and its steps A–C were done on `claude/interesting-carson-ae7c83`. **Nothing is pushed, and no code changed.**

- **§18:** trade-offs and migration consequences added to ADR-0006, 0007 and 0008; alternatives added to ADR-0006.
- **§16, §40:** `docs/research/2026-11-08-originfs-design.md`. OriginFS v0 cannot grow into the filesystem §40 requires. New proposed decision F7: v0 is a bring-up format that never ships in an approved release. F3's "the rest reserved zero" was wrong (the inode is full) and is corrected.
- **§58:** `docs/engineering/THREAT_MODEL_IOMMU.md`, the repository's first threat model. 24 threats, 9 still open.

#### Next

- **Before real hardware:** the threat model's T4–T6 (early-boot DMA, firmware-enabled units, failing open), T8–T9 (ACS), T14 (RMRR identity maps) and T23 (nobody reads runtime faults).
- M4's system-call boundary needs its own threat model (§58).
- **The owner:** the sheet. ADR-0007 also waits on the spec checks (items 18 and 23).

### Continued (2026-11-08, ~14:55 IST): the owner's answers applied

The owner accepted the sheet's recommendations for items 1–6, 8, 9 and 13–20 (`docs/engineering/OWNER_DECISIONS.md`). On `claude/interesting-carson-ae7c83`, **not pushed**:

- **ADR-0006 and ADR-0008 are Accepted.** ADR-0007 records its answers to questions 1, 2, 4 and 5, and stays Proposed.
- **`boot-matrix` ends with `boot-x2apic`.** Item 15.
- **A signature statement's `id` must be non-empty printable ASCII.** Item 17, errata O1, enforced in `SignatureBlock::parse`, with a host test and the soak's model updated. With the check removed, both fail.
- **Intel's VT-d Rev 5.20 PDF is downloaded,** outside the repository; its hash is in the VT-d ledger. Legacy mode, translation type 0 and the blocking of compatibility-format interrupts match the text. Every encoding has still to be checked against it.
- **Items 19 and 20 are approved, not fetched:** each file's source and size are confirmed when the work that uses them starts.

**A flaky boot proof.** Two of three full `boot-matrix` runs failed with `clock: the RTC did not complete two updates in 1.2 s`, while other sessions were loading the machine. Each configuration then passed alone, and a third full run passed. `time.rs`'s `update_end` must see QEMU's RTC update bit, which is set for only 244 µs each second, so a descheduled virtual CPU can miss it. Parallel sessions make this likely. It is not fixed.

#### Next

- Check every VT-d encoding against Rev 5.20, then ADR-0007's question 3.
- ~~Make the RTC wait in `time.rs` robust under host load.~~ Done; see the next section.
- **The owner:** the AMD manuals' hashes (item 23), then ADR-0007's questions 6–8; the licence, the push, the hardware (items 21, 22, 24).

### Continued (2026-11-08, ~15:25 IST): every VT-d encoding checked against Rev 5.20

On `claude/interesting-carson-ae7c83`, **not pushed**. The table is the last section of `docs/research/2026-11-08-vt-d-iommu.md`. Everything else matches. Corrected: the fault record's interrupt index for five reasons, the fault-event mask, DRHD alignment, `DMA_REMAP_OPT_OUT`, fault-reason names, reserved GCMD names, chapter references.

**A correction to an earlier claim:** legacy mode needs second-stage translation, and a newer unit reporting `ECAP.SSTS` clear has none. The kernel refuses such a unit (threat T6).

#### Next

- **Before real hardware:** protected memory regions (`PMEN.EPM`); the threat model's list.
- **The owner:** item 23 (then 10–12); items 21, 22 and 24. Items 7 and 25 were accepted on 2026-11-08.

### Continued (2026-11-08, ~15:00–15:50 IST): external known-answer vectors for `origin-crypto` (CRYPTO-KAT)

On branch **`claude/amazing-einstein-4eb320`** (on `b3efe8a`, not pushed): owner decision 19's downloads, fetched after each file's name, source and size were confirmed with the owner. Evidence: `SOURCE_PROVENANCE.md`, "External known-answer vectors" (sources, hashes, the exceptions, the errata table, the planted faults).

- **`security/crypto/tests/kat/`** runs 3,882 vectors through the public API: NIST ACVP for SHA-256, SHA-512, SHA3-256, SHA3-512, SHAKE128, SHAKE256 and ML-DSA-65/87 signature verification, and all 210 of Wycheproof's Ed25519. All pass. `cargo xtask check` picks them up through `cargo test --package origin-crypto`, about 4 s more.
- **The data** (`tests/vectors/acvp/`, `tests/vectors/wycheproof/`, 6.5 MB) is the applicable subset, converted by `tools/convert_kat_vectors.py` from upstream files at pinned commits and SHA-256 hashes, and committed with NIST's notice and Apache-2.0's `LICENSE`, because the public repository needs both to redistribute them.
- **`ORIGIN_KAT_SCALE`** is the largest large-data vector in GiB (default 1; 8 runs all 16, about 1 min; 0 none).
- **Errata:** the FIPS 204 errata sheet's 12 entries are recorded; one check changed (`UseHint`'s upper bound, asserted over all of Z_q).

#### What to know

- ACVP's two SHAKE revisions encode a partial last output byte differently: revision 1.0 keeps the bits in place, FIPS202 moves them to the high-order end. The tests carry both (`PartialByte`).
- The library is untouched, on purpose: the signed-boot session uses it. `Verify_internal` has no public entry point, so ACVP's internal-interface vectors don't apply; if a future change exposes one, those vectors could run.

#### Not done

- An independent review of the crate (§59). The Python references and these tests have the same author as the code.

#### Next

- Merge this branch into `claude/interesting-carson-ae7c83` when the coordinator is ready; it touches only `security/crypto/` and the records (STATUS, TASKS, OWNER_DECISIONS, this file, SOURCE_PROVENANCE), so conflicts would be in the records.

### Continued (2026-11-08, ~14:30–16:45 IST): the kernel's signature, verified by the loader, and host signing in Rust (TASKS item 6)

On branch **`claude/adoring-hopper-a8f66b`** (on `claude/interesting-carson-ae7c83` at `6a2b29c`; **not pushed**). Governing: ONBF §7, ADR-0002 B2, B7, B8, S1–S4. Research and the separation of what is specified from what is open: `docs/research/2026-11-08-krn-signature-verification.md`. Evidence: `SOURCE_PROVENANCE.md`, `boot/uefi/` "Step 3" and `developer-tools/onbf-sign/`.

| What | Where |
|---|---|
| The KRN policy over a trust store: one entry, `origin-kernel`, no chain, a trusted key, both signatures | `onbf_verify::verify_krn_block` |
| The compiled-in trust store and the order of checks (B2 step 3); a message per refusal | `boot/uefi/src/trust.rs`, `kernel.rs`, `main.rs` |
| Ed25519 and ML-DSA-87 signing, original Rust, host only | `developer-tools/onbf-sign/` |
| `onbf-pack sign` and `keygen`: the key file, the statement, self-verification | `developer-tools/onbf-pack/src/sign.rs`, `main.rs` |
| The boots: 11 scenarios, each naming its loader build | `developer-tools/xtask/src/main.rs` |
| The test key: public halves only, `TEST-ONLY-…`, README | `boot/uefi/trust/` |

**Result, on the final code:** `cargo xtask check` (719 tests, 0 failures, 6 ignored), `boot-test` (11 of 11), `boot-matrix` (both IOMMUs at 1, 2 and 4 CPUs) and `boot-x2apic` pass, each on the first attempt once the host was quiet, with the signed kernel as the main boot and developer mode off. The signatures are made by `onbf-sign`, and verified by `origin-crypto` and OpenSSL. 36 negative controls were applied and restored, each failing its named check. Two fresh reviewing agents read the work: the first found no way to get a refused image accepted and six low-severity points (five fixed, the sixth recorded); the second found no signature-correctness bug in the signer and three real problems (a compiled data branch in Ed25519, gaps in wiping, a key-file overwrite), all fixed.

**The owner decided on 2026-11-08** (errata, "Decided readings of §7" and "Decided for certificate chains"): V1–V6 as recommended; host signing in **Rust** rather than OpenSSL; O2 `origin/onbf/1/okc`; O3 **expiry against signed time** (so chains wait for SYS); O4 **exact IDs only**; O5 bare public keys; and best-effort zeroization accepted for host development keys (no `unsafe` volatile wipe; release keys belong in an HSM).

#### What to know

- **Three loader builds.** No feature: the trust store is empty and every kernel is refused. `test-trust-store`: the test key is trusted, developer mode is off; it is what `boot-test` boots. `developer-mode`: includes the test key and also accepts unsigned images. The two builds with the test key do not compile with a non-zero build number in `origin-version.toml`.
- **`onbf-sign` is host only.** `cargo xtask check` fails if the loader or the kernel can reach it, over every target and feature. It also counts the conditional branches of `ed25519::sign` in the release assembly: LLVM turned the reduction's mask into data branches once, `black_box` stopped it, and the check keeps it stopped.
- `onbf-pack keygen` writes one key file (`--key`, mode 0600, magic `ONBFKEY1`, or `ONBFTKEY` for a key from `--insecure-test-seed`) and two public-key files, and never overwrites anything. `sign` takes `--key` and `--signing-time`, refuses a key file others can read, and refuses `--output` equal to the key.
- OpenSSL is out of the signing path and optional: `openssl_accepts_these_signatures` uses it as an outside check when installed. Signing is hedged with fresh randomness (`/dev/urandom`), so a signature differs from run to run; the container, digest and statement stay byte-identical.
- `xtask` regenerates the test keys into `target/xtask/test-keys/` from text constants and refuses to boot if `boot/uefi/trust/` no longer matches them. The Rust key generation reproduces those public keys, which OpenSSL made from the same seeds.
- **A heavily loaded host fails the boot gates, in kernel proofs this work does not touch.** With several sessions running at once (load average 70–140), the kernel's clock proof failed (`the RTC did not complete two updates in 1.2 s`, `one RTC second lasted 742615 us`), and the timer-queue proof (`: timers failed`, which wants 2 ms lateness at most); both need QEMU to keep real time under TCG. In those boots the loader's signature line had already passed. The session [Make the RTC wait survive a loaded host](#local_aa5e8e22-45d8-4e7d-a14b-85de4eb4439a) (branch `claude/musing-hamilton-587f7b`) is fixing the RTC wait. If a gate fails with one of those lines on a busy machine, check the load before suspecting the change.
- `kernel::verify` takes the trust store as an argument (`trust::STORE` in `main`), so the host tests pass their own.
- `ORIGIN_BOOT_TEST_ONLY=<label,label>` runs a subset of the scenarios. The summary says how many ran, and a name that matches nothing is an error.
- The tamper helpers (`tampered`, `flipped`, `substituted`) sit next to the scenario list in `boot_test_scenarios`. A new refusal is a `refusal_scenario` line plus a `KernelError` message.

#### Open for the owner

- **O1** (the statement's `id` and NFC) is untouched: the KRN `id` is ASCII, and the loader does not normalize.

#### Not done

- OKC chains, the Kernel CA and the Origin root (decided: O2–O5; they wait for SYS's signed time).
- Release signing and HSM key handling (§60); ML-DSA-65 (OSS-1), until a role needs it; a timing measurement of the signer.
- B7's Secure Boot first stage; anti-rollback and TPM measurement (§7.6, §8.1).

#### Next

Backlog: OKC chains once SYS exists; item 4 (M3 scheduling) and the rest as they were.

### Continued (2026-11-08, ~17:00–19:15 IST): signed boot and the crypto vectors merged, the licence, the hardware shortlist, and the branch pushed for review

On branch **`claude/interesting-carson-ae7c83`**. The owner answered four questions on 2026-11-08: the licence is MIT OR Apache-2.0; merge the finished side branches (signed boot and the crypto vectors), run every gate, then push and open a PR; the owner compares the two AMD manuals; research a hardware shortlist.

| What | Commit |
|---|---|
| The crypto vectors merged | `9e46aed` |
| Signed boot merged | `e101bc7` |
| The licence: `LICENSE-MIT`, `LICENSE-APACHE`, every package | `b45dc34` |
| `docs/hardware/SHORTLIST.md`: research only, nothing bought or tested | `30993b0` |
| Signed boot's later commits: the owner's answers, host signing in Rust | `d0e91d1` |

**Result, on `d0e91d1`** (only documents changed after it): `cargo xtask check` passes with 752 tests passed, 0 failed, 6 ignored. `cargo xtask boot-matrix` passes in full at 19:10 IST: both IOMMUs at 1, 2 and 4 CPUs, then `boot-x2apic` under leaves 1Fh and 0Bh, each 11 of 11 scenarios. Peak host load 4.83, one QEMU at a time.

**That pass was the sixth attempt on this tree.** The first five failed:

| Attempt | Host | Failed at | Proof |
|---|---|---|---|
| 1 | Load 170, another QEMU running | Intel, 1 CPU | `the RTC did not complete two updates in 1.2 s` |
| 2 | Load 94, another QEMU running | Intel, 2 CPUs | `timers`: worst lateness 2205 us (limit 2 ms) |
| 3 | Load 57, another QEMU running | Intel, 1 CPU | The RTC wait again |
| 4 | Load 5 at the start; another session's control boots ran during it | AMD, 2 CPUs | `one RTC second lasted 740508 us` |
| 5 | Load 66, another QEMU running | Intel, 1 CPU | The RTC wait again |
| 6 | Quiet (peak load 4.83) | None | Full pass |

The tree before the last merge failed six times on the clock proofs before it passed in full at 17:13 at load 6.6. Most of the load came from other sessions measuring exactly this problem, with one `yes` per core.

#### What to know

- **The boot gates are unreliable on a loaded host**, and that is a defect of the gates. Three kinds of proof depend on QEMU keeping real time under TCG: the clock proofs (fixed on `20b8817`, not merged), the scheduler's weighted shares (fixed on `118834b`, not merged), and the timer-queue and ping-pong proofs (no fix yet; that session reports they still fail at double load). Run `boot-matrix` with nothing else booting.
- A pass after retries shows only that the proofs can pass. Each failure was a timing proof, and the loader's signature check had already passed in those boots, but a timing bug in the merged code could hide among them. The quiet pass is the evidence.
- **O1 was decided as item 17** (a statement's `id` is printable ASCII). The signed-boot section above lists it as open because that session did not know.
- `docs/hardware/SHORTLIST.md` asks one question: does "introduced in 2026 or later" (CLAUDE.md, header) mean the platform, or would a 2026 CPU on an older socket do? On the first reading, start with #2 and #5; on the second, #1 and #4 are much easier first machines.
- There are no GitHub workflows in this repository, so a pull request runs no CI. The gates above are the evidence.

#### Open for the owner

- Item 23: compare the two AMD manuals with AMD's own copies. Items 10–12 and ADR-0007's acceptance follow it.
- Which reading of the hardware target, then which machines.
- Review and merge: PR vighnesha-os/origin-os#1, and the pull request stacked on it.
- Whether to merge these. None is on this branch:

| Branch | Tip | What |
|---|---|---|
| `claude/musing-hamilton-587f7b` | `20b8817` | The clock proofs tolerate a loaded host |
| `claude/elegant-thompson-48edc8` | `118834b` | The scheduler share proof too, on top of `20b8817` |
| `claude/agitated-raman-4a0775` | `494f872` | Early-boot DMA; T4 partly closed |
| `claude/cranky-ramanujan-88d205` | `dc855db` | M4: system calls, handles, channels |
| `claude/unruffled-morse-eade76` | `e4c6bbe` | Loom models |

#### Not done

- The independent review of the cryptography that §59 requires; so far it has only been reviewed by agents.
- Nine of the 24 IOMMU threats are open, and T4, T5 and T6 are the most serious.
- Nothing has run on physical hardware (§74).

#### Next

Merge the clock and scheduler fixes if the owner agrees, since the unreliable gates are what made this push slow. Then the timer-queue and ping-pong proofs.

### Continued (2026-11-08, ~15:10–17:15 IST): the clock proofs under host load (task TIME-RTC-UIP)

On `claude/musing-hamilton-587f7b`, from `b3efe8a`, **committed locally, not merged, not pushed**.

The task was the RTC wait (`time.rs`). Measuring it first showed that was only half the flake: with every host core loaded, the boot fails 83 % of the time, with the RTC wait timing out in 40 % of boots and, in most of the rest, the *PIT calibration* off by up to 39 % (the RTC second itself was right to 0.02 %). So the fix covers both, and the cross-CPU check, the HPET and PM-timer read pairs, and `prove_timers`' lateness bound, which share the cause: every one polls a clock and assumed it was never descheduled. The parent session left that scope to judgement.

- **The RTC second** is now timed from the seconds register, between brackets, and decided only when a whole interval of uncertainty is inside or outside 1 % (`rtc::UpdateWatcher`, `rtc::SecondMeter`). A stalled edge costs precision, not the measurement; a vague pair waits for the next edge, up to a 3.2 s budget (at most one second more than the old two, used only when an edge was spoiled).
- **The PIT calibration** measures a 10 ms window again when its ends were not observed closely (`apic::CalibrationWindow`; up to 32 windows; QEMU's PIT counts from the count write, not the gate, and the bracket covers both).
- **The others:** `prove_consistent` extends to 500 ms while there are fewer than 100 handoffs; HPET and PM-timer reads are paired with the TSC through `clock::closest_read`; `prove_timers` repeats a run whose polling loop stalled, and fails at once if a timer is late with the poller running.
- **Measured** (`boot-test`, one `yes` per logical core, 30 boots per arm, alternating): original 83 % → final 3 % failures; RTC-wait timeouts 11 → 0; PIT-off 13 → 0; the one final failure is the scheduler's weighted-share bound. All the before/after numbers, the 12 negative controls and the 3 positive ones are in `docs/engineering/SOURCE_PROVENANCE.md`, "The clock proofs under host load".
- **Gates:** `cargo xtask check` (701 tests, 0 failures, 6 ignored); `cargo xtask boot-matrix` (Intel and AMD at 1, 2, 4 CPUs, `boot-x2apic`).
- **Note for whoever edits a boot proof:** QEMU's `hw/timer/i8254.c` ignores the gate in mode 0; any new PIT window must bracket the count write. Any proof that polls a clock and compares to a bound should keep the bracket-and-retry shape, or a loaded host will fail it.

#### Next

- ~~The scheduler's weighted-share bound is the last load-sensitive proof seen (1 boot in 30 with every core loaded). It is a separate proof (`sched`), not changed here.~~ Done; see the next section.
- Merge this branch (it touches `time.rs`, `timer.rs`, `main.rs`'s `prove_timers`, `rtc.rs`, `apic.rs`, `clock.rs`).


### Continued (2026-11-08, ~17:10–19:01 IST): the weighted-share proof under host load (task SCHED-SHARE-LOAD)

On `claude/elegant-thompson-48edc8`, from `20b8817` (the clock proofs above), **committed locally, not merged, not pushed**. It touches `framekernel/src/sched.rs` and `runqueue.rs` only, neither of which differs on `claude/interesting-carson-ae7c83`, so it merges cleanly after the clock-proof branch.

The task assumed a stall distorting a slice. Measuring first showed something broader: the proof counted loop iterations, and on a loaded host the iterations in a 10 ms slice vary by more than tenfold whatever the scheduler did (0.27 to 1.53 per µs between the 10th and 90th percentiles over 5992 slices), so the lightest thread's share, the sum of about ten slices, is noise. In the probe's failing run the iteration share read 80 per mille while the time between ticks read 53.

- **The fix:** each worker reports the time it saw itself running: every TSC step shorter than 3 ms (about a third of a tick; a longer one is a preemption or the vCPU being away). The bound is untouched and pinned by a host test (`runqueue::per_mille`, `runqueue::share_near_weight`). A wrong result from a run that visibly lost at least 3 of its 200 timer ticks, or whose workers ran for under nine tenths of the window, is measured again (at most three runs); a wrong result from an undisturbed run fails at once. The boot line gains `over N ms of running`, and `(after a stalled run)` when a run was repeated.
- **Measured** (each row the same runs; `docs/engineering/SOURCE_PROVENANCE.md`, "The weighted-share proof under host load"): one `yes` per logical core, 150 runs: the iteration bound 1 of 150 (and 1 of 30 in an earlier probe boot), the new measure 0 of 150 (weight-1 spread 3.8 per mille against 9.3 for iterations). Two `yes` per core: iterations 9 of 150 (6.0 %), the new measure 2 of 150 (1.3 %), both of those with 28 and more ticks lost, so a retry covers them. Boot level: at one `yes` per core neither kernel failed in 30 boots; at two per core the old kernel failed this proof in 2 boots of 15 (`shares 90, 203 and 706` and `85, 172 and 741`) and this change in none, with `timers` failing one boot of each.
- **Controls:** a wrong weight, the wrong pick, weights ignored, no preemption and a preposterous step limit all fail the boot; a 70 ms interrupts-masked stall in a run is detected, repeated, and passes (with the retry disabled the same stall fails it; stalled every time, it fails after three runs).
- **Gates:** `cargo xtask check` (702 tests, 0 failures, 6 ignored); `cargo xtask boot-matrix` (Intel and AMD at 1, 2, 4 CPUs, `boot-x2apic`; every boot's shares 47–51, 189–194 and 755–760 per mille).
- **Note for whoever edits a boot proof:** iterations of a spin loop per timer slice measure the host under TCG; proportional-share style proofs should count the time a thread saw itself running. At double host load, `wait queues: ping-pong`'s 2000 µs bound failed in 3 boots of 5 (2003, 2045 and 2084 µs) and `prove_timers` failed 2 boots of 30: different proofs, not touched.

#### Next

- Merge this branch after the clock-proof branch (`claude/musing-hamilton-587f7b`).
- The two-worker fairness check in `sched::prove` still counts iterations (worst ratio 1.52 in 30 samples at full load, bound 2).
- ~~`wait::prove`'s ping-pong bound and `prove_timers`' 2 ms bound fail at double load.~~ Done; see "~19:07–23:50 IST" below.

### Continued (2026-11-08, ~19:30–19:50 IST): the clock and scheduler proof fixes merged

On branch **`claude/interesting-carson-ae7c83`**, merge commit `31b4347`, bringing in `118834b` (which contains `20b8817`), on the owner's word, and pushed to PR vighnesha-os/origin-os#2, again on the owner's word. The kernel code merged without conflict; `docs/STATUS.md` and this file kept both sides.

The code diff was read before merging. It changes the boot proofs and the boot-time PIT calibration, and no ABI or format. No accuracy bound was loosened: the RTC's 1 %, the timer queue's 2 ms, the HPET and PM timer's 2 % and the scheduler's share bound are as they were, and the share bound is now pinned by a host test. What changed is that a proof whose run was visibly stalled may measure again. The RTC proof's time budget grew to 3.2 s, spent only when an edge was spoiled.

**Result, on `31b4347`:** `cargo xtask check` passes with 772 tests passed, 0 failed, 6 ignored. `cargo xtask boot-matrix` passes in full **on its first attempt, with the host at load 50–260 (mean 168)** and another session's QEMU running for 128 of 141 samples: both IOMMUs at 1, 2 and 4 CPUs, then `boot-x2apic` under leaves 1Fh and 0Bh, each 11 of 11 scenarios.

| In that run | |
|---|---|
| Kernel boots with the timing proofs | 8 |
| `timers` runs repeated after a stall | 1 |
| PIT calibration windows discarded | 41, over 6 boots, at most 15 in one |
| The RTC second | All confirmed within 1 %; widest uncertainty ±4.7 ms |

#### What to know

- One pass under load is not a failure rate. The measured rates are in `SOURCE_PROVENANCE.md`, "The clock proofs under host load" (83 % to 3 % of boots failing with every core busy) and "The weighted-share proof under host load".
- The section above says the timer proof had no fix. `20b8817` already repeats a stalled `timers` run, and that path fired once in this run.
- At two busy-loop processes per core, the `timers` and ping-pong proofs are reported to still fail. Another session is working on that on `claude/priceless-einstein-d6f3b9`; its boot loops may run until about 22:00 IST. A session started for the same work stood down without changing anything.

#### Open for the owner

- As in the section above; early-boot DMA, M4 and the Loom models are still on their own branches.

### Continued (2026-11-08, ~19:55–20:10 IST): the AMD manuals checked and the hardware scope decided

The owner told Claude to do both ("You do"). On branch **`claude/interesting-carson-ae7c83`**, pushed to PR vighnesha-os/origin-os#2 on the owner's word.

**Sheet item 23, the AMD manuals: byte-identical to AMD's own copies.** Compared through the desktop app's browser pane, on AMD's Technical Information Portal.

| Manual | AMD's listing | Bytes | SHA-256 |
|---|---|---|---|
| IOMMU specification, 48882 | `48882_3.11_IOMMU_PUB.pdf`, Revision 3.11, released 2026-04-08 | 7,705,637 | `f7c375a15db5ed63de760356867211063d164a2ed59f2d38613daec95894ce22` |
| APM Vol. 3, 24594 | `24594_3.39_APM_Vol3.pdf`, Revision 3.39, released 2026-10-28 | 16,226,519 | `b598514b19f204d973040d9867a7ffc48c949009a8b3dc86b20c9188e721ee45` |

Each was fetched from the address the portal's own search returned for it (`docs.amd.com/api/khub/documents/<id>/content`) and hashed in the browser's memory. No PDF was saved, and there was no sign-in and no challenge. The old `amd.com` PDF addresses are 404, and the portal's reader address answers 401, even in a browser.

**Sheet item 24, the hardware: `docs/adr/ADR-0009-hardware-targets.md`.** A platform counts if the vendor launched its generation in 2026 or later, so a refresh launched as a new generation counts and a new model on an older platform does not. The first machines are #2 (ASRock iBOX-358H, Intel), then #5 (Lenovo M75q Gen 6, AMD). Claude decided, by delegation, and the owner has not reviewed the ADR. **Nothing has been bought, and buying is the owner's.**

#### What to know

- The ADR's H5 lists what to confirm before buying, and H6 what must happen before the first boot, including a console that does not depend on a 16550 at `0x3F8`.
- Sheet items 10–12 (ADR-0007 questions 6–8) are no longer blocked. They are still the owner's to answer.

#### Open for the owner

- Buying the machines. Sheet items 10–12 were answered just after this; see the next section.

### Continued (2026-11-08, ~20:15 IST): ADR-0007 accepted

The owner accepted sheet items 10–12 (ADR-0007 questions 6–8) as recommended: blocking AMD-Vi device table entries and one interrupt table per device; the two QEMU gaps in the AMD-Vi proofs, as QEMU limits that hardware qualification must check; and refusing devices an IVMD names, with the same deadline as RMRRs (identity maps before real hardware). Those were ADR-0007's last open questions, so it is **Accepted**, and no ADR is waiting on the owner. No code changed.

Status words left stale by earlier acceptances in `docs/STATUS.md` and `docs/engineering/TASKS.md` (ADR-0006, ADR-0008, errata V1–V6) are corrected in the same commit. Dated provenance headings and research ledgers keep the status they had when written.

Pushed to PR vighnesha-os/origin-os#2 on the owner's word, with the PR's description updated.

### Continued (2026-11-08, ~20:20–22:30 IST): the early console, for ADR-0009 H6 (ADR-0010, accepted)

On branch **`claude/funny-benz-fa7d10`**, local only, not pushed, on top of `claude/interesting-carson-ae7c83` at `f9a948d` (fast-forwarded before any work). The code and these records land in one local commit.

Before this, the kernel's only console was a 16550 at the fixed port `0x3F8`, which neither first machine in ADR-0009 documents. Now:

- **The framebuffer.** The loader passes the UEFI GOP framebuffer through boot info **0.4** (`Framebuffer`: address, size, the address it mapped it at, dimensions, stride, channel masks). The loader maps it uncached in the MMIO window so the kernel draws from its first line, and across its switch to its own tables. Everything the console prints goes to the screen first and then the UART: text in an original 8×16 font, wrapped rather than scrolled, so each line costs one row of pixel writes.
- **The UART.** The console starts on COM1 (it has nothing better until the ACPI tables can be mapped), and as soon as they can be, it reads the SPCR. If the firmware names a UART this kernel can drive, I/O or memory-mapped, the console moves there, with everything printed so far sent to it again (8 KiB kept), and COM1 is not used again. Otherwise COM1 stays: "fall back to 0x3F8 only when the firmware gives nothing better".
- **A missing console never stops a boot.** No GOP, a Blt-only mode, a framebuffer the checks refuse, a table that is refused or names a port with nothing behind it, a UART that stops taking bytes, or no UART at all: each is named on whatever console there is, and the boot goes on.

**Evidence** (`docs/engineering/SOURCE_PROVENANCE.md`, "The early console"):

- `cargo xtask check` (819 tests, 0 failed, 6 ignored) and a full `cargo xtask boot-matrix` on a quiet host: both IOMMUs at 1, 2 and 4 CPUs (11 of 11 scenarios each), `boot-x2apic` under leaves 1Fh and 0Bh, and `boot-console` (8 of 8). An earlier matrix run lost only `boot-console`'s third scenario, to another session's load test starting mid-run (`wait queues` timed-wait bound); host load, unrelated to this work.
- `boot-test`'s kernel scenario checks a QEMU screen dump, cell by cell in the literal colors, against the rows the serial log implies (computed independently of the drawing code), in every cell of `boot-matrix`.
- `boot-console`: all the kernel's output on a UART at port `0x2F8` named by an SPCR, while COM1's log stops at the move line; COM1 named by the table itself; a refused table, a dead port, registers in RAM; a UART that answers but takes no bytes; no display; and no UART at all (judged from screen dumps only).
- Fourteen negative controls fail these proofs (FB1–FB6, SP1–SP8). One of them, red and blue swapped, passed against the first version of the screen check, which compared the dump with a frame from the kernel's own drawing code; that check was rewritten so a bug in the drawing cannot vouch for itself.
- A fresh reviewing agent found no memory-safety bug and several real gaps (a UART that answers but will not take bytes lost the console; a zero divisor could be restored; the entry stub could lose its page tables to the framebuffer; the UART speed rule), all fixed.
- The picture was looked at, not assumed: the screen dump of a full boot shows legible text and the cursor bar right after the last line.

#### What to know

- **Boot info changed (0.3 → 0.4), the ABI decision of §84. The owner accepted it and the rest of ADR-0010 on 2026-11-08** (the three questions, sheet items 27–29).
- The screen check in `boot-test` runs in every cell of `boot-matrix`. `-serial mon:stdio` replaced `-serial stdio -monitor none` for the kernel scenario only.
- `boot-console` is new and adds eight full boots to `boot-matrix` (about 4 minutes).
- **On real hardware expect the framebuffer to go dark at `iommu::init`** on a machine whose firmware scans out through the IOMMU, until identity maps (T14) or the T4 changes land: ADR-0010, "What this does not prove". A serial line is still wanted for a first boot (`adopt_firmware_uart` will find it if firmware names it).
- The MMIO UART path has no boot proof: QEMU's x86 machines have no memory-mapped 16550.

#### Open for the owner

- Whether and when to merge this branch.

### Continued (2026-11-08, ~19:07–23:50 IST): the wake-up ping-pong and the timer queue under host load (task WAIT-TIMERS-LOAD)

On `claude/priceless-einstein-d6f3b9`, from `118834b` (the weighted-share proof above), **committed locally, not merged, not pushed**: `05deb69` and the correction on top of it. It touches `framekernel/src/main.rs` (`prove_timers`), `runqueue.rs`, `sched.rs`, `timers.rs` and `wait.rs`, and merges after the weighted-share branch.

Measuring first showed neither proof was timing the kernel. In 36,000 firings at up to four `yes` per core no timer was decisively late: the proof was timing the poller. And at double load the slowest tenth of the ping-pong's round trips carries 60 % of the time: a run's mean ranges from 145 to 1713 µs while its median stays under 640 µs.

- **The fix, timers:** every firing's lateness is an interval from the polls around it (`timers::Firing`, `Verdict`, `judge`), and a run decides only if every firing's whole interval is inside or outside 2 ms. A timer held past the limit at a poll that did not fire it, or a wrong order, fails at once; an undecided run is repeated, up to 64 runs of about 15 ms.
- **The fix, ping-pong:** only a mean under the old 2000 µs bound passes a run (`runqueue::judge_round_trips`). A run over it is measured again if the boot flow went without a turn for at least a tenth of the run (`sched::yield_until_away`, steps of at least 3 ms), at most 8 runs; otherwise it fails at once, and any run on one processor fails at once. The boot line gains the reading and the run number.
- **The rule was corrected after review.** `05deb69` let that reading swap the mean for the median and the reschedule-IPI share; two reviewing sessions showed a kernel that delays itself passing (the pinger spinning 10 ms on every fourth round trip, no host load: 2555 and 2554 µs, passed, where `118834b` failed). A version on lost ticks on both processors was then built and rejected: a kernel holding interrupts off on both processors passed it (`pp-self-delay-masked-both`: 2553 µs). The reading can now only earn a repeat, the method of the clock and weighted-share proofs.
- **Measured** (`docs/engineering/SOURCE_PROVENANCE.md`, "The wait-queue ping-pong and the timer queue under host load"): per run, the old six-run timer budget ran out for 0.14 % of the starting points at 1× and 4.4 % at 2× (3600 runs each), and no budget of 24 or more ever ran out. Boot level, alternating with `118834b`: at 2× (15 pairs) no boot of either kernel failed the ping-pong and the old kernel failed the timer proof once where the final rule did not; at 1× (10 pairs) neither failed either proof. **At 4× (20 pairs) the host's ordinary round trip was over the bound: the final rule passed 2 of the 6 boots that reached the ping-pong (at runs 5 and 8) and failed 4 after all 8 runs; the old kernel failed all 5 it reached.** The host was about 2.5 times slower per round trip in that batch than earlier the same day, for the same kernel.
- **Controls** (all on the final tree): for the timers, a timer 3 ms late with the poller running, a cancelled timer firing and a queue that never fires all fail the boot; a 9 ms stall across a deadline is repeated and passes, and fails with the repeat disabled. For the ping-pong, the pinger or the ponger spinning 10 ms on every fourth round trip, a hold with interrupts off on either side or both, no IPIs, every second or fourth IPI lost, a 3 ms slow wake path (2.5 ms at 2×) and, on one processor, every fourth round trip 10 ms slow all fail the boot; a 2.5 s stall of one or both processors in the first run is repeated and passes, and fails with the repeat disabled.
- **Gates:** `cargo xtask check` passes (710 tests, 0 failures, 6 ignored, eight tests more than `118834b`); `cargo xtask boot-matrix` passes (Intel VT-d and AMD-Vi at 1, 2 and 4 CPUs, then `boot-x2apic` on leaves 1Fh and 0Bh). In that matrix every ping-pong passed at its first run, with means of 41 to 55 µs on two or more processors (boot flow away for 0 per mille) and 4 and 5 µs on one, and every `prove_timers` line read `worst lateness 0 us`.
- **Note for whoever edits a boot proof:** evidence of a disturbance should only ever earn a repeat against the same bound, because a kernel under test can produce most evidence itself: a thread on the same processor can look like a descheduled vCPU, and a thread holding interrupts off can lose ticks. And `xtask`'s serial cleaner is quadratic in the log's size: a probe that prints thousands of lines makes a boot take minutes.

#### Next

- Merge this branch after the weighted-share branch (`claude/elegant-thompson-48edc8`).
- Still load-sensitive and not touched: at 1× and 2× the RTC proof (`too uncertain to confirm within 1 %`), the timed wait (`prove_timed`, 81 ms against a bound of 80), and the iteration-counted two-worker fairness check in `sched::prove`; at 4× the APIC timer proof and the cross-CPU TSC check, and the ping-pong itself (the host's ordinary round trip was over its bound in that batch).

### Continued (2026-11-09, ~03:20–03:35 IST): the early console and the wait-queue and timer proof fixes merged

On branch **`claude/interesting-carson-ae7c83`**, on the owner's word ("Yes, merge both"): merge commits `8040882` (the early console, `a96bf7b`, ADR-0010) and `31c3d39` (the wait-queue and timer proofs, `cc375ac`). Pushed to PR vighnesha-os/origin-os#2 on the owner's word, with the PR's description updated.

Both diffs were read before merging: the loader's and the kernel's changes, the SPCR parser, the framebuffer console and the screen-dump proof, the timer verdicts and the ping-pong's rule. No code conflicted, and `framekernel/src/main.rs`, the one code file both change, merged automatically. The conflicts were records only and kept both sides' facts. No bound was loosened: a timer may still be at most 2 ms late, and only a mean round trip under 2000 µs passes a run.

**Result, on `31c3d39`:** `cargo xtask check` passes with 827 tests passed, 0 failed, 6 ignored. `cargo xtask boot-matrix` passes in full on its first attempt on a quiet host (load 2.7–4.0): both IOMMUs at 1, 2 and 4 CPUs, `boot-x2apic` under leaves 1Fh and 0Bh, and the 8 `boot-console` scenarios.

| In that run | |
|---|---|
| Kernel boots | 8, then the 8 console boots |
| Ping-pongs | 15, all passed at their first run: 41 to 56 µs on two or more processors, 5 µs on one |
| `timers` runs | 15, all decided at their first run: worst lateness 0 µs in 14, 38 µs in one |
| Drawing a console line | 226 to 245 µs on average, at most 808 µs (14 boots, QEMU TCG) |

#### What to know

- One pass on a quiet host says nothing about load. The measurements at 1×, 2× and 4× are in `SOURCE_PROVENANCE.md`; at 4× the ping-pong still fails most of the boots that reach it, because the host's ordinary round trip is over the bound.
- The framebuffer console has only run under QEMU. On a machine whose firmware scans out through the IOMMU, expect the screen to go dark at `iommu::init` until T14's identity maps or the T4 work land (ADR-0010, "What this does not prove"). A serial line is still wanted for a first boot.
- `boot-matrix` now takes about 8 minutes on a quiet host, with `boot-console`'s eight boots in it.

#### Open for the owner

- Buying the machines ADR-0009 chose.
- Early-boot DMA, M4 and the Loom models are still on their own branches.

### Continued (2026-11-08, ~16:35 IST): threat T4, bus mastering (`claude/agitated-raman-4a0775`, on `72285c5`; not pushed, not merged)

The task (IOMMU-T4) closed what could be closed of threat T4 in `docs/engineering/THREAT_MODEL_IOMMU.md`: a device the firmware left bus-mastering writes memory until translation is on.

| Commit | What |
|---|---|
| `36d664a` | the quiesce (`pci`, `pcie`), `DeviceDma::enable_bus_master`, both drivers' bind paths, the boot order, xtask markers, `boot-bridge`; ledger, ADR-0007 I12, threat model |
| `ffbc42f`, `3ea8d7c` | the translating line only when translation is on; the quiesce line's wording |
| (records) | provenance, STATUS, TASKS, the ledger's measurements, this section |

What changed in the boot:
- `pcie::quiesce` runs right after the kernel's page tables and heap: every function of every MCFG window, bridges included, Bus Master Enable cleared and read back. The IOMMU's own function (AMD-Vi, named by the IVRS) is left alone. `pcie::settle` waits for Transactions Pending once the TSC is calibrated.
- `iommu::init` runs before `smp::start`, not after `pcie::init`. `pcie::init` (PAT everywhere, the function list, BAR sizing) now runs after SMP as before, and takes the online CPUs.
- Drivers set the bit only through `dma::DeviceDma::enable_bus_master`, after attaching; the NVMe driver also after resetting the controller. `pcie::enable` is gone (`pcie::enable_memory` does decoding only).
- virtio-net refuses a device without `VIRTIO_F_ACCESS_PLATFORM` while the IOMMU translates (ADR-0007 I10 tightened).

Merge notes for the M4 and signed-boot sessions: `framekernel/src/main.rs` (the boot order around `memory::init`, `timer::init`, `smp::start`, `pcie::init`; `quiesced`), `developer-tools/xtask/src/main.rs` (marker order: the ECAM line moved before the heap proofs, the IOMMU lines before the MADT line, a new `qemu_bridge`/`boot-bridge`), and the records.

Lessons:
- **OVMF left every function bus-mastering, even the host bridge.** QEMU's reset value is 0 for all of them (read with the monitor before any firmware ran), so the firmware set each one. A control that relies on the firmware's state must check it first (the older lesson, now confirmed function by function).
- **QEMU's model cannot show two things:** a bridge's Bus Master Enable stopping traffic (only the function's own bit gates its DMA) and a real Transactions Pending bit. The read-backs and C6/C7 cover the code; the effect needs hardware.
- **Two proofs in the old boot can flake under host load:** `clock failed` (the RTC did not complete two updates in 1.2 s; `time::init`, whose window is the 244 µs update-in-progress pulse, is probably missed under host load, not investigated) and `timers failed` (a 7 ms lateness). They appeared while another session's `yes` stress run pushed the load past 200 (three of thirteen control boots, and several gate attempts on the final tree), and in those runs the TSC calibration itself can come out wrong (`TSC 4270 MHz` instead of about 1000). Neither proof touches this code. A failed run names the proof that failed, so check that line before blaming a change.

Gates on the final tree (`35a5189`): `cargo xtask check`: 687 tests, 0 failures, 6 ignored (681 before); `boot-test`; `boot-matrix` (both IOMMUs at 1, 2 and 4 CPUs; one attempt stopped on the `clock failed` flake, the next passed); `boot-x2apic` (leaves 1Fh and 0Bh; two attempts stopped on that flake, the third passed); `boot-bridge` (both IOMMUs). Controls C1–C10 each failed the boot and were restored (C2 to C10 run one commit earlier, C1 again on the final wording).

#### Next

- **The owner:** ADR-0007 question 9 (clearing bus mastering on every function, the bridges included; leaving the AMD IOMMU's own function alone).
- Hardware (§74): the ordering claim in the ledger §2 (the read-back flushing posted writes) against the PCIe text, the AMD function's bit, firmware-owned controllers (USB legacy, a firmware framebuffer) when they exist, and VT-d's Abort DMA Mode (`ECAP.ADMS`, Rev 5.20 §3.4.4), which would bring translation up before any table exists and cannot be proven under QEMU.

### Continued (2026-11-08, afternoon IST): M4's first slice, real system calls between isolated processes

Branch **`claude/cranky-ramanujan-88d205`**, built on `claude/interesting-carson-ae7c83` at `6a2b29c`. **Nothing is pushed.**

| Commit | What |
|---|---|
| `6b1106e` | process objects, the system-call layer, channels and handles through it, `wait_one`, `swapgs`/`clac` at the boundary, the ring-3 boot proofs and their xtask markers |
| `2147928` | fixes from an independent review (thread start, reaping, developer-call gate, UMIP) |
| `adb7907` | ADR-0005 Amendment 1 (Proposed), `SOURCE_PROVENANCE.md` ("M4, first slice"), STATUS, TASKS item 5, this section |
| (records) | the owner accepted Amendment 1, with the recommended option of each item |

Gates on the final tree: `cargo xtask check` (698 tests, 0 failures, 6 ignored), `boot-test`, `boot-matrix` (Intel and AMD at 1, 2, 4 CPUs, in one run) and `boot-x2apic`. Other sessions' boots and load kept this machine busy (load average up to 230): failing attempts of `boot-matrix` and `boot-x2apic`, before the passing runs, failed only on the clock and timer proofs, and an attempt that finished the IPC proofs passed them. 31 negative controls, each failing the boot for its own reason, listed in `SOURCE_PROVENANCE.md`.

#### What to know

- **API changes other work will hit:**
  - `process::Process` is the real process object now: `new(limits, syscall_origin)`, `map_page`, `write_frame`, `grant`, `start_thread`, `thread_ended`, `reports`, `ending`. The old `destroy`, `map` and `spawn_in`-with-a-trampoline flow are gone.
  - `cpu::enter_user`, `cpu::copy_from_user`, `cpu::EFAULT`, `cpu::cpu_local_base` and `vm::user_readable` are gone. Copies go through `syscall::copy_in`/`copy_out` (checked against the caller's tables) and `cpu::copy_user`.
  - `sched::spawn_user` starts a thread in ring 3 from an IRETQ frame; the `int 0x80` gate no longer exists.
  - `framekernel::channel::Channel::try_new(depth, bytes)` replaces `new` (queues are bounded in messages and bytes, and pre-reserved); `HandleTable::insert` is fallible on allocation; `reserve`, `check_all`, `take_all` are new.
  - `origin_abi`: `status_name`, `Error::name`, `decode_bool`, `rights::CHANNEL`, `signals::ALL`, `read_sizes`.
  - xtask: `ipc_markers`, the failure string `: ipc failed`, and changed marker text for the user-mode lines (`EFAULT` became `FAULT`, the two user threads became processes).
- **Boot proofs are timing-sensitive under host load.** The clock, HPET, PM-timer and timer proofs (not this work) can fail on a machine under heavy load from other boots; a re-run settles it. The session "Make the RTC wait survive a loaded host" is on that.
- **ADR-0005 Amendment 1 is Accepted** (the owner, 2026-11-08, the recommended option of each item), and the code implements it: channel-end rights, cancellation, the transfer rules, the `out_sizes` layout, when a process ends, the syscall origin before `origin.core`.
- A boolean system-call argument is `origin_abi::decode_bool`; this slice has none yet, so nothing calls it from the dispatcher.
- An independent review found, and these commits fixed: a new thread runnable before its state was recorded (dangerous for ring 3, harmless before for kernel threads; `spawn_with` holds the queue lock), no reaping while a processor stays busy (now from every system call; a CPU running only user spins still needs the planned per-CPU reaper), the developer-only calls open to everyone (`Limits::debug_calls`), `sidt` revealing the KASLR slide (UMIP on), `cli`/`sti` not being compiler barriers. Open, recorded in `SOURCE_PROVENANCE.md`: a process's kernel half is a snapshot from creation time, which matters once processes outlive early boot.

#### Next

- TASKS item 5: the start record, memory objects, the Loader Service, `.ose` loading; PCIDs.


### Continued (2026-11-08, afternoon IST): Loom models of `SpinLock`, `RwLock` and the wait-queue protocol (TASKS item 8)

On `claude/unruffled-morse-eade76`, based on `claude/interesting-carson-ae7c83` at `b3efe8a`, **not pushed**.

- **`loom` 0.7.2** is a `[target.'cfg(loom)'.dev-dependencies]` entry of `origin-sync`, after the owner approved its exact version, licences, transitive crates and sizes (`SOURCE_PROVENANCE.md`, "Loom models"; owner decision 20). `cargo xtask check` now fails if a shipping crate gains a third-party dependency, or if `Cargo.lock` holds one that is not on `APPROVED_THIRD_PARTY` at its approved version: **changing that list is a new approval**.
- **The wait-queue protocol moved**, unchanged, from `framekernel/src/wait.rs` to `origin_sync::wait` (`framekernel/sync/src/wait.rs`): `WaitQueue<T, N>` over a `Scheduler` trait. The kernel keeps a `WaitQueue` type that forwards to it, and all its boot proofs.
- **`cargo xtask loom`** (part of `check`) runs 18 models of the real code, `SpinLock`, `RwLock`, `wait` and the real `RunQueue`, then 22 negative controls, each applied to a scratch copy of the source (`developer-tools/xtask/src/loom.rs`). Every model has at least one control.
- **Read `framekernel/sync/src/models.rs` before trusting these models:** its header says what is proved and what is not (the bounds, interrupts, the context switch, fairness, the machine). The bounds per model and the deep results are in `SOURCE_PROVENANCE.md`.
- **A spin bound had to be added** (`prim::spin_loop`, models only): with two threads spinning on one lock, Loom's search is unbounded whatever the preemption bound, and past a bound it now runs each execution to its end without branching. Raising the bound from 4 to 8 changes nothing for the two-thread SpinLock and waker/waiter models.
- **Don't take the newest `origin_sync-*` binary from `target/loom/release/deps` for an experiment.** The controls' binaries are there too. One of them made a "deadlock" that took a while to tell apart from a real one.

#### Evidence (Rust 1.99.1 stable; commits `9efadd2`, `d60af7c`, `48cd052`, and the commits that add the `RwLock` and timed boot-flow models)

- `cargo xtask check`: passes; 26 test binaries, 702 passed, 0 failed, 6 ignored (the download-gated crypto vectors). The no-panic check and the new dependency check hold for the shipping crates.
- `cargo xtask loom`: 18 models pass (530,152 executions), all 22 negative controls caught (29 model runs). `ORIGIN_LOOM_DEEP=1 cargo xtask loom`: 18 models pass over 26,174,404 executions in about 20 minutes.
- `cargo xtask boot-matrix` passes on this code (VT-d and AMD-Vi at 1, 2 and 4 CPUs, then the sparse machine under leaves 1Fh and 0Bh), with the wait-queue boot proofs unchanged, and with them the default `boot-test` configuration. Boots during another session's CPU stress test (load average above 100) failed at the `clock` proof, before any wait-queue code runs; these passing runs came on a quiet machine. That is the RTC flake recorded above, and the `musing-hamilton` worktree has uncommitted edits to `framekernel/src/time.rs`.
- Security review: no change to any privileged interface. `loom` and its crates are host-only and enforced out of the shipping crates by `cargo xtask check`.

#### Next

- A timed waiter that finds its queue full, the one path not modelled; Kani bounds (TASKS item 8).
- M4's `wake_all` will conflict with the moved protocol; a note for that session is in the shared notes folder.

### Continued (2026-11-09, ~03:50–06:30 IST): the pre-merge review, and landing on `main`

The owner: "You review and merge them, review extremely very good design, security, performance", with the question of whether everything, even the kernel, should be native SDK. Claude took "them" to mean everything then waiting on review or merge: PR #1, PR #2, and the three side branches (early-boot DMA, M4's first slice, the Loom models). M4 was also what fixed the review's first critical finding.

**The record is `docs/engineering/PRE_MERGE_REVIEW_2026-11-09.md`:** what was found, what was fixed, what was not and why, and what no reviewer opened. AI reviewers only: no human has reviewed this code.

**Result, on `c465fe6`:** `cargo xtask check` 968 passed, 0 failed, 6 ignored, with 26 Loom models and 35 negative controls; `cargo xtask boot-matrix` a full pass on its first attempt at load 1.9–11.7 (mean 2.9), now including `boot-bridge` and `boot-console`. Ping-pongs 5 to 35 µs, down from 41 to 56 µs, after interrupts stopped forcing a context switch. Under QEMU only.

#### What to know

- **The kernel no longer writes to a disk it does not recognise.** Boot write proofs need the boot test's disk.
- **The first physical boot needs the review's open list**, above all the IOMMU threats ADR-0009 H6 names.
- The fix branches (`fix/*`) and the agents' worktrees can be deleted: everything is merged.

#### Open for the owner

- Sheet items 30–32: the bus-mastering question, the Loom download's deviation, CI.
- The SDK-first decision: written as ADR-0011 (Proposed); see the next section.
- Buying the machines ADR-0009 chose.

### Continued (2026-11-09, ~06:50–07:00 IST): ADR-0011, the native SDK

The owner asked for the SDK-first decision to be written. `docs/adr/ADR-0011-native-sdk.md` is **Proposed**, with its research ledger at `docs/research/2026-11-09-native-sdk.md` (§16). Nothing in it is built. Committed on `claude/interesting-carson-ae7c83` on top of what merged to `main`, and landed on `main` on the owner's word once accepted.

In short: every capability is defined once in a typed interface, and GUI, command-line tools, shell, terminal and recovery are clients with no private doors. Stable promises go per capability, not per module. The kernel is below the SDK and reached only through `origin.core`, and inside it the Framekernel becomes an SDK of its own: one privileged crate holding every `unsafe`, with safe services above. One original interface language generates the Rust, the C ABI, the messages and the docs. The SDK is not a security boundary, so checks live in the kernel and the services. The exceptions are a closed list.

#### Open for the owner

- Sheet items 33–39, the ADR's seven questions.

### Continued (2026-11-09, ~07:00 IST): ADR-0011 accepted

The owner accepted ADR-0011 as recommended: questions 1–6, sheet items 33–38. ADR-0005 gains Amendment 2 (kernel-stamped caller identity, per-call system-call pinning), and ADR-0003 K1 notes what replaces it. **Nothing in any of them is built.** Question 7, whether to amend CLAUDE.md §48 and §70, is still the owner's, and Claude has not changed the directive.

The next work, in ADR-0011 N14's order, starts with the Framekernel framework crate and its gate, before the kernel grows further.

#### Open for the owner

- Sheet items 30–32 and 39.
- Buying the machines ADR-0009 chose.

### Continued (2026-11-09, ~08:20–09:16 IST): CI

The owner decided sheet item 32: yes, with the boots on a real processor. `.github/workflows/ci.yml` runs on every pull request and on `main`: `cargo xtask check`, `boot-matrix` on QEMU's software CPU, and `boot-matrix` plus a `-cpu host` boot through KVM on two runner images. Only GitHub's free standard runners; no secrets; the token only reads.

Under KVM xtask holds every proof to its markers, not only the last line, with two exceptions, both in `kvm_forgives`: the two VT-d pin steps QEMU cannot show there, and a rare step back in the TSC of under 1,000 ticks.

#### What to know

- **`cargo xtask check` passes on an x86-64 Linux host,** the first time it has run on anything but the Mac. That includes the signing check's branch counts, which the review had left unconfirmed there.
- **A real finding:** an EPYC 9V45 runner showed one TSC step back of 16 ticks in 261,083 handoffs, in 1 of 13 kernel boots. The processors are virtual processors of a virtual machine. The kernel's proof stays strict, and `time::monotonic_nanos` is not clamped, so a step like that can reach a caller on another processor.
- `-cpu max,vendor=GenuineIntel` works under KVM on an AMD host: the leaf 1Fh markers hold there.

#### Open for the owner

- Sheet items 30, 31 and 39.
