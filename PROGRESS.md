# Origin OS progress

**3.4% of 1,686 requirements are done.** Done is the only state that counts as complete.

A requirement is one statement of [`CLAUDE.md`](CLAUDE.md), or one decision of an accepted ADR.

## Overall

| State | Requirements | Share |
|---|--:|--:|
| Done | 58 | 3.4% |
| Built | 373 | 22.1% |
| In progress | 251 | 14.9% |
| Not started | 1,004 | 59.6% |
| **Total** | **1,686** | **100.0%** |

## What the states mean

| State | Meaning |
|---|---|
| Done | Its acceptance tests pass in the environment the requirement asks for. |
| Built | It is written and tested, but not all of its acceptance tests pass yet, or they pass only in a weaker environment than it asks for, such as a virtual machine where it asks for a real one. |
| In progress | Part of it is written and tested, or something is written with no test evidence yet. |
| Not started | Nothing for it is in the tree. |

## Where the evidence ran

| Environment | Requirements |
|---|--:|
| The development host | 231 |
| An emulator (QEMU) | 126 |
| A virtual machine on a real processor | 285 |
| Physical hardware | 0 |
| No evidence yet | 1,044 |

## By milestone

Each milestone counts the requirements that [`docs/ROADMAP.md`](docs/ROADMAP.md) places in it. Its state is the roadmap's own word: done, built, partial or not started. That word is judged by the milestone's exit tests, not by the shares on its line. A milestone is done only when every one of its exit tests passes (decision items 255 and 256).

| Milestone | State in the roadmap | Requirements | Done | Built | In progress | Not started |
|---|---|--:|--:|--:|--:|--:|
| M0. Toolchain and engineering foundation | built | 107 | 38.3% | 33.7% | 11.2% | 16.8% |
| M1. Bootable Framekernel | built | 55 | 1.8% | 92.7% | 3.7% | 1.8% |
| M2. CPU and memory | built | 44 | 0.0% | 72.7% | 20.5% | 6.8% |
| M3. SMP and scheduler | partial | 107 | 0.9% | 43.0% | 10.3% | 45.8% |
| M4. Processes and IPC | partial | 140 | 0.7% | 69.3% | 16.4% | 13.6% |
| M5. Storage | partial | 115 | 0.0% | 42.6% | 17.4% | 40.0% |
| M6. Native runtime | not started | 125 | 0.0% | 11.2% | 28.0% | 60.8% |
| M7. Native packaging | not started | 77 | 2.6% | 3.9% | 14.3% | 79.2% |
| M8. Native graphics | not started | 89 | 0.0% | 0.0% | 2.2% | 97.8% |
| M9. Installable operating system | not started | 85 | 0.0% | 0.0% | 1.2% | 98.8% |
| M10. Connectivity and laptop support | partial | 131 | 0.8% | 0.0% | 8.4% | 90.8% |
| M11. Graphics acceleration and compute | not started | 28 | 0.0% | 0.0% | 0.0% | 100.0% |
| M12. System updates and recovery | not started | 75 | 0.0% | 8.0% | 1.3% | 90.7% |
| M13. Production qualification | not started | 113 | 1.8% | 4.4% | 9.7% | 84.1% |

In no milestone: 68 requirements that the roadmap lists as in none, with its reasons, and 327 standing rules, which belong to no milestone (decision item 256).

## By feature area

The requirements of `CLAUDE.md` that ask for something to be built, by the section they are in. Standing rules and ADR decisions are under "Details".

