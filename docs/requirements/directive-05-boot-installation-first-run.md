# Requirements matrix: PART V, Boot, installation and first-run experience

One row for each statement of CLAUDE.md in §19 to §23. The columns and their rules are in [README.md](README.md), and `cargo xtask requirements-gate` holds this file to them and to the directive.

| ID | Requirement | Kind | Scope | Owner | State | Implementation | Acceptance | Evidence | Env | Review | Limitations | Decided by |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| S19-01 | Implement a real x86-64 UEFI bootloader. | build | both/hw | 7 | built | boot/uefi, boot/info | The loader boots the signed kernel on each declared machine (§74), and refuses each unsigned or tampered image with its own message. | xtask:boot-test, xtask:boot-matrix | virt | agent | The trust store holds a test key only, and nothing verifies the loader itself. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S19-02 | Required: Kernel image loading. | build | both/hw | 7 | built | boot/uefi, boot/info | The loader boots the signed kernel on each declared machine (§74), and refuses each unsigned or tampered image with its own message. | xtask:boot-test, xtask:boot-matrix | virt | agent | The trust store holds a test key only, and nothing verifies the loader itself. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S19-03 | Required: Boot-image validation. | build | both/hw | 7 | built | boot/uefi, boot/info | The loader boots the signed kernel on each declared machine (§74), and refuses each unsigned or tampered image with its own message. | xtask:boot-test, xtask:boot-matrix | virt | agent | The trust store holds a test key only, and nothing verifies the loader itself. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S19-04 | Required: Memory-map acquisition. | build | both/hw | 7 | built | boot/uefi, boot/info | The loader boots the signed kernel on each declared machine (§74), and refuses each unsigned or tampered image with its own message. | xtask:boot-test, xtask:boot-matrix | virt | agent | The trust store holds a test key only, and nothing verifies the loader itself. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S19-05 | Required: ACPI discovery. | build | both/hw | 7 | built | boot/uefi, boot/info | The loader boots the signed kernel on each declared machine (§74), and refuses each unsigned or tampered image with its own message. | xtask:boot-test, xtask:boot-matrix | virt | agent | The trust store holds a test key only, and nothing verifies the loader itself. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S19-06 | Required: Boot-info structure. | build | both/hw | 7 | built | boot/uefi, boot/info | The loader boots the signed kernel on each declared machine (§74), and refuses each unsigned or tampered image with its own message. | xtask:boot-test, xtask:boot-matrix | virt | agent | The trust store holds a test key only, and nothing verifies the loader itself. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S19-07 | Required: Stack setup. | build | both/hw | 7 | built | boot/uefi, boot/info | The loader boots the signed kernel on each declared machine (§74), and refuses each unsigned or tampered image with its own message. | xtask:boot-test, xtask:boot-matrix | virt | agent | The trust store holds a test key only, and nothing verifies the loader itself. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S19-08 | Required: Page-table setup where applicable. | build | both/hw | 7 | built | boot/uefi, boot/info | The loader boots the signed kernel on each declared machine (§74), and refuses each unsigned or tampered image with its own message. | xtask:boot-test, xtask:boot-matrix | virt | agent | The trust store holds a test key only, and nothing verifies the loader itself. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S19-09 | Required: Correct `ExitBootServices`. | build | both/hw | 7 | built | boot/uefi, boot/info | The loader boots the signed kernel on each declared machine (§74), and refuses each unsigned or tampered image with its own message. | xtask:boot-test, xtask:boot-matrix | virt | agent | The trust store holds a test key only, and nothing verifies the loader itself. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S19-10 | Required: CPU entry preparation. | build | both/hw | 7 | built | boot/uefi, boot/info | The loader boots the signed kernel on each declared machine (§74), and refuses each unsigned or tampered image with its own message. | xtask:boot-test, xtask:boot-matrix | virt | agent | The trust store holds a test key only, and nothing verifies the loader itself. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S19-11 | Required: Early diagnostics. | build | both/hw | 7 | built | boot/uefi, boot/info | The loader boots the signed kernel on each declared machine (§74), and refuses each unsigned or tampered image with its own message. | xtask:boot-test, xtask:boot-matrix | virt | agent | The trust store holds a test key only, and nothing verifies the loader itself. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S19-12 | Required: Secure Boot integration. | build | both/hw | 17 | not-started | - | - | - | - | none | The loader verifies the kernel's signature (ADR-0002 B2). UEFI Secure Boot, with Origin's keys enrolled, is not built. | - |
| S19-13 | Required: Recovery boot support. | build | both/hw | 7 | not-started | - | - | - | - | none | - | - |
| S19-14 | The UEFI loader, kernel image and `.ose` user executable are distinct binary concepts. | rule | both/hw | 7 | not-started | - | - | - | - | none | - | - |
| S19-15 | Secure Boot trust: by default, users and OEMs enroll Origin's keys. A first stage signed by Microsoft follows once an organisation exists that can hold that signature (ADR-0002 B7, Amendment 1). | build | both/hw | 17 | not-started | - | - | - | - | none | - | 78 |
| S20-01 | Implement: Boot entries. | build | both/hw | 7 | not-started | - | - | - | - | none | - | - |
| S20-02 | Implement: Default boot selection. | build | both/hw | 7 | not-started | - | - | - | - | none | - | - |
| S20-03 | Implement: Boot timeout policy. | build | both/hw | 7 | not-started | - | - | - | - | none | - | - |
| S20-04 | Implement: Recovery selection. | build | both/hw | 7 | not-started | - | - | - | - | none | - | - |
| S20-05 | Implement: Active deployment selection. | build | both/hw | 7 | not-started | - | - | - | - | none | - | - |
| S20-06 | Implement: Previous known-good deployment selection. | build | both/hw | 7 | not-started | - | - | - | - | none | - | - |
| S20-07 | Implement: Boot-attempt recording. | build | both/hw | 7 | not-started | - | - | - | - | none | - | - |
| S20-08 | Implement: Boot-failure detection. | build | both/hw | 7 | not-started | - | - | - | - | none | - | - |
| S20-09 | Implement: Boot configuration validation. | build | both/hw | 7 | not-started | - | - | - | - | none | - | - |
| S20-10 | Ensure that boot configuration changes are recoverable. | build | both/hw | 7 | not-started | - | - | - | - | none | - | - |
| S21-01 | Create a bootable Origin OS installation environment. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S21-02 | Support: UEFI boot from installation media. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S21-03 | Support: Storage discovery. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S21-04 | Support: NVMe discovery. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S21-05 | Support: GPT partitioning. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S21-06 | Support: EFI System Partition creation. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S21-07 | Support: Origin system partitions/deployments. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S21-08 | Support: User-data partition or volume. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S21-09 | Support: Recovery partition or equivalent. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S21-10 | Support: Filesystem formatting. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S21-11 | Support: Encryption setup. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S21-12 | Support: Safe disk selection. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S21-13 | Support: Data-loss warnings. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S21-14 | Support: Installation progress. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S21-15 | Support: Installation verification. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S21-16 | Support: Bootloader installation. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S21-17 | Support: Initial system deployment. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S21-18 | Support: Reboot into the installed system. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S21-19 | Do not call a bootable QEMU kernel image a complete OS installer. | rule | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S22-01 | The installation UI must use native Origin services and typed APIs. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S22-02 | It must not call hidden shell commands to partition disks or install the system. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S22-03 | For early bring-up, a recovery console may exist, but the final production installer must provide an accessible native GUI. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S23-01 | Implement Origin Setup for: Language and locale. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S23-02 | Implement Origin Setup for: Keyboard layout. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S23-03 | Implement Origin Setup for: Time zone. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S23-04 | Implement Origin Setup for: Account creation. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S23-05 | Implement Origin Setup for: Authentication credential setup. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S23-06 | Implement Origin Setup for: Device naming. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S23-07 | Implement Origin Setup for: Network setup. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S23-08 | Implement Origin Setup for: Storage-encryption configuration. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S23-09 | Implement Origin Setup for: Privacy choices. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S23-10 | Implement Origin Setup for: Accessibility preferences. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S23-11 | Implement Origin Setup for: Initial update check where available. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S23-12 | Provide offline first-run support. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S23-13 | Do not require online account creation unless explicitly approved as a product requirement. | rule | both/hw | 21 | not-started | - | - | - | - | none | - | - |
