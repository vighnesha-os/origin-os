# Wycheproof test vectors

`ed25519.txt` is derived from `testvectors_v1/ed25519_test.json` of Project Wycheproof,
<https://github.com/C2SP/wycheproof>, commit `d17cc89fa5c8f4f4d378b5385ebf75da5d0926c9`, under the
Apache License 2.0 (`LICENSE` beside this file, unmodified from that commit).

It is test data for the host-only tests in `security/crypto/tests/kat/`, and is not part of the
Origin OS runtime.

## What Origin OS changed

On 2026-11-08 `security/crypto/tools/convert_kat_vectors.py` converted the source file from JSON
to the line format described in the header of `ed25519.txt`, dropping each test's comment and the
DER, PEM and JWK forms of the public keys. All 210 tests are kept, and no value in them was
changed. The header names the source file and its SHA-256.
