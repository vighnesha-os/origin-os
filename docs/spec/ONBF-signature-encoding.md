# ONBF v1 signature-block encodings (proposal)

| | |
|---|---|
| Applies to | `docs/spec/ONBF.md` §7.1–§7.3 |
| Status | **S1–S4 decided by the owner on 2026-11-07: the recommended option for each** (integer roles 1–5, integer suites 1–2, integer statement keys 0–6, integer OKC keys 0–10). Normative for the signature-block parser, the host signer and any second implementation. |
| Follows | The key-registry policy K1–K4 (`docs/spec/ONBF-manifest-keys.md`): integer keys, a shared registry, ascending order, extensions at 1000 and above |

§7.1 names the entry keys 1–8 but leaves four things undefined:
- how `role` and the suite ID are encoded;
- the statement, whose fields are written as text names, which conflicts with K1;
- the key numbers of the Origin Key Certificate (OKC) in §7.3.

The recommendations below keep everything inside OCBOR-1 with integer keys, so one validator and one decoder style cover manifests, statements and certificates.

## S1. `role` (entry key 1, statement key 5): unsigned integer

| Value | Role | ML-DSA context string (§7.1) |
|---|---|---|
| 1 | publisher | `origin/onbf/1/publisher` |
| 2 | notary | `origin/onbf/1/notary` |
| 3 | origin-driver-attest | `origin/onbf/1/origin-driver-attest` |
| 4 | origin-kernel | `origin/onbf/1/origin-kernel` |
| 5 | origin-release | `origin/onbf/1/origin-release` |

0 and unknown values are rejected. The text names exist only in the context string, which the verifier builds from this table, so no string from the file is ever compared. *Alternative:* text roles. They are self-describing, but they cost a string comparison and a normalization rule on a verification path.

## S2. `suite` (entry key 2): unsigned integer

| Value | Suite | Keys | Signatures |
|---|---|---|---|
| 1 | OSS-1: Ed25519 + ML-DSA-65 | 32 + 1,952 B | 64 + 3,309 B |
| 2 | OSS-1R: Ed25519 + ML-DSA-87 | 32 + 2,592 B | 64 + 4,627 B |

0 and unknown values are rejected. `sig_mldsa` must have exactly the length its suite requires, and a role must use its required suite (§7.2: roots, kernel and release use OSS-1R).

## S3. Statement (entry key 5): an OCBOR-1 map with integer keys

| Key | Field | Type |
|---|---|---|
| 0 | `format` | uint, `1` = "onbf-sig-1" |
| 1 | `profile` | uint, the §1 profile ID; must equal the header's profile |
| 2 | `content_digest` | bytes, 64; must equal the recomputed SHA-512 |
| 3 | `id` | text (NFC); the artifact's ID from its manifest |
| 4 | `version` | text, SemVer 2.0 |
| 5 | `role` | uint (S1); must equal the entry's role |
| 6 | `claimed_signing_time` | uint, seconds since 1970-01-01 UTC; informational only (§7.6) |

The signatures cover the **exact encoded statement bytes**. Because OCBOR-1 is canonical, a statement has exactly one encoding, so the signer and the verifier cannot disagree about what was signed.

## S4. Origin Key Certificate (§7.3): an OCBOR-1 map with integer keys

| Key | Field | Type |
|---|---|---|
| 0 | `version` | uint, `1` |
| 1 | `serial` | bytes, 16 |
| 2 | `issuer_key_id` | bytes, 32 (SHA-256 of the issuer's canonical key encoding) |
| 3 | `subject` | map {0 `publisher_id` text, 1 `legal_name` text, 2 `country` text (ISO 3166-1 alpha-2)} |
| 4 | `keys` | map {0 `ed25519` bytes 32, 1 `mldsa` bytes (1,952 or 2,592)} |
| 5 | `roles` | array of uint (S1), ascending, no duplicates |
| 6 | `id_namespaces` | array of text, ascending, no duplicates |
| 7 | `not_before` | uint, seconds since 1970-01-01 UTC |
| 8 | `not_after` | uint, the same; must be greater than `not_before` |
| 9 | `constraints` | map; every key below 1000 is reserved and rejected in v1 |
| 10 | `signature` | map {0 `sig_ed25519` bytes 64, 1 `sig_mldsa` bytes}, over the certificate encoded *without* key 10 |

The canonical public-key encoding behind `key_id` is the OCBOR-1 encoding of the `keys` map.
