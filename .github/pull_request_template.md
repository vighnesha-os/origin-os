## What this changes

<!-- One or two sentences. Name the TASKS item, ADR, issue or requirement it implements. -->

## Checklist

See [CONTRIBUTING.md](../CONTRIBUTING.md) for each rule.

- [ ] Every commit is signed off (`git commit -s`), with the declaration of original work that `CONTRIBUTING.md` asks for.
- [ ] The code is written for Origin OS. Nothing in it is copied, translated or reproduced from another project (CLAUDE.md §3).
- [ ] The pinned toolchain built it, and `rust-toolchain.toml` is unchanged.
- [ ] I ran these, and they passed (paste the last lines under Evidence):
  - [ ] `cargo xtask check`
  - [ ] `cargo xtask boot-test`, if the loader or the kernel changed
  - [ ] `cargo xtask boot-matrix`, if the kernel, the loader or a driver changed
- [ ] Any new `unsafe` is in a file the gate allows, and states the invariants it relies on.
- [ ] The records the change affects are updated: provenance, the requirement rows it moves, and `PROGRESS.md` (`cargo xtask progress`).
- [ ] The change, its commits and this description hold no personal data, keys or secrets, and no details of a vulnerability.

## Evidence

<!-- What ran, where (host, QEMU's software processor, KVM, a physical machine), and what it printed. Say what this does not show. -->