| Feature area | Sections | Requirements | Done | Built | In progress | Not started |
|---|---|--:|--:|--:|--:|--:|
| Toolchain, build and CI | §8 to §15 | 53 | 20.8% | 41.5% | 5.6% | 32.1% |
| Boot, installer and first-run setup | §19 to §23 | 57 | 0.0% | 19.3% | 0.0% | 80.7% |
| Kernel core | §24 to §30 | 92 | 0.0% | 53.2% | 18.5% | 28.3% |
| Devices | §31 to §38 | 110 | 0.0% | 15.4% | 5.5% | 79.1% |
| Storage and filesystem | §39 to §41 | 37 | 0.0% | 0.0% | 16.2% | 83.8% |
| Native applications | §42 to §47 | 79 | 0.0% | 0.0% | 22.8% | 77.2% |
| SDK and the SDK-only rule | §48 to §49 | 48 | 0.0% | 0.0% | 2.1% | 97.9% |
| GUI | §50 to §53 | 67 | 0.0% | 0.0% | 4.5% | 95.5% |
| Networking, audio and peripherals | §54 to §56 | 42 | 0.0% | 0.0% | 23.8% | 76.2% |
| Accounts, security, cryptography and privacy | §57 to §61 | 60 | 5.0% | 13.3% | 15.0% | 66.7% |
| Versions, updates and recovery | §62 to §65 | 48 | 4.2% | 12.5% | 0.0% | 83.3% |
| Performance, power, time and reliability | §66 to §69 | 45 | 0.0% | 8.9% | 6.7% | 84.4% |
| Developer platform | §70 to §71 | 27 | 0.0% | 3.7% | 7.4% | 88.9% |
| Testing, hardware qualification and release | §72 to §76 | 69 | 1.4% | 13.0% | 7.3% | 78.3% |
| **All fourteen areas** | | 834 | 2.0% | 15.2% | 10.0% | 72.8% |

Outside these areas: 154 more of `CLAUDE.md` that ask for something to be built, in the header (7), §1 (69), §6 (17), §18 (17), §79 (12), §82 (17), §84 (14), the final rules (1). They are counted in "Overall", and in a milestone where the roadmap places them.

## Details

<details>
<summary>Each feature area, section by section</summary>

**Toolchain, build and CI (§8 to §15)**

| Section | Requirements | Done | Built | In progress | Not started |
|---|--:|--:|--:|--:|--:|
| §8. Compiler version | 10 | 50.0% | 50.0% | 0.0% | 0.0% |
| §9. Compiler verification | 11 | 9.1% | 90.9% | 0.0% | 0.0% |
| §10. Rust 1.99.1 regression coverage | 5 | 40.0% | 60.0% | 0.0% | 0.0% |
| §11. Separate compilation targets | 5 | 40.0% | 0.0% | 20.0% | 40.0% |
| §12. Stable-only baseline and nightly exception policy | 0 | - | - | - | - |
| §13. Rust kernel coding policy | 8 | 12.5% | 50.0% | 0.0% | 37.5% |
| §14. Compiler and linker security | 14 | 0.0% | 0.0% | 14.3% | 85.7% |
| §15. Future Rust upgrades | 0 | - | - | - | - |

**Boot, installer and first-run setup (§19 to §23)**

| Section | Requirements | Done | Built | In progress | Not started |
|---|--:|--:|--:|--:|--:|
| §19. UEFI boot chain | 14 | 0.0% | 78.6% | 0.0% | 21.4% |
| §20. Boot manager | 10 | 0.0% | 0.0% | 0.0% | 100.0% |
| §21. Real Origin OS installer | 18 | 0.0% | 0.0% | 0.0% | 100.0% |
| §22. Native graphical installer | 3 | 0.0% | 0.0% | 0.0% | 100.0% |
| §23. First-run setup | 12 | 0.0% | 0.0% | 0.0% | 100.0% |

**Kernel core (§24 to §30)**

| Section | Requirements | Done | Built | In progress | Not started |
|---|--:|--:|--:|--:|--:|
| §24. CPU and SMP | 19 | 0.0% | 73.7% | 21.0% | 5.3% |
| §25. Kernel threading | 10 | 0.0% | 70.0% | 10.0% | 20.0% |
| §26. Scheduling | 15 | 0.0% | 33.3% | 13.3% | 53.4% |
| §27. Topology-aware scheduling | 9 | 0.0% | 0.0% | 0.0% | 100.0% |
| §28. Memory management | 18 | 0.0% | 50.0% | 22.2% | 27.8% |
| §29. Processes and isolation | 10 | 0.0% | 60.0% | 30.0% | 10.0% |
| §30. IPC and capabilities | 11 | 0.0% | 72.7% | 27.3% | 0.0% |

**Devices (§31 to §38)**

