# Pre-merge review, 2026-11-09

| | |
|---|---|
| Scope | Everything that lands on `main` with PR #1 and PR #2: about 85,000 lines of Rust at `9a10189`, plus the three branches merged during the review (early-boot DMA, M4's first slice, the Loom models) |
| Instruction | The owner: "You review and merge them, review extremely very good design, security, performance" |
| Who reviewed | AI reviewers only: eight independent read-only reviews by area, two more of the combined fixes, and one correctness pass by the session that merged them. **No human has reviewed this code** |
| What this is not | The independent security review §59 requires of the cryptography, and physical-hardware testing (§74). Both are still to come |
| Result | Both critical findings and every high finding that was a bug in running code are fixed. What was not fixed is listed below, with the reason |

## How it was done

Each reviewer read one area in full and reported defects with a concrete failure scenario: the kernel's privilege boundary, the device and DMA boundary, the boot chain and cryptography, concurrency and performance, parsers, filesystems and networking, design and directive conformance, and then the M4 and the early-boot DMA and Loom branches. Findings were checked against the code before being acted on. Fixes were made on separate branches, each with its own negative controls, and merged one at a time. Two further reviewers then read the combined fixes, looking for regressions and for fixes that did not hold, and their findings were fixed too.

## Critical and high findings

| Severity | Finding | Outcome |
|---|---|---|
| Critical | Ring 3 could replace the GS base every kernel entry trusted, then make the kernel write chosen data to a chosen address | Fixed by merging M4: `swapgs` on every path, with ring-3 proofs |
| Critical | A device's BAR was used as an MMIO address unchecked, so a device could aim the kernel's register writes at RAM, at ECAM or at the IOMMU's own registers and switch translation off | Fixed: a BAR is used only if non-zero, aligned, capped and clear of RAM, reserved ranges and other BARs. `map_mmio` refuses RAM |
| High | No `clac` on interrupt entry: ring 3 could run the kernel with SMAP off | Fixed (M4) |
| High | UMIP off: `sidt` and `sgdt` from ring 3 reveal kernel addresses and defeat KASLR | Fixed (M4) |
| High | Every boot wrote and flushed test patterns on whatever NVMe disk it found. The first physical boot would have overwritten real data | Fixed: only on the disk the boot test builds, identified positively. Elsewhere no write or flush is issued |
| High | A failed block read corrupted the cache index: wrong bytes on a later hit, written back to disk, then a panic after four failures | Fixed |
| High | An NVMe timeout left the command live: its DMA could land in a reused buffer, and every later command was off by one | Fixed: the queue is dead and the buffer quarantined. There is no controller reset yet |
| High | A hostile NIC could hang a processor with interrupts off | Fixed: a bounded reclaim, and a bad index is a device fault |
| High | A present IOMMU that failed still let drivers bind with unconfined DMA | Fixed: drivers are refused (T6). With no IOMMU in the firmware at all, internal devices still bind, per sheet item 9 |
| High | Bus mastering was on before a device's domain existed and was never cleared | Fixed by merging early-boot DMA, and failure paths now clear it |
| High | No RELRO, against §14 | Fixed: relocation data is read-only after loading |
| High | The page-table pools could not map machines with very large RAM | Fixed: both are sized from the memory map |
| High | DHCP bound any reply carrying a constant transaction ID | Fixed |
| High | Every interrupt charged a full quantum and forced a context switch | Fixed |
| High | A spawned thread could run before its state was written | Already fixed by M4 |

## Other findings fixed

- **Boot and format:** 5-level paging is refused with a message instead of a silent triple fault; nothing calls firmware after a failed `ExitBootServices`; an unsigned image is refused at the check itself; segment permissions match ONBF §6.1; MERKLE sections are verified at boot; negative relocation addends follow errata P5.
- **Gates that read as passing when they saw nothing:** the no-panic check now fails if `llvm-nm` fails, and the signing check now reads every function that handles a secret. The `--cfg loom` gate reads whole predicates.
- **Devices and DMA:** a page whose IOMMU map failed is never freed; domain IDs are consumed before use; a failing unit is shut down; bridges are found by walking up from the function; the quiesce probes all 8 functions of all 32 devices; the bus-master waiver applies only at 00:00.0; an unreadable DMAR fails closed; the admin doorbell is bounds-checked first; polls wait outside the driver lock; received frames are copied once and rings use volatile access.
- **CPU and scheduler:** #MC and #DF are no longer a thread's own fault; CR4.MCE and CR0.NE are set on every CPU; firmware's SYSENTER and CR4 leftovers are cleared; SMEP, SMAP and the v3 baseline are required on every CPU; IST stacks have guard pages and #DB has its own; a spawn that cannot finish gives everything back and never runs without an FPU area; shootdowns are bounded by time; `SpinGuard` needs `T: Sync` to be shared.
- **System calls:** queued channel bytes are charged to the sender; the transfer cycle walk is capped; a non-#PF fault no longer reports another process's CR2.
- **Networking and filesystems:** IPv4 fragments are dropped; DHCP and ARP check what they accept; a NAK restarts the exchange; receive loops run to their deadline; TFTP uses a random port; one bad XSDT entry no longer hides later tables; OriginFS and VFS path walking and directory records are checked.
- **Supply chain:** Loom left the kernel's workspace. The root `Cargo.lock` holds only workspace packages, so building the kernel needs no network, and the gate fails if a third-party package returns.

