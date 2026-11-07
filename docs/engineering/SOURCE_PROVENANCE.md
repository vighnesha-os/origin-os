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

## `boot/uefi/` (`origin-boot-uefi`), the UEFI loader, M1 step 1

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

## `developer-tools/xtask/` (`xtask`)

- **What:** host-side build orchestration, with four commands:
  - `check`: rustfmt, clippy and tests per crate and target; the UEFI release build; two lints:
    - CLAUDE.md §11: no `target_os = "linux"` in any Rust file outside `developer-tools/`;
    - CLAUDE.md §62: no `version` in the `[package]` or `[workspace.package]` section of any Cargo manifest.
  - `build`.
  - `boot-test`: stages a fresh ESP (`\EFI\BOOT\BOOTX64.EFI`, UEFI 2.11 §3.5.1.1) and a fresh NVRAM copy; boots QEMU with serial on a pipe; requires the loader's lines and OVMF's status line in order; then stops QEMU.
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