| Section | Requirements | Done | Built | In progress | Not started |
|---|--:|--:|--:|--:|--:|
| §31. Native driver framework | 21 | 0.0% | 4.8% | 14.3% | 80.9% |
| §32. PCIe and IOMMU | 13 | 0.0% | 61.5% | 7.7% | 30.8% |
| §33. NVMe | 14 | 0.0% | 57.1% | 14.3% | 28.6% |
| §34. USB 3.x | 16 | 0.0% | 0.0% | 0.0% | 100.0% |
| §35. USB4 and Thunderbolt | 14 | 0.0% | 0.0% | 0.0% | 100.0% |
| §36. GPU | 14 | 0.0% | 0.0% | 0.0% | 100.0% |
| §37. NPU and AI | 10 | 0.0% | 0.0% | 0.0% | 100.0% |
| §38. Firmware policy | 8 | 0.0% | 0.0% | 0.0% | 100.0% |

**Storage and filesystem (§39 to §41)**

| Section | Requirements | Done | Built | In progress | Not started |
|---|--:|--:|--:|--:|--:|
| §39. Storage architecture | 11 | 0.0% | 0.0% | 54.5% | 45.5% |
| §40. Native filesystem requirements | 16 | 0.0% | 0.0% | 0.0% | 100.0% |
| §41. Filesystem correctness | 10 | 0.0% | 0.0% | 0.0% | 100.0% |

**Native applications (§42 to §47)**

| Section | Requirements | Done | Built | In progress | Not started |
|---|--:|--:|--:|--:|--:|
| §42. Origin executable ABI | 14 | 0.0% | 0.0% | 57.1% | 42.9% |
| §43. `.ose` | 14 | 0.0% | 0.0% | 71.4% | 28.6% |
| §44. Origin standard library | 15 | 0.0% | 0.0% | 0.0% | 100.0% |
| §45. `.oapp` | 11 | 0.0% | 0.0% | 0.0% | 100.0% |
| §46. `.osi` | 13 | 0.0% | 0.0% | 0.0% | 100.0% |
| §47. `.od` | 12 | 0.0% | 0.0% | 0.0% | 100.0% |

**SDK and the SDK-only rule (§48 to §49)**

| Section | Requirements | Done | Built | In progress | Not started |
|---|--:|--:|--:|--:|--:|
| §48. Absolute SDK-first requirement | 11 | 0.0% | 0.0% | 0.0% | 100.0% |
| §49. Origin SDK | 37 | 0.0% | 0.0% | 2.7% | 97.3% |

**GUI (§50 to §53)**

| Section | Requirements | Done | Built | In progress | Not started |
|---|--:|--:|--:|--:|--:|
| §50. Origin UI | 17 | 0.0% | 0.0% | 0.0% | 100.0% |
| §51. Native text engine | 18 | 0.0% | 0.0% | 11.1% | 88.9% |
| §52. Origin Compositor | 20 | 0.0% | 0.0% | 0.0% | 100.0% |
| §53. Smoothness qualification | 12 | 0.0% | 0.0% | 8.3% | 91.7% |

**Networking, audio and peripherals (§54 to §56)**

| Section | Requirements | Done | Built | In progress | Not started |
|---|--:|--:|--:|--:|--:|
| §54. Native network stack | 18 | 0.0% | 0.0% | 50.0% | 50.0% |
| §55. Audio | 12 | 0.0% | 0.0% | 0.0% | 100.0% |
| §56. Peripherals | 12 | 0.0% | 0.0% | 8.3% | 91.7% |

**Accounts, security, cryptography and privacy (§57 to §61)**

| Section | Requirements | Done | Built | In progress | Not started |
|---|--:|--:|--:|--:|--:|
| §57. Authentication and identity | 12 | 0.0% | 0.0% | 0.0% | 100.0% |
| §58. Security architecture | 18 | 0.0% | 27.8% | 16.7% | 55.5% |
| §59. Cryptographic implementation | 11 | 9.1% | 18.2% | 45.4% | 27.3% |
| §60. Software supply-chain security | 11 | 18.2% | 9.1% | 9.1% | 63.6% |
| §61. Privacy | 8 | 0.0% | 0.0% | 0.0% | 100.0% |

**Versions, updates and recovery (§62 to §65)**

| Section | Requirements | Done | Built | In progress | Not started |
|---|--:|--:|--:|--:|--:|
| §62. Canonical version identity | 16 | 12.5% | 18.7% | 0.0% | 68.8% |
| §63. Separate release version from ABI compatibility | 6 | 0.0% | 50.0% | 0.0% | 50.0% |
| §64. Secure updates | 13 | 0.0% | 0.0% | 0.0% | 100.0% |
| §65. Recovery | 13 | 0.0% | 0.0% | 0.0% | 100.0% |