## Not fixed

| Severity | Finding | Why not now |
|---|---|---|
| High | The Framekernel's small trusted base does not exist yet: 310 `unsafe` sites in 21 files, and memory-unsafe operations exposed as safe functions (`vm::map_mmio`, `Process::map`, `memory::free_frame`). ADR-0001's security model depends on it | A refactor needing its own ADR. It is the same work as the owner's SDK-first direction |
| High | No speculative-execution controls (IBRS or AutoIBRS, IBPB; §24, ADR-0004 H5) | QEMU TCG has no speculation, so it could not be proven. Required before any real-hardware claim |
| High | CET shadow stacks are not enabled (§14) | As above: needs hardware to prove |
| High | No CI, though §82 puts it in M0 and §8 requires it | Needs the owner's word (§80: no silent external costs). STATUS now says so. **Added on the owner's word the same day:** `.github/workflows/ci.yml` |
| High | `tcp.rs` has no acceptance checks | Not connected to the NIC; only an in-memory loopback uses it. Must be rewritten against RFC 9293 before it is |
| Medium | IOMMU threats T5, T8, T9, T11, T15, T19 and T23 are open, and T14's identity maps are not built | ADR-0009 H6 requires closing or accepting them before the first physical boot |
| Medium | Scheduler: time before a block is uncharged; thread identity blocks migration; 15 threads per CPU; a 10 ms tick; no load balancing | M3 work (§26, §27) |
| Medium | Scaling: one global allocator lock with linear scans and a 4 MiB slab cap; IOVAs never reused; every spawn is a global TLB shootdown; no PCID | Limits, not bugs today (§28, §66) |
| Medium | Layering: `cpu::dispatch` calls upward on hard-wired vectors, `kernel_main` is a proof harness, addresses are raw `u64`, about 4,900 lines the kernel binary never uses, and SemVer exists three times | Belongs with the Framekernel refactor (P8, §7) |
| Medium | The no-panic check excludes the kernel library, and a panic halts only one processor | §13, §69 |
| Medium | The boot proofs' negative controls are hand edits recorded in prose. Loom's are committed | §72 |
| Medium | NVMe has no abort or controller reset after a timeout, and two threads cannot share a CPU's queue | Needed before the block layer has real users |
| Low | The KASLR slide is printed in every build and there is no `EFI_RNG` fallback; NUMA markers accept "no SRAT"; §10's `wcslen` test is outside the loader; the boot proofs cost over 5 s per boot; `thread_ended` on the #DB path runs with interrupts off; the signing branch counts are unconfirmed on an x86-64 host, and ML-DSA signing is outside that gate; RELRO is write-fault-tested once by hand, not on every boot | Recorded |

## What no reviewer opened

`framekernel/src/apic.rs`, `cache.rs`, `cpumap.rs`, `entropy.rs`, `rtc.rs` and `topology.rs`, most of `i8042.rs` and `ps2.rs`, and the bodies of most tests and vector files. The crypto was read against FIPS 204 from memory, not against the errata sheet.

## Evidence

On `c465fe6`, with Rust 1.99.1 and QEMU 11.1.3 (TCG) + OVMF:

- `cargo xtask check`: 968 passed, 0 failed, 6 ignored. Loom: 26 models pass and 35 negative controls are caught.
- `cargo xtask boot-matrix`: a full pass on its first attempt with the host at load 1.9–11.7 (mean 2.9): both IOMMUs at 1, 2 and 4 CPUs, `boot-x2apic` under leaves 1Fh and 0Bh, `boot-bridge` with both IOMMUs, and the 8 `boot-console` scenarios.

One pass on a quiet host is not a failure rate, and it says nothing about physical hardware (rule 30).
