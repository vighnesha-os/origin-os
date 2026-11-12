# Requirements matrix: PART VII, Device and hardware support

One row for each statement of CLAUDE.md in §31 to §38. The columns and their rules are in [README.md](README.md), and `cargo xtask requirements-gate` holds this file to them and to the directive.

| ID | Requirement | Kind | Scope | Owner | State | Implementation | Acceptance | Evidence | Env | Review | Limitations | Decided by |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| S31-01 | Implement: Device discovery. | build | both/hw | 9 | partial | framekernel/src/pcie.rs, framekernel/frame/src/pci.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:PCIe enumeration | virt | agent | PCIe functions are listed through the MCFG. No ACPI or USB discovery. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S31-02 | Implement: Resource assignment. | build | both/hw | 9 | not-started | - | - | - | - | none | - | - |
| S31-03 | Implement: Driver binding. | build | both/hw | 9 | not-started | - | - | - | - | none | - | - |
| S31-04 | Implement: Interrupt management. | build | both/hw | 9 | partial | framekernel/frame/src/irq.rs, framekernel/frame/src/pci.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:IOMMU proofs through NVMe, proof:remapped I/O APIC | virt | agent | MSI-X vectors and I/O APIC pins are registered through the framework and remapped. No level-triggered interrupt. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S31-05 | Implement: DMA mapping. | build | both/hw | 9 | built | framekernel/frame/src/dma.rs, framekernel/frame/src/iommu.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:IOMMU proofs through NVMe, proof:IOMMU bring-up, proof:bus mastering after the drivers | virt | agent | Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S31-06 | Implement: Device lifecycle. | build | both/hw | 9 | not-started | - | - | - | - | none | - | - |
| S31-07 | Implement: Driver isolation. | build | both/hw | 9 | not-started | - | - | - | - | none | - | - |
| S31-08 | Implement: User-mode drivers where feasible. | build | both/hw | 9 | not-started | - | - | - | - | none | - | - |
| S31-09 | Implement: Kernel-mode drivers where justified. These are first-party only (ADR-0001). | build | both/hw | 9 | partial | framekernel/src/nvme.rs, framekernel/src/virtionet.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:NVMe, proof:network packets | virt | agent | Two first-party drivers run in the kernel. No package or signature binds a driver yet. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S31-10 | Implement: Device reset. | build | both/hw | 9 | not-started | - | - | - | - | none | - | - |
| S31-11 | Implement: Device suspension. | build | both/hw | 9 | not-started | - | - | - | - | none | - | - |
| S31-12 | Implement: Hot-plug. | build | both/hw | 9 | not-started | - | - | - | - | none | - | - |
| S31-13 | Implement: Driver fault handling. | build | both/hw | 9 | not-started | - | - | - | - | none | - | - |
| S31-14 | Implement: Recovery. | build | both/hw | 9 | not-started | - | - | - | - | none | - | - |
| S31-15 | Implement: Rollback. | build | both/hw | 9 | not-started | - | - | - | - | none | - | - |
| S31-16 | Drivers must use a documented Origin Driver ABI. | build | both/hw | 9 | not-started | - | - | - | - | none | - | - |
| S31-17 | Programs reach every device technology through typed SDK interfaces in three layers (ADR-0015): the device class; | build | both/hw | 9 | not-started | - | - | - | - | none | - | 82 |
| S31-18 | Programs reach every device technology through typed SDK interfaces in three layers (ADR-0015): everything the published standard defines; | build | both/hw | 9 | not-started | - | - | - | - | none | - | 82 |
| S31-19 | Programs reach every device technology through typed SDK interfaces in three layers (ADR-0015): vendor extras, only where the vendor documents them. | build | both/hw | 9 | not-started | - | - | - | - | none | - | 82 |
| S31-20 | Each device reports truthfully which features it supports. An unsupported feature returns "not supported", never a pretended success. | build | both/hw | 9 | not-started | - | - | - | - | none | - | 82 |
| S31-21 | Dangerous operations need rights of their own: firmware update, format, sanitize, device approval and radio transmit settings. Raw device access is a diagnostics right for administrators only. Radio host stacks and the USB4 connection manager run unprivileged. | build | both/hw | 9 | not-started | - | - | - | - | none | - | 83 |
| S32-01 | Support: Host bridge discovery. | build | both/hw | 8 | built | framekernel/src/pcie.rs, framekernel/frame/src/pci.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:PCIe enumeration | virt | agent | QEMU's machine: the MCFG's windows only. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S32-02 | Support: PCIe enumeration. | build | both/hw | 8 | built | framekernel/src/pcie.rs, framekernel/frame/src/pci.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:PCIe enumeration | virt | agent | QEMU's machine: the MCFG's windows only. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S32-03 | Support: Configuration access. | build | both/hw | 8 | built | framekernel/src/pcie.rs, framekernel/frame/src/pci.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:PCIe enumeration | virt | agent | QEMU's machine: the MCFG's windows only. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S32-04 | Support: BAR mapping. | build | both/hw | 8 | built | framekernel/src/pcie.rs, framekernel/frame/src/pci.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:PCIe enumeration | virt | agent | QEMU's machine: the MCFG's windows only. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S32-05 | Support: MSI/MSI-X. | build | both/hw | 8 | partial | framekernel/frame/src/pci.rs, framekernel/frame/src/irq.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:IOMMU proofs through NVMe, xtask:boot-x2apic | virt | agent | MSI-X for two devices. Plain MSI is not used. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S32-06 | Support: DMA. | build | both/hw | 8 | built | framekernel/frame/src/dma.rs, framekernel/frame/src/iommu.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:IOMMU proofs through NVMe, proof:IOMMU bring-up, proof:bus mastering after the drivers | virt | agent | Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S32-07 | Support: Intel VT-d. | build | both/hw | 8 | built | framekernel/frame/src/vtd.rs, framekernel/frame/src/dmar.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:IOMMU bring-up, proof:IOMMU proofs through NVMe, proof:remapped I/O APIC | virt | agent | QEMU's VT-d only: legacy mode, RMRR identity maps shown only for ranges a test-only firmware step plants (`boot-reserved`), and it cannot show the source-ID check on I/O APIC interrupts. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S32-08 | Support: AMD IOMMU. | build | both/hw | 8 | built | framekernel/frame/src/amdvi.rs, framekernel/frame/src/ivrs.rs, framekernel/frame/src/iommu | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:IOMMU bring-up, proof:IOMMU proofs through NVMe, xtask:boot-matrix | virt | agent | QEMU's AMD-Vi only: it records faults at address 0 and logs no interrupt fault. I/O APIC pins are not routed, and IVMD maps are shown only for ranges a test-only firmware step plants (`boot-reserved`). Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S32-09 | Support: Interrupt remapping. | build | both/hw | 8 | built | framekernel/frame/src/vtd.rs, framekernel/frame/src/dmar.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:IOMMU bring-up, proof:IOMMU proofs through NVMe, proof:remapped I/O APIC | virt | agent | QEMU's VT-d only: legacy mode, RMRR identity maps shown only for ranges a test-only firmware step plants (`boot-reserved`), and it cannot show the source-ID check on I/O APIC interrupts. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S32-10 | Support: PCIe error reporting. | build | both/hw | 8 | not-started | - | - | - | - | none | - | - |
| S32-11 | Support: Device reset. | build | both/hw | 8 | not-started | - | - | - | - | none | - | - |
| S32-12 | Support: Hot-plug. | build | both/hw | 8 | not-started | - | - | - | - | none | - | - |
| S32-13 | Support: External-device authorization. | build | both/hw | 8 | not-started | - | - | - | - | none | - | - |
| S32-14 | Do not implement PHY signaling in software. | rule | both/hw | 8 | not-started | - | - | - | - | none | - | - |
| S33-01 | Implement: Controller discovery. | build | both/hw | 57 | built | framekernel/src/nvme.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:NVMe, proof:IOMMU proofs through NVMe | virt | agent | QEMU's NVMe controller only. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S33-02 | Implement: Initialization. | build | both/hw | 57 | built | framekernel/src/nvme.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:NVMe, proof:IOMMU proofs through NVMe | virt | agent | QEMU's NVMe controller only. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S33-03 | Implement: Admin and I/O queues. | build | both/hw | 57 | built | framekernel/src/nvme.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:NVMe, proof:IOMMU proofs through NVMe | virt | agent | QEMU's NVMe controller only. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S33-04 | Implement: Namespace discovery. | build | both/hw | 57 | built | framekernel/src/nvme.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:NVMe, proof:IOMMU proofs through NVMe | virt | agent | QEMU's NVMe controller only. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S33-05 | Implement: DMA. | build | both/hw | 57 | built | framekernel/src/nvme.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:NVMe, proof:IOMMU proofs through NVMe | virt | agent | QEMU's NVMe controller only. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S33-06 | Implement: Multiqueue operation. | build | both/hw | 57 | built | framekernel/src/nvme.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:NVMe, proof:IOMMU proofs through NVMe | virt | agent | QEMU's NVMe controller only. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S33-07 | Implement: Interrupt handling. | build | both/hw | 57 | built | framekernel/src/nvme.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:NVMe, proof:IOMMU proofs through NVMe | virt | agent | QEMU's NVMe controller only. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S33-08 | Implement: Queue timeouts. | build | both/hw | 57 | not-started | - | - | - | - | none | - | - |
| S33-09 | Implement: Controller resets. | build | both/hw | 57 | not-started | - | - | - | - | none | The controller is reset once, when the driver attaches. No reset recovers from an error. | - |
| S33-10 | Implement: Flushes. | build | both/hw | 57 | built | framekernel/src/nvme.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:NVMe, proof:IOMMU proofs through NVMe | virt | agent | QEMU's NVMe controller only. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S33-11 | Implement: Data-integrity handling. | build | both/hw | 57 | not-started | - | - | - | - | none | - | - |
| S33-12 | Implement: Power states. | build | both/hw | 57 | not-started | - | - | - | - | none | - | - |
| S33-13 | Implement: Health reporting. | build | both/hw | 57 | not-started | - | - | - | - | none | - | - |
| S33-14 | Implement: Recovery. | build | both/hw | 57 | not-started | - | - | - | - | none | - | - |
| S33-15 | Treat PCIe generation and NVMe protocol revision separately. | rule | both/hw | 57 | not-started | - | - | - | - | none | - | - |
| S34-01 | Implement xHCI and required USB protocols. | build | both/hw | 40 | not-started | - | - | - | - | none | - | - |
| S34-02 | Include: USB 2 compatibility. | build | both/hw | 40 | not-started | - | - | - | - | none | - | - |
| S34-03 | Include: USB 3.0/3.1/3.2. | build | both/hw | 40 | not-started | - | - | - | - | none | - | - |
| S34-04 | Include: Hub management. | build | both/hw | 40 | not-started | - | - | - | - | none | - | - |
| S34-05 | Include: Device enumeration. | build | both/hw | 40 | not-started | - | - | - | - | none | - | - |
| S34-06 | Include: Control transfers. | build | both/hw | 40 | not-started | - | - | - | - | none | - | - |
| S34-07 | Include: Bulk transfers. | build | both/hw | 40 | not-started | - | - | - | - | none | - | - |
| S34-08 | Include: Interrupt transfers. | build | both/hw | 40 | not-started | - | - | - | - | none | - | - |
| S34-09 | Include: Isochronous transfers. | build | both/hw | 40 | not-started | - | - | - | - | none | - | - |
| S34-10 | Include: HID. | build | both/hw | 40 | not-started | - | - | - | - | none | - | - |
| S34-11 | Include: Mass storage. | build | both/hw | 40 | not-started | - | - | - | - | none | - | - |
| S34-12 | Include: Audio. | build | both/hw | 40 | not-started | - | - | - | - | none | - | - |
| S34-13 | Include: Networking. | build | both/hw | 40 | not-started | - | - | - | - | none | - | - |
| S34-14 | Include: Video devices. | build | both/hw | 40 | not-started | - | - | - | - | none | - | - |
| S34-15 | Include: Disconnect and recovery. | build | both/hw | 40 | not-started | - | - | - | - | none | - | - |
| S34-16 | Fuzz untrusted device descriptors. | build | both/hw | 40 | not-started | - | - | - | - | none | - | - |
| S35-01 | Implement: USB Type-C platform integration. | build | both/hw | 40 | not-started | - | - | - | - | none | - | - |
| S35-02 | Implement: Connection manager. | build | both/hw | 40 | not-started | - | - | - | - | none | - | - |
| S35-03 | Implement: USB4 router discovery. | build | both/hw | 40 | not-started | - | - | - | - | none | - | - |
| S35-04 | Implement: Tunnel management. | build | both/hw | 40 | not-started | - | - | - | - | none | - | - |
| S35-05 | Implement: PCIe tunneling. | build | both/hw | 40 | not-started | - | - | - | - | none | - | - |
| S35-06 | Implement: DisplayPort tunneling. | build | both/hw | 40 | not-started | - | - | - | - | none | - | - |
| S35-07 | Implement: Bandwidth allocation. | build | both/hw | 40 | not-started | - | - | - | - | none | - | - |
| S35-08 | Implement: Hot-plug. | build | both/hw | 40 | not-started | - | - | - | - | none | - | - |
| S35-09 | Implement: Docking-station integration. | build | both/hw | 40 | not-started | - | - | - | - | none | - | - |
| S35-10 | Implement: Power-state coordination. | build | both/hw | 40 | not-started | - | - | - | - | none | - | - |
| S35-11 | Implement: External DMA protection. | build | both/hw | 40 | not-started | - | - | - | - | none | - | - |
| S35-12 | Implement: Thunderbolt 4/5 interoperability. | build | both/hw | 40 | not-started | - | - | - | - | none | - | - |
| S35-13 | Record USB-IF and Thunderbolt licensing/certification requirements. | build | both/hw | 40 | not-started | - | - | - | - | none | - | - |
| S35-14 | Thunderbolt 4/5 is part of the USB4 interface, not a separate SDK. Read Intel's Thunderbolt licence before its specification. Use the Thunderbolt name only after Intel certification (ADR-0015 D5). | build | both/hw | 40 | not-started | - | - | - | - | none | - | 84 |
| S36-01 | Implement a native graphics-driver architecture for selected Intel, AMD and NVIDIA devices. | build | both/hw | 55 | not-started | - | - | - | - | none | - | - |
| S36-02 | Required capabilities include: Display discovery. | build | both/hw | 55 | not-started | - | - | - | - | none | - | - |
| S36-03 | Required capabilities include: Modesetting. | build | both/hw | 55 | not-started | - | - | - | - | none | - | - |
| S36-04 | Required capabilities include: GPU memory management. | build | both/hw | 55 | not-started | - | - | - | - | none | - | - |
| S36-05 | Required capabilities include: Command submission. | build | both/hw | 55 | not-started | - | - | - | - | none | - | - |
| S36-06 | Required capabilities include: Synchronization. | build | both/hw | 55 | not-started | - | - | - | - | none | - | - |
| S36-07 | Required capabilities include: Shader compilation. | build | both/hw | 55 | not-started | - | - | - | - | none | - | - |
| S36-08 | Required capabilities include: Hardware rendering. | build | both/hw | 55 | not-started | - | - | - | - | none | - | - |
| S36-09 | Required capabilities include: Compositing. | build | both/hw | 55 | not-started | - | - | - | - | none | - | - |
| S36-10 | Required capabilities include: Video acceleration. | build | both/hw | 55 | not-started | - | - | - | - | none | - | - |
| S36-11 | Required capabilities include: Compute. | build | both/hw | 55 | not-started | - | - | - | - | none | - | - |
| S36-12 | Required capabilities include: GPU reset and recovery. | build | both/hw | 55 | not-started | - | - | - | - | none | - | - |
| S36-13 | Begin with a validated display fallback and one supported GPU target. | build | both/hw | 55 | not-started | - | - | - | - | none | The kernel draws its console on the firmware's framebuffer (ADR-0010). There is no GPU target. | - |
| S36-14 | Support DisplayPort first. HDMI 2.1's high-rate (FRL) modes wait until their licensing allows an open implementation (ADR-0015 D7). | build | both/hw | 55 | not-started | - | - | - | - | none | - | 80 |
| S36-15 | Do not claim full acceleration without a functional native driver. | rule | both/hw | 55 | not-started | - | - | - | - | none | - | - |
| S37-01 | Implement a native Origin Compute API for: Accelerator discovery. | build | both/hw | 56 | not-started | - | - | - | - | none | - | - |
| S37-02 | Implement a native Origin Compute API for: Tensor allocation. | build | both/hw | 56 | not-started | - | - | - | - | none | - | - |
| S37-03 | Implement a native Origin Compute API for: Memory transfer. | build | both/hw | 56 | not-started | - | - | - | - | none | - | - |
| S37-04 | Implement a native Origin Compute API for: Asynchronous execution. | build | both/hw | 56 | not-started | - | - | - | - | none | - | - |
| S37-05 | Implement a native Origin Compute API for: CPU/GPU/NPU scheduling. | build | both/hw | 56 | not-started | - | - | - | - | none | - | - |
| S37-06 | Implement a native Origin Compute API for: Model-runtime integration. | build | both/hw | 56 | not-started | - | - | - | - | none | - | - |
| S37-07 | Implement a native Origin Compute API for: Capability discovery. | build | both/hw | 56 | not-started | - | - | - | - | none | - | - |
| S37-08 | Implement a native Origin Compute API for: Error recovery. | build | both/hw | 56 | not-started | - | - | - | - | none | - | - |
| S37-09 | Implement a native Origin Compute API for: CPU fallback. | build | both/hw | 56 | not-started | - | - | - | - | none | - | - |
| S37-10 | Device-specific NPU functionality depends on accessible hardware interfaces and firmware. | build | both/hw | 56 | not-started | - | - | - | - | none | - | - |
| S38-01 | Track: Required device firmware. | build | both/hw | 47 | not-started | - | - | - | - | none | - | - |
| S38-02 | Track: Firmware versions. | build | both/hw | 47 | not-started | - | - | - | - | none | - | - |
| S38-03 | Track: Firmware signatures. | build | both/hw | 47 | not-started | - | - | - | - | none | - | - |
| S38-04 | Track: Firmware loading process. | build | both/hw | 47 | not-started | - | - | - | - | none | - | - |
| S38-05 | Track: Firmware licensing. | build | both/hw | 47 | not-started | - | - | - | - | none | - | - |
| S38-06 | Track: Compatibility. | build | both/hw | 47 | not-started | - | - | - | - | none | - | - |
| S38-07 | Track: Firmware update requirements. | build | both/hw | 47 | not-started | - | - | - | - | none | - | - |
| S38-08 | Track: Recovery after failed firmware activation. | build | both/hw | 47 | not-started | - | - | - | - | none | - | - |
| S38-09 | Do not redistribute restricted vendor firmware without authorization. | rule | both/hw | 47 | not-started | - | - | - | - | none | - | - |
| S38-10 | Do not interpret unavailable firmware as something that can always be recreated in Rust. | rule | both/hw | 47 | not-started | - | - | - | - | none | - | - |
