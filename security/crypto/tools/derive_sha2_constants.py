#!/usr/bin/env python3
"""Derive the SHA-256 and SHA-512 constants from their FIPS 180-4 definitions.

FIPS 180-4 §4.2.2 / §4.2.3: the round constants are the first 32 (SHA-256) or 64 (SHA-512)
bits of the fractional parts of the cube roots of the first 64 / 80 primes.
§5.3.3 / §5.3.5: the initial hash values are the first 32 / 64 bits of the fractional parts
of the square roots of the first 8 primes.

Everything uses exact integer arithmetic, so there is no floating-point rounding:
the first n fractional bits of root_k(p) are floor(root_k(p * 2^(k*n))) mod 2^n.

Usage: python3 -I derive_sha2_constants.py > constants.txt
Host-only engineering tool; nothing here ships.
"""

from math import isqrt


def icbrt(n: int) -> int:
    """Largest x with x**3 <= n (exact, Newton iteration on integers)."""
    if n < 0:
        raise ValueError("negative")
    x = 1 << ((n.bit_length() + 2) // 3)  # x >= cbrt(n)
    while True:
        y = (2 * x + n // (x * x)) // 3
        if y >= x:
            break
        x = y
    while x * x * x > n:
        x -= 1
    while (x + 1) ** 3 <= n:
        x += 1
    return x


def primes(count: int) -> list[int]:
    found: list[int] = []
    n = 2
    while len(found) < count:
        if all(n % p for p in found if p * p <= n):
            found.append(n)
        n += 1
    return found


def frac_bits(root: int, p: int, bits: int) -> int:
    scaled = p << (root * bits)
    whole = isqrt(scaled) if root == 2 else icbrt(scaled)
    return whole & ((1 << bits) - 1)


def rust_array(name: str, ty: str, values: list[int], digits: int, per_line: int) -> str:
    lines = [f"pub(crate) const {name}: [{ty}; {len(values)}] = ["]
    for i in range(0, len(values), per_line):
        chunk = ", ".join(f"0x{v:0{digits}x}" for v in values[i : i + per_line])
        lines.append(f"    {chunk},")
    lines.append("];")
    return "\n".join(lines)


def main() -> None:
    p80 = primes(80)
    k256 = [frac_bits(3, p, 32) for p in p80[:64]]
    h256 = [frac_bits(2, p, 32) for p in p80[:8]]
    k512 = [frac_bits(3, p, 64) for p in p80]
    h512 = [frac_bits(2, p, 64) for p in p80[:8]]
    # Internal consistency: the 32-bit values are the top halves of the 64-bit ones.
    assert all((k512[i] >> 32) == k256[i] for i in range(64))
    assert all((h512[i] >> 32) == h256[i] for i in range(8))
    print(rust_array("H256", "u32", h256, 8, 4))
    print(rust_array("K256", "u32", k256, 8, 4))
    print(rust_array("H512", "u64", h512, 16, 2))
    print(rust_array("K512", "u64", k512, 16, 2))


if __name__ == "__main__":
    main()
