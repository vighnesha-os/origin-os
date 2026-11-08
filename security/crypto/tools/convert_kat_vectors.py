#!/usr/bin/env python3
"""Converts upstream known-answer vectors into the line format `tests/kat` reads (CLAUDE.md §59).

Inputs are the NIST ACVP-Server `internalProjection.json` files and Wycheproof's
`ed25519_test.json`, at the pinned revisions and SHA-256 hashes in the tables below (owner
decision 19, 2026-11-08). A file whose hash differs is refused. The tool keeps the vectors that
apply to `origin-crypto`, counts the rest with the reason, and writes one text file per input.

Applicability (each exclusion is recorded in the output header and pinned by the Rust tests):
- SHA-3 and SHAKE messages that are not a whole number of bytes: `origin-crypto` has no
  bit-granular input, because everything Origin OS hashes is bytes.
- ML-DSA-44, HashML-DSA and the internal interface (Verify_internal, external mu): the crate has
  none of them; Origin OS verifies pure ML-DSA-65 and ML-DSA-87 only (CLAUDE.md §59).

SHAKE digests whose length is not a whole number of bytes ARE kept: squeezing whole bytes and
truncating is exactly what those vectors test. The two ACVP revisions write the partial last byte
differently (`tests/kat/hashes.rs`, `PartialByte`).

Usage: python3 -I convert_kat_vectors.py RAW_DIR OUT_DIR
  RAW_DIR  holds <ACVP set>/internalProjection.json and wycheproof/ed25519_test.json
  OUT_DIR  is security/crypto/tests/vectors

Standard library only. Host-only engineering tool: nothing here ships or runs in the build.
"""

import hashlib
import json
import sys
from pathlib import Path

# The date the conversion rules were last changed, so that re-running the tool reproduces the
# committed files byte for byte. NIST's notice asks for the date of any change.
CONVERTED = "2026-11-08"

ACVP_URL = "https://github.com/usnistgov/ACVP-Server"
ACVP_COMMIT = "975de31eb83d87039ec88934fdc47d8c312b892d"
ACVP_RELEASE = "v1.1.0.43"
WYCHEPROOF_URL = "https://github.com/C2SP/wycheproof"
WYCHEPROOF_COMMIT = "d17cc89fa5c8f4f4d378b5385ebf75da5d0926c9"

# (output name, ACVP set, SHA-256 of internalProjection.json, size in bytes)
FIXED_OUTPUT = [
    ("sha2-256", "SHA2-256-1.0",
     "a7e0bf4f5c661f1787b343bf3ae1a809b1e62f8fb2c8a627791510d03a570a6c", 1006446),
    ("sha2-512", "SHA2-512-1.0",
     "2b514c517a2ef150d240efc52fdb2c4127925f0b4291b5a5b9153515e0ec7478", 2020454),
    ("sha3-256", "SHA3-256-2.0",
     "dba4689436c7e440e61dc517210def3e8e09e50c0e9eff29ea66d89bdd0f2c63", 1217364),
    ("sha3-512", "SHA3-512-2.0",
     "b0521468a178f7fd362d63319aa69b708885d08f0c71cee99ac70729f5bdf64b", 1148507),
]
SHAKE = [
    ("shake128-1.0", "SHAKE-128-1.0",
     "ec2f9da6b146ae58fde2b73e552d3b29753fdfce7e05fbbb66329a075fa5d103", 1282487),
    ("shake128-fips202", "SHAKE-128-FIPS202",
     "1cbebb33db1f7e86ca265f89705dfac95cf90e8e0807b378e088af42cd0ae5db", 871436),
    ("shake256-1.0", "SHAKE-256-1.0",
     "94d0992ed2acbebe45f359cb1ffaceaf881a170339935ba9d2d36c8388005d26", 1319076),
    ("shake256-fips202", "SHAKE-256-FIPS202",
     "a9348d17e009cad62a2baa70160353f5bca936a27116f60dd5811f39b91c6991", 953531),
]
MLDSA = ("mldsa-sigver", "ML-DSA-sigVer-FIPS204",
         "47cdd6314c7f746d02421ffcba89d4dbc7bb875ac49e07a029fdfc26fba55437", 4533178)
WYCHEPROOF_ED25519 = ("ed25519_test.json",
                      "ea80ffebac085e21cb9d3e903e7fa2b448d96702da5b31f061d0b83b430b74b3", 172628)

MLDSA_REASONS = {
    "valid signature and message - signature should verify successfully": "valid",
    "modified message": "message",
    "modified signature - commitment": "commitment",
    "modified signature - z": "z",
    "modified signature - hint": "hint",
}


def load(path: Path, sha256: str, size: int):
    data = path.read_bytes()
    if len(data) != size or hashlib.sha256(data).hexdigest() != sha256:
        sys.exit(f"{path}: not the pinned file (want {size} bytes, SHA-256 {sha256})")
    return json.loads(data)


def lower(value: str) -> str:
    """Lower-case hex, with `-` for the empty string."""
    return value.lower() if value else "-"


