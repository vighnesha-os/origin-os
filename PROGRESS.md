# Origin OS progress

**3.4% of 1,686 requirements are done.** Done is the only state that counts as complete.

A requirement is one statement of [`CLAUDE.md`](CLAUDE.md), or one decision of an accepted ADR.

## Overall

| State | Requirements | Share |
|---|--:|--:|
| Done | 58 | 3.4% |
| Built | 374 | 22.2% |
| In progress | 254 | 15.1% |
| Not started | 1,000 | 59.3% |
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
| The development host | 235 |
| An emulator (QEMU) | 126 |
| A virtual machine on a real processor | 285 |
| Physical hardware | 0 |
| No evidence yet | 1,040 |

## By milestone

Each milestone counts the requirements that [`docs/ROADMAP.md`](docs/ROADMAP.md) places in it. Its state is the roadmap's own word: done, built, partial or not started. That word is judged by the milestone's exit tests, not by the shares on its line. A milestone is done only when every one of its exit tests passes (decision items 255 and 256).

| Milestone | State in the roadmap | Requirements | Done | Built | In progress | Not started |
|---|---|--:|--:|--:|--:|--:|
| M0. Toolchain and engineering foundation | built | 107 | 38.3% | 33.7% | 11.2% | 16.8% |
| M1. Bootable Framekernel | built | 55 | 1.8% | 92.7% | 3.7% | 1.8% |
| M2. CPU and memory | built | 44 | 0.0% | 75.0% | 22.7% | 2.3% |
| M3. SMP and scheduler | partial | 107 | 0.9% | 43.0% | 10.3% | 45.8% |
| M4. Processes and IPC | partial | 140 | 0.7% | 69.3% | 17.1% | 12.9% |
| M5. Storage | partial | 115 | 0.0% | 42.6% | 17.4% | 40.0% |
| M6. Native runtime | not started | 125 | 0.0% | 11.2% | 28.8% | 60.0% |
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
| Testing, hardware qualification and release | §72 to §76 | 69 | 1.4% | 14.5% | 11.6% | 72.5% |
| **All fourteen areas** | | 834 | 2.0% | 15.4% | 10.3% | 72.3% |

Outside these areas: 154 more of `CLAUDE.md` that ask for something to be built, in the header (7), §1 (69), §6 (17), §18 (17), §79 (12), §82 (17), §84 (14), the final rules (1). They are counted in "Overall", and in a milestone where the roadmap places them.

## By specification

Each specification, standard and protocol the project relies on, with the requirements that rest on it. The list is [`docs/requirements/specifications.md`](docs/requirements/specifications.md). The figures count Origin's requirements, not how much of a specification is implemented: a specification has many parts that no requirement asks for. No figure claims conformance, which needs the specification's own tests (CLAUDE.md §46). A specification with no requirement yet shows "-".

| Specification | Requirements | Done | Built | In progress | Not started |
|---|--:|--:|--:|--:|--:|
| Unified Extensible Firmware Interface (UEFI) 2.11 | 45 | 0.0% | 53.3% | 0.0% | 46.7% |
| Advanced Configuration and Power Interface (ACPI) 6.6 | 20 | 0.0% | 30.0% | 10.0% | 60.0% |
| Rust language, compiler and Cargo 1.100.0 stable, edition 2024 | 88 | 17.0% | 40.9% | 9.1% | 33.0% |
| Intel 64 and IA-32 Architectures Software Developer's Manual current (version 093 read) | 79 | 1.3% | 77.2% | 13.9% | 7.6% |
| AMD64 Architecture Programmer's Manual current | 79 | 1.3% | 77.2% | 13.9% | 7.6% |
| PCI Express Base 6.4 and 7.0 | 11 | 0.0% | 45.4% | 9.1% | 45.5% |
| PCI Firmware 3.3 | 2 | 0.0% | 100.0% | 0.0% | 0.0% |
| NVM Express Base 2.4 | 16 | 0.0% | 50.0% | 12.5% | 37.5% |
| NVM Express NVM Command Set 1.3 | 3 | 0.0% | 66.7% | 33.3% | 0.0% |
| NVM Express over PCIe Transport 1.4 | 7 | 0.0% | 85.7% | 0.0% | 14.3% |
| Intel Virtualization Technology for Directed I/O (VT-d) 5.20 | 33 | 0.0% | 90.9% | 0.0% | 9.1% |
| AMD I/O Virtualization Technology (IOMMU), document 48882 3.11 | 33 | 0.0% | 90.9% | 0.0% | 9.1% |
| USB4 v2.0, November 2025 edition | 11 | 0.0% | 0.0% | 0.0% | 100.0% |
| USB Type-C and USB Power Delivery Power Delivery 3.3 version 1.0; Type-C: the relevant revisions | 3 | 0.0% | 0.0% | 0.0% | 100.0% |
| Thunderbolt 4 and 5 (not public; Intel licence required) | 3 | 0.0% | 0.0% | 0.0% | 100.0% |
| Universal Serial Bus 2.0 and 3.2, and the xHCI host controller interface not pinned yet | 11 | 0.0% | 0.0% | 0.0% | 100.0% |
| USB device classes: HID, Mass Storage, Audio, communications and video not pinned yet | 14 | 0.0% | 0.0% | 7.1% | 92.9% |
| Vulkan 1.4 | 0 | - | - | - | - |
| DisplayPort and HDMI HDMI 2.1; DisplayPort not pinned yet | 5 | 0.0% | 0.0% | 0.0% | 100.0% |
| Intel High Definition Audio not pinned yet | 1 | 0.0% | 0.0% | 0.0% | 100.0% |
| Unicode 18.0 | 11 | 0.0% | 0.0% | 9.1% | 90.9% |
| OpenType font format not pinned yet | 4 | 0.0% | 0.0% | 0.0% | 100.0% |
| UTF-8 (RFC 3629) RFC 3629 | 1 | 0.0% | 0.0% | 100.0% | 0.0% |
| TPM 2.0 Library and TCG PC Client Library version 185; PC Client: current | 2 | 0.0% | 0.0% | 0.0% | 100.0% |
| The Update Framework (TUF) 1.0.37 | 10 | 0.0% | 0.0% | 0.0% | 100.0% |
| System V x86-64 psABI 1.0, at commit e1ce0983 of 2025-03-12 | 6 | 0.0% | 16.6% | 66.7% | 16.7% |
| ELF generic ABI not pinned yet | 2 | 0.0% | 0.0% | 50.0% | 50.0% |
| Secure Hash Standard (FIPS 180-4) FIPS 180-4 | 17 | 0.0% | 82.4% | 17.6% | 0.0% |
| SHA-3 and SHAKE (FIPS 202) FIPS 202 | 1 | 0.0% | 0.0% | 100.0% | 0.0% |
| ML-DSA (FIPS 204) FIPS 204 with NIST's errata | 1 | 0.0% | 0.0% | 100.0% | 0.0% |
| SLH-DSA (FIPS 205) FIPS 205 | 0 | - | - | - | - |
| Stateful hash-based signatures (NIST SP 800-208) SP 800-208 | 0 | - | - | - | - |
| Ed25519 (RFC 8032) RFC 8032 | 1 | 0.0% | 0.0% | 100.0% | 0.0% |
| CBOR (RFC 8949) RFC 8949 | 1 | 0.0% | 0.0% | 100.0% | 0.0% |
| Zstandard (RFC 8878) RFC 8878 | 1 | 0.0% | 0.0% | 0.0% | 100.0% |
| IEEE 802.11 (Wi-Fi) 802.11-2024 with the 802.11be amendment | 1 | 0.0% | 0.0% | 0.0% | 100.0% |
| Bluetooth Core 6.2 or later | 1 | 0.0% | 0.0% | 0.0% | 100.0% |
| Deterministic random bit generators (NIST SP 800-90A) Rev. 1, June 2015 | 16 | 0.0% | 93.7% | 6.3% | 0.0% |
| Entropy sources (NIST SP 800-90B) January 2018 | 15 | 0.0% | 93.3% | 6.7% | 0.0% |
| Random bit generator constructions (NIST SP 800-90C) September 2025 | 15 | 0.0% | 93.3% | 6.7% | 0.0% |
| HMAC (FIPS 198-1) July 2008 | 15 | 0.0% | 93.3% | 6.7% | 0.0% |
| Ethernet (IEEE 802.3) not pinned yet | 1 | 0.0% | 0.0% | 100.0% | 0.0% |
| IPv4 (RFC 791) and ICMP (RFC 792) RFC 791, RFC 792 | 2 | 0.0% | 0.0% | 100.0% | 0.0% |
| IPv6 (RFC 8200) and ICMPv6 (RFC 4443) RFC 8200, RFC 4443 | 2 | 0.0% | 0.0% | 50.0% | 50.0% |
| ARP (RFC 826) and Neighbor Discovery (RFC 4861) RFC 826, RFC 4861 | 1 | 0.0% | 0.0% | 100.0% | 0.0% |
| TCP (RFC 9293) RFC 9293 | 1 | 0.0% | 0.0% | 100.0% | 0.0% |
| UDP (RFC 768) RFC 768 | 1 | 0.0% | 0.0% | 100.0% | 0.0% |
| DHCP (RFC 2131) and DHCPv6 (RFC 8415) RFC 2131, RFC 8415 | 1 | 0.0% | 0.0% | 100.0% | 0.0% |
| DNS (RFC 1034 and RFC 1035) RFC 1034, RFC 1035 | 1 | 0.0% | 0.0% | 100.0% | 0.0% |
| TLS 1.3 (RFC 8446) RFC 8446 | 1 | 0.0% | 0.0% | 0.0% | 100.0% |
| NTP (RFC 5905) RFC 5905 | 1 | 0.0% | 0.0% | 0.0% | 100.0% |
| IANA time zone database not pinned yet | 1 | 0.0% | 0.0% | 0.0% | 100.0% |
| Serial Port Console Redirection (SPCR) and Debug Port 2 (DBG2) tables SPCR table revision 4; DBG2 table 3 | 11 | 0.0% | 100.0% | 0.0% | 0.0% |
| 16550-compatible UART not pinned yet | 11 | 0.0% | 100.0% | 0.0% | 0.0% |
| High Precision Event Timer (IA-PC HPET) not pinned yet | 2 | 0.0% | 50.0% | 50.0% | 0.0% |
| 8254 programmable interval timer not pinned yet | 1 | 0.0% | 100.0% | 0.0% | 0.0% |
| MC146818-compatible real-time clock not pinned yet | 1 | 0.0% | 0.0% | 100.0% | 0.0% |
| 8042 keyboard and mouse controller not pinned yet | 2 | 0.0% | 0.0% | 50.0% | 50.0% |
| Virtual I/O devices (virtio) 1.1 | 1 | 0.0% | 100.0% | 0.0% | 0.0% |
| Semantic Versioning 2.0.0 | 5 | 40.0% | 40.0% | 0.0% | 20.0% |
| Origin Native Binary Format (ONBF) docs/spec/ONBF.md | 75 | 0.0% | 1.3% | 22.7% | 76.0% |
| Origin Interface Definition Language (OIDL) docs/spec/OIDL.md | 39 | 0.0% | 25.6% | 18.0% | 56.4% |

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
| §73. Formal methods | 11 | 0.0% | 36.3% | 27.3% | 36.4% |
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
<summary>SP-01. Unified Extensible Firmware Interface (UEFI) 2.11: 45 requirements</summary>

