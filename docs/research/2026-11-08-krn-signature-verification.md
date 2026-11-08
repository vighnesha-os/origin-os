# Research ledger: verifying the kernel's signature in the boot loader

| | |
|---|---|
| Date | 2026-11-08 |
| For | `onbf-verify::verify_krn_block`, the loader's `trust` and `kernel` modules, `onbf-pack sign` and `keygen`, the signature scenarios of `cargo xtask boot-test`; TASKS item 6 |
| Requires | CLAUDE.md §16 (research first), §19, §58–§60, §84; ONBF §4.2, §7, §8.1; ADR-0001 D3; ADR-0002 B2, B7, B8; encodings S1–S4; errata C3, C4, E1, M1 |
| Result | The smallest correct slice: direct trust in a compiled-in store, chains refused. Six readings are **Proposed** (errata V1–V6) and four gaps are open for the owner (errata O2–O5) |

## 1. What is specified, and what enforces it

| Requirement | Source | Enforced by |
|---|---|---|
| The block's layout, 1–8 entries, entry keys 1–7, limits | ONBF §7.1, App. A, S1–S3 | `onbf-format`, `SignatureBlock::parse` (soaked) |
| The role is an integer; the kernel role must use OSS-1R, Ed25519 + ML-DSA-87 | S1, S2, §7.2, D3 | The parser: `RoleSuite` and the fixed signature lengths |
| The statement's digest and profile equal the image's | §7.1 | `onbf_verify::verify_entry`, before any signature |
| Both signatures verify over the exact statement bytes, ML-DSA with context `origin/onbf/1/<role>` | §7.1 | `verify_entry`, Ed25519 then ML-DSA, each strict (errata E1, M1) |
| A KRN image needs the `origin-kernel` role | §7.5 | new: `verify_krn_block` |
| The verification order: parse, profile, content digest, signatures, manifest, sections | ADR-0002 B2, ONBF-R1 | `kernel::verify`; the signature check comes before any section's content is read |
| The roots live in the loader | ONBF §8.1 | new: `boot/uefi/src/trust.rs`, compiled in |
| An unsigned image is allowed only in developer mode or under a verified parent | §4.2, B8 | `Image::parse`, and now a message of its own (`KernelError::Unsigned`) |
| The OS only verifies; signing runs in host tools | CLAUDE.md §59, ONBF §7.2 | `onbf-pack sign` calls OpenSSL; no private key enters the OS or this tool |
| The signer produces only what the verifier accepts | E1, M1 | `sign_krn` re-verifies with `verify_krn_block` before writing |

`onbf-format` and `onbf-verify` already provided the parser and `verify_entry`, proven against reference-signed vectors. What was missing was the policy (which keys, which role, how many entries), the host signer, the loader wiring and the boot proofs.

## 2. What is still open

| Question | Where the text stops | This slice | Record |
|---|---|---|---|
| What an empty `chain` means | §7.1 says the chain ends "below a trust-store root" and S-rules allow length 0 | Length 0 means the signer's own key is in the trust store; any chain is refused | **V1, Proposed** |
| OKC chain validation | §7.3, §7.4, S4 define the format only | Not implemented: a chain is refused, never ignored | Remaining work |
| The ML-DSA context of an OKC's own signature | S4 gives no context string for it; §7.1's is for entries | Not needed yet | **O2, open** |
| Trusted time for `not_before`/`not_after` at boot | §7.6 needs signed timestamps; the loader has only the RTC | Not needed yet | **O3, open** |
| How `id_namespaces` match an `id` | §7.1, S4 | Not needed yet | **O4, open** |
| What a trust-store root *is*: a key, a self-signed OKC, or TUF-style root metadata | §7.4, §8.1 | The store holds raw public keys; a release build has none and refuses everything | **O5, open; V4, Proposed** |
| How many entries a KRN block holds | §7.5 names one role, §7.1 allows up to 8 | Exactly one (ONBF-R6: nothing unchecked) | **V2, Proposed** |
| The statement's `id` and `version` for a KRN | S3 says "from its manifest"; the KRN manifest has neither (K4) | `origin.framekernel`; the release version; the loader compares neither | **V3, Proposed** |
| Developer mode and a signed image | B8 only says it accepts unsigned ones | A signed image is verified in every mode | **V5, Proposed** |
| The signing time against ONBF-R9 | §7.1 puts it in the statement; R9 forbids build timestamps | The caller supplies it; no build reads the clock | **V6, Proposed** |
| The statement `id`'s NFC rule | errata O1 | **Untouched.** The KRN `id` is ASCII, and the loader never normalizes | O1 stays open |

