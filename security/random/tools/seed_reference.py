#!/usr/bin/env python3
"""An independent reference for how `origin-random` puts seeds together (ADR-0016 R5, R6).

It writes `security/random/src/testdata/seed_reference.txt`: what a generator must give after
a scripted start, a reseed, a reseed that falls back to RDRAND, a reseed from the root and a
request with prediction resistance. The crate's host tests run the same scripts through the
policy and compare.

The assembly here is written from the ADR's tables, not from the crate's code, and it runs on
the Python HMAC_DRBG of `security/crypto/tools/hmac_drbg_reference.py`, which NIST's vectors
and an installed OpenSSL confirmed (SOURCE_PROVENANCE.md). The scripted instructions give
SplitMix64 sequences, as `src/tests/world.rs` does.

    python3 -I security/random/tools/seed_reference.py            # writes the file
    python3 -I security/random/tools/seed_reference.py --check    # fails if the file differs

Development tool only; nothing here ships.
"""

import importlib.util
import pathlib
import struct
import sys

ROOT = pathlib.Path(__file__).resolve().parents[3]
OUT = ROOT / "security/random/src/testdata/seed_reference.txt"

spec = importlib.util.spec_from_file_location(
    "hmac_drbg_reference", ROOT / "security/crypto/tools/hmac_drbg_reference.py"
)
reference = importlib.util.module_from_spec(spec)
spec.loader.exec_module(reference)
HmacDrbg = reference.HmacDrbg

MASK = (1 << 64) - 1
TSC = 0x1122334455667788
ROOT_LABEL = b"Origin OS random root, 1"
PROCESSOR_LABEL = b"Origin OS random processor, 1"
RDSEED, RDRAND = 1, 2


def splitmix(seed, n):
    z = (seed + (n + 1) * 0x9E3779B97F4A7C15) & MASK
    z = ((z ^ (z >> 30)) * 0xBF58476D1CE4E5B9) & MASK
    z = ((z ^ (z >> 27)) * 0x94D049BB133111EB) & MASK
    return z ^ (z >> 31)


def le64(value):
    return struct.pack("<Q", value)


class Instruction:
    """One instruction of one processor: its sequence, handed out in order."""

    def __init__(self, cpu, which):
        self.seed = 0x4F524947494E0000 + cpu * 0x100 + which
        self.given = 0

    def draws(self, count):
        out = b"".join(le64(splitmix(self.seed, self.given + n)) for n in range(count))
        self.given += count
        return out


def root_of(entropy, nonce, extra=b""):
    """R5: a root's personalization string is a label, the time-stamp counter, and the extra
    bytes when there are any."""
    return HmacDrbg(entropy, nonce, ROOT_LABEL + le64(TSC) + extra)


def processor_of(root, index, own_draws):
    """R5: 64 bytes of the root's output, 16 further bytes of it, and a label, the processor's
    index, the time-stamp counter and the processor's own draws."""
    handed = root.generate(80, b"")
    return HmacDrbg(handed[:64], handed[64:], PROCESSOR_LABEL + le64(index) + le64(TSC) + own_draws)


def stamp(index):
    """R6: a reseed's additional input begins with the time-stamp counter and the index."""
    return le64(TSC) + le64(index)


def scenario_a():
    """Three processors: both instructions on two of them, neither on the third. The root is
    given 64 extra bytes."""
    s0, r0 = Instruction(0, RDSEED), Instruction(0, RDRAND)
    s1, r1 = Instruction(1, RDSEED), Instruction(1, RDRAND)
    extra = bytes(0x40 + i for i in range(64))
    root = root_of(s0.draws(1024), r0.draws(1024), extra)
    # The boot processor draws again for its own generator.
    cpu0 = processor_of(root, 0, s0.draws(1024) + r0.draws(1024))
    # Another processor's start-up draws are its own draws.
    cpu1 = processor_of(root, 1, s1.draws(1024) + r1.draws(1024))
    # A processor with no instruction has none.
    cpu2 = processor_of(root, 2, b"")
    yield "a.cpu0", cpu0.generate(64, b"")
    yield "a.cpu1", cpu1.generate(64, b"")
    yield "a.cpu2", cpu2.generate(64, b"")
    # Sixty seconds on: processor 0 reseeds from eight draws of its RDSEED. The root is due
    # as well (R6: the same schedule), and the processor that has just reseeded itself
    # reseeds it from eight more.
    cpu0.reseed(s0.draws(8), stamp(0))
    root.reseed(s0.draws(8), stamp(0))
    yield "a.cpu0.reseeded", cpu0.generate(64, b"")
    # Processor 2 has no instruction: it reseeds from 64 bytes of the root's output.
    cpu2.reseed(root.generate(64, b""), stamp(2))
    yield "a.cpu2.reseeded", cpu2.generate(64, b"")
    # Prediction resistance on processor 1: the root is reseeded from eight RDSEED draws of
    # that processor, all made inside the request, then generates.
    root.reseed(s1.draws(8), stamp(1))
    yield "a.fresh", root.generate(64, b"")


def scenario_one(name, which):
    """One processor that reports one instruction: the root's nonce is four further draws."""
    only = Instruction(0, which)
    root = root_of(only.draws(1024), only.draws(4))
    cpu0 = processor_of(root, 0, only.draws(1024))
    yield f"{name}.cpu0", cpu0.generate(64, b"")


def scenario_e():
    """A reseed that meets a dry RDSEED after three draws: eight RDRAND draws are the entropy
    input, and the three RDSEED draws go in as additional input."""
    s0, r0 = Instruction(0, RDSEED), Instruction(0, RDRAND)
    root = root_of(s0.draws(1024), r0.draws(1024))
    cpu0 = processor_of(root, 0, s0.draws(1024) + r0.draws(1024))
    aside = s0.draws(3)
    cpu0.reseed(r0.draws(8), stamp(0) + aside)
    yield "e.cpu0", cpu0.generate(64, b"")


def render():
    lines = [
        "# What origin-random's generators must give after scripted starts and reseeds.",
        "# Written by security/random/tools/seed_reference.py; do not edit by hand.",
        "# Lines: <name> <64 bytes of output, hex>",
    ]
    for scenario in (
        scenario_a(),
        scenario_one("b", RDSEED),
        scenario_one("c", RDRAND),
        scenario_e(),
    ):
        for name, value in scenario:
            lines.append(f"{name} {value.hex()}")
    return "\n".join(lines) + "\n"


def main():
    text = render()
    if sys.argv[1:] == ["--check"]:
        if OUT.read_text() != text:
            raise SystemExit(f"{OUT} differs from what this tool writes")
        print(f"{OUT.relative_to(ROOT)} is what this tool writes")
        return
    if sys.argv[1:]:
        raise SystemExit(__doc__)
    OUT.write_text(text)
    print(f"wrote {OUT.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