Named in CLAUDE.md §17, §19; ADR-0002. §17 baseline: S17-01. Implemented in: `origin-boot-uefi`, `origin-boot-info`.

| Requirement | State |
|---|---|
| A02-B1: Where the kernel comes from | built |
| A02-B2: Verification order (ONBF-R1, §8.1) | built |
| A02-B3: Placement | built |
| A02-B4: The boot-info structure | built |
| A02-B5: Leaving the firmware | built |
| A02-B6: Entering the kernel | built |
| A02-B7: Boot trust anchor (decided 2026-11-07; default order reversed by Amendment 1, 2026-11-10) | not started |
| A02-B8: Developer mode | built |
| A02-Am1: Origin-owned keys first | not started |
| A02-Am2: one line on the SPCR's UART before a halt after `ExitBootServices` | built |
| A02-Am3: a bench build may call `ResetSystem` after `ExitBootServices` | built |
| A02-Am4: a bench build may reset the machine with no firmware code | built |
| A02-Am5: a bench build's minimal-program mode | built |
| A02-Am6: a bench build's bisection mode | built |
| A02-Am7: the boot info and the kernel stack allocated as loader data | built |
| S19-01: Implement a real x86-64 UEFI bootloader. | built |
| S19-02: Required: Kernel image loading. | built |
| S19-03: Required: Boot-image validation. | built |
| S19-04: Required: Memory-map acquisition. | built |
| S19-05: Required: ACPI discovery. | built |
| S19-06: Required: Boot-info structure. | built |
| S19-07: Required: Stack setup. | built |
| S19-08: Required: Page-table setup where applicable. | built |
| S19-09: Required: Correct `ExitBootServices`. | built |
| S19-10: Required: CPU entry preparation. | built |
| S19-11: Required: Early diagnostics. | built |
| S19-12: Required: Secure Boot integration. | not started |
| S19-13: Required: Recovery boot support. | not started |
| S19-14: The UEFI loader, kernel image and `.ose` user executable are distinct binary concepts. | not started |
| S19-15: Secure Boot trust: by default, users and OEMs enroll Origin's keys. A first stage signed by Microsoft follows once an organisation exists that can hold that signature (ADR-0002 B7, Amendment 1). | not started |
| S20-01: Implement: Boot entries. | not started |
| S20-02: Implement: Default boot selection. | not started |
| S20-03: Implement: Boot timeout policy. | not started |
| S20-04: Implement: Recovery selection. | not started |
| S20-05: Implement: Active deployment selection. | not started |
| S20-06: Implement: Previous known-good deployment selection. | not started |
| S20-07: Implement: Boot-attempt recording. | not started |
| S20-08: Implement: Boot-failure detection. | not started |
| S20-09: Implement: Boot configuration validation. | not started |
| S20-10: Ensure that boot configuration changes are recoverable. | not started |
| S21-02: Support: UEFI boot from installation media. | not started |
| S21-05: Support: GPT partitioning. | not started |
| S21-06: Support: EFI System Partition creation. | not started |
| S21-16: Support: Bootloader installation. | not started |
| S58-01: Implement: Secure Boot integration. | not started |

</details>

<details>
<summary>SP-02. Advanced Configuration and Power Interface (ACPI) 6.6: 20 requirements</summary>

Named in CLAUDE.md §17; ADR-0019. §17 baseline: S17-02. Implemented in: `origin-acpi-tables`, `origin-aml`, `framekernel`.

| Requirement | State |
|---|---|
| A19-A1: "ACPI 6.6 in full" is everything the specification asks of the operating system on x86-64 | not started |
| A19-A2: The firmware's AML runs in a user-mode ACPI service, not in the kernel | not started |
| A19-A3: The AML interpreter is built first as a library | built |
| A19-A4: Sources, in this order | not started |
| A19-A5: Test definition blocks come from an original AML encoder | built |
| A19-A6: The order of work | in progress |
| A19-A7: What may be claimed | not started |
| A19-Am1: the answers the interpreter gives to `_OSI` and `_REV` | built |
| A19-Am2: the answer the interpreter gives to `_OS` | built |
| S19-05: Required: ACPI discovery. | built |
| S24-05: Implement: Interrupt routing. | in progress |
| S24-12: Implement: NUMA topology. | built |
| S67-01: Support: CPU idle states. | not started |
| S67-04: Support: Suspend/resume. | not started |
| S67-05: Support: Modern low-power idle where supported. | not started |
| S67-06: Support: Battery reporting. | not started |
| S67-07: Support: Embedded controller integration where documented. | not started |
| S67-08: Support: Thermal monitoring. | not started |
| S67-09: Support: Fan/power policies where exposed. | not started |
| S67-12: Support: Safe shutdown/restart. | not started |

</details>

<details>
<summary>SP-03. Rust language, compiler and Cargo 1.100.0 stable, edition 2024: 88 requirements</summary>

Named in CLAUDE.md §8, §17. §17 baseline: S17-03. Implemented in: `xtask`.

| Requirement | State |
|---|---|
| S8-01: Rust 1.100.0 is the mandatory production compiler. | done |
| S8-02: Require: Rust 1.100.0. | done |
| S8-03: Require: Rust edition 2024. | built |
| S8-04: Require: Cargo corresponding to the pinned toolchain. | done |
| S8-05: Require: Verified Rust installation. | built |
| S8-06: Require: Pinned toolchain configuration. | done |
| S8-07: Require: Consistent compiler usage across all first-party build environments. | built |
| S8-08: Use `rust-toolchain.toml`: | done |
| S8-09: Verify target installation rather than assuming it succeeded. | built |
| S8-10: Use an exact compiler version in CI. | built |
| S8-11: Do not silently upgrade Rust. | built |
| S9-01: At environment initialization: Inspect `rustc --version --verbose`. | built |
| S9-02: At environment initialization: Inspect `cargo --version`. | built |
| S9-03: At environment initialization: Inspect installed targets. | built |
| S9-04: At environment initialization: Inspect linker availability. | built |
| S9-05: At environment initialization: Verify edition support. | built |
| S9-06: At environment initialization: Verify host architecture. | built |
| S9-07: At environment initialization: Verify UEFI cross-compilation. | built |
| S9-08: At environment initialization: Verify freestanding x86-64 compilation. | built |
| S9-09: At environment initialization: Verify release-profile linking. | built |
| S9-10: At environment initialization: Run toolchain smoke tests. | built |
| S9-11: Record results in: `docs/toolchain/VERIFICATION.md` | done |
| S10-01: Because Rust 1.99.1 specifically fixes regressions relevant to Origin OS, create targeted regression tests for: UEFI linking and the `wcslen` symbol issue. | built |
| S10-02: Because Rust 1.99.1 specifically fixes regressions relevant to Origin OS, create targeted regression tests for: x86-64 C-compatible FFI boolean return behavior. | built |
| S10-03: Because Rust 1.99.1 specifically fixes regressions relevant to Origin OS, create targeted regression tests for: `rust-lld` operation on Apple Silicon/macOS. | built |
| S10-04: The `wcslen` test must link a NUL-terminated 16-bit (`CHAR16`) length loop inside the `no_std` UEFI loader. That is the pattern LLVM rewrites into a `wcslen` call. | done |
| S10-05: The FFI-bool lesson applies to every ABI and IPC boundary: a boolean crossing a boundary is a `u8` validated as 0 or 1. | in progress |
| S10-06: Record observed linker and compiler behavior. | done |
| S10-07: Do not assume release notes substitute for local verification. | built |
| S11-01: Use: **UEFI bootloader** `x86_64-unknown-uefi` | done |
| S11-02: **Freestanding Framekernel** `x86_64-unknown-none` | done |
| S11-03: **Origin userspace** Do not build userspace for `x86_64-unknown-none`: it uses the soft-float ABI and the kernel code model. | built |
| S11-04: Initially, compile userspace as `no_std` for the `x86_64-unknown-linux-gnu` triple, used purely as a code generator (hard-float System V ABI, `-C target-cpu=x86-64-v3`). Origin supplies `_start`, the `mem*` functions and the linker configuration, and `onbf-pack` converts the linker output to `.ose`. Origin code must never use `cfg(target_os = "linux")` or any Linux API; a lint enforces this. | in progress |
| S11-05: Pursue an upstream Origin target in Rust as the stable route to `std` (§44). | not started |
| S11-06: Develop a formal Origin userspace target definition as the ABI stabilizes. | not started |
| S11-07: Do not assume that a custom Origin target automatically provides `std`, threading, dynamic linking or operating-system services. | not started |
| S12-01: Stable Rust 1.100.0 is mandatory for the standard release build. | done |
| S12-02: Avoid unstable features unless technically required. | built |
| S12-04: If a feature cannot be implemented using stable 1.100.0: 1. Identify the exact limitation. | not started |
| S12-05: If a feature cannot be implemented using stable 1.100.0: 2. Verify the Rust documentation. | not started |
| S12-06: If a feature cannot be implemented using stable 1.100.0: 3. Evaluate a stable-compatible alternative. | not started |
| S12-07: If a feature cannot be implemented using stable 1.100.0: 4. Document the nightly dependency if unavoidable. | not started |
| S12-08: If a feature cannot be implemented using stable 1.100.0: 5. Pin the exact nightly commit. | not started |
| S12-09: If a feature cannot be implemented using stable 1.100.0: 6. Isolate that build path. | not started |
| S12-10: If a feature cannot be implemented using stable 1.100.0: 7. Require approval before it enters a release. | not started |
| S12-11: If a feature cannot be implemented using stable 1.100.0: 8. Preserve stable Rust 1.100.0 as the main compiler baseline. | not started |
| S12-12: Do not secretly build shipping artifacts with another compiler. | not started |
| S13-01: Use: `#![no_std]` for the kernel. | done |
| S13-02: Use: `alloc` only after a real allocator exists. | built |
| S13-03: Use: Explicit panic handling. | built |
| S13-04: Use: No assumptions about hosted operating-system APIs. | built |
| S13-05: Use: Explicit memory ordering. | built |
| S13-06: Use: Documented `unsafe` invariants. | built |
| S13-07: Use: Architecture-specific assembly only where required. | built |
| S13-08: Use: Defined stack and calling conventions. | not started |
| S13-09: Use: Checked parsing of untrusted input. | built |
| S13-10: Use: Explicit alignment and overflow checks. | not started |
| S13-11: Use: Bounded allocation policies. | not started |
| S13-12: Use Rust lints such as `unsafe_op_in_unsafe_fn` and appropriate Clippy checks. | done |
| S13-13: Avoid globally suppressing useful diagnostics. | not started |
| S13-14: Two rules for the kernel's privileged code, each checked by a gate: An `asm!` block declares `preserves_flags` only if every instruction in it leaves the status flags unchanged (decision item 110). | done |
| S13-15: Two rules for the kernel's privileged code, each checked by a gate: Only the files the gate lists may hold `unsafe`. Every other file of the kernel forbids it (decision item 111). | done |
| S14-01: Review: Target CPU feature baseline. | not started |
| S14-02: Review: Stack protection. | not started |
| S14-03: Review: Stack guards. | not started |
| S14-04: Review: Unwinding strategy. | not started |
| S14-05: Review: W^X memory layout. | not started |
| S14-06: Review: ELF segment permissions. | not started |
| S14-07: Review: Relocation handling. | not started |
| S14-08: Review: Code generation flags. | not started |
| S14-09: Review: CPU state preservation. | not started |
| S14-10: Review: C-compatible ABI behavior. | not started |
| S14-11: Review: Build reproducibility. | not started |
| S14-13: Do not globally enable aggressive CPU features that are unavailable on some supported processors. | not started |
| S14-14: The CPU baseline is `x86-64-v3`. Higher features (AVX-512/AVX10, APX) need runtime detection or a per-image declaration (ONBF §6.1.3). | in progress |
| S14-15: On stable Rust 1.100.0, `-Z cf-protection` (IBT/ENDBR64) and `-Z stack-protector` are unavailable; re-verify per §9. Enforce CET shadow stacks (they need no code generation), guard pages, W^X, full RELRO and frame pointers (`-C force-frame-pointers=yes`). Enable IBT once a stable flag exists. | in progress |
| S14-16: Build the kernel and the loader with integer overflow checks on in release builds. Build the kernel with x86-64-v3's integer instructions (POPCNT, LZCNT, BMI1, BMI2, MOVBE) once the loader refuses a processor below the baseline before it enters the kernel. The kernel stays without SIMD (decision item 123). | not started |
| S15-01: Future Rust versions must be evaluated through a dedicated toolchain-upgrade task. | built |
| S15-02: Require: Compiler release-note analysis. | built |
| S15-03: Require: ABI regression tests. | built |
| S15-04: Require: Bootloader tests. | built |
| S15-05: Require: Kernel boot tests. | built |
| S15-06: Require: Unsafe-code review where compiler behavior changes. | in progress |
| S15-07: Require: Performance regression tests. | in progress |
| S15-08: Require: Reproducible build comparison. | built |
| S15-09: Require: Origin SDK compatibility verification. | in progress |
| S15-10: Require: Release engineering approval. | in progress |
| S15-11: Do not upgrade the compiler merely because a newer version exists. | built |