Out of scope, by the task and the ADR: Secure Boot's first stage (B7's first link), anti-rollback (§7.6, needs SYS and a TPM NV index), TPM measurement (§8.1), and release signing itself (HSM, CLAUDE.md §60, §75).

## 3. Why OpenSSL does the signing

Host tools sign; the OS only verifies (CLAUDE.md §59). So the choice was how a host tool should sign:

| Option | Verdict |
|---|---|
| **Run OpenSSL (3.5 or later) as a subprocess** | **Chosen.** No signing algorithm is written or owned here, so there is nothing to make constant-time or to zeroize, and no RNG to get right (the things §59 lists for secret-handling code). It is permitted host infrastructure (§4), already on the machine, and already cross-checked against `origin-crypto` (the ML-DSA provenance entry). The private key stays in a file that only OpenSSL opens; `onbf-pack` never reads it. A boot proof then shows a real cross-implementation agreement: OpenSSL signs, `origin-crypto` verifies. |
| Write Ed25519 and ML-DSA signing in Rust | Rejected. §4 says to implement established algorithms only when necessary. Signing handles secrets, which the verifier never does, and would need its own side-channel assessment and review. |
| Use the Python references | Rejected as the signer. `ed25519_reference.py` and `mldsa_reference.py` are test oracles (variable-time, big-integer); they stay what they are. |
| A signing crate (`ring`, `dalek`, a PQ crate) | Rejected. Each would be a host-tool dependency needing §5 approval, and none is needed. |

Details that follow from the choice:
- `onbf-pack` refuses anything but OpenSSL 3.5 or later (LibreSSL, macOS's `/usr/bin/openssl`, has no ML-DSA).
- Public keys are read from `openssl pkey -pubout -outform DER` and accepted only with the exact fixed `SubjectPublicKeyInfo` prefix for Ed25519 or ML-DSA-87, which also pins the parameter set.
- ML-DSA goes through `-pkeyopt hexcontext-string:<role context>`.
- OpenSSL's ML-DSA is hedged by default, so a signature differs per run. Signing is the one non-reproducible step of a build; the container, the content digest and the statement stay reproducible (ONBF-R9).
- `onbf-pack` is given the keys, so a compromised OpenSSL could misuse them. The boundary is the build host, as for any host-tool signer; the release path (HSM) is future work.

## 4. Test keys

The boot tests must sign with something, and the loader must trust it. A development key must never pass for a release key, so:
- `onbf-pack keygen --insecure-test-seed <text>` derives the key from public text; anyone can recompute it.
- No private key is stored in the repository. `cargo xtask boot-test` derives the private keys into `target/xtask/test-keys/`.
- The repository holds only the public halves, in `boot/uefi/trust/`, named `TEST-ONLY-…`, with a README. `xtask` checks them against the derived key before it boots anything.
- Only a loader built with `test-trust-store` (or `developer-mode`, which includes it) compiles them in, and it prints a warning on every boot. A build without the feature trusts nothing and refuses every kernel.
- "Must never ship" is enforced, not only stated: a loader holding the test key does not compile with a non-zero build number (`origin-version.toml`), which release engineering sets for a released build.

## 5. Threats considered

- **A kernel swapped on disk, with its digests made consistent.** The attacker recomputes the section digests, Merkle roots and content digest. Only the signed statement names the original digest, so the loader refuses it (the "substituted kernel" scenario). This is the attack the signature exists to stop.
- **A signed image with a corrupted segment.** The digest in the section table no longer matches: "section N does not match its digest".
- **A signature made for another image, or a replayed one.** The statement names another content digest. Refused before any signature is checked.
- **A key the loader does not trust.** The entry's key ID finds nothing in the store.
- **Parsing before verifying** (ONBF-R1). The signature block is the only variable-length structure read before the signature is verified, as it must be. `SignatureBlock::parse` is `no_std`, panic-free and soaked.
- **Downgrade to developer mode.** Developer mode is a build feature, not a runtime choice: no console input, file or firmware variable selects it (B8).

Not covered: a loader that is itself replaced (B7's first stage), rollback to an older signed kernel (§7.6), key revocation (§7.6), and the bytes of MERKLE sections, which the loader does not read (it recomputes each segment's root from the content, and nothing uses the hash blocks yet; page-level verification will check them against the root, ONBF-R2).