**Performance, power, time and reliability (§66 to §69)**

| Section | Requirements | Done | Built | In progress | Not started |
|---|--:|--:|--:|--:|--:|
| §66. Multicore performance | 10 | 0.0% | 0.0% | 0.0% | 100.0% |
| §67. Power management | 13 | 0.0% | 0.0% | 0.0% | 100.0% |
| §68. Timekeeping | 11 | 0.0% | 27.3% | 18.2% | 54.5% |
| §69. Reliability | 11 | 0.0% | 9.1% | 9.1% | 81.8% |

**Developer platform (§70 to §71)**

| Section | Requirements | Done | Built | In progress | Not started |
|---|--:|--:|--:|--:|--:|
| §70. Origin development tools | 16 | 0.0% | 6.2% | 12.5% | 81.3% |
| §71. Native application development | 11 | 0.0% | 0.0% | 0.0% | 100.0% |

**Testing, hardware qualification and release (§72 to §76)**

| Section | Requirements | Done | Built | In progress | Not started |
|---|--:|--:|--:|--:|--:|
| §72. Required testing methods | 16 | 6.3% | 25.0% | 25.0% | 43.7% |
| §73. Formal methods | 11 | 0.0% | 27.3% | 0.0% | 72.7% |
| §74. Hardware test matrix | 17 | 0.0% | 0.0% | 0.0% | 100.0% |
| §75. Reproducibility | 9 | 0.0% | 22.2% | 11.1% | 66.7% |
| §76. Release qualification | 16 | 0.0% | 0.0% | 0.0% | 100.0% |

</details>

<details>
<summary>ADR decisions</summary>

The requirements of each accepted ADR, one for each of its decisions.

| ADR | Requirements | Done | Built | In progress | Not started |
|---|--:|--:|--:|--:|--:|
| ADR-0001: Origin Native Binary Format (ONBF) | 17 | 0.0% | 5.9% | 35.3% | 58.8% |
| ADR-0002: Boot path from UEFI to the Framekernel (M1) | 15 | 0.0% | 86.7% | 0.0% | 13.3% |
| ADR-0003: Framekernel memory, interrupts, SMP start-up, first scheduler and first user mode (M2, M3, M4 groundwork) | 7 | 0.0% | 100.0% | 0.0% | 0.0% |
| ADR-0004: Virtual address-space layout, higher-half kernel and KASLR | 11 | 0.0% | 81.8% | 18.2% | 0.0% |
| ADR-0005: Process ABI, capability handles and IPC | 78 | 0.0% | 73.1% | 9.0% | 17.9% |
| ADR-0006: OriginFS v0, the native filesystem on-disk format | 6 | 0.0% | 0.0% | 83.3% | 16.7% |
| ADR-0007: DMA isolation and interrupt remapping with the IOMMU (Intel VT-d and AMD-Vi) | 28 | 0.0% | 96.4% | 0.0% | 3.6% |
| ADR-0008: Dense CPU indexes for per-CPU state; APIC IDs only as addresses | 14 | 7.1% | 92.9% | 0.0% | 0.0% |
| ADR-0009: Hardware target scope and the first test machines | 9 | 0.0% | 11.1% | 11.1% | 77.8% |
| ADR-0010: An early console that does not need a 16550 at 0x3F8 | 10 | 0.0% | 100.0% | 0.0% | 0.0% |
| ADR-0011: The native SDK is the only way to the operating system | 39 | 0.0% | 25.6% | 18.0% | 56.4% |
| ADR-0012: The monotonic clock never goes backwards, whatever the processors' TSCs say | 6 | 0.0% | 83.3% | 16.7% | 0.0% |
| ADR-0013: What a failed boot proof means on a real machine | 9 | 0.0% | 100.0% | 0.0% | 0.0% |
| ADR-0014: Origin Server is an edition of the same release, with the same native desktop | 10 | 0.0% | 0.0% | 0.0% | 100.0% |
| ADR-0015: Every device standard, completely, through the SDK | 9 | 11.1% | 0.0% | 0.0% | 88.9% |
| ADR-0016: The kernel's random-number generator | 14 | 0.0% | 92.9% | 7.1% | 0.0% |
| ADR-0017: Speculation and processor hardening | 13 | 0.0% | 61.5% | 0.0% | 38.5% |
| ADR-0018: Performance cost policy | 9 | 44.4% | 0.0% | 11.1% | 44.5% |
| ADR-0019: ACPI 6.6, everything it asks of the operating system on x86-64 | 9 | 0.0% | 44.4% | 11.1% | 44.5% |
| ADR-0020: Scheduling across processors: moving threads, balancing, placement, classes, accounting and quotas (M3) | 24 | 0.0% | 4.2% | 8.3% | 87.5% |

