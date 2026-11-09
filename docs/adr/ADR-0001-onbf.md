# ADR-0001: Origin Native Binary Format (ONBF)

| | |
|---|---|
| Status | **Accepted** by the owner, 2026-11-07 |
| Covers these CLAUDE.md §18 topics | application executable format; package format; cryptographic profile; driver packaging and trust (the runtime Driver ABI gets its own ADR) |
| Specification | `docs/spec/ONBF.md` v1.0 (design) |
| Evidence | `docs/research/2026-11-07-onbf.md` (R1–R20, E1–E4) |

## Context

CLAUDE.md v7.0 named four native formats (`.ose`, `.osi`, `.oapp`, `.od`). It left three formats undefined: shared libraries, the kernel image and the release manifest. It also said `.ose` would carry an ELF64 PIE payload "initially".

Origin uses a framekernel. Its memory-safety argument depends on every non-TCB service being *compiled* as safe Rust against the privileged framework (R1).

Executable and package formats are long-lived ABI commitments. Every binary a third party ships must stay loadable. So these decisions were made before any loader code exists.

## Decision 1: one container, native segments

**Decision.** All native artifacts are profiles of one container, ONBF: EXE, LIB, APP, PKG, DRV, KRN, SYS and SYM. Executables use ONBF's own segment table. ELF64 PIE exists only as `rust-lld` output on the build host, and `onbf-pack` converts it under strict rules (spec §6.1.2).

**Alternatives.**
- (b) ELF64 inside the container, as §43 said.
- (c) ELF now, native later.

**Evidence.**
- ELF allows segments that share file pages, text relocations, interpreters and many dynamic tags. A privileged loader would have to parse and police all of them.
- Content-addressed, Merkle-verified package stores show the value of page-aligned, verifiable layouts (R10, R11).

**Trade-offs.**
- Gains:
  - a small, strict parser in the privileged loader
  - W^X, page separation and "no text relocations" hold by construction
- Costs:
  - a host converter
  - a detached symbol file (`.osym`) for debugging

**Migration consequences.** None now, because nothing has shipped. Option (c) would have meant two loaders and permanent support for ELF-payload binaries.

## Decision 2: third-party drivers are user-mode only

**Decision.**
- Vendor drivers run as separate processes, confined by the IOMMU, with device-bound capabilities. They are restarted on failure.
- Kernel-mode drivers are first-party only, built from Origin source inside the measured kernel image of the unified release.

**Alternatives.**
- (b) Origin builds vendor kernel source. This conflicts with CLAUDE.md §3 and §5, which forbid third-party driver code in the runtime.
- (c) Binary kernel drivers with an Origin signature, as Windows does.

**Evidence.**
- A binary loaded into ring 0 cannot show that it was compiled as safe Rust, so it voids the framekernel's TCB argument (R1).
- Windows moved the same way in 2026. Microsoft's Driver Quality Initiative describes "the third-party kernel mode driver transition" to user-mode drivers or Microsoft's own class drivers. Microsoft is also removing trust for cross-signed kernel drivers from April 2026 (R18).

**Trade-offs.**
- Third-party drivers pay for interrupt delivery and IPC. That overhead is to be measured.
- Vendors must target the user-mode Origin Driver ABI.
- Devices that need a high-performance user-mode model, such as GPUs, need careful driver-ABI design.

**Migration consequences.** Loosening this later would need a new ADR and would weaken the TCB argument. Starting strict is the reversible direction.

## Decision 3: hybrid signatures

**Decision.**
- Every ONBF signature carries Ed25519 (RFC 8032) and ML-DSA-65 (FIPS 204), and both must verify.
- Root, kernel and release keys use Ed25519 with ML-DSA-87.
- Hashes: SHA-512 for content digests, SHA-256 for Merkle trees.
- Origin OS contains verification code only.

**Alternatives.**
- (b) ML-DSA-87 alone.
- (c) Ed25519 alone, with post-quantum later.
- (d) Hybrid plus hash-based (LMS or SLH-DSA) root keys.

