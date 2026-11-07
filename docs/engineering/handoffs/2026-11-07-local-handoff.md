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

Commits: `b5a446a` (M0), `87f50fd` (workspace + loader), then the T5 commit that adds this file. The owner approved all three.

`cargo xtask check` and `cargo xtask boot-test` pass at the end of the session.

## 2. Decision taken: T9 comes before M1 step 2

KRN, the Framekernel image, is an ONBF profile (ONBF §6.5), and only the boot loader parses it, through `onbf-format` (ONBF §8.2). An ELF-loading interim loader would contradict ADR-0001 and would be thrown away. The order is therefore:

1. **T9 `onbf-format`** in `runtime/onbf/`. It is host-testable: `no_std`, no alloc, `forbid(unsafe_code)`, and the no-panic lints from ONBF §12. It parses the header, sections, content digest and OCBOR-1, and starts the malicious corpus (§13 item 4). The `origin-crypto` SHA-512 it needs for the content digest is its own task.
2. **A conversion path for KRN**: an `onbf-pack` subset that turns the kernel's ELF linker output into KRN.
3. **M1 step 2**:
   - a *Proposed* boot ADR covering the boot-info structure and its ABI version (add it to `origin-version.toml [compatibility]` then), KASLR and relocation, and the trust anchor;
   - a `framekernel` crate for `x86_64-unknown-none` that writes to COM1;
   - the loader steps: memory map, load KRN, `ExitBootServices` (clear `PANIC_CONSOLE` first), jump;
   - `boot-test` assertions on COM1 output, with an `isa-debug-exit` exit path.

## 3. Waiting on the owner

- Merge `m0/toolchain-verification` into `main`, and push. Both are the owner's call; nothing has been pushed.
- A project licence. Every provenance entry says "not chosen yet".
- The boot trust anchor (Microsoft UEFI CA 2023, or Origin keys enrolled in firmware) for the boot ADR.
- Whether to install Rosetta 2 for the *executed* FFI-bool test (§10). Only the assembly-level check exists so far.
- The starting release values in `origin-version.toml` (`0.1.0-dev`, build 0, SVN 0) are a development default. Change them if you prefer others.

## 4. Environment facts worth keeping

- Running `rustup` inside the repository auto-installs the pinned toolchain. Use `RUSTUP_AUTO_INSTALL=0` to prevent that, which `verify.sh` does in check mode.
- QEMU ignores `SIGALRM`, so a test harness must kill it. `xtask boot-test` does this.
- The shell in the Claude Code desktop app is zsh, where `PIPESTATUS` is `pipestatus` and indexed from 1.
- OVMF's ConOut is mirrored to the serial port with ANSI sequences; `xtask/src/serial.rs` strips them.
- `-cpu max` under TCG on Apple Silicon. x86-64-v3 code in userspace will need the emulated CPU to expose AVX2: check before M6.
