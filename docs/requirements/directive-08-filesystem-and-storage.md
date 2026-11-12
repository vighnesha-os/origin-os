# Requirements matrix: PART VIII, Filesystem and storage

One row for each statement of CLAUDE.md in §39 to §41. The columns and their rules are in [README.md](README.md), and `cargo xtask requirements-gate` holds this file to them and to the directive.

| ID | Requirement | Kind | Scope | Owner | State | Implementation | Acceptance | Evidence | Env | Review | Limitations | Decided by |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| S39-01 | Use separate layers for: Device drivers. | build | both/hw | 11 | partial | framekernel/src/nvme.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:NVMe | virt | agent | One storage driver, NVMe. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S39-02 | Use separate layers for: Block I/O. | build | both/hw | 11 | partial | framekernel/src/block.rs, framekernel/src/blockcache.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:NVMe | virt | agent | A block layer and a cache with one proof instance and no callers. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S39-03 | Use separate layers for: I/O scheduling. | build | both/hw | 11 | not-started | - | - | - | - | none | - | - |
| S39-04 | Use separate layers for: Partition tables. | build | both/hw | 11 | partial | framekernel/src/gpt.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:NVMe | virt | agent | Both copies of a GPT are read and checked. Nothing writes a table. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S39-05 | Use separate layers for: Volume management. | build | both/hw | 11 | not-started | - | - | - | - | none | - | - |
| S39-06 | Use separate layers for: Filesystem. | build | both/hw | 11 | partial | framekernel/src/originfs.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | file:framekernel/src/originfs.rs | host | agent | A read-only parser of the v0 bring-up format, tested on the host and not mounted. | - |
| S39-07 | Use separate layers for: Page cache. | build | both/hw | 11 | partial | framekernel/src/blockcache.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:NVMe | virt | agent | A cache of blocks with no callers. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S39-08 | Use separate layers for: Virtual filesystem. | build | both/hw | 11 | partial | framekernel/src/vfs.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:VFS, proof:/sys | virt | agent | An in-memory tree that serves `/sys`. No file system is mounted in it. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S39-09 | Use separate layers for: Encryption. | build | both/hw | 11 | not-started | - | - | - | - | none | - | - |
| S39-10 | Use separate layers for: Backup and recovery. | build | both/hw | 11 | not-started | - | - | - | - | none | - | - |
| S39-11 | Support native asynchronous I/O. | build | both/hw | 11 | not-started | - | - | - | - | none | - | - |
| S40-01 | Implement or select an independently authored, standards-informed filesystem design with: Crash consistency. | build | both/hw | 11 | not-started | - | - | - | - | none | - | - |
| S40-02 | Implement or select an independently authored, standards-informed filesystem design with: Data integrity. | build | both/hw | 11 | not-started | - | - | - | - | none | - | - |
| S40-03 | Implement or select an independently authored, standards-informed filesystem design with: Metadata integrity. | build | both/hw | 11 | not-started | - | - | - | - | none | - | - |
| S40-04 | Implement or select an independently authored, standards-informed filesystem design with: Atomic rename. | build | both/hw | 11 | not-started | - | - | - | - | none | - | - |
| S40-05 | Implement or select an independently authored, standards-informed filesystem design with: Access control. | build | both/hw | 11 | not-started | - | - | - | - | none | - | - |
| S40-06 | Implement or select an independently authored, standards-informed filesystem design with: Sparse files. | build | both/hw | 11 | not-started | - | - | - | - | none | - | - |
| S40-07 | Implement or select an independently authored, standards-informed filesystem design with: Large files. | build | both/hw | 11 | not-started | - | - | - | - | none | - | - |
| S40-08 | Implement or select an independently authored, standards-informed filesystem design with: Memory-mapped files. | build | both/hw | 11 | not-started | - | - | - | - | none | - | - |
| S40-09 | Implement or select an independently authored, standards-informed filesystem design with: Quotas. | build | both/hw | 11 | not-started | - | - | - | - | none | - | - |
| S40-10 | Implement or select an independently authored, standards-informed filesystem design with: File locking. | build | both/hw | 11 | not-started | - | - | - | - | none | - | - |
| S40-11 | Implement or select an independently authored, standards-informed filesystem design with: Snapshots where feasible. | build | both/hw | 11 | not-started | - | - | - | - | none | - | - |
| S40-12 | Implement or select an independently authored, standards-informed filesystem design with: Encryption. | build | both/hw | 11 | not-started | - | - | - | - | none | - | - |
| S40-13 | Implement or select an independently authored, standards-informed filesystem design with: Repair tools. | build | both/hw | 11 | not-started | - | - | - | - | none | - | - |
| S40-14 | Implement or select an independently authored, standards-informed filesystem design with: Versioned on-disk structures. | build | both/hw | 11 | not-started | - | - | - | - | none | - | - |
| S40-15 | Implement or select an independently authored, standards-informed filesystem design with: Safe migrations. | build | both/hw | 11 | not-started | - | - | - | - | none | - | - |
| S40-16 | Implement or select an independently authored, standards-informed filesystem design with: Power-loss recovery. | build | both/hw | 11 | not-started | - | - | - | - | none | - | - |
| S40-17 | Do not declare a filesystem production-ready without extensive corruption and recovery testing. | rule | both/hw | 11 | not-started | - | - | - | - | none | - | - |
| S41-01 | Define the semantics of: Durable writes. | build | both/hw | 11 | not-started | - | - | - | - | none | - | - |
| S41-02 | Define the semantics of: Flush operations. | build | both/hw | 11 | not-started | - | - | - | - | none | - | - |
| S41-03 | Define the semantics of: Directory updates. | build | both/hw | 11 | not-started | - | - | - | - | none | - | - |
| S41-04 | Define the semantics of: Rename atomicity. | build | both/hw | 11 | not-started | - | - | - | - | none | - | - |
| S41-05 | Define the semantics of: Metadata ordering. | build | both/hw | 11 | not-started | - | - | - | - | none | - | - |
| S41-06 | Define the semantics of: Storage barriers. | build | both/hw | 11 | not-started | - | - | - | - | none | - | - |
| S41-07 | Define the semantics of: Transaction completion. | build | both/hw | 11 | not-started | - | - | - | - | none | - | - |
| S41-08 | Define the semantics of: Interrupted operations. | build | both/hw | 11 | not-started | - | - | - | - | none | - | - |
| S41-09 | Define the semantics of: Snapshot consistency. | build | both/hw | 11 | not-started | - | - | - | - | none | - | - |
| S41-10 | The filesystem must not promise durability beyond what the controller and storage device can actually provide. | build | both/hw | 11 | not-started | - | - | - | - | none | - | - |