</details>

<details>
<summary>SP-04. Intel 64 and IA-32 Architectures Software Developer's Manual current (version 093 read): 79 requirements</summary>

Named in CLAUDE.md §17. §17 baseline: S17-04. Implemented in: `framekernel`, `origin-frame`, `origin-paging`.

| Requirement | State |
|---|---|
| A03-K1: The library is safe code; hardware access is capability-typed in the binary | built |
| A03-K3: Physical memory: a bitmap frame allocator; strict frees | built |
| A03-K4: Kernel heap: bitmap slabs plus frame runs | built |
| A03-K5: Interrupts: x2APIC only, PIT-calibrated timer | built |
| A03-K6: SMP start-up: MADT-driven INIT-SIPI-SIPI with a W^X trampoline | built |
| A03-K7: First scheduler: per-CPU round-robin, context = interrupt frame | built |
| A03-K8: First user mode: one DPL-3 gate, RSP0 per switch, faults end the thread | built |
| A04-H1: 4-level paging; the lower half belongs to user space | built |
| A04-H2: Kernel half layout (PML4 slots 256–511) | built |
| A04-H3: KASLR | in progress |
| A04-H4: The loader builds the first page tables | built |
| A04-H5: TLB efficiency and isolation | in progress |
| A04-A1: The kernel heap is in the direct map | built |
| A04-A2: Kernel thread stacks move to `0xFFFF_C000_0000_0000` … | built |
| A04-A3: PML4 slot 480 (`0xFFFF_F000_0000_0000`) holds one page and nothing else | built |
| A04-A4: The stack region's base is random | built |
| A04-A5: The kernel chooses this slide, not the loader | built |
| A04-A6: H3's rule on printing covers it | built |
| A08-C1: A dense CPU index, given at discovery | built |
| A08-C2: Per-CPU arrays are indexed by `CpuIndex` | built |
| A08-C3: The APIC ID is only an address | built |
| A08-C4: An application processor proves who it is | built |
| A08-C5: Discovery reads everything the firmware lists | built |
| A08-C7: An AP that misses its handshake is parked before the trampoline data is rewritten | built |
| A08-D1: Per-processor state is allocated in two phases | built |
| A08-D2: Enabled processors are indexed first, online-capable ones next, and an index never changes | built |
| A08-D3: No processor count is compiled in; the bounds that remain are reported at boot | built |
| A08-D4: Per-processor state allocated at discovery stays outside the kernel-wide bound's root | built |
| A08-D5: `MAX_CPUS` is deleted, and a gate keeps it out | done |
| A08-D6: A processor gets its own NVMe queue while the controller has one left | built |
| A08-D7: The machines that test any processor count | built |
| A08-D8: Application processors still start one at a time | built |
| A12-C1: The contract | built |
| A12-C2: The floor: one shared high-water mark, on every read, always | built |
| A12-C3: A TSC that is not invariant | in progress |
| A12-C4: TSCs that disagree across processors | built |
| A12-C6: Who may read the TSC directly | built |
| A12-C8: What it costs | built |
| A17-Z1: The kernel reads what each processor reports, and says what it found | built |
| A17-Z2: Indirect branches in the kernel: the hardware isolation, set once | built |
| A17-Z3: IBPB between programs | built |
| A17-Z4: Speculative store bypass and data-independent timing: off, with the mechanism ready | not started |
| A17-Z5: SMT stays on, with branch isolation between sibling threads | built |
| A17-Z6: FRED where the processor has it; today's path where it does not | not started |
| A17-Z7: LASS where the processor has it | not started |
| A17-Z8: Shadow stacks: programs first, then the kernel under FRED, then the kernel on the IDT path | not started |
| A17-Z9: One primitive for every index and address that comes from ring 3 | built |
| A17-Z10: The compiler's options | built |
| A17-Z12: A guard for nested NMIs on the IDT path | not started |
| A17-Am1: the toolchain's prebuilt libraries are outside Z10 (decision item 152) | built |
| A17-Am2: the option behind Z10's INT3 after every RET is to become an error in the compiler, and stays while the compiler accepts it (decision items 280 and 281) | built |
| S24-01: Implement: CPU feature discovery. | built |
| S24-02: Implement: GDT, IDT and TSS. | built |
| S24-03: Implement: Exceptions. | built |
| S24-04: Implement: APIC/x2APIC. | built |
| S24-05: Implement: Interrupt routing. | in progress |
| S24-06: Implement: SMP startup. | built |
| S24-07: Implement: Secondary CPU initialization. | built |
| S24-08: Implement: Per-CPU state. | built |
| S24-09: Implement: Interprocessor interrupts. | built |
| S24-10: Implement: CPU topology. | built |
| S24-11: Implement: SMT awareness. | in progress |
| S24-12: Implement: NUMA topology. | built |
| S24-13: Implement: Hybrid-core capabilities. | in progress |
| S24-14: Implement: CPU online/offline lifecycle. | not started |
| S24-15: Implement: Speculative-execution controls and processor hardening, decided in an ADR before any program that is not a boot proof runs in ring 3 (decision item 112). | in progress |
| S24-16: Implement: FPU/SIMD state management. | built |
| S24-17: Use hardware capability discovery, not processor marketing names. | built |
| S24-18: The kernel names the core generation from CPUID's vendor, family and model, using one table built from Intel's and AMD's documents. That table also holds errata. The kernel reports at boot whether the machine is in the declared scope (ADR-0009 Amendment 1): AMD family 1Ah or higher is Zen 5 or newer. | built |
| S24-19: The kernel names the core generation from CPUID's vendor, family and model, using one table built from Intel's and AMD's documents. That table also holds errata. The kernel reports at boot whether the machine is in the declared scope (ADR-0009 Amendment 1): Intel family 18 or higher, or a listed 2026 family-6 model, is a 2026 generation. | built |
| S24-20: It never refuses to boot on that basis, and instructions are still chosen from feature bits. | built |
| S28-03: Implement: Page tables. | built |
| S28-07: Implement: Page faults. | in progress |
| S28-14: Implement: Large pages. | in progress |
| S28-17: Implement: TLB shootdowns. | built |
| S58-07: Implement: W^X. | in progress |
| S68-03: Implement: High-resolution timers. | in progress |
| S68-04: Implement: Timer calibration. | built |
| S68-05: Implement: Cross-CPU time consistency. | built |

</details>

<details>
<summary>SP-05. AMD64 Architecture Programmer's Manual current: 79 requirements</summary>

Named in CLAUDE.md §17. §17 baseline: S17-05. Implemented in: `framekernel`, `origin-frame`, `origin-paging`.

| Requirement | State |
|---|---|
| A03-K1: The library is safe code; hardware access is capability-typed in the binary | built |
| A03-K3: Physical memory: a bitmap frame allocator; strict frees | built |
| A03-K4: Kernel heap: bitmap slabs plus frame runs | built |
| A03-K5: Interrupts: x2APIC only, PIT-calibrated timer | built |
| A03-K6: SMP start-up: MADT-driven INIT-SIPI-SIPI with a W^X trampoline | built |
| A03-K7: First scheduler: per-CPU round-robin, context = interrupt frame | built |
| A03-K8: First user mode: one DPL-3 gate, RSP0 per switch, faults end the thread | built |
| A04-H1: 4-level paging; the lower half belongs to user space | built |
| A04-H2: Kernel half layout (PML4 slots 256–511) | built |
| A04-H3: KASLR | in progress |
| A04-H4: The loader builds the first page tables | built |
| A04-H5: TLB efficiency and isolation | in progress |
| A04-A1: The kernel heap is in the direct map | built |
| A04-A2: Kernel thread stacks move to `0xFFFF_C000_0000_0000` … | built |
| A04-A3: PML4 slot 480 (`0xFFFF_F000_0000_0000`) holds one page and nothing else | built |
| A04-A4: The stack region's base is random | built |
| A04-A5: The kernel chooses this slide, not the loader | built |
| A04-A6: H3's rule on printing covers it | built |
| A08-C1: A dense CPU index, given at discovery | built |
| A08-C2: Per-CPU arrays are indexed by `CpuIndex` | built |
| A08-C3: The APIC ID is only an address | built |
| A08-C4: An application processor proves who it is | built |
| A08-C5: Discovery reads everything the firmware lists | built |
| A08-C7: An AP that misses its handshake is parked before the trampoline data is rewritten | built |
| A08-D1: Per-processor state is allocated in two phases | built |
| A08-D2: Enabled processors are indexed first, online-capable ones next, and an index never changes | built |
| A08-D3: No processor count is compiled in; the bounds that remain are reported at boot | built |
| A08-D4: Per-processor state allocated at discovery stays outside the kernel-wide bound's root | built |
| A08-D5: `MAX_CPUS` is deleted, and a gate keeps it out | done |
| A08-D6: A processor gets its own NVMe queue while the controller has one left | built |
| A08-D7: The machines that test any processor count | built |
| A08-D8: Application processors still start one at a time | built |
| A12-C1: The contract | built |
| A12-C2: The floor: one shared high-water mark, on every read, always | built |
| A12-C3: A TSC that is not invariant | in progress |
| A12-C4: TSCs that disagree across processors | built |
| A12-C6: Who may read the TSC directly | built |
| A12-C8: What it costs | built |
| A17-Z1: The kernel reads what each processor reports, and says what it found | built |
| A17-Z2: Indirect branches in the kernel: the hardware isolation, set once | built |
| A17-Z3: IBPB between programs | built |
| A17-Z4: Speculative store bypass and data-independent timing: off, with the mechanism ready | not started |
| A17-Z5: SMT stays on, with branch isolation between sibling threads | built |
| A17-Z6: FRED where the processor has it; today's path where it does not | not started |
| A17-Z7: LASS where the processor has it | not started |
| A17-Z8: Shadow stacks: programs first, then the kernel under FRED, then the kernel on the IDT path | not started |
| A17-Z9: One primitive for every index and address that comes from ring 3 | built |
| A17-Z10: The compiler's options | built |
| A17-Z12: A guard for nested NMIs on the IDT path | not started |
| A17-Am1: the toolchain's prebuilt libraries are outside Z10 (decision item 152) | built |
| A17-Am2: the option behind Z10's INT3 after every RET is to become an error in the compiler, and stays while the compiler accepts it (decision items 280 and 281) | built |
| S24-01: Implement: CPU feature discovery. | built |
| S24-02: Implement: GDT, IDT and TSS. | built |
| S24-03: Implement: Exceptions. | built |
| S24-04: Implement: APIC/x2APIC. | built |
| S24-05: Implement: Interrupt routing. | in progress |
| S24-06: Implement: SMP startup. | built |
| S24-07: Implement: Secondary CPU initialization. | built |
| S24-08: Implement: Per-CPU state. | built |
| S24-09: Implement: Interprocessor interrupts. | built |
| S24-10: Implement: CPU topology. | built |
| S24-11: Implement: SMT awareness. | in progress |
| S24-12: Implement: NUMA topology. | built |
| S24-13: Implement: Hybrid-core capabilities. | in progress |
| S24-14: Implement: CPU online/offline lifecycle. | not started |
| S24-15: Implement: Speculative-execution controls and processor hardening, decided in an ADR before any program that is not a boot proof runs in ring 3 (decision item 112). | in progress |
| S24-16: Implement: FPU/SIMD state management. | built |
| S24-17: Use hardware capability discovery, not processor marketing names. | built |
| S24-18: The kernel names the core generation from CPUID's vendor, family and model, using one table built from Intel's and AMD's documents. That table also holds errata. The kernel reports at boot whether the machine is in the declared scope (ADR-0009 Amendment 1): AMD family 1Ah or higher is Zen 5 or newer. | built |
| S24-19: The kernel names the core generation from CPUID's vendor, family and model, using one table built from Intel's and AMD's documents. That table also holds errata. The kernel reports at boot whether the machine is in the declared scope (ADR-0009 Amendment 1): Intel family 18 or higher, or a listed 2026 family-6 model, is a 2026 generation. | built |
| S24-20: It never refuses to boot on that basis, and instructions are still chosen from feature bits. | built |
| S28-03: Implement: Page tables. | built |
| S28-07: Implement: Page faults. | in progress |
| S28-14: Implement: Large pages. | in progress |
| S28-17: Implement: TLB shootdowns. | built |
| S58-07: Implement: W^X. | in progress |
| S68-03: Implement: High-resolution timers. | in progress |
| S68-04: Implement: Timer calibration. | built |
| S68-05: Implement: Cross-CPU time consistency. | built |

