# Origin OS

[Browse the public source and documentation index](https://vighnesha-os.github.io/origin-os/).

Origin OS is an original-source operating system for x86-64, written in Rust. It has its own kernel (the Framekernel), its own drivers and its own native SDK. It is planned as two editions of one release, Origin Desktop and Origin Server, which share one kernel, one SDK and the native desktop.

It is in early development: [`docs/PROGRESS.md`](docs/PROGRESS.md) says how much is done.

- **The directive.** [`CLAUDE.md`](CLAUDE.md) is the engineering directive that every change follows.
- **Original source.** Origin's own code is written for this project from published specifications and vendor documents. No code is copied or translated from another operating system (`CLAUDE.md` §3). [`docs/engineering/SOURCE_PROVENANCE.md`](docs/engineering/SOURCE_PROVENANCE.md) records what each crate was written from.
- **Hardware scope.** 64-bit x86 only, started by UEFI: Intel platforms introduced in 2026 or later, and AMD platforms introduced in 2026 or later with Zen 5 or newer cores ([ADR-0009](docs/adr/ADR-0009-hardware-targets.md)).

## Progress

[`docs/PROGRESS.md`](docs/PROGRESS.md) shows how much of Origin OS is done, built, in progress and not started: overall, by milestone and by feature. It is generated from the requirements matrix by `cargo xtask progress`. `cargo xtask check` refuses a pull request whose page is not the one its tree generates, so the page on `main` is always current.

[`docs/ROADMAP.md`](docs/ROADMAP.md) lists the milestones and their exit tests, and [`docs/STATUS.md`](docs/STATUS.md) says what each task built.

A result from QEMU, or from a virtual machine on a real processor, is not hardware qualification. The progress page says where each requirement's evidence ran.

## Building and running

You need:
- **rustup.** [`rust-toolchain.toml`](rust-toolchain.toml) selects the pinned toolchain, Rust 1.100.0.
- **QEMU with its EDK2 x86-64 firmware.** CI builds QEMU 11.1.3, and the boot checks are written against what that version prints. On macOS, `brew install qemu`.

The project is developed on macOS on Apple Silicon. CI builds and boots it on Linux, and compares the images a macOS host builds with the Linux host's.

```bash
bash developer-tools/toolchain/verify.sh --install   # first time: install and verify the pinned toolchain
cargo xtask check                                    # formatting, Clippy, host tests, concurrency models and the repository's gates
cargo xtask build                                    # build the UEFI loader and the kernel image
cargo xtask boot-test                                # boot the loader and the kernel under QEMU + OVMF, and check what they report
cargo xtask boot-matrix                              # boot-test on every emulated machine; much longer
cargo xtask run                                      # boot under QEMU with the serial console on this terminal (Ctrl-A X quits)
```

`cargo xtask` alone lists every command. Each image is built for its own target, so plain `cargo build` and `cargo test` at the root reach only the crates that build for the host.

The release version is written only in `origin-version.toml`; every module reads it through the generated `origin-version` crate, and `cargo xtask check` rejects a version in any Cargo manifest.

## Where things are

| Folder | What it holds |
|---|---|
| [`boot/`](boot) | The UEFI loader, and the boot information it hands to the kernel |
| [`framekernel/`](framekernel) | The kernel: its small privileged framework (`frame/`), paging, and the generated system-call code |
| [`firmware/acpi/`](firmware/acpi) | ACPI tables and the AML interpreter |
| [`security/`](security) | Cryptography and the random-number generator |
| [`runtime/`](runtime) | The native binary format (ONBF), the ring-3 runtime and `origin.core`, and the release version |
| [`sdk/`](sdk) | Interface definitions in the Origin interface language, and the SDK's first library |
| [`services/`](services) | The first system service |
| [`developer-tools/`](developer-tools) | `xtask` (the build tool), the ONBF packer and signer, the interface compiler, and the toolchain verification |
| [`tests/`](tests) | Toolchain regression tests, the programs the kernel carries for its boot checks, and a test-only UEFI program for the emulated machines |
| [`docs/`](docs) | Everything written down: see below |

**In `docs/`:**
- [`adr/`](docs/adr): the architecture decisions.
- [`spec/`](docs/spec): the specifications Origin defines, such as [ONBF](docs/spec/ONBF.md) and the [interface language](docs/spec/OIDL.md).
- [`requirements/`](docs/requirements/README.md): the requirements matrix, one row for each statement of the directive and each decision of an ADR.
- [`research/`](docs/research): what was read before each subsystem was designed, with sources and dates.
- [`engineering/`](docs/engineering): the task list ([`TASKS.md`](docs/engineering/TASKS.md)), the owner's decisions ([`OWNER_DECISIONS.md`](docs/engineering/OWNER_DECISIONS.md)), provenance, reviews, the threat model and the performance budgets.
- [`hardware/`](docs/hardware): records of runs on real machines, and the shortlist of test machines.
- [`toolchain/`](docs/toolchain/VERIFICATION.md): the toolchain's verification report.

## Contributing

[`CONTRIBUTING.md`](CONTRIBUTING.md) says what a contribution needs: it must be original, and every commit carries a sign-off. [`CODEOWNERS`](CODEOWNERS) names the owners of the kernel and of the SDK's interfaces.

Do not put the details of a vulnerability in a public issue or pull request: [`SECURITY.md`](SECURITY.md) says how to report one privately.

## Licence

Origin OS is licensed under either of

- the Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)), or
- the MIT License ([LICENSE-MIT](LICENSE-MIT)),

at your option. Third-party test data in this repository keeps its own licence, stated beside it.

