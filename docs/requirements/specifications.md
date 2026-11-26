# Specifications, standards and protocols

Every specification, standard and protocol Origin OS relies on, with the requirements of the matrix that rest on it (owner item 374; TASKS item 151). `cargo xtask progress` reads this list and shows each specification on [`PROGRESS.md`](../../PROGRESS.md), with its requirements by state.

## What the figures mean

They count Origin's requirements that rest on each specification.

- **They are not a measure of the specification.** A specification has many parts that no requirement asks for. A requirement counts once for every specification it rests on, so the figures of two specifications do not add up.
- **They claim no conformance.** Conformance needs the specification's own conformance tests (CLAUDE.md §46).

## How the list is kept

- **Rows:** each row names the requirements that rest on it, by ID, or as a prefix ending in `*`: `S33-*` is every requirement of §33, and `A19-*` every decision of ADR-0019. So a requirement added later to that section or ADR is counted with no edit here.
- **Baseline:** names the statements of CLAUDE.md §17 that the specification stands for. Every one of §17's baseline statements is named by some specification.
- **Kind:**
  - `standard`, a published standard;
  - `vendor`, a vendor's document;
  - `origin`, Origin's own specification;
  - `toolchain`, the compiler.
- **Revision:** the one the project names. "Not pinned yet" means the project has not yet confirmed a revision, which happens when its work starts (CLAUDE.md §16).
- **Crates:** the crates that implement parts of it, by the provenance table's names.

The gate refuses:
- a requirement or prefix that names no row of the matrix;
- a baseline cell naming a row that is not one of §17's baseline statements;
- one of §17's baseline statements that no specification names;
- a crate the provenance table has no row for.

It cannot judge whether a requirement truly rests on a specification, or whether a specification that §17 does not name is missing. Both are read in review.