</details>

<details>
<summary>SP-06. PCI Express Base 6.4 and 7.0: 11 requirements</summary>

Named in CLAUDE.md §17, §32. §17 baseline: S17-06. Implemented in: `framekernel`.

| Requirement | State |
|---|---|
| S32-01: Support: Host bridge discovery. | built |
| S32-02: Support: PCIe enumeration. | built |
| S32-03: Support: Configuration access. | built |
| S32-04: Support: BAR mapping. | built |
| S32-05: Support: MSI/MSI-X. | in progress |
| S32-06: Support: DMA. | built |
| S32-10: Support: PCIe error reporting. | not started |
| S32-11: Support: Device reset. | not started |
| S32-12: Support: Hot-plug. | not started |
| S32-14: Do not implement PHY signaling in software. | not started |
| S35-05: Implement: PCIe tunneling. | not started |

</details>

<details>
<summary>SP-07. PCI Firmware 3.3: 2 requirements</summary>

Named in the provenance table. §17 baseline: none. Implemented in: `framekernel`.

| Requirement | State |
|---|---|
| S32-01: Support: Host bridge discovery. | built |
| S32-03: Support: Configuration access. | built |

</details>

<details>
<summary>SP-08. NVM Express Base 2.4: 16 requirements</summary>

Named in CLAUDE.md §17, §33. §17 baseline: S17-07. Implemented in: `framekernel`.

| Requirement | State |
|---|---|
| S21-04: Support: NVMe discovery. | not started |
| S33-01: Implement: Controller discovery. | built |
| S33-02: Implement: Initialization. | built |
| S33-03: Implement: Admin and I/O queues. | built |
| S33-04: Implement: Namespace discovery. | built |
| S33-05: Implement: DMA. | built |
| S33-06: Implement: Multiqueue operation. | built |
| S33-07: Implement: Interrupt handling. | built |
| S33-08: Implement: Queue timeouts. | in progress |
| S33-09: Implement: Controller resets. | not started |
| S33-10: Implement: Flushes. | built |
| S33-11: Implement: Data-integrity handling. | in progress |
| S33-12: Implement: Power states. | not started |
| S33-13: Implement: Health reporting. | not started |
| S33-14: Implement: Recovery. | not started |
| S33-15: Treat PCIe generation and NVMe protocol revision separately. | not started |

</details>

<details>
<summary>SP-09. NVM Express NVM Command Set 1.3: 3 requirements</summary>

Named in CLAUDE.md §17. §17 baseline: S17-08. Implemented in: `framekernel`.

| Requirement | State |
|---|---|
| S33-04: Implement: Namespace discovery. | built |
| S33-10: Implement: Flushes. | built |
| S33-11: Implement: Data-integrity handling. | in progress |

</details>

<details>
<summary>SP-10. NVM Express over PCIe Transport 1.4: 7 requirements</summary>

Named in CLAUDE.md §17. §17 baseline: S17-09. Implemented in: `framekernel`.

| Requirement | State |
|---|---|
| S33-01: Implement: Controller discovery. | built |
| S33-02: Implement: Initialization. | built |
| S33-03: Implement: Admin and I/O queues. | built |
| S33-05: Implement: DMA. | built |
| S33-06: Implement: Multiqueue operation. | built |
| S33-07: Implement: Interrupt handling. | built |
| S33-09: Implement: Controller resets. | not started |

</details>

<details>
<summary>SP-11. Intel Virtualization Technology for Directed I/O (VT-d) 5.20: 33 requirements</summary>

Named in ADR-0007. §17 baseline: none. Implemented in: `framekernel`, `origin-frame`.

| Requirement | State |
|---|---|
| A07-I1: Legacy-mode translation first; scalable mode later | built |
| A07-I2: Default deny | built |
| A07-I3: One domain per requester ID | built |
| A07-I4: IOVAs are not physical addresses | built |
| A07-I5: Unmapping invalidates, drains and waits | built |
| A07-I6: Queued invalidation only | built |
| A07-I7: Coherency rules from the capability registers | built |
| A07-I8: Interrupt remapping with 32-bit x2APIC destinations and source checks | built |
| A07-I9: Faults are polled and recorded | built |
| A07-I10: virtio devices | built |
| A07-I11: What is refused rather than half-done | built |
| A07-I12: Bus mastering is cleared at enumeration and set again only after a device is attached (added 2026-11-08; threat T4) | built |
| A07-A1: Discovery from the IVRS, each IOMMU once | built |
| A07-A2: Default deny (I2), as device table entries that block | built |
| A07-A3: One domain per requester (I3), host page tables, the same IOVA window (I4) | built |
| A07-A4: Unmapping invalidates and waits (I5) | built |
| A07-A5: The command buffer only (I6) | built |
| A07-A6: Coherency (I7) | built |
| A07-A7: Interrupt remapping per device (I8) | built |
| A07-A8: Faults polled from the event log (I9) | built |
| A07-A9: Refused rather than half-done (I11) | built |
| A07-A10: What the QEMU proofs cannot show (CLAUDE.md §74) | not started |
| A07-R1: Identity maps are made when a driver attaches the device | built |
| A07-R2: The IOVA window skips identity-mapped pages | built |
| A07-R3: What a firmware-reserved range must be before it is mapped | built |
| A07-R4: What an IVMD's flags give | built |
| A07-R5: Which devices an IVMD covers | built |
| A07-R6: A never-map type-20h IVMD applies to every device in the system | built |
| S32-06: Support: DMA. | built |
| S32-07: Support: Intel VT-d. | built |
| S32-09: Support: Interrupt remapping. | built |
| S32-13: Support: External-device authorization. | not started |
| S35-11: Implement: External DMA protection. | not started |

</details>

<details>
<summary>SP-12. AMD I/O Virtualization Technology (IOMMU), document 48882 3.11: 33 requirements</summary>

Named in ADR-0007. §17 baseline: none. Implemented in: `framekernel`, `origin-frame`.

| Requirement | State |
|---|---|
| A07-I1: Legacy-mode translation first; scalable mode later | built |
| A07-I2: Default deny | built |
| A07-I3: One domain per requester ID | built |
| A07-I4: IOVAs are not physical addresses | built |
| A07-I5: Unmapping invalidates, drains and waits | built |
| A07-I6: Queued invalidation only | built |
| A07-I7: Coherency rules from the capability registers | built |
| A07-I8: Interrupt remapping with 32-bit x2APIC destinations and source checks | built |
| A07-I9: Faults are polled and recorded | built |
| A07-I10: virtio devices | built |
| A07-I11: What is refused rather than half-done | built |
| A07-I12: Bus mastering is cleared at enumeration and set again only after a device is attached (added 2026-11-08; threat T4) | built |
| A07-A1: Discovery from the IVRS, each IOMMU once | built |
| A07-A2: Default deny (I2), as device table entries that block | built |
| A07-A3: One domain per requester (I3), host page tables, the same IOVA window (I4) | built |
| A07-A4: Unmapping invalidates and waits (I5) | built |
| A07-A5: The command buffer only (I6) | built |
| A07-A6: Coherency (I7) | built |
| A07-A7: Interrupt remapping per device (I8) | built |
| A07-A8: Faults polled from the event log (I9) | built |
| A07-A9: Refused rather than half-done (I11) | built |
| A07-A10: What the QEMU proofs cannot show (CLAUDE.md §74) | not started |
| A07-R1: Identity maps are made when a driver attaches the device | built |
| A07-R2: The IOVA window skips identity-mapped pages | built |
| A07-R3: What a firmware-reserved range must be before it is mapped | built |
| A07-R4: What an IVMD's flags give | built |
| A07-R5: Which devices an IVMD covers | built |
| A07-R6: A never-map type-20h IVMD applies to every device in the system | built |
| S32-06: Support: DMA. | built |
| S32-08: Support: AMD IOMMU. | built |
| S32-09: Support: Interrupt remapping. | built |
| S32-13: Support: External-device authorization. | not started |
| S35-11: Implement: External DMA protection. | not started |

</details>

<details>
<summary>SP-13. USB4 v2.0, November 2025 edition: 11 requirements</summary>

Named in CLAUDE.md §17, §35. §17 baseline: S17-10. Implemented in: none yet.

