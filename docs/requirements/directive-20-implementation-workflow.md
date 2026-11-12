# Requirements matrix: PART XX, Required implementation workflow

One row for each statement of CLAUDE.md in §83 to §85. The columns and their rules are in [README.md](README.md), and `cargo xtask requirements-gate` holds this file to them and to the directive.

| ID | Requirement | Kind | Scope | Owner | State | Implementation | Acceptance | Evidence | Env | Review | Limitations | Decided by |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| S83-01 | Follow: 1. Inspect existing implementation. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S83-02 | Follow: 2. Verify current applicable standards. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S83-03 | Follow: 3. Read relevant ADRs. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S83-04 | Follow: 4. Identify source ownership. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S83-05 | Follow: 5. Specify behavior and invariants. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S83-06 | Follow: 6. Define tests. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S83-07 | Follow: 7. Implement original Rust source. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S83-08 | Follow: 8. Compile with the approved toolchain. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S83-09 | Follow: 9. Run relevant tests. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S83-10 | Follow: 10. Investigate failures. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S83-11 | Follow: 11. Review unsafe code. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S83-12 | Follow: 12. Review security. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S83-13 | Follow: 13. Benchmark when applicable. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S83-14 | Follow: 14. Review maintainability. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S83-15 | Follow: 15. Verify compatibility. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S83-16 | Follow: 16. Integrate. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S83-17 | Follow: 17. Record source provenance. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S83-18 | Follow: 18. Save durable task state. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S83-19 | No implementation is complete merely because source code was generated. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S84-01 | Immediately: 1. Inspect actual repository state. | build | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S84-02 | Immediately: 2. Check Rust 1.99.1 availability. | build | eng/host | 28 | built | developer-tools/toolchain/verify.sh, .github/workflows/ci.yml | `verify.sh` passes in every build environment, and CI runs it. | file:docs/toolchain/VERIFICATION.md, file:.github/workflows/ci.yml | host | none | Passes on the development host, recorded on 2026-11-12 from a tree with nothing uncommitted. CI's workflow runs it since TASKS 22a, and no run of that workflow exists yet: the run on the batch's pull request is the proof. | - |
| S84-03 | Immediately: 3. Verify Cargo and cross-compilation targets. | build | eng/host | 28 | built | developer-tools/toolchain/verify.sh, .github/workflows/ci.yml | `verify.sh` passes in every build environment, and CI runs it. | file:docs/toolchain/VERIFICATION.md, file:.github/workflows/ci.yml | host | none | Passes on the development host, recorded on 2026-11-12 from a tree with nothing uncommitted. CI's workflow runs it since TASKS 22a, and no run of that workflow exists yet: the run on the batch's pull request is the proof. | - |
| S84-04 | Immediately: 4. Verify QEMU/OVMF. | build | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S84-05 | Immediately: 5. Verify linker operation on Apple Silicon. | build | eng/host | 28 | built | developer-tools/toolchain/verify.sh, tests/toolchain, boot/uefi/src/uefi.rs | Each regression has a test that fails on a compiler that has it, run by `verify.sh` in every build environment. | file:tests/toolchain/none_smoke.rs, file:tests/toolchain/ffi_bool.rs, test:boot/uefi/src/uefi.rs::char16_len, file:docs/toolchain/VERIFICATION.md | host | none | Passes on the development Mac, an Apple M1 Max, recorded on 2026-11-12. CI's runners are Linux x86-64, so no CI run shows this row. Not run on Rust 1.99.0, the compiler that has the regressions. | - |
| S84-06 | Immediately: 6. Initialize the minimum required workspace. | build | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S84-07 | Immediately: 7. Create canonical Origin OS versioning. | build | eng/host | 25 | done | origin-version.toml | `origin-version.toml` is in the repository. | file:origin-version.toml | host | none | - | - |
| S84-08 | Immediately: 8. Create research and provenance records. | build | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S84-09 | Immediately: 9. Implement the first UEFI bootloader. | build | both/hw | 7 | built | boot/uefi, boot/info | The loader boots the signed kernel on each declared machine (§74), and refuses each unsigned or tampered image with its own message. | xtask:boot-test, xtask:boot-matrix | virt | agent | The trust store holds a test key only, and nothing verifies the loader itself. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S84-10 | Immediately: 10. Implement initial Framekernel entry. | build | both/hw | 3 | built | framekernel/frame/entry/main.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | xtask:boot-test | virt | agent | Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S84-11 | Immediately: 11. Build a QEMU boot image. | build | eng/host | 28 | done | developer-tools/xtask/src/main.rs, developer-tools/xtask/src/disk.rs | `cargo xtask` builds the loader, packs the kernel's image and makes the disk QEMU boots from. | xtask:build, xtask:boot-test | host | none | - | - |
| S84-12 | Immediately: 12. Execute and capture boot tests. | build | eng/emu | 28 | done | developer-tools/xtask/src/main.rs, .github/workflows/ci.yml | `cargo xtask boot-test` boots the image under QEMU, captures the serial log and checks it, locally and in CI. | xtask:boot-test, file:.github/workflows/ci.yml | virt | none | - | - |
| S84-13 | Immediately: 13. Implement host-testable native-format parsers in independent tasks. | build | both/hw | 18 | partial | runtime/onbf/format, runtime/onbf/merkle, runtime/onbf/verify, developer-tools/onbf-pack | Each native format's parser is tested on the host, with soaks, before anything loads the format. | xtask:boot-test, file:runtime/onbf/format/src/tests.rs, file:runtime/onbf/format/src/tests_soak_image.rs | virt | agent | The container, OCBOR-1, the KRN manifest, the signature block and the Merkle tree have host-tested parsers. The EXE, LIB, OSI, OAPP and OD profiles have none. | - |
| S84-14 | Immediately: 14. Start SMP/topology architecture development. | build | both/hw | 4 | built | framekernel/frame/src/smp.rs, framekernel/frame/src/apstart.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:application processors, proof:per-processor control state | virt | agent | 1 to 8 processors are tested, and 64 is the most (ADR-0008 C6). Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S84-15 | Immediately: 15. Record actual results and failures. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S84-16 | Immediately: 16. Continue to the next safe, unblocked engineering task. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S84-17 | Do not stop after writing plans. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S84-18 | Exception: ABI and format decisions (ADRs) require owner approval before they are Accepted. Agents may build host-testable parsers against a Proposed specification, but must not ship them. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S85-01 | Report: Actual source code implemented. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S85-02 | Report: Files changed. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S85-03 | Report: Build commands. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S85-04 | Report: Compiler version. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S85-05 | Report: Test commands. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S85-06 | Report: Test results. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S85-07 | Report: Security review. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S85-08 | Report: Performance results. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S85-09 | Report: Research decisions. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S85-10 | Report: Source provenance. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S85-11 | Report: Architecture changes. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S85-12 | Report: Unified-version consistency. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S85-13 | Report: Git commit information. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S85-14 | Report: Known limitations. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S85-15 | Report: Next tasks. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
| S85-16 | Never fabricate test results or implementation status. | rule | eng/host | 28 | not-started | - | - | - | - | none | - | - |