| ID | Specification | Revision | Kind | Named in | Baseline | Rows | Crates |
|---|---|---|---|---|---|---|---|
| SP-01 | Unified Extensible Firmware Interface (UEFI) | 2.11 | standard | CLAUDE.md §17, §19; ADR-0002 | S17-01 | S19-*, S20-*, S21-02, S21-05, S21-06, S21-16, S58-01, A02-* | origin-boot-uefi, origin-boot-info |
| SP-02 | Advanced Configuration and Power Interface (ACPI) | 6.6 | standard | CLAUDE.md §17; ADR-0019 | S17-02 | S19-05, S24-05, S24-12, S67-01, S67-04, S67-05, S67-06, S67-07, S67-08, S67-09, S67-12, A19-* | origin-acpi-tables, origin-aml, framekernel |
| SP-03 | Rust language, compiler and Cargo | 1.100.0 stable, edition 2024 | toolchain | CLAUDE.md §8, §17 | S17-03 | S8-*, S9-*, S10-*, S11-*, S12-*, S13-*, S14-*, S15-* | xtask |
| SP-04 | Intel 64 and IA-32 Architectures Software Developer's Manual | current (version 093 read) | vendor | CLAUDE.md §17 | S17-04 | S24-*, S28-03, S28-07, S28-14, S28-17, S58-07, S68-03, S68-04, S68-05, A03-*, A04-*, A08-*, A12-*, A17-* | framekernel, origin-frame, origin-paging |
| SP-05 | AMD64 Architecture Programmer's Manual | current | vendor | CLAUDE.md §17 | S17-05 | S24-*, S28-03, S28-07, S28-14, S28-17, S58-07, S68-03, S68-04, S68-05, A03-*, A04-*, A08-*, A12-*, A17-* | framekernel, origin-frame, origin-paging |
| SP-06 | PCI Express Base | 6.4 and 7.0 | standard | CLAUDE.md §17, §32 | S17-06 | S32-01, S32-02, S32-03, S32-04, S32-05, S32-06, S32-10, S32-11, S32-12, S32-14, S35-05 | framekernel |
| SP-07 | PCI Firmware | 3.3 | standard | the provenance table | - | S32-01, S32-03 | framekernel |
| SP-08 | NVM Express Base | 2.4 | standard | CLAUDE.md §17, §33 | S17-07 | S21-04, S33-01, S33-02, S33-03, S33-04, S33-05, S33-06, S33-07, S33-08, S33-09, S33-10, S33-11, S33-12, S33-13, S33-14, S33-15 | framekernel |
| SP-09 | NVM Express NVM Command Set | 1.3 | standard | CLAUDE.md §17 | S17-08 | S33-04, S33-10, S33-11 | framekernel |
| SP-10 | NVM Express over PCIe Transport | 1.4 | standard | CLAUDE.md §17 | S17-09 | S33-01, S33-02, S33-03, S33-05, S33-06, S33-07, S33-09 | framekernel |
| SP-11 | Intel Virtualization Technology for Directed I/O (VT-d) | 5.20 | vendor | ADR-0007 | - | S32-06, S32-07, S32-09, S32-13, S35-11, A07-* | framekernel, origin-frame |
| SP-12 | AMD I/O Virtualization Technology (IOMMU), document 48882 | 3.11 | vendor | ADR-0007 | - | S32-06, S32-08, S32-09, S32-13, S35-11, A07-* | framekernel, origin-frame |
| SP-13 | USB4 | v2.0, November 2025 edition | standard | CLAUDE.md §17, §35 | S17-10 | S35-02, S35-03, S35-04, S35-05, S35-06, S35-07, S35-08, S35-09, S35-10, S35-11, S56-12 | - |
| SP-14 | USB Type-C and USB Power Delivery | Power Delivery 3.3 version 1.0; Type-C: the relevant revisions | standard | CLAUDE.md §17, §35 | S17-11 | S35-01, S35-10, S67-11 | - |
| SP-15 | Thunderbolt | 4 and 5 (not public; Intel licence required) | vendor | CLAUDE.md §17, §35 | S17-12 | S35-12, S35-13, S35-14 | - |
| SP-16 | Universal Serial Bus 2.0 and 3.2, and the xHCI host controller interface | not pinned yet | standard | CLAUDE.md §34 | - | S34-01, S34-02, S34-03, S34-04, S34-05, S34-06, S34-07, S34-08, S34-09, S34-15, S34-16 | - |
| SP-17 | USB device classes: HID, Mass Storage, Audio, communications and video | not pinned yet | standard | CLAUDE.md §34, §55, §56 | - | S34-10, S34-11, S34-12, S34-13, S34-14, S55-07, S56-01, S56-02, S56-03, S56-04, S56-05, S56-06, S56-07, S56-08 | - |
| SP-18 | Vulkan | 1.4 | standard | CLAUDE.md §17 | S17-13 | - | - |
| SP-19 | DisplayPort and HDMI | HDMI 2.1; DisplayPort not pinned yet | standard | CLAUDE.md §36; ADR-0015 D7 | - | S35-06, S36-02, S36-03, S36-14, S56-11 | - |
| SP-20 | Intel High Definition Audio | not pinned yet | vendor | CLAUDE.md §55 | - | S55-08 | - |
| SP-21 | Unicode | 18.0 | standard | CLAUDE.md §17, §51 | S17-14 | S51-01, S51-02, S51-03, S51-04, S51-05, S51-06, S51-07, S51-08, S51-13, S51-17, S51-18 | - |
| SP-22 | OpenType font format | not pinned yet | standard | CLAUDE.md §51 | - | S51-09, S51-10, S51-11, S51-12 | - |
| SP-23 | UTF-8 (RFC 3629) | RFC 3629 | standard | the provenance table | - | S51-01 | onbf-format |
| SP-24 | TPM 2.0 Library and TCG PC Client | Library version 185; PC Client: current | standard | CLAUDE.md §17, §58 | S17-15 | S58-02, S58-03 | - |
| SP-25 | The Update Framework (TUF) | 1.0.37 | standard | CLAUDE.md §17, §64 | S17-16 | S64-01, S64-02, S64-03, S64-04, S64-05, S64-06, S64-07, S64-08, S64-14, S64-15 | - |
| SP-26 | System V x86-64 psABI | 1.0, at commit e1ce0983 of 2025-03-12 | standard | CLAUDE.md §17, §42 | S17-17 | S10-02, S10-05, S42-02, S42-03, S42-04, S42-16 | origin-rt, origin-abi, origin-syscalls |
| SP-27 | ELF generic ABI | not pinned yet | standard | CLAUDE.md §43; the provenance table | - | S14-06, S43-05 | onbf-pack |
| SP-28 | Secure Hash Standard (FIPS 180-4) | FIPS 180-4 | standard | CLAUDE.md §17 | S17-18 | S43-13, S59-14, S59-15, A16-* | origin-crypto, origin-random, onbf-verify |
| SP-29 | SHA-3 and SHAKE (FIPS 202) | FIPS 202 | standard | CLAUDE.md §17 | S17-18 | S59-14 | origin-crypto |
| SP-30 | ML-DSA (FIPS 204) | FIPS 204 with NIST's errata | standard | CLAUDE.md §17, §59; ADR-0001 | S17-18 | S59-14 | origin-crypto, onbf-verify |
| SP-31 | SLH-DSA (FIPS 205) | FIPS 205 | standard | CLAUDE.md §17 | S17-18 | - | - |
| SP-32 | Stateful hash-based signatures (NIST SP 800-208) | SP 800-208 | standard | CLAUDE.md §17 | S17-18 | - | - |
| SP-33 | Ed25519 (RFC 8032) | RFC 8032 | standard | CLAUDE.md §17, §59; ADR-0001 | S17-19 | S59-14 | origin-crypto, onbf-verify |
| SP-34 | CBOR (RFC 8949) | RFC 8949 | standard | CLAUDE.md §17; ADR-0001 | S17-19 | A01-def3 | onbf-format |
| SP-35 | Zstandard (RFC 8878) | RFC 8878 | standard | CLAUDE.md §17; ADR-0001 | S17-19 | A01-def6 | - |
| SP-36 | IEEE 802.11 (Wi-Fi) | 802.11-2024 with the 802.11be amendment | standard | CLAUDE.md §17, §54 | S17-20 | S54-14 | - |
| SP-37 | Bluetooth Core | 6.2 or later | standard | CLAUDE.md §17, §54 | S17-21 | S54-15 | - |
| SP-38 | Deterministic random bit generators (NIST SP 800-90A) | Rev. 1, June 2015 | standard | CLAUDE.md §17, §59; ADR-0016 | S17-22 | S59-07, S59-15, A16-* | origin-random, origin-frame |
| SP-39 | Entropy sources (NIST SP 800-90B) | January 2018 | standard | CLAUDE.md §17; ADR-0016 | S17-22 | S59-15, A16-* | origin-random |
| SP-40 | Random bit generator constructions (NIST SP 800-90C) | September 2025 | standard | CLAUDE.md §17; ADR-0016 | S17-22 | S59-15, A16-* | origin-random |
| SP-41 | HMAC (FIPS 198-1) | July 2008 | standard | CLAUDE.md §17; ADR-0016 | S17-22 | S59-15, A16-* | origin-crypto, origin-random |
| SP-42 | Ethernet (IEEE 802.3) | not pinned yet | standard | CLAUDE.md §54 | - | S54-01 | - |
| SP-43 | IPv4 (RFC 791) and ICMP (RFC 792) | RFC 791, RFC 792 | standard | CLAUDE.md §54 | - | S54-02, S54-07 | - |
| SP-44 | IPv6 (RFC 8200) and ICMPv6 (RFC 4443) | RFC 8200, RFC 4443 | standard | CLAUDE.md §54 | - | S54-03, S54-07 | - |
| SP-45 | ARP (RFC 826) and Neighbor Discovery (RFC 4861) | RFC 826, RFC 4861 | standard | CLAUDE.md §54 | - | S54-04 | - |
| SP-46 | TCP (RFC 9293) | RFC 9293 | standard | CLAUDE.md §54 | - | S54-05 | - |
| SP-47 | UDP (RFC 768) | RFC 768 | standard | CLAUDE.md §54 | - | S54-06 | - |
| SP-48 | DHCP (RFC 2131) and DHCPv6 (RFC 8415) | RFC 2131, RFC 8415 | standard | CLAUDE.md §54 | - | S54-08 | - |
| SP-49 | DNS (RFC 1034 and RFC 1035) | RFC 1034, RFC 1035 | standard | CLAUDE.md §54 | - | S54-09 | - |
| SP-50 | TLS 1.3 (RFC 8446) | RFC 8446 | standard | CLAUDE.md §54 | - | S54-13 | - |
| SP-51 | NTP (RFC 5905) | RFC 5905 | standard | CLAUDE.md §68 | - | S68-07 | - |
| SP-52 | IANA time zone database | not pinned yet | standard | CLAUDE.md §68 | - | S68-06 | - |
| SP-53 | Serial Port Console Redirection (SPCR) and Debug Port 2 (DBG2) tables | SPCR table revision 4; DBG2 table 3 | vendor | ADR-0010 | - | S19-11, A10-* | origin-spcr, framekernel |
| SP-54 | 16550-compatible UART | not pinned yet | vendor | ADR-0010 | - | S19-11, A10-* | framekernel |
| SP-55 | High Precision Event Timer (IA-PC HPET) | not pinned yet | vendor | ADR-0012 | - | S68-03, S68-04 | framekernel |
| SP-56 | 8254 programmable interval timer | not pinned yet | vendor | the boot proofs (ADR-0013) | - | S68-04 | framekernel |
| SP-57 | MC146818-compatible real-time clock | not pinned yet | vendor | the boot proofs (ADR-0013) | - | S68-02 | framekernel |
| SP-58 | 8042 keyboard and mouse controller | not pinned yet | vendor | the boot proofs (ADR-0013) | - | S56-01, S56-02 | framekernel |
| SP-59 | Virtual I/O devices (virtio) | 1.1 | standard | ADR-0007 | - | A07-I10 | framekernel |
| SP-60 | Semantic Versioning | 2.0.0 | standard | the provenance table | - | S62-01, S62-16, S63-01, S63-02, S63-03 | origin-version |
| SP-61 | Origin Native Binary Format (ONBF) | docs/spec/ONBF.md | origin | CLAUDE.md, header; ADR-0001 | - | H-14, H-15, H-16, H-17, H-18, H-19, S43-*, S45-*, S46-*, S47-*, A01-* | onbf-format, onbf-merkle, onbf-verify, onbf-pack, onbf-sign, origin-boot-uefi |
| SP-62 | Origin Interface Definition Language (OIDL) | docs/spec/OIDL.md | origin | ADR-0011 | - | A11-* | oidl, origin-syscalls, origin-core |