| Requirement | State |
|---|---|
| S35-02: Implement: Connection manager. | not started |
| S35-03: Implement: USB4 router discovery. | not started |
| S35-04: Implement: Tunnel management. | not started |
| S35-05: Implement: PCIe tunneling. | not started |
| S35-06: Implement: DisplayPort tunneling. | not started |
| S35-07: Implement: Bandwidth allocation. | not started |
| S35-08: Implement: Hot-plug. | not started |
| S35-09: Implement: Docking-station integration. | not started |
| S35-10: Implement: Power-state coordination. | not started |
| S35-11: Implement: External DMA protection. | not started |
| S56-12: Support appropriate standard device interfaces for: USB docks. | not started |

</details>

<details>
<summary>SP-14. USB Type-C and USB Power Delivery Power Delivery 3.3 version 1.0; Type-C: the relevant revisions: 3 requirements</summary>

Named in CLAUDE.md §17, §35. §17 baseline: S17-11. Implemented in: none yet.

| Requirement | State |
|---|---|
| S35-01: Implement: USB Type-C platform integration. | not started |
| S35-10: Implement: Power-state coordination. | not started |
| S67-11: Support: USB power. | not started |

</details>

<details>
<summary>SP-15. Thunderbolt 4 and 5 (not public; Intel licence required): 3 requirements</summary>

Named in CLAUDE.md §17, §35. §17 baseline: S17-12. Implemented in: none yet.

| Requirement | State |
|---|---|
| S35-12: Implement: Thunderbolt 4/5 interoperability. | not started |
| S35-13: Record USB-IF and Thunderbolt licensing/certification requirements. | not started |
| S35-14: Thunderbolt 4/5 is part of the USB4 interface, not a separate SDK. Read Intel's Thunderbolt licence before its specification. Use the Thunderbolt name only after Intel certification (ADR-0015 D5). | not started |

</details>

<details>
<summary>SP-16. Universal Serial Bus 2.0 and 3.2, and the xHCI host controller interface not pinned yet: 11 requirements</summary>

Named in CLAUDE.md §34. §17 baseline: none. Implemented in: none yet.

| Requirement | State |
|---|---|
| S34-01: Implement xHCI and required USB protocols. | not started |
| S34-02: Include: USB 2 compatibility. | not started |
| S34-03: Include: USB 3.0/3.1/3.2. | not started |
| S34-04: Include: Hub management. | not started |
| S34-05: Include: Device enumeration. | not started |
| S34-06: Include: Control transfers. | not started |
| S34-07: Include: Bulk transfers. | not started |
| S34-08: Include: Interrupt transfers. | not started |
| S34-09: Include: Isochronous transfers. | not started |
| S34-15: Include: Disconnect and recovery. | not started |
| S34-16: Fuzz untrusted device descriptors. | not started |

</details>

<details>
<summary>SP-17. USB device classes: HID, Mass Storage, Audio, communications and video not pinned yet: 14 requirements</summary>

Named in CLAUDE.md §34, §55, §56. §17 baseline: none. Implemented in: none yet.

| Requirement | State |
|---|---|
| S34-10: Include: HID. | not started |
| S34-11: Include: Mass storage. | not started |
| S34-12: Include: Audio. | not started |
| S34-13: Include: Networking. | not started |
| S34-14: Include: Video devices. | not started |
| S55-07: Build: USB Audio. | not started |
| S56-01: Support appropriate standard device interfaces for: Keyboards. | in progress |
| S56-02: Support appropriate standard device interfaces for: Mice. | not started |
| S56-03: Support appropriate standard device interfaces for: Touchpads. | not started |
| S56-04: Support appropriate standard device interfaces for: Touchscreens. | not started |
| S56-05: Support appropriate standard device interfaces for: Pen. | not started |
| S56-06: Support appropriate standard device interfaces for: Game controllers. | not started |
| S56-07: Support appropriate standard device interfaces for: Cameras. | not started |
| S56-08: Support appropriate standard device interfaces for: Webcams. | not started |

</details>

<details>
<summary>SP-18. Vulkan 1.4: 0 requirements</summary>

Named in CLAUDE.md §17. §17 baseline: S17-13. Implemented in: none yet.

No requirement rests on it yet.

</details>

<details>
<summary>SP-19. DisplayPort and HDMI HDMI 2.1; DisplayPort not pinned yet: 5 requirements</summary>

Named in CLAUDE.md §36; ADR-0015 D7. §17 baseline: none. Implemented in: none yet.

| Requirement | State |
|---|---|
| S35-06: Implement: DisplayPort tunneling. | not started |
| S36-02: Required capabilities include: Display discovery. | not started |
| S36-03: Required capabilities include: Modesetting. | not started |
| S36-14: Support DisplayPort first. HDMI 2.1's high-rate (FRL) modes wait until their licensing allows an open implementation (ADR-0015 D7). | not started |
| S56-11: Support appropriate standard device interfaces for: Displays. | not started |

</details>

<details>
<summary>SP-20. Intel High Definition Audio not pinned yet: 1 requirement</summary>

Named in CLAUDE.md §55. §17 baseline: none. Implemented in: none yet.

| Requirement | State |
|---|---|
| S55-08: Build: HD Audio. | not started |

</details>

<details>
<summary>SP-21. Unicode 18.0: 11 requirements</summary>

Named in CLAUDE.md §17, §51. §17 baseline: S17-14. Implemented in: none yet.

| Requirement | State |
|---|---|
| S51-01: Implement Unicode-aware text behavior, including: UTF-8. | in progress |
| S51-02: Implement Unicode-aware text behavior, including: Unicode 18.0 data compatibility. | not started |
| S51-03: Implement Unicode-aware text behavior, including: Normalization. | not started |
| S51-04: Implement Unicode-aware text behavior, including: Grapheme segmentation. | not started |
| S51-05: Implement Unicode-aware text behavior, including: Word segmentation. | not started |
| S51-06: Implement Unicode-aware text behavior, including: Line breaking. | not started |
| S51-07: Implement Unicode-aware text behavior, including: Bidirectional text. | not started |
| S51-08: Implement Unicode-aware text behavior, including: Script detection. | not started |
| S51-13: Implement Unicode-aware text behavior, including: Emoji. | not started |
| S51-17: Test Telugu, Hindi, Arabic, Latin and mixed-direction text. | not started |
| S51-18: Fonts and Unicode data must have appropriate licensing and provenance. | not started |

</details>

<details>
<summary>SP-22. OpenType font format not pinned yet: 4 requirements</summary>

Named in CLAUDE.md §51. §17 baseline: none. Implemented in: none yet.

| Requirement | State |
|---|---|
| S51-09: Implement Unicode-aware text behavior, including: Font fallback. | not started |
| S51-10: Implement Unicode-aware text behavior, including: OpenType shaping. | not started |
| S51-11: Implement Unicode-aware text behavior, including: Ligatures. | not started |
| S51-12: Implement Unicode-aware text behavior, including: Complex-script positioning. | not started |

</details>

<details>
<summary>SP-23. UTF-8 (RFC 3629) RFC 3629: 1 requirement</summary>

Named in the provenance table. §17 baseline: none. Implemented in: `onbf-format`.

| Requirement | State |
|---|---|
| S51-01: Implement Unicode-aware text behavior, including: UTF-8. | in progress |

</details>

<details>
<summary>SP-24. TPM 2.0 Library and TCG PC Client Library version 185; PC Client: current: 2 requirements</summary>

Named in CLAUDE.md §17, §58. §17 baseline: S17-15. Implemented in: none yet.

| Requirement | State |
|---|---|
| S58-02: Implement: TPM integration. | not started |
| S58-03: Implement: Measured boot where supported. | not started |

</details>

<details>
<summary>SP-25. The Update Framework (TUF) 1.0.37: 10 requirements</summary>

Named in CLAUDE.md §17, §64. §17 baseline: S17-16. Implemented in: none yet.

| Requirement | State |
|---|---|
| S64-01: Implement: Signed manifests. | not started |
| S64-02: Implement: Artifact hashes. | not started |
| S64-03: Implement: Trusted roots. | not started |
| S64-04: Implement: Key rotation. | not started |
| S64-05: Implement: Expiration checking. | not started |
| S64-06: Implement: Rollback protection. | not started |
| S64-07: Implement: Freeze-attack protection. | not started |
| S64-08: Implement: Mixed-metadata protection. | not started |
| S64-14: Use TUF's threat model as a design reference. | not started |
| S64-15: Do not claim full TUF conformance without implementing its relevant requirements. | not started |

</details>

<details>
<summary>SP-26. System V x86-64 psABI 1.0, at commit e1ce0983 of 2025-03-12: 6 requirements</summary>

Named in CLAUDE.md §17, §42. §17 baseline: S17-17. Implemented in: `origin-rt`, `origin-abi`, `origin-syscalls`.

| Requirement | State |
|---|---|
| S10-02: Because Rust 1.99.1 specifically fixes regressions relevant to Origin OS, create targeted regression tests for: x86-64 C-compatible FFI boolean return behavior. | built |
| S10-05: The FFI-bool lesson applies to every ABI and IPC boundary: a boolean crossing a boundary is a `u8` validated as 0 or 1. | in progress |
| S42-02: Specify: Calling convention. | in progress |
| S42-03: Specify: Stack alignment. | not started |
| S42-04: Specify: Register preservation. | in progress |
| S42-16: Origin userspace is 64-bit only (LP64 x86-64). There are no 32-bit or x32 processes, no 32-bit compatibility mode and no 32-bit system-call entry. | in progress |

</details>

<details>
<summary>SP-27. ELF generic ABI not pinned yet: 2 requirements</summary>

Named in CLAUDE.md §43; the provenance table. §17 baseline: none. Implemented in: `onbf-pack`.

| Requirement | State |
|---|---|
| S14-06: Review: ELF segment permissions. | not started |
| S43-05: Create a native executable container with: A native ONBF segment table. ELF64 PIE exists only as linker output and is converted by `onbf-pack`; the loader never parses ELF (ADR-0001). | in progress |

</details>

<details>
<summary>SP-28. Secure Hash Standard (FIPS 180-4) FIPS 180-4: 17 requirements</summary>

Named in CLAUDE.md §17. §17 baseline: S17-18. Implemented in: `origin-crypto`, `origin-random`, `onbf-verify`.

