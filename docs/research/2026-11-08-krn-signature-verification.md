# Research ledger: verifying the kernel's signature in the boot loader

| | |
|---|---|
| Date | 2026-11-08 |
| For | `onbf-verify::verify_krn_block`, the loader's `trust` and `kernel` modules, `onbf-pack sign` and `keygen`, the signature scenarios of `cargo xtask boot-test`; TASKS item 6 |
| Requires | CLAUDE.md §16 (research first), §19, §58–§60, §84; ONBF §4.2, §7, §8.1; ADR-0001 D3; ADR-0002 B2, B7, B8; encodings S1–S4; errata C3, C4, E1, M1 |
| Result | The smallest correct slice: direct trust in a compiled-in store, chains refused. The owner decided V1–V6 and O2–O5 on 2026-11-08 (`ONBF-errata.md`) and chose Rust signing; the tables below are kept as written when they were open |

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
| The OS only verifies; signing runs in host tools | CLAUDE.md §59, ONBF §7.2 | `onbf-sign`, host only; `cargo xtask check` fails if the loader or the kernel depends on it |
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

## 3. Who signs, and in what

Host tools sign; the OS only verifies (CLAUDE.md §59). The first version ran OpenSSL 3.5+ as a subprocess, which meant no signing code in the tree. **The owner chose original Rust signing instead (2026-11-08)**, accepting that it handles secrets and needs its own constant-time work and review.

So signing lives in a new host-only crate, `developer-tools/onbf-sign` (ONBF §12 names that crate):

| Decision | Why |
|---|---|
| A crate of its own, not `origin-crypto` | `origin-crypto` ships in the loader and the kernel, and ONBF §7.2 says the OS contains verification code only. `cargo xtask check` now fails if the loader or the kernel can reach `onbf-sign`. |
| Only the hashes are reused from `origin-crypto` | Their state updates have no data-dependent branches or table lookups. The signing arithmetic is written separately, so a signature that the verifiers accept is agreement between two implementations. |
| ML-DSA-87 only, Ed25519 | The kernel role's suite (OSS-1R). ML-DSA-65 (OSS-1) is added when a role needs it. |
| OpenSSL kept only as a test cross-check | It verifies what `onbf-sign` produces (`openssl_accepts_these_signatures`, skipped if no OpenSSL is installed). The committed test public keys were made by OpenSSL from the same seeds, so `test_keys_come_from_their_seed_only` also shows the two derive identical keys. Nothing else uses it, and `boot-test` no longer needs it. |

Validation: the Ed25519 signer against RFC 8032 and 296 more vectors from `ed25519_reference.py signvectors` (seed, public key, signature), and the ML-DSA-87 signer against all 13 keys and 52 signatures in `mldsa-differential.txt`, which the independent Python reference produced and OpenSSL 4.1.0 matched byte for byte; every signature must also pass `origin-crypto`'s strict verifiers.

The side-channel assessment is in `developer-tools/onbf-sign/src/lib.rs`: fixed-width arithmetic with masks, doubling and adding on every scalar bit, and rejection decisions made only on the combined result of each check. What it does not establish is stated there too: compiler and processor behaviour are assumed, not proven, and there is no timing measurement yet. Zeroization is best effort because the crate forbids `unsafe`, so it cannot use volatile writes. Release signing still belongs in an HSM (ONBF §7.2, CLAUDE.md §60).

ML-DSA signing is hedged with fresh randomness from `/dev/urandom` (FIPS 204 §3.4), so signing remains the one non-reproducible step of a build; the container, the content digest and the statement stay reproducible (ONBF-R9).

## 4. Test keys

The boot tests must sign with something, and the loader must trust it. A development key must never pass for a release key, so:
- `onbf-pack keygen --insecure-test-seed <text>` derives the key from public text; anyone can recompute it.
- No private key is stored in the repository. `cargo xtask boot-test` derives the private keys into `target/xtask/test-keys/` (one key file, mode 0600, read only by `onbf-pack`).
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