def message_of(test: dict) -> str | None:
    """A byte-aligned message as hex, or None for a bit-oriented one."""
    if test["len"] % 8:
        return None
    assert len(test["msg"]) == test["len"] // 4, test["tcId"]
    return lower(test["msg"])


def acvp_header(name: str, acvp_set: str, sha256: str, size: int, doc: dict) -> list[str]:
    return [
        f"# NIST ACVP vectors: {doc['algorithm']}{' ' + doc['mode'] if 'mode' in doc else ''}, "
        f"revision {doc['revision']}, for origin-crypto's known-answer tests.",
        f"# Source: {ACVP_URL} commit {ACVP_COMMIT} ({ACVP_RELEASE}),",
        f"#   gen-val/json-files/{acvp_set}/internalProjection.json",
        f"#   SHA-256 {sha256}, {size} bytes, vsId {doc['vsId']}, isSample {doc['isSample']}.",
        f"# Changed by Origin OS on {CONVERTED}: converted from JSON to these lines by",
        "#   security/crypto/tools/convert_kat_vectors.py, keeping only the vectors that apply to",
        "#   origin-crypto; hex is lower-case and the empty string is `-`. Acknowledgement to NIST",
        "#   as the source, and NIST's notice: NOTICE.md beside this file.",
        "# Host-only test data; never part of the shipping runtime (CLAUDE.md §5).",
        "#",
    ]


def convert_fixed(name, acvp_set, sha256, size, raw, out):
    doc = load(raw / acvp_set / "internalProjection.json", sha256, size)
    lines, kept, excluded = [], {"aft": 0, "mct": 0, "ldt": 0}, 0
    for group in doc["testGroups"]:
        for test in group["tests"]:
            kind = group["testType"]
            tc = test["tcId"]
            if kind == "AFT":
                message = message_of(test)
                if message is None:
                    excluded += 1
                    continue
                lines.append(f"aft {tc} {message} {lower(test['md'])}")
                kept["aft"] += 1
            elif kind == "MCT":
                assert len(test["resultsArray"]) == 100, tc
                assert test["len"] % 8 == 0 and test["len"] // 4 == len(test["msg"]), tc
                lines.append(f"mct {tc} {group['mctVersion']} {lower(test['msg'])}")
                lines += [f"res {lower(result['md'])}" for result in test["resultsArray"]]
                kept["mct"] += 1
            elif kind == "LDT":
                large = test["largeMsg"]
                assert large["expansionTechnique"] == "repeating", tc
                assert large["contentLength"] == len(large["content"]) * 4, tc
                assert large["fullLength"] % 8 == 0, tc
                lines.append(
                    f"ldt {tc} {lower(large['content'])} {large['fullLength'] // 8} "
                    f"{lower(test['md'])}"
                )
                kept["ldt"] += 1
            else:
                sys.exit(f"{name}: unknown test type {kind}")
    header = acvp_header(name, acvp_set, sha256, size, doc) + [
        f"# Kept: {kept['aft']} AFT, {kept['mct']} MCT, {kept['ldt']} LDT. "
        f"Excluded: {excluded} AFT cases whose message is not a whole number of bytes.",
        "# Lines: aft <tcId> <message> <digest>",
        "#        mct <tcId> <alternate|standard> <seed>, then 100 of: res <digest>",
        "#        ldt <tcId> <content> <total bytes of repeated content> <digest>",
    ]
    write(out / "acvp" / f"{name}.txt", header + lines)
    print(f"{name}: kept {kept}, excluded {excluded}")


