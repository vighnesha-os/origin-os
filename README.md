# origin-os
Origin Operating System

An original-source x86-64 operating system written in Rust. `CLAUDE.md` is the engineering directive; `docs/` holds the specifications, ADRs, research and provenance records.

## Building

Requirements: rustup (the pinned toolchain, Rust 1.99.1, is selected by `rust-toolchain.toml`), and QEMU with its EDK2 x86-64 firmware for boot tests (`brew install qemu` on macOS).

```bash
bash developer-tools/toolchain/verify.sh --install   # first time: install and verify the toolchain
cargo xtask check                                    # fmt, clippy, unit tests, UEFI build, lints
cargo xtask boot-test                                # boot the UEFI loader under QEMU + OVMF
cargo xtask run                                      # same, with the serial console on this terminal
```

The release version is written only in `origin-version.toml`; every module reads it through the generated `origin-version` crate, and `cargo xtask check` rejects a version in any Cargo manifest.

Status: M0 (toolchain) and the first step of M1 (a UEFI loader that reports itself and returns to the firmware). There is no kernel yet. QEMU results are not hardware qualification.

## Licence

Origin OS is licensed under either of

- the Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)), or
- the MIT License ([LICENSE-MIT](LICENSE-MIT)),

at your option. Third-party test data in this repository keeps its own licence, stated beside it.