</details>

<details>
<summary>Standing rules</summary>

Rules on how the work is done, of `CLAUDE.md` and of the ADRs. They hold at every milestone.

| | Requirements | Done | Built | In progress | Not started |
|---|--:|--:|--:|--:|--:|
| Standing rules | 392 | 3.6% | 10.5% | 20.1% | 65.8% |

</details>

<details>
<summary>Exact counts</summary>

The milestone and feature-area tables, with the number of requirements in each state in place of its share.

| Milestone | Requirements | Done | Built | In progress | Not started |
|---|--:|--:|--:|--:|--:|
| M0. Toolchain and engineering foundation | 107 | 41 | 36 | 12 | 18 |
| M1. Bootable Framekernel | 55 | 1 | 51 | 2 | 1 |
| M2. CPU and memory | 44 | 0 | 32 | 9 | 3 |
| M3. SMP and scheduler | 107 | 1 | 46 | 11 | 49 |
| M4. Processes and IPC | 140 | 1 | 97 | 23 | 19 |
| M5. Storage | 115 | 0 | 49 | 20 | 46 |
| M6. Native runtime | 125 | 0 | 14 | 35 | 76 |
| M7. Native packaging | 77 | 2 | 3 | 11 | 61 |
| M8. Native graphics | 89 | 0 | 0 | 2 | 87 |
| M9. Installable operating system | 85 | 0 | 0 | 1 | 84 |
| M10. Connectivity and laptop support | 131 | 1 | 0 | 11 | 119 |
| M11. Graphics acceleration and compute | 28 | 0 | 0 | 0 | 28 |
| M12. System updates and recovery | 75 | 0 | 6 | 1 | 68 |
| M13. Production qualification | 113 | 2 | 5 | 11 | 95 |

| Feature area | Sections | Requirements | Done | Built | In progress | Not started |
|---|---|--:|--:|--:|--:|--:|
| Toolchain, build and CI | §8 to §15 | 53 | 11 | 22 | 3 | 17 |
| Boot, installer and first-run setup | §19 to §23 | 57 | 0 | 11 | 0 | 46 |
| Kernel core | §24 to §30 | 92 | 0 | 49 | 17 | 26 |
| Devices | §31 to §38 | 110 | 0 | 17 | 6 | 87 |
| Storage and filesystem | §39 to §41 | 37 | 0 | 0 | 6 | 31 |
| Native applications | §42 to §47 | 79 | 0 | 0 | 18 | 61 |
| SDK and the SDK-only rule | §48 to §49 | 48 | 0 | 0 | 1 | 47 |
| GUI | §50 to §53 | 67 | 0 | 0 | 3 | 64 |
| Networking, audio and peripherals | §54 to §56 | 42 | 0 | 0 | 10 | 32 |
| Accounts, security, cryptography and privacy | §57 to §61 | 60 | 3 | 8 | 9 | 40 |
| Versions, updates and recovery | §62 to §65 | 48 | 2 | 6 | 0 | 40 |
| Performance, power, time and reliability | §66 to §69 | 45 | 0 | 4 | 3 | 38 |
| Developer platform | §70 to §71 | 27 | 0 | 1 | 2 | 24 |
| Testing, hardware qualification and release | §72 to §76 | 69 | 1 | 9 | 5 | 54 |
| **All fourteen areas** | | 834 | 17 | 127 | 83 | 607 |

</details>

## How this page is made

- **From the records.** `cargo xtask progress` writes it from three of them. They are the requirements matrix ([`docs/requirements/`](docs/requirements/README.md)), the headings of `CLAUDE.md` and of the ADRs, and `docs/ROADMAP.md`. Every figure is a count of requirements or a share of one, and each requirement counts as one.
- **Not by hand.** `cargo xtask check` refuses a tree whose page differs from the one it generates, so the page on `main` is current after every merge.
- **Not counted:** 51 requirements kept for information only, and 3 removed from scope. Only done counts as complete, and neither built nor in progress counts as coverage (decision items 193 and 221).