def convert_shake(name, acvp_set, sha256, size, raw, out):
    doc = load(raw / acvp_set / "internalProjection.json", sha256, size)
    lines, kept, excluded = [], {"aft": 0, "vot": 0, "mct": 0}, 0
    for group in doc["testGroups"]:
        for test in group["tests"]:
            kind = group["testType"]
            tc = test["tcId"]
            if kind in ("AFT", "VOT"):
                message = message_of(test)
                if message is None:
                    excluded += 1
                    continue
                bits = test["outLen"]
                assert len(test["md"]) == 2 * ((bits + 7) // 8), tc
                lines.append(f"{kind.lower()} {tc} {message} {bits} {lower(test['md'])}")
                kept[kind.lower()] += 1
            elif kind == "MCT":
                assert len(test["resultsArray"]) == 100, tc
                assert test["len"] == 128 and len(test["msg"]) == 32, tc
                assert group["minOutLen"] % 8 == 0 and group["maxOutLen"] % 8 == 0, tc
                lines.append(
                    f"mct {tc} {lower(test['msg'])} {group['minOutLen']} {group['maxOutLen']}"
                )
                for result in test["resultsArray"]:
                    assert result["outLen"] % 8 == 0, tc
                    lines.append(f"res {result['outLen']} {lower(result['md'])}")
                kept["mct"] += 1
            else:
                sys.exit(f"{name}: unknown test type {kind}")
    header = acvp_header(name, acvp_set, sha256, size, doc) + [
        f"# Kept: {kept['aft']} AFT, {kept['vot']} VOT, {kept['mct']} MCT. "
        f"Excluded: {excluded} cases whose message is not a whole number of bytes.",
        "# Lines: aft <tcId> <message> <output bits> <digest>",
        "#        vot <tcId> <message> <output bits> <digest>",
        "#        mct <tcId> <seed> <min output bits> <max output bits>,",
        "#          then 100 of: res <output bits> <digest>",
    ]
    write(out / "acvp" / f"{name}.txt", header + lines)
    print(f"{name}: kept {kept}, excluded {excluded}")


def convert_mldsa(name, acvp_set, sha256, size, raw, out):
    doc = load(raw / acvp_set / "internalProjection.json", sha256, size)
    lines, kept = [], {"65": 0, "87": 0}
    excluded = {"ML-DSA-44": 0, "HashML-DSA": 0, "internal interface": 0}
    for group in doc["testGroups"]:
        tests = group["tests"]
        if group["parameterSet"] == "ML-DSA-44":
            excluded["ML-DSA-44"] += len(tests)
        elif group["signatureInterface"] == "internal":
            excluded["internal interface"] += len(tests)
        elif group["preHash"] != "pure":
            assert group["preHash"] == "preHash"
            excluded["HashML-DSA"] += len(tests)
        else:
            assert group["signatureInterface"] == "external" and not group["externalMu"]
            digits = group["parameterSet"].removeprefix("ML-DSA-")
            assert digits in kept
            for test in tests:
                assert test["hashAlg"] == "none" and not test["deferred"], test["tcId"]
                case = MLDSA_REASONS[test["reason"]]
                assert test["testPassed"] == (case == "valid"), test["tcId"]
                lines.append(
                    f"mldsa {test['tcId']} {digits} {case} {lower(test['pk'])} "
                    f"{lower(test['message'])} {lower(test['context'])} {lower(test['signature'])}"
                )
                kept[digits] += 1
    header = acvp_header(name, acvp_set, sha256, size, doc) + [
        f"# Kept: {kept['65']} ML-DSA-65 and {kept['87']} ML-DSA-87 external, pure signature "
        "verification vectors (FIPS 204 Algorithm 3).",
        f"# Excluded: {excluded['ML-DSA-44']} ML-DSA-44, {excluded['HashML-DSA']} HashML-DSA and "
        f"{excluded['internal interface']} internal-interface vectors, none of which the crate "
        "provides.",
        "# Each line's case is the vector's `reason`: valid, message, commitment, z or hint.",
        "# Lines: mldsa <tcId> <65|87> <case> <public key> <message> <context> <signature>",
    ]
    write(out / "acvp" / f"{name}.txt", header + lines)
    print(f"{name}: kept {kept}, excluded {excluded}")


def convert_wycheproof(raw, out):
    file_name, sha256, size = WYCHEPROOF_ED25519
    doc = load(raw / "wycheproof" / file_name, sha256, size)
    assert doc["algorithm"] == "EDDSA"
    lines = []
    for group in doc["testGroups"]:
        key = group["publicKey"]
        assert key["curve"] == "edwards25519" and key["keySize"] == 255
        for test in group["tests"]:
            assert len(test["flags"]) == 1, test["tcId"]
            lines.append(
                f"ed {test['tcId']} {test['result']} {test['flags'][0]} {lower(key['pk'])} "
                f"{lower(test['msg'])} {lower(test['sig'])}"
            )
    assert len(lines) == doc["numberOfTests"]
    header = [
        "# Wycheproof vectors: Ed25519 signature verification, for origin-crypto's known-answer",
        "# tests.",
        f"# Source: {WYCHEPROOF_URL} commit {WYCHEPROOF_COMMIT},",
        f"#   testvectors_v1/{file_name}",
        f"#   SHA-256 {sha256}, {size} bytes.",
        f"# Modified by Origin OS on {CONVERTED}: converted from JSON to these lines by",
        "#   security/crypto/tools/convert_kat_vectors.py, dropping each test's comment and the",
        "#   DER, PEM and JWK forms of the keys; every test is kept. Apache License 2.0: see",
        "#   LICENSE beside this file.",
        "# Host-only test data; never part of the shipping runtime (CLAUDE.md §5).",
        "#",
        f"# Kept: {len(lines)} of {doc['numberOfTests']}.",
        "# Lines: ed <tcId> <valid|invalid|acceptable> <flag> <public key> <message> <signature>",
    ]
    write(out / "wycheproof" / "ed25519.txt", header + lines)
    print(f"ed25519: kept {len(lines)}")


def write(path: Path, lines: list[str]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def main() -> None:
    if len(sys.argv) != 3:
        sys.exit(f"usage: {sys.argv[0]} RAW_DIR OUT_DIR")
    raw, out = Path(sys.argv[1]), Path(sys.argv[2])
    for entry in FIXED_OUTPUT:
        convert_fixed(*entry, raw, out)
    for entry in SHAKE:
        convert_shake(*entry, raw, out)
    convert_mldsa(*MLDSA, raw, out)
    convert_wycheproof(raw, out)


if __name__ == "__main__":
    main()
