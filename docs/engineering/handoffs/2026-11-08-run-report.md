# Origin OS — twelve-hour continuous run, report

| | |
|---|---|
| Run | 2026-11-07 ~21:42 IST → 2026-11-08 (continuous, autonomous, under the owner's delegation) |
| Branch | `m0/toolchain-verification` — **local only, not pushed, not merged into `main`** |
| Compiler | Rust 1.99.1 stable, edition 2024 (pinned) |
| Reproduce | `cargo xtask check`, `cargo xtask boot-test`, `cargo xtask boot-matrix` (QEMU 11.1.3 TCG + OVMF) |
| Attribution | 2026-11-08 commits are co-authored `Claude Opus 4.8` |

This report covers the continuation of the twelve-hour run (from ~02:00 IST 2026-11-08 onward); the earlier part is in `2026-11-07-local-handoff.md`, which this run also kept current. Everything below is **built, tested and committed locally**. Nothing was pushed, merged, or run on physical hardware.

## 1. Headline state at the end of the run

- `cargo xtask check` passes: `rustfmt --check`, workspace Clippy with `-D warnings`, the no-panic object scan on the freestanding crates, and the §11 (`cfg(target_os)`) and §62 (manifest `version`) lints.
- **505 tests across 27 workspace suites, 0 failures** (6 ignored = the download-gated crypto vectors). **264 of them** are `framekernel` host library tests across **50 modules**.
- `cargo xtask boot-test` and `cargo xtask boot-matrix` (1, 2 and 4 CPUs) pass; an 8-CPU boot (`ORIGIN_QEMU_SMP=8`) was confirmed. Every kernel feature added this run prints a gated boot marker and has at least one negative control that was shown to fail the boot and then restored.
- A 40× mutation soak (`ORIGIN_MUTATION_SCALE=40`) over 12 untrusted-input parser suites ran with 0 failures.
- **Everything runs under QEMU 11.1.3 (TCG) + OVMF only. No physical-hardware qualification (CLAUDE.md §74).**

## 2. What was built this run

Each item was built → host-tested → `cargo xtask check` → committed, with a `SOURCE_PROVENANCE.md` entry (module, author, specs, tests, limitations). Kernel-integrated items additionally have a boot proof and a negative control.

### NUMA, end to end (kernel; §24, §27, §28, §66)
- **ACPI SRAT** parser (`acpi::srat_cpu_affinities` / `srat_memory_affinities`): processor (type 0/2) and memory (type 1) proximity domains.
- **ACPI SLIT** parser (`acpi::parse_slit`): the inter-node distance matrix.
- **Locality map** (`numa`): CPU/address → node, nearest-node order (stable sort), the self-nearest invariant.
- **Boot proof:** QEMU is booted with two `-numa` nodes and a cross-node distance, so the kernel reads a *real firmware* SRAT/SLIT and reports `numa: 2 nodes …; consistent true` and `slit: … self-nearest true, from 0 [0, 1]; consistent true` at `-smp` 1/2/4/8. Three negative controls (SRAT entry offset, SLIT matrix offset, inverted nearest-node comparator).

### Security — hardware entropy (kernel; §59)
- `entropy`: a bounded-retry `draw`/`fill` layer over RDSEED/RDRAND (`cpu::rdrand`/`cpu::rdseed`), the kernel's first entropy source. **Boot proof:** 16 draws all distinct and non-zero; negative control: a constant source fails the boot. Documented as a true-random source, *not* a DRBG (that needs KATs + review).

### Filesystem / deployment (§21, §39–§41, §46, §84)
- **OriginFS v0** (`originfs`): a read-only on-disk parser + image builder against the **Proposed** ADR-0006 (superblock, inode table, 12 direct + 1 single-indirect pointers, linked directory entries). Host-only, **not mounted** — the format decision awaits the owner (§84). A `read_whole` allocation cap was added after the mutation soak found an unbounded-allocation path.
- **ustar** (`ustar`): a read-only tar reader (checksum + magic verified).
- **initramfs** (`initramfs`): extracts a ustar archive into the VFS, creating implicit parents, with the tar **path-traversal defense** (`..`/absolute names refused). **Boot proof:** the kernel embeds `framekernel/assets/initramfs.tar` and extracts it into a VFS at boot (`initramfs: 4 entries … consistent true`); negative control on the entry count. This is the first end-to-end §21 step.
- **bit allocator** (`bitmap`): the free-space-map / id-pool primitive (lowest-free alloc, exact free count).

### Compression (§17, §46)
- **DEFLATE** (`deflate`): raw RFC 1951 inflate — stored, fixed- and dynamic-Huffman blocks, LZ77 back-references — with a decompression-bomb output cap. Validated against zlib-generated known-answer vectors (including a verified dynamic block).
- **zlib / gzip wrappers** over DEFLATE, with **Adler-32** and CRC-32 (reusing `gpt::crc32`) trailer verification.

### Text engine (§51)
- **UTF-8** streaming, error-locating decoder (`text`): byte-exact against `core::str` and `String::from_utf8_lossy` over 20 000 fuzzed inputs.
- **UTF-16** codec (`utf16`): surrogate pairs, byte-exact against `str::encode_utf16` / `char::decode_utf16`.
- **gap buffer** (`editbuffer`): a scalar-indexed editable text buffer (50 000-op model test).

### Networking (§54)
- **routing** (`route`): an IPv4 longest-prefix-match bit trie (oracle-tested against a brute-force scan).
- **DNS** (`dns`): message parsing with **bounded name decompression** (a decompression bomb is a clean parse failure).
- **DHCP** (`dhcp`): a client — DISCOVER/REQUEST build, OFFER/ACK/NAK parse, the INIT → SELECTING → REQUESTING → BOUND lease state machine.

### Data formats & utilities
- **Base64** (`base64`, RFC 4648, whitespace-tolerant) — RFC vectors.
- **JSON** (`json`, RFC 8259, recursive descent, **depth-limited** against stack overflow; numbers kept as source text since `core` has no `no_std` float parser).
- **semver** (`semver`, semver.org 2.0.0): parse, precedence (the canonical §11 chain), and a requirement matcher (`^`/`~`/ranges) with the Cargo/npm prerelease gate (§46).
- **glob** (`glob`): `*`/`?`/`[...]` matching for names and paths (single-star-pointer backtracking, terminates on adversarial patterns).
- **ring buffer** (`ring`): the bounded input/audio/log queue (§55/§56/§69).
- **hash map** (`hashmap`): a string-keyed FNV-1a open-addressing map for name tables (§29/§30).
- **kernel log buffer** (`klog`): a `dmesg`-style ring of level+message records that retains the most recent and drops the oldest (§69 structured logging / failure records); groundwork, not yet teed from the console.

## 3. How it was validated

- **Per-feature discipline:** a pure, host-tested library module; where it runs in the kernel, a boot proof (a serial marker matched by `xtask`) **and** a negative control (inject a fault → the boot test fails → restore).
- **Model-based tests** against reference oracles where there is one: `std` (UTF-8/UTF-16 codecs), `core::str::from_utf8`, `String::from_utf8_lossy`, `VecDeque` (ring), `BTreeMap` (hash map), `Vec<char>`/`Vec<bool>` (gap buffer, bitmap), a brute-force scan (routing).
- **Known-answer vectors** for format/algorithm correctness: RFC 4648 (Base64), zlib/gzip output (DEFLATE), the FNV-1a and Adler-32 published values, the semver.org precedence chain. These are test data only; the implementations are original (CLAUDE.md §3, §72).
- **Mutation soaks** (`crate::testing`, scaled by `ORIGIN_MUTATION_SCALE`): "never panics on mutated input" for every untrusted-input parser — ACPI (incl. SRAT/SLIT), net, TCP, GPT, PCI, NVMe, DNS, DHCP, OriginFS, ustar, DEFLATE/zlib/gzip, JSON.
- **Security properties that are explicitly tested:** tar path traversal, DNS decompression bombs, JSON nesting-depth overflow, DEFLATE decompression bombs, and bounded allocation on corrupt on-disk inodes.

## 4. Honest limitations

- **QEMU only.** Nothing is qualified on physical hardware (§74). QEMU TCG does not populate some structures (e.g. CPUID leaf 04h caches), which the relevant lines note.
- **Many new modules are host-tested libraries, not wired into the live kernel yet.** OriginFS, the text/codec stack, routing, DNS, DHCP, the data-format parsers, the containers — all compile for the kernel target and are proven by host tests, but only NUMA, entropy and the initramfs extraction run (and are boot-proven) in the kernel so far. They are the decision/parse layers a driver, syscall or service will call.
- **OriginFS (ADR-0006) is Proposed, not Accepted.** The parser is host-only and the kernel does not mount it, pending the owner's approval of the on-disk format (§84).
- **No compressor, no DRBG, no SipHash.** DEFLATE is decompression-only; the entropy source is true-random, not a reseedable DRBG; the hash map is not collision-flood-resistant. Each is noted where it would matter, with the reason it is deferred (a DRBG and SipHash need their own KATs/review, §59).
- **Not done / owner-only:** ADR-0005 (the kernel syscall layer) remains untouched; downloading NIST/Wycheproof vectors and `loom`; choosing a project licence; and merging into `main` / pushing.

## 5. Suggested next steps

1. **Owner decisions:** accept or revise ADR-0006 (OriginFS) so the read path can be mounted; approve the vector/`loom` downloads so the crypto and concurrency tests can be completed; choose a licence.
2. **Wire the decision layers into the kernel:** NUMA-aware frame allocation and scheduler placement (the `numa` map is the decision layer, §66); the keyboard IRQ → `ring` → `editbuffer` input path; mounting OriginFS beneath `vfs` once accepted.
3. **Finish the networking path:** a NIC driver and a UDP/TCP socket layer to carry the existing route/DNS/DHCP logic on the wire.
4. **Package/runtime:** a dependency resolver over `semver`, and reading real `.osi`/`.oapp` payloads through `ustar` + `deflate`.

## 6. Where the detail is

- Per-module evidence, unsafe inventories, specs and negative controls: `docs/engineering/SOURCE_PROVENANCE.md`.
- Milestone state: `docs/STATUS.md`.
- The full commit list and lessons: `docs/engineering/handoffs/2026-11-07-local-handoff.md`.
- Each commit message lists its tests, boot-test lines and controls.