**Evidence.**
- FIPS 204 and 205 are final, and FIPS 204 has a NIST errata sheet (R7, R8).
- CNSA 2.0 allows ML-DSA-87 for software signing and prefers LMS/XMSS (R9).
- BoringSSL advises ML-DSA-65 unless CNSA 2.0 compliance is the goal (R7).
- Signature size is about 3.4 KB, which is negligible for packages.

**Trade-offs.**
- Two verifiers to implement originally and test: SHA-512, SHAKE, Curve25519 and the ML-DSA NTT.
- Signatures are larger than classical ones.
- In return, a forgery must break both algorithms. That protects against a future quantum computer and against a flaw in a young algorithm or a young implementation.

**Migration consequences.** Suite IDs and the reserved 32-byte digest slot let a new suite (for example hash-based roots) be added without a new format major version. Old suites can be retired by signed policy.

## Decision 4: open publishing, in tiers

**Decision.** Three tiers: Verified (free registration with identity checks), Notarized (Verified plus a notary counter-signature) and Unverified (self-generated key).
- Unverified apps install only after explicit consent.
- They are per-user and always sandboxed.
- Their signing key is pinned at first install.
- Drivers are never accepted from the Unverified tier and always need Origin attestation (spec §7.7).

**Alternatives.**
- (b) Origin-signed only.
- (c) Fully open with warnings only.

**Evidence.** On Windows, signing certificates cost money and no OV or EV certificate bypasses SmartScreen reputation (R17). Free, identity-based registration removes that barrier without giving up accountability.

**Trade-offs.** Origin must run a Publisher CA, identity verification and a notary service. The Unverified tier is attack surface, bounded by consent and the sandbox.

**Migration consequences.** Tier policy is signed data. It can tighten or relax without a format change.

## Decision 5: 64-bit only, no Windows formats

**Decision** (owner's own wording: "No Windows .exe or .msi file support, no 32 bit support, only 64 bit"):
- the header `arch` accepts only x86-64 (LP64)
- there is no IA-32, no x32 and no 16-bit code
- there is no foreign-format handler entry point
- the Framekernel has no 32-bit compatibility mode and no 32-bit system-call entry
- `onbf-pack` rejects 32-bit and x32 objects

**Alternatives.**
- (a) Native-only, with a reserved slot for a sandboxed compatibility runtime.
- (c) Windows compatibility in version 1.

**Trade-offs.** There is no access to the Windows application catalogue, so Origin's value rests entirely on native apps. In return there is no Win32 reimplementation, no 32-bit legacy paths in kernel entry and context switching, and less attack surface.

**Migration consequences.** Adding Windows compatibility later would need a new ADR and a new manifest entry-point type, which is a non-breaking extension. There is no plan for 32-bit support at any point.

## Approved defaults

The owner accepted all twelve defaults in spec Appendix B:
1. the umbrella format with the `.osl` extension
2. KRN and SYS profiles
3. OCBOR-1 manifests compiled from TOML
4. no X.509 in the package path
5. Merkle-SHA256-4K hashing
6. zstd for transport, with uncompressed installs
7. an `x86-64-v3` baseline, shadow stack on by default, IBT later
8. frame pointers, with `panic=abort` until the Origin `std` exists
9. eager binding and full RELRO, with no static constructors
10. private system calls, with `origin.core` as the only stable ABI
11. no downgrade except to the version kept locally
12. "Try" without installing

## Still open (owned elsewhere)

- **Boot trust anchor**: signing under Microsoft UEFI CA 2023, or Origin keys enrolled in firmware. Owned by the M1 boot ADR (R5).
- **Integer key numbers** for the OCBOR-1 schemas: assigned when `onbf-format` is implemented. *Resolved for the policy and the KRN profile on 2026-11-07 (owner decisions K1–K4): `docs/spec/ONBF-manifest-keys.md`. The other profiles follow the same registry.*
- **User-process ABI details** (TLS model, calling-convention specifics, system-call mechanism): owned by the user-ABI ADR, M4–M6.
- **Vector icon format**: owned by the UI ADR.