| Requirement | State |
|---|---|
| A16-R1: One generator, one algorithm | built |
| A16-R2: Where the code lives | built |
| A16-R3: The sources, and what each is trusted for | built |
| A16-R4: Health tests on the raw draws | built |
| A16-R5: Seeding | built |
| A16-R6: Reseeding | built |
| A16-R7: State for each processor | built |
| A16-R8: With no healthy seed | built |
| A16-R9: Overwriting secrets (§59) | built |
| A16-R10: The loader: its own generator, and a seed for the kernel | built |
| A16-R11: What the rest of the kernel sees | built |
| A16-R12: Cost | in progress |
| A16-R13: What a boot says | built |
| A16-Am1: the loader also hands over the value the kernel stacks' region is placed from | built |
| S43-13: Create a native executable container with: Integrity checks. | in progress |
| S59-14: The signature suite is Ed25519 + ML-DSA-65. Root, kernel and release keys use Ed25519 + ML-DSA-87. Both signatures must verify (ADR-0001). | in progress |
| S59-15: The kernel has one random-number generator: an established deterministic generator (NIST SP 800-90A's HMAC_DRBG over SHA-512), seeded from more than one source and reseeded, with health tests on what each source returns. Those tests catch gross failure only, and Origin claims no SP 800-90B conformance (ADR-0016, decision item 149). Nothing else draws from the processor's instructions directly (decision item 113). | built |

</details>

<details>
<summary>SP-29. SHA-3 and SHAKE (FIPS 202) FIPS 202: 1 requirement</summary>

Named in CLAUDE.md §17. §17 baseline: S17-18. Implemented in: `origin-crypto`.

| Requirement | State |
|---|---|
| S59-14: The signature suite is Ed25519 + ML-DSA-65. Root, kernel and release keys use Ed25519 + ML-DSA-87. Both signatures must verify (ADR-0001). | in progress |

</details>

<details>
<summary>SP-30. ML-DSA (FIPS 204) FIPS 204 with NIST's errata: 1 requirement</summary>

Named in CLAUDE.md §17, §59; ADR-0001. §17 baseline: S17-18. Implemented in: `origin-crypto`, `onbf-verify`.

| Requirement | State |
|---|---|
| S59-14: The signature suite is Ed25519 + ML-DSA-65. Root, kernel and release keys use Ed25519 + ML-DSA-87. Both signatures must verify (ADR-0001). | in progress |

</details>

<details>
<summary>SP-31. SLH-DSA (FIPS 205) FIPS 205: 0 requirements</summary>

Named in CLAUDE.md §17. §17 baseline: S17-18. Implemented in: none yet.

No requirement rests on it yet.

</details>

<details>
<summary>SP-32. Stateful hash-based signatures (NIST SP 800-208) SP 800-208: 0 requirements</summary>

Named in CLAUDE.md §17. §17 baseline: S17-18. Implemented in: none yet.

No requirement rests on it yet.

</details>

<details>
<summary>SP-33. Ed25519 (RFC 8032) RFC 8032: 1 requirement</summary>

Named in CLAUDE.md §17, §59; ADR-0001. §17 baseline: S17-19. Implemented in: `origin-crypto`, `onbf-verify`.

| Requirement | State |
|---|---|
| S59-14: The signature suite is Ed25519 + ML-DSA-65. Root, kernel and release keys use Ed25519 + ML-DSA-87. Both signatures must verify (ADR-0001). | in progress |

</details>

<details>
<summary>SP-34. CBOR (RFC 8949) RFC 8949: 1 requirement</summary>

Named in CLAUDE.md §17; ADR-0001. §17 baseline: S17-19. Implemented in: `onbf-format`.

| Requirement | State |
|---|---|
| A01-def3: OCBOR-1 manifests compiled from TOML | in progress |

</details>

<details>
<summary>SP-35. Zstandard (RFC 8878) RFC 8878: 1 requirement</summary>

Named in CLAUDE.md §17; ADR-0001. §17 baseline: S17-19. Implemented in: none yet.

| Requirement | State |
|---|---|
| A01-def6: zstd for transport, with uncompressed installs | not started |

</details>

<details>
<summary>SP-36. IEEE 802.11 (Wi-Fi) 802.11-2024 with the 802.11be amendment: 1 requirement</summary>

Named in CLAUDE.md §17, §54. §17 baseline: S17-20. Implemented in: none yet.

| Requirement | State |
|---|---|
| S54-14: Implement: Wi-Fi. | not started |

</details>

<details>
<summary>SP-37. Bluetooth Core 6.2 or later: 1 requirement</summary>

Named in CLAUDE.md §17, §54. §17 baseline: S17-21. Implemented in: none yet.

| Requirement | State |
|---|---|
| S54-15: Implement: Bluetooth. | not started |

</details>

<details>
<summary>SP-38. Deterministic random bit generators (NIST SP 800-90A) Rev. 1, June 2015: 16 requirements</summary>

Named in CLAUDE.md §17, §59; ADR-0016. §17 baseline: S17-22. Implemented in: `origin-random`, `origin-frame`.

| Requirement | State |
|---|---|
| A16-R1: One generator, one algorithm | built |
| A16-R2: Where the code lives | built |
| A16-R3: The sources, and what each is trusted for | built |
| A16-R4: Health tests on the raw draws | built |
| A16-R5: Seeding | built |
| A16-R6: Reseeding | built |
| A16-R7: State for each processor | built |
| A16-R8: With no healthy seed | built |
| A16-R9: Overwriting secrets (§59) | built |
| A16-R10: The loader: its own generator, and a seed for the kernel | built |
| A16-R11: What the rest of the kernel sees | built |
| A16-R12: Cost | in progress |
| A16-R13: What a boot says | built |
| A16-Am1: the loader also hands over the value the kernel stacks' region is placed from | built |
| S59-07: Require: Secure randomness. | built |
| S59-15: The kernel has one random-number generator: an established deterministic generator (NIST SP 800-90A's HMAC_DRBG over SHA-512), seeded from more than one source and reseeded, with health tests on what each source returns. Those tests catch gross failure only, and Origin claims no SP 800-90B conformance (ADR-0016, decision item 149). Nothing else draws from the processor's instructions directly (decision item 113). | built |

</details>

<details>
<summary>SP-39. Entropy sources (NIST SP 800-90B) January 2018: 15 requirements</summary>

Named in CLAUDE.md §17; ADR-0016. §17 baseline: S17-22. Implemented in: `origin-random`.

| Requirement | State |
|---|---|
| A16-R1: One generator, one algorithm | built |
| A16-R2: Where the code lives | built |
| A16-R3: The sources, and what each is trusted for | built |
| A16-R4: Health tests on the raw draws | built |
| A16-R5: Seeding | built |
| A16-R6: Reseeding | built |
| A16-R7: State for each processor | built |
| A16-R8: With no healthy seed | built |
| A16-R9: Overwriting secrets (§59) | built |
| A16-R10: The loader: its own generator, and a seed for the kernel | built |
| A16-R11: What the rest of the kernel sees | built |
| A16-R12: Cost | in progress |
| A16-R13: What a boot says | built |
| A16-Am1: the loader also hands over the value the kernel stacks' region is placed from | built |
| S59-15: The kernel has one random-number generator: an established deterministic generator (NIST SP 800-90A's HMAC_DRBG over SHA-512), seeded from more than one source and reseeded, with health tests on what each source returns. Those tests catch gross failure only, and Origin claims no SP 800-90B conformance (ADR-0016, decision item 149). Nothing else draws from the processor's instructions directly (decision item 113). | built |

</details>

<details>
<summary>SP-40. Random bit generator constructions (NIST SP 800-90C) September 2025: 15 requirements</summary>

Named in CLAUDE.md §17; ADR-0016. §17 baseline: S17-22. Implemented in: `origin-random`.

| Requirement | State |
|---|---|
| A16-R1: One generator, one algorithm | built |
| A16-R2: Where the code lives | built |
| A16-R3: The sources, and what each is trusted for | built |
| A16-R4: Health tests on the raw draws | built |
| A16-R5: Seeding | built |
| A16-R6: Reseeding | built |
| A16-R7: State for each processor | built |
| A16-R8: With no healthy seed | built |
| A16-R9: Overwriting secrets (§59) | built |
| A16-R10: The loader: its own generator, and a seed for the kernel | built |
| A16-R11: What the rest of the kernel sees | built |
| A16-R12: Cost | in progress |
| A16-R13: What a boot says | built |
| A16-Am1: the loader also hands over the value the kernel stacks' region is placed from | built |
| S59-15: The kernel has one random-number generator: an established deterministic generator (NIST SP 800-90A's HMAC_DRBG over SHA-512), seeded from more than one source and reseeded, with health tests on what each source returns. Those tests catch gross failure only, and Origin claims no SP 800-90B conformance (ADR-0016, decision item 149). Nothing else draws from the processor's instructions directly (decision item 113). | built |

</details>

<details>
<summary>SP-41. HMAC (FIPS 198-1) July 2008: 15 requirements</summary>

Named in CLAUDE.md §17; ADR-0016. §17 baseline: S17-22. Implemented in: `origin-crypto`, `origin-random`.

| Requirement | State |
|---|---|
| A16-R1: One generator, one algorithm | built |
| A16-R2: Where the code lives | built |
| A16-R3: The sources, and what each is trusted for | built |
| A16-R4: Health tests on the raw draws | built |
| A16-R5: Seeding | built |
| A16-R6: Reseeding | built |
| A16-R7: State for each processor | built |
| A16-R8: With no healthy seed | built |
| A16-R9: Overwriting secrets (§59) | built |
| A16-R10: The loader: its own generator, and a seed for the kernel | built |
| A16-R11: What the rest of the kernel sees | built |
| A16-R12: Cost | in progress |
| A16-R13: What a boot says | built |
| A16-Am1: the loader also hands over the value the kernel stacks' region is placed from | built |
| S59-15: The kernel has one random-number generator: an established deterministic generator (NIST SP 800-90A's HMAC_DRBG over SHA-512), seeded from more than one source and reseeded, with health tests on what each source returns. Those tests catch gross failure only, and Origin claims no SP 800-90B conformance (ADR-0016, decision item 149). Nothing else draws from the processor's instructions directly (decision item 113). | built |

</details>

<details>
<summary>SP-42. Ethernet (IEEE 802.3) not pinned yet: 1 requirement</summary>

Named in CLAUDE.md §54. §17 baseline: none. Implemented in: none yet.

| Requirement | State |
|---|---|
| S54-01: Implement: Ethernet. | in progress |

</details>

<details>
<summary>SP-43. IPv4 (RFC 791) and ICMP (RFC 792) RFC 791, RFC 792: 2 requirements</summary>

Named in CLAUDE.md §54. §17 baseline: none. Implemented in: none yet.

| Requirement | State |
|---|---|
| S54-02: Implement: IPv4. | in progress |
| S54-07: Implement: ICMP. | in progress |

</details>

<details>
<summary>SP-44. IPv6 (RFC 8200) and ICMPv6 (RFC 4443) RFC 8200, RFC 4443: 2 requirements</summary>

Named in CLAUDE.md §54. §17 baseline: none. Implemented in: none yet.

| Requirement | State |
|---|---|
| S54-03: Implement: IPv6. | not started |
| S54-07: Implement: ICMP. | in progress |

</details>

<details>
<summary>SP-45. ARP (RFC 826) and Neighbor Discovery (RFC 4861) RFC 826, RFC 4861: 1 requirement</summary>

Named in CLAUDE.md §54. §17 baseline: none. Implemented in: none yet.

| Requirement | State |
|---|---|
| S54-04: Implement: ARP/NDP. | in progress |

</details>

<details>
<summary>SP-46. TCP (RFC 9293) RFC 9293: 1 requirement</summary>

Named in CLAUDE.md §54. §17 baseline: none. Implemented in: none yet.

| Requirement | State |
|---|---|
| S54-05: Implement: TCP. | in progress |

</details>

<details>
<summary>SP-47. UDP (RFC 768) RFC 768: 1 requirement</summary>

Named in CLAUDE.md §54. §17 baseline: none. Implemented in: none yet.

| Requirement | State |
|---|---|
| S54-06: Implement: UDP. | in progress |

</details>

<details>
<summary>SP-48. DHCP (RFC 2131) and DHCPv6 (RFC 8415) RFC 2131, RFC 8415: 1 requirement</summary>

Named in CLAUDE.md §54. §17 baseline: none. Implemented in: none yet.

| Requirement | State |
|---|---|
| S54-08: Implement: DHCP. | in progress |

</details>

<details>
<summary>SP-49. DNS (RFC 1034 and RFC 1035) RFC 1034, RFC 1035: 1 requirement</summary>

Named in CLAUDE.md §54. §17 baseline: none. Implemented in: none yet.

| Requirement | State |
|---|---|
| S54-09: Implement: DNS. | in progress |

</details>

<details>
<summary>SP-50. TLS 1.3 (RFC 8446) RFC 8446: 1 requirement</summary>

Named in CLAUDE.md §54. §17 baseline: none. Implemented in: none yet.

| Requirement | State |
|---|---|
| S54-13: Implement: TLS integration. | not started |

</details>

<details>
<summary>SP-51. NTP (RFC 5905) RFC 5905: 1 requirement</summary>

Named in CLAUDE.md §68. §17 baseline: none. Implemented in: none yet.

| Requirement | State |
|---|---|
| S68-07: Implement: Clock synchronization. | not started |

</details>

<details>
<summary>SP-52. IANA time zone database not pinned yet: 1 requirement</summary>

Named in CLAUDE.md §68. §17 baseline: none. Implemented in: none yet.

| Requirement | State |
|---|---|
| S68-06: Implement: Time zones. | not started |

</details>

<details>
<summary>SP-53. Serial Port Console Redirection (SPCR) and Debug Port 2 (DBG2) tables SPCR table revision 4; DBG2 table 3: 11 requirements</summary>

Named in ADR-0010. §17 baseline: none. Implemented in: `origin-spcr`, `framekernel`.

| Requirement | State |
|---|---|
| A10-E1: Boot info 0.4: a `Framebuffer` (the ABI decision; accepted by the owner on 2026-11-08) | built |
| A10-E2: Where the framebuffer is mapped | built |
| A10-E3: The console: framebuffer first, then the UART | built |
| A10-E4: The UART: from the SPCR when the firmware names one | built |
| A10-E5: A missing or unusable console never stops the boot | built |
| A10-E6: The font | built |
| A10-Am1: the UART first, then the screen a section at a time | built |
| A10-Am2: the UART a section at a time | built |
| A10-Am3: the loader names the SPCR's UART in boot info 0.8 | built |
| A10-Am4: the loader's one direct write to the SPCR's UART | built |
| S19-11: Required: Early diagnostics. | built |

</details>

<details>
<summary>SP-54. 16550-compatible UART not pinned yet: 11 requirements</summary>

Named in ADR-0010. §17 baseline: none. Implemented in: `framekernel`.

| Requirement | State |
|---|---|
| A10-E1: Boot info 0.4: a `Framebuffer` (the ABI decision; accepted by the owner on 2026-11-08) | built |
| A10-E2: Where the framebuffer is mapped | built |
| A10-E3: The console: framebuffer first, then the UART | built |
| A10-E4: The UART: from the SPCR when the firmware names one | built |
| A10-E5: A missing or unusable console never stops the boot | built |
| A10-E6: The font | built |
| A10-Am1: the UART first, then the screen a section at a time | built |
| A10-Am2: the UART a section at a time | built |
| A10-Am3: the loader names the SPCR's UART in boot info 0.8 | built |
| A10-Am4: the loader's one direct write to the SPCR's UART | built |
| S19-11: Required: Early diagnostics. | built |

</details>

<details>
<summary>SP-55. High Precision Event Timer (IA-PC HPET) not pinned yet: 2 requirements</summary>

Named in ADR-0012. §17 baseline: none. Implemented in: `framekernel`.

| Requirement | State |
|---|---|
| S68-03: Implement: High-resolution timers. | in progress |
| S68-04: Implement: Timer calibration. | built |

</details>

<details>
<summary>SP-56. 8254 programmable interval timer not pinned yet: 1 requirement</summary>

Named in the boot proofs (ADR-0013). §17 baseline: none. Implemented in: `framekernel`.

| Requirement | State |
|---|---|
| S68-04: Implement: Timer calibration. | built |

</details>

<details>
<summary>SP-57. MC146818-compatible real-time clock not pinned yet: 1 requirement</summary>

Named in the boot proofs (ADR-0013). §17 baseline: none. Implemented in: `framekernel`.

| Requirement | State |
|---|---|
| S68-02: Implement: Wall clock. | in progress |

</details>

<details>
<summary>SP-58. 8042 keyboard and mouse controller not pinned yet: 2 requirements</summary>

Named in the boot proofs (ADR-0013). §17 baseline: none. Implemented in: `framekernel`.

| Requirement | State |
|---|---|
| S56-01: Support appropriate standard device interfaces for: Keyboards. | in progress |
| S56-02: Support appropriate standard device interfaces for: Mice. | not started |

</details>

<details>
<summary>SP-59. Virtual I/O devices (virtio) 1.1: 1 requirement</summary>

Named in ADR-0007. §17 baseline: none. Implemented in: `framekernel`.

| Requirement | State |
|---|---|
| A07-I10: virtio devices | built |

</details>

<details>
<summary>SP-60. Semantic Versioning 2.0.0: 5 requirements</summary>

Named in the provenance table. §17 baseline: none. Implemented in: `origin-version`.

| Requirement | State |
|---|---|
| S62-01: Create: `origin-version.toml` | done |
| S62-16: Use generated version metadata. | done |
| S63-01: Maintain: Product release identity. | built |
| S63-02: Maintain: ABI compatibility identifiers. | built |
| S63-03: Maintain: Package format versions. | not started |

</details>

<details>
<summary>SP-61. Origin Native Binary Format (ONBF) docs/spec/ONBF.md: 75 requirements</summary>

Named in CLAUDE.md, header; ADR-0001. §17 baseline: none. Implemented in: `onbf-format`, `onbf-merkle`, `onbf-verify`, `onbf-pack`, `onbf-sign`, `origin-boot-uefi`.

| Requirement | State |
|---|---|
| A01-D1: one container, native segments | in progress |
| A01-D2: third-party drivers are user-mode only | not started |
| A01-D3: hybrid signatures | in progress |
| A01-D4: open publishing, in tiers | not started |
| A01-D5: 64-bit only, no Windows formats | not started |
| A01-def1: the umbrella format with the `.osl` extension | not started |
| A01-def2: KRN and SYS profiles | in progress |
| A01-def3: OCBOR-1 manifests compiled from TOML | in progress |
| A01-def4: no X.509 in the package path | not started |
| A01-def5: Merkle-SHA256-4K hashing | built |
| A01-def6: zstd for transport, with uncompressed installs | not started |
| A01-def7: an `x86-64-v3` baseline, shadow stack on by default, IBT later | in progress |
| A01-def8: frame pointers, with `panic=abort` until the Origin `std` exists | in progress |
| A01-def9: eager binding and full RELRO, with no static constructors | not started |
| A01-def10: private system calls, with `origin.core` as the only stable ABI | not started |
| A01-def11: no downgrade except to the version kept locally | not started |
| A01-def12: "Try" without installing | not started |
| H-14: `.ose`: Origin System Executable | not started |
| H-15: `.osi`: Origin Software Installer | not started |
| H-16: `.oapp`: Origin Application Bundle | not started |
| H-17: `.od`: Origin Driver Package | not started |
| H-18: `.osl`: Origin Shared Library | not started |
| H-19: All native formats are profiles of one container, the Origin Native Binary Format (ONBF). So are the Framekernel image, the signed release manifest and detached debug symbols (`.osym`). Specification: `docs/spec/ONBF.md`. Decisions: `docs/adr/ADR-0001-onbf.md`. | in progress |
| S43-01: Create a native executable container with: File magic. | in progress |
| S43-02: Create a native executable container with: Header version. | in progress |
| S43-03: Create a native executable container with: Architecture. | in progress |
| S43-04: Create a native executable container with: ABI version. | not started |
| S43-05: Create a native executable container with: A native ONBF segment table. ELF64 PIE exists only as linker output and is converted by `onbf-pack`; the loader never parses ELF (ADR-0001). | in progress |
| S43-06: Create a native executable container with: Executable segments. | in progress |
| S43-07: Create a native executable container with: Entry point. | in progress |
| S43-08: Create a native executable container with: Relocations. | in progress |
| S43-09: Create a native executable container with: Dynamic dependencies. | not started |
| S43-10: Create a native executable container with: Capabilities. | not started |
| S43-11: Create a native executable container with: Signature policy. | in progress |
| S43-12: Create a native executable container with: Resource references. | not started |
| S43-13: Create a native executable container with: Integrity checks. | in progress |
| S43-14: Implement malicious-input handling, fuzzing and loader tests. | in progress |
| S45-01: Define: Application identity. | not started |
| S45-02: Define: Main `.ose`. | not started |
| S45-03: Define: Resource layout. | not started |
| S45-04: Define: Libraries. | not started |
| S45-05: Define: Localization. | not started |
| S45-06: Define: Icons. | not started |
| S45-07: Define: Capabilities. | not started |
| S45-08: Define: File associations. | not started |
| S45-09: Define: Launch policy. | not started |
| S45-10: Define: Signing. | not started |
| S45-11: Define: Update metadata. | not started |
| S46-01: Implement: Package parsing. | not started |
| S46-02: Implement: Signatures. | not started |
| S46-03: Implement: Dependency resolution. | not started |
| S46-04: Implement: File verification. | not started |
| S46-05: Implement: Transactional installation. | not started |
| S46-06: Implement: Atomic registration. | not started |
| S46-07: Implement: Repair. | not started |
| S46-08: Implement: Upgrade. | not started |
| S46-09: Implement: Uninstall. | not started |
| S46-10: Implement: Rollback. | not started |
| S46-11: Implement: Offline installation. | not started |
| S46-12: Implement: Disk-space validation. | not started |
| S46-13: Implement: Interrupted-installation recovery. | not started |
| S46-14: Avoid arbitrary privileged installer scripts. | not started |
| S46-15: No install-time code runs at all; installation is declarative (ADR-0001). | not started |
| S47-01: Define: Driver identity. | not started |
| S47-02: Define: Hardware IDs. | not started |
| S47-03: Define: Supported Origin Driver ABI. | not started |
| S47-04: Define: Firmware dependencies. | not started |
| S47-05: Define: Capabilities. | not started |
| S47-06: Define: Privileges. | not started |
| S47-07: Define: Signature. | not started |
| S47-08: Define: Driver binary. | not started |
| S47-09: Define: Activation. | not started |
| S47-10: Define: Rollback. | not started |
| S47-11: Define: Recovery. | not started |
| S47-12: Third-party drivers run only in user mode, confined by the IOMMU and signed with Origin attestation. Kernel-mode drivers are first-party only, built from Origin source in the unified release (ADR-0001). | not started |

</details>

<details>
<summary>SP-62. Origin Interface Definition Language (OIDL) docs/spec/OIDL.md: 39 requirements</summary>

Named in ADR-0011. §17 baseline: none. Implemented in: `oidl`, `origin-syscalls`, `origin-core`.

| Requirement | State |
|---|---|
| A11-N1: One rule | not started |
| A11-N2: Everyone is a client, with no private doors | not started |
| A11-N3: Every module has a typed interface; only a capability gets a promise | not started |
| A11-N4: Three SDKs with three different promises | not started |
| A11-N5: The kernel is below the SDK, and the SDK is its only door | in progress |
| A11-N6: The Framekernel is itself an SDK: one privileged crate, safe services above it | built |
| A11-N7: One interface definition, generated everywhere | in progress |
| A11-N8: One definition, three costs | not started |
| A11-N9: The SDK is not a security boundary | in progress |
| A11-N10: Versions | in progress |
| A11-N12: The exceptions, a closed list | not started |
| A11-N13: Enforced by the gates | in progress |
| A11-N14: Order of work | in progress |
| A11-M1: Starting a program is a named capability | not started |
| A11-M2: A release gate reads every first-party manifest | not started |
| A11-M3: A program without the capability is never handed the launch interface | not started |
| A11-M4: The base image holds no command interpreter | not started |
| A11-M5: The environment carries nothing by default | not started |
| A11-M6: A GUI test reads the kernel's record of process creation | not started |
| A11-M7: No interface takes text that its receiver executes or hands to a program | not started |
| A11-L1: Until the Loader Service exists, step 4's programs reach ring 3 inside the kernel's image | built |
| A11-L2: `origin.core` is one image per release, with no writable data and nothing to relocate | built |
| A11-L3: The launch path binds each program's imports from `origin.core` | built |
| A11-L4: In step 4 `origin.core` holds the generated stubs and nothing else | built |
| A11-L5: `origin.core` exports exactly the entry points OIDL generates for `origin.kernel`, and its version is `user-abi` | in progress |
| A11-L6: Each stub is a naked function | built |
| A11-L7: The kernel knows each call's offset in `origin.core` from the build, and records `origin.core`'s base when it maps it | built |
| A11-L8: The kernel's own proof processes keep ADR-0005 A6's range; every other process is pinned per call | built |
| A11-L9: Step 4 goes ahead with row I2 of the system-call threat model at "Partly" | not started |
| A11-L10: `origin.diagnostics` 0.1 serves the boot report | not started |
| A11-L11: What N8 measures in step 4 | not started |
| A11-L12: N8's figure on the declared hardware is owed before any performance claim and before release qualification. Step 5 does not wait for it | not started |
| A11-L13: Each generated stub extends every parameter narrower than 64 bits to its whole register before the call | built |
| A11-L14: `origin.core` reserves no exit codes at major 0 | not started |
| A11-T1: Host tools are libraries first | built |
| A11-T2: When Origin hosts its own builds, the same libraries become the Developer SDK | not started |
| A11-T3: CLAUDE.md §49's list gains six SDK areas, each defined once in OIDL (N7) | not started |
| A11-T4: The desktop shell, Settings, the installer, Origin Setup and the server's management app use only published SDK interfaces | not started |
| A11-T5: A module that more than one SDK domain uses, and that programs outside the release do not, gets an interface in OIDL marked first-party | not started |

</details>

<details>
<summary>Exact counts</summary>

The milestone, feature-area and specification tables, with the number of requirements in each state in place of its share.

| Milestone | Requirements | Done | Built | In progress | Not started |
|---|--:|--:|--:|--:|--:|
| M0. Toolchain and engineering foundation | 107 | 41 | 36 | 12 | 18 |
| M1. Bootable Framekernel | 55 | 1 | 51 | 2 | 1 |
| M2. CPU and memory | 44 | 0 | 33 | 10 | 1 |
| M3. SMP and scheduler | 107 | 1 | 46 | 11 | 49 |
| M4. Processes and IPC | 140 | 1 | 97 | 24 | 18 |
| M5. Storage | 115 | 0 | 49 | 20 | 46 |
| M6. Native runtime | 125 | 0 | 14 | 36 | 75 |
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
| Testing, hardware qualification and release | §72 to §76 | 69 | 1 | 10 | 8 | 50 |
| **All fourteen areas** | | 834 | 17 | 128 | 86 | 603 |

| Specification | Requirements | Done | Built | In progress | Not started |
|---|--:|--:|--:|--:|--:|
| Unified Extensible Firmware Interface (UEFI) 2.11 | 45 | 0 | 24 | 0 | 21 |
| Advanced Configuration and Power Interface (ACPI) 6.6 | 20 | 0 | 6 | 2 | 12 |
| Rust language, compiler and Cargo 1.100.0 stable, edition 2024 | 88 | 15 | 36 | 8 | 29 |
| Intel 64 and IA-32 Architectures Software Developer's Manual current (version 093 read) | 79 | 1 | 61 | 11 | 6 |
| AMD64 Architecture Programmer's Manual current | 79 | 1 | 61 | 11 | 6 |
| PCI Express Base 6.4 and 7.0 | 11 | 0 | 5 | 1 | 5 |
| PCI Firmware 3.3 | 2 | 0 | 2 | 0 | 0 |
| NVM Express Base 2.4 | 16 | 0 | 8 | 2 | 6 |
| NVM Express NVM Command Set 1.3 | 3 | 0 | 2 | 1 | 0 |
| NVM Express over PCIe Transport 1.4 | 7 | 0 | 6 | 0 | 1 |
| Intel Virtualization Technology for Directed I/O (VT-d) 5.20 | 33 | 0 | 30 | 0 | 3 |
| AMD I/O Virtualization Technology (IOMMU), document 48882 3.11 | 33 | 0 | 30 | 0 | 3 |
| USB4 v2.0, November 2025 edition | 11 | 0 | 0 | 0 | 11 |
| USB Type-C and USB Power Delivery Power Delivery 3.3 version 1.0; Type-C: the relevant revisions | 3 | 0 | 0 | 0 | 3 |
| Thunderbolt 4 and 5 (not public; Intel licence required) | 3 | 0 | 0 | 0 | 3 |
| Universal Serial Bus 2.0 and 3.2, and the xHCI host controller interface not pinned yet | 11 | 0 | 0 | 0 | 11 |
| USB device classes: HID, Mass Storage, Audio, communications and video not pinned yet | 14 | 0 | 0 | 1 | 13 |
| Vulkan 1.4 | 0 | 0 | 0 | 0 | 0 |
| DisplayPort and HDMI HDMI 2.1; DisplayPort not pinned yet | 5 | 0 | 0 | 0 | 5 |
| Intel High Definition Audio not pinned yet | 1 | 0 | 0 | 0 | 1 |
| Unicode 18.0 | 11 | 0 | 0 | 1 | 10 |
| OpenType font format not pinned yet | 4 | 0 | 0 | 0 | 4 |
| UTF-8 (RFC 3629) RFC 3629 | 1 | 0 | 0 | 1 | 0 |
| TPM 2.0 Library and TCG PC Client Library version 185; PC Client: current | 2 | 0 | 0 | 0 | 2 |
| The Update Framework (TUF) 1.0.37 | 10 | 0 | 0 | 0 | 10 |
| System V x86-64 psABI 1.0, at commit e1ce0983 of 2025-03-12 | 6 | 0 | 1 | 4 | 1 |
| ELF generic ABI not pinned yet | 2 | 0 | 0 | 1 | 1 |
| Secure Hash Standard (FIPS 180-4) FIPS 180-4 | 17 | 0 | 14 | 3 | 0 |
| SHA-3 and SHAKE (FIPS 202) FIPS 202 | 1 | 0 | 0 | 1 | 0 |
| ML-DSA (FIPS 204) FIPS 204 with NIST's errata | 1 | 0 | 0 | 1 | 0 |
| SLH-DSA (FIPS 205) FIPS 205 | 0 | 0 | 0 | 0 | 0 |
| Stateful hash-based signatures (NIST SP 800-208) SP 800-208 | 0 | 0 | 0 | 0 | 0 |
| Ed25519 (RFC 8032) RFC 8032 | 1 | 0 | 0 | 1 | 0 |
| CBOR (RFC 8949) RFC 8949 | 1 | 0 | 0 | 1 | 0 |
| Zstandard (RFC 8878) RFC 8878 | 1 | 0 | 0 | 0 | 1 |
| IEEE 802.11 (Wi-Fi) 802.11-2024 with the 802.11be amendment | 1 | 0 | 0 | 0 | 1 |
| Bluetooth Core 6.2 or later | 1 | 0 | 0 | 0 | 1 |
| Deterministic random bit generators (NIST SP 800-90A) Rev. 1, June 2015 | 16 | 0 | 15 | 1 | 0 |
| Entropy sources (NIST SP 800-90B) January 2018 | 15 | 0 | 14 | 1 | 0 |
| Random bit generator constructions (NIST SP 800-90C) September 2025 | 15 | 0 | 14 | 1 | 0 |
| HMAC (FIPS 198-1) July 2008 | 15 | 0 | 14 | 1 | 0 |
| Ethernet (IEEE 802.3) not pinned yet | 1 | 0 | 0 | 1 | 0 |
| IPv4 (RFC 791) and ICMP (RFC 792) RFC 791, RFC 792 | 2 | 0 | 0 | 2 | 0 |
| IPv6 (RFC 8200) and ICMPv6 (RFC 4443) RFC 8200, RFC 4443 | 2 | 0 | 0 | 1 | 1 |
| ARP (RFC 826) and Neighbor Discovery (RFC 4861) RFC 826, RFC 4861 | 1 | 0 | 0 | 1 | 0 |
| TCP (RFC 9293) RFC 9293 | 1 | 0 | 0 | 1 | 0 |
| UDP (RFC 768) RFC 768 | 1 | 0 | 0 | 1 | 0 |
| DHCP (RFC 2131) and DHCPv6 (RFC 8415) RFC 2131, RFC 8415 | 1 | 0 | 0 | 1 | 0 |
| DNS (RFC 1034 and RFC 1035) RFC 1034, RFC 1035 | 1 | 0 | 0 | 1 | 0 |
| TLS 1.3 (RFC 8446) RFC 8446 | 1 | 0 | 0 | 0 | 1 |
| NTP (RFC 5905) RFC 5905 | 1 | 0 | 0 | 0 | 1 |
| IANA time zone database not pinned yet | 1 | 0 | 0 | 0 | 1 |
| Serial Port Console Redirection (SPCR) and Debug Port 2 (DBG2) tables SPCR table revision 4; DBG2 table 3 | 11 | 0 | 11 | 0 | 0 |
| 16550-compatible UART not pinned yet | 11 | 0 | 11 | 0 | 0 |
| High Precision Event Timer (IA-PC HPET) not pinned yet | 2 | 0 | 1 | 1 | 0 |
| 8254 programmable interval timer not pinned yet | 1 | 0 | 1 | 0 | 0 |
| MC146818-compatible real-time clock not pinned yet | 1 | 0 | 0 | 1 | 0 |
| 8042 keyboard and mouse controller not pinned yet | 2 | 0 | 0 | 1 | 1 |
| Virtual I/O devices (virtio) 1.1 | 1 | 0 | 1 | 0 | 0 |
| Semantic Versioning 2.0.0 | 5 | 2 | 2 | 0 | 1 |
| Origin Native Binary Format (ONBF) docs/spec/ONBF.md | 75 | 0 | 1 | 17 | 57 |
| Origin Interface Definition Language (OIDL) docs/spec/OIDL.md | 39 | 0 | 10 | 7 | 22 |

</details>

## How this page is made

- **From the records.** `cargo xtask progress` writes it from four of them. They are the requirements matrix ([`docs/requirements/`](docs/requirements/README.md)), the headings of `CLAUDE.md` and of the ADRs, `docs/ROADMAP.md`, and the list of specifications beside the matrix. Every figure is a count of requirements or a share of one, and each requirement counts as one.
- **Not by hand.** `cargo xtask check` refuses a tree whose page differs from the one it generates, so the page on `main` is current after every merge.
- **Not counted:** 51 requirements kept for information only, and 3 removed from scope. Only done counts as complete, and neither built nor in progress counts as coverage (decision items 193 and 221).
