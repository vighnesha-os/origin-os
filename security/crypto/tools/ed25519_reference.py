#!/usr/bin/env python3
"""Independent pure-Python Ed25519 (RFC 8032 §5.1, the pure variant) for origin-crypto's tests.

Written from the RFC 8032 math with Python integers. Points are affine (x, y) and use the
twisted Edwards addition law with modular inverses, so this shares no formulas with the
Rust code (extended coordinates, 51-bit limbs, a joint double-scalar loop). Square roots
use Euler's criterion and (x^2)^((p+3)/8) on an explicit quotient, not the Rust code's
combined u*v^3*(u*v^7)^((p-5)/8) form. SHA-512 is hashlib's.

Strict verification, in this order; the Rust verifier must give the same result:
  1. A (public key): y >= p                        -> PublicKeyNonCanonical
                     no x for this y (no root)     -> PublicKeyNotOnCurve
                     x = 0 and the sign bit set    -> PublicKeyNonCanonical
                     [8]A = identity               -> PublicKeySmallOrder
  2. R: the same four checks                       -> RNonCanonical / RNotOnCurve / RSmallOrder
  3. S >= L                                        -> SNonCanonical
  4. k = SHA-512(R || A || M) mod L; cofactorless [S]B != R + [k]A -> EquationFailed

Usage: python3 -I ed25519_reference.py <command>
  constants  print the Rust constants module, src/ed25519/constants.rs
  selftest   reproduce RFC 8032 §7.1 TEST 1, 2, 3 and TEST SHA(abc); exit 1 on a mismatch
  vectors    print tests/vectors/ed25519-differential.txt
  negative   print tests/vectors/ed25519-negative.txt
Host-only engineering tool; nothing here ships.
"""

import hashlib
import sys

# ---------------------------------------------------------------------------------------
# Parameters, from their definitions (RFC 8032 §5.1)

P = 2**255 - 19
L = 2**252 + 27742317777372353535851937790883648493


def inverse(a: int) -> int:
    return pow(a, -1, P)


D = -121665 * inverse(121666) % P
SQRT_M1 = pow(2, (P - 1) // 4, P)
IDENTITY = (0, 1)


def on_curve(pt: tuple[int, int]) -> bool:
    x, y = pt
    return (-x * x + y * y - 1 - D * x * x * y * y) % P == 0


def recover_x(y: int, sign: int) -> int | None:
    """The x with x^2 = (y^2 - 1) / (d y^2 + 1) and parity `sign`, or None if there is none.

    For 0 <= y < p. The x = 0 case returns 0 whatever `sign` is; the caller rejects it.
    """
    x2 = (y * y - 1) * inverse(D * y * y + 1) % P
    if x2 == 0:
        return 0
    if pow(x2, (P - 1) // 2, P) != 1:  # Euler's criterion: x2 is not a square
        return None
    x = pow(x2, (P + 3) // 8, P)
    if (x * x - x2) % P != 0:
        x = x * SQRT_M1 % P
    assert (x * x - x2) % P == 0
    if x & 1 != sign:
        x = P - x
    return x


BY = 4 * inverse(5) % P
BX = recover_x(BY, 0)
B = (BX, BY)


def add(p1: tuple[int, int], p2: tuple[int, int]) -> tuple[int, int]:
    """The twisted Edwards addition law for a = -1. It is complete: d is not a square, so
    neither denominator is zero for points on the curve."""
    (x1, y1), (x2, y2) = p1, p2
    t = D * x1 * x2 * y1 * y2 % P
    return ((x1 * y2 + y1 * x2) * inverse(1 + t) % P, (y1 * y2 + x1 * x2) * inverse(1 - t) % P)


def mul(n: int, pt: tuple[int, int]) -> tuple[int, int]:
    acc = IDENTITY
    for bit in bin(n)[2:]:
        acc = add(acc, acc)
        if bit == "1":
            acc = add(acc, pt)
    return acc


def neg(pt: tuple[int, int]) -> tuple[int, int]:
    return ((-pt[0]) % P, pt[1])


def encode(pt: tuple[int, int]) -> bytes:
    x, y = pt
    return (y | (x & 1) << 255).to_bytes(32, "little")


def encode_raw(y: int, sign: int) -> bytes:
    """Any 255-bit y with a sign bit, canonical or not."""
    return (y | sign << 255).to_bytes(32, "little")


def decode(b: bytes) -> tuple[tuple[int, int] | None, str | None]:
    """RFC 8032 §5.1.3 with strict checks. Returns (point, None) or (None, error kind)."""
    n = int.from_bytes(b, "little")
    y, sign = n & ((1 << 255) - 1), n >> 255
    if y >= P:
        return None, "NonCanonical"
    x = recover_x(y, sign)
    if x is None:
        return None, "NotOnCurve"
    if x == 0 and sign == 1:
        return None, "NonCanonical"
    assert on_curve((x, y))
    return (x, y), None


def small_order(pt: tuple[int, int]) -> bool:
    return mul(8, pt) == IDENTITY


def sha512_int(data: bytes) -> int:
    return int.from_bytes(hashlib.sha512(data).digest(), "little")


# ---------------------------------------------------------------------------------------
# Key generation, signing and verification (RFC 8032 §5.1.5-§5.1.7)


def secret_scalar(seed: bytes) -> tuple[int, bytes]:
    h = hashlib.sha512(seed).digest()
    a = int.from_bytes(h[:32], "little")
    a &= (1 << 254) - 8  # clear bits 0-2 and 254-255
    a |= 1 << 254
    return a, h[32:]


def public_key(seed: bytes) -> bytes:
    return encode(mul(secret_scalar(seed)[0], B))


def sign_with_scalar(a: int, prefix: bytes, pk: bytes, msg: bytes) -> bytes:
    r = sha512_int(prefix + msg) % L
    big_r = encode(mul(r, B))
    k = sha512_int(big_r + pk + msg) % L
    s = (r + k * a) % L
    return big_r + s.to_bytes(32, "little")


def sign(seed: bytes, msg: bytes) -> tuple[bytes, bytes]:
    a, prefix = secret_scalar(seed)
    pk = encode(mul(a, B))
    return pk, sign_with_scalar(a, prefix, pk, msg)


def verify(pk: bytes, msg: bytes, sig: bytes) -> str:
    """Strict verification. Returns "ok" or the error name the Rust verifier must return."""
    assert len(pk) == 32 and len(sig) == 64
    a, err = decode(pk)
    if err:
        return "PublicKey" + err
    if small_order(a):
        return "PublicKeySmallOrder"
    r, err = decode(sig[:32])
    if err:
        return "R" + err
    if small_order(r):
        return "RSmallOrder"
    s = int.from_bytes(sig[32:], "little")
    if s >= L:
        return "SNonCanonical"
    k = sha512_int(sig[:32] + pk + msg) % L
    if mul(s, B) != add(r, mul(k, a)):
        return "EquationFailed"
    return "ok"


# ---------------------------------------------------------------------------------------
# Self-checks of the parameters


def probably_prime(n: int) -> bool:
    """Miller-Rabin with the first 20 prime bases (a sanity check, not a proof)."""
    bases = [2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47, 53, 59, 61, 67, 71]
    d, s = n - 1, 0
    while d % 2 == 0:
        d, s = d // 2, s + 1
    for a in bases:
        x = pow(a, d, n)
        if x in (1, n - 1):
            continue
        for _ in range(s - 1):
            x = x * x % n
            if x == n - 1:
                break
        else:
            return False
    return True


def check_parameters() -> None:
    assert probably_prime(P) and probably_prime(L)
    assert SQRT_M1 * SQRT_M1 % P == P - 1
    assert pow(D, (P - 1) // 2, P) == P - 1, "d must be a non-square (complete addition law)"
    assert BX is not None and BX % 2 == 0 and on_curve(B)
    assert mul(L, B) == IDENTITY and B != IDENTITY
    assert encode(B) == bytes([0x58]) + bytes([0x66]) * 31


def small_order_points() -> list[tuple[int, int]]:
    """All 8 points of order dividing 8, from the group structure.

    The curve group has order 8L (checked below on several points), so [L]Q lies in the
    8-torsion subgroup for every point Q. A point T of order exactly 8 generates that
    subgroup (its order-8 cyclic group has the full size of the 2-part), so the small-order
    points are exactly [j]T for j = 0..7.
    """
    found = None
    y = 2
    while found is None:
        x = recover_x(y, 0)
        if x is not None:
            q = (x, y)
            assert mul(8 * L, q) == IDENTITY
            t = mul(L, q)
            if mul(4, t) != IDENTITY:
                found = t
        y += 1
    pts = [mul(j, found) for j in range(8)]
    assert len(set(pts)) == 8
    orders = []
    for pt in pts:
        assert on_curve(pt)
        order = next(n for n in (1, 2, 4, 8) if mul(n, pt) == IDENTITY)
        orders.append(order)
    assert sorted(orders) == [1, 2, 4, 4, 8, 8, 8, 8]
    # The analytic ones: (0, 1), (0, -1) and (+-sqrt(-1), 0).
    assert {(0, 1), (0, P - 1), (SQRT_M1, 0), (P - SQRT_M1, 0)} <= set(pts)
    return sorted(pts, key=lambda pt: (orders[pts.index(pt)], encode(pt)))


# ---------------------------------------------------------------------------------------
# Commands


def limbs51(v: int) -> list[int]:
    return [(v >> (51 * i)) & ((1 << 51) - 1) for i in range(5)]


def rust_array(name: str, ty: str, values: list[int], wrap: str = "", doc: str = "") -> str:
    lines = [f"/// {line}" for line in doc.splitlines()] if doc else []
    open_, close = (f"{wrap}([", "]);") if wrap else ("[", "];")
    lines.append(f"pub(crate) const {name}: {wrap or ty} = {open_}")
    lines += [f"    0x{v:x}," for v in values]
    lines.append(close)
    return "\n".join(lines)


def cmd_constants() -> None:
    check_parameters()
    four_p = [4 * limb for limb in limbs51(P)]
    assert sum(limb << (51 * i) for i, limb in enumerate(four_p)) == 4 * P
    out = [
        "//! Ed25519 constants (RFC 8032 §5.1).",
        "//!",
        "//! Generated by `python3 -I security/crypto/tools/ed25519_reference.py constants`,",
        "//! which computes each value from its definition with Python integers and checks it",
        "//! (p and L prime, d a non-square, B on the curve with order L, sqrt(-1)^2 = -1). Do",
        "//! not edit by hand; regenerate and compare. `src/tests_ed25519.rs` re-derives every",
        "//! value in Rust.",
        "",
        "use super::field::Fe;",
        "",
        rust_array("D", "", limbs51(D), "Fe", "d = -121665 / 121666 mod p."),
        rust_array("D2", "", limbs51(2 * D % P), "Fe", "2d mod p, for the addition law."),
        rust_array("SQRT_M1", "", limbs51(SQRT_M1), "Fe", "sqrt(-1) = 2^((p - 1) / 4) mod p."),
        rust_array(
            "BASE_X", "", limbs51(BX), "Fe",
            f"The base point B: the even x for y = 4/5. B encodes as\n{encode(B).hex()}.",
        ),
        rust_array("BASE_Y", "", limbs51(BY), "Fe", "y = 4/5 mod p."),
        rust_array("BASE_T", "", limbs51(BX * BY % P), "Fe", "x * y mod p (extended T)."),
        rust_array(
            "FOUR_P", "[u64; 5]", four_p, "",
            "4p as unreduced 51-bit limbs; `Fe::sub` adds it so no limb underflows.",
        ),
        rust_array(
            "L", "[u64; 4]", [(L >> (64 * i)) & (2**64 - 1) for i in range(4)], "",
            "L = 2^252 + 27742317777372353535851937790883648493, little-endian 64-bit words.",
        ),
    ]
    print("\n".join(out))


# Candidate values for RFC 8032 §7.1. Each one is embedded in the Rust tests only if this
# reference reproduces its public key and signature exactly from the seed and message.
RFC8032_VECTORS = [
    (
        "TEST 1",
        "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
        "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
        "",
        "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
    ),
    (
        "TEST 2",
        "4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb",
        "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c",
        "72",
        "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00",
    ),
    (
        "TEST 3",
        "c5aa8df43f9f837bedb7442f31dcb7b166d38535076f094b85ce3a2e0b4458f7",
        "fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025",
        "af82",
        "6291d657deec24024827e69c3abe01a30ce548a284743a445e3680d7db5ac3ac18ff9b538d16f290ae67f760984dc6594a7c15e9716ed28dc027beceea1ec40a",
    ),
    (
        "TEST SHA(abc)",
        "833fe62409237b9d62ec77587520911e9a759cec1d19755b7da901b96dca3d42",
        "ec172b93ad5e563bf4932c70e1245034c35467ef2efd4d64ebf819683467e2bf",
        hashlib.sha512(b"abc").hexdigest(),
        "dc2a4459e7369633a52b1bf277839a00201009a3efbf3ecb69bea2186c26b58909351fc9ac90b3ecfdfbc7c66431e0303dca179c138ac17ad9bef1177331a704",
    ),
]


def cmd_selftest() -> None:
    check_parameters()
    failed = 0
    for name, seed, want_pk, msg, want_sig in RFC8032_VECTORS:
        pk, sig = sign(bytes.fromhex(seed), bytes.fromhex(msg))
        ok_pk, ok_sig = pk.hex() == want_pk, sig.hex() == want_sig
        status = verify(pk, bytes.fromhex(msg), sig)
        good = ok_pk and ok_sig and status == "ok"
        failed += not good
        print(f"{name}: public key {'match' if ok_pk else 'MISMATCH'}, "
              f"signature {'match' if ok_sig else 'MISMATCH'}, verify {status}")
        if not good:
            print(f"  got pk  {pk.hex()}\n  got sig {sig.hex()}")
    pts = small_order_points()
    print("small-order points (canonical encodings):")
    for pt in pts:
        order = next(n for n in (1, 2, 4, 8) if mul(n, pt) == IDENTITY)
        print(f"  order {order}: {encode(pt).hex()}")
    sys.exit(1 if failed else 0)


def stream(label: str, n: int) -> bytes:
    """n deterministic pseudo-random bytes: SHA-512 in counter mode over a label."""
    out = b""
    counter = 0
    while len(out) < n:
        out += hashlib.sha512(f"origin-crypto/ed25519/{label}/{counter}".encode()).digest()
        counter += 1
    return out[:n]


LENGTHS = list(range(256)) + [
    256, 257, 300, 319, 320, 321, 383, 384, 385, 447, 448, 449, 511, 512, 513,
    600, 639, 640, 641, 767, 768, 769, 895, 896, 897, 1000, 1023, 1024, 1025,
]  # fmt: skip


def differential_vectors() -> list[tuple[bytes, bytes, bytes]]:
    """300 (pk, msg, sig): 285 keys over lengths 0-1025, then one key signing 15 messages."""
    vectors = []
    for i, n in enumerate(LENGTHS):
        seed = stream(f"seed/{i}", 32)
        msg = stream(f"msg/{i}", n)
        pk, sig = sign(seed, msg)
        vectors.append((pk, msg, sig))
    shared = stream("seed/shared", 32)
    special = [b"", b"\x00", b"\x00" * 64, b"\xff" * 64, b"\x00" * 128, b"\xff" * 128, b"abc",
               b"Origin OS", stream("shared/0", 47), stream("shared/1", 48),
               stream("shared/2", 111), stream("shared/3", 112), stream("shared/4", 175),
               stream("shared/5", 176), stream("shared/6", 4096)]
    for msg in special:
        pk, sig = sign(shared, msg)
        vectors.append((pk, msg, sig))
    assert len(vectors) == 300
    for pk, msg, sig in vectors:
        assert verify(pk, msg, sig) == "ok"
    return vectors


def hexmsg(msg: bytes) -> str:
    return msg.hex() if msg else "-"


def cmd_signvectors() -> None:
    """Vectors for the host signer (`developer-tools/onbf-sign`): the seed of every key, so a
    signer can reproduce each public key and signature from it. The RFC's vectors come first."""
    check_parameters()
    print("# Ed25519 signing vectors for developer-tools/onbf-sign. Generated by")
    print("# `python3 -I security/crypto/tools/ed25519_reference.py signvectors`: an independent")
    print("# pure-Python RFC 8032 implementation signed each message and verified it strictly.")
    print("# The four RFC 8032 section 7.1 vectors come first, then the same seeds and messages")
    print("# as ed25519-differential.txt, and the shared key.")
    print("# Columns: seed public_key message(hex, '-' if empty) signature")
    for _, seed, want_pk, msg, want_sig in RFC8032_VECTORS:
        pk, sig = sign(bytes.fromhex(seed), bytes.fromhex(msg))
        assert (pk.hex(), sig.hex()) == (want_pk, want_sig)
        print(seed, want_pk, msg if msg else "-", want_sig)
    for i, n in enumerate(LENGTHS):
        seed = stream(f"seed/{i}", 32)
        msg = stream(f"msg/{i}", n)
        pk, sig = sign(seed, msg)
        assert verify(pk, msg, sig) == "ok"
        print(seed.hex(), pk.hex(), hexmsg(msg), sig.hex())
    shared = stream("seed/shared", 32)
    for msg in [b"", b"\x00", b"\x00" * 64, b"\xff" * 64, b"abc", b"Origin OS",
                stream("shared/0", 47), stream("shared/1", 48), stream("shared/2", 111),
                stream("shared/3", 112), stream("shared/6", 4096)]:
        pk, sig = sign(shared, msg)
        assert verify(pk, msg, sig) == "ok"
        print(shared.hex(), pk.hex(), hexmsg(msg), sig.hex())


def cmd_vectors() -> None:
    check_parameters()
    print("# Ed25519 differential vectors for origin-crypto. Generated by")
    print("# `python3 -I security/crypto/tools/ed25519_reference.py vectors`: an independent")
    print("# pure-Python RFC 8032 implementation signed each message and verified it strictly.")
    print("# Seeds and messages are SHA-512 counter-mode streams; lines 1-285 use one key each")
    print("# (message lengths 0-255 and 256-1025), lines 286-300 share one key.")
    print("# Columns: public_key message(hex, '-' if empty) signature")
    for pk, msg, sig in differential_vectors():
        print(pk.hex(), hexmsg(msg), sig.hex())


def cmd_negative() -> None:
    check_parameters()
    vectors = differential_vectors()
    lines: list[str] = []

    def raw(label: str, pk: bytes, msg: bytes, sig: bytes, expect: str | None = None) -> None:
        got = verify(pk, msg, sig)
        if expect is not None:
            assert got == expect, f"{label}: expected {expect}, reference says {got}"
        lines.append(f"raw {got} {pk.hex()} {hexmsg(msg)} {sig.hex()} {label}")

    def flip(data: bytes, bit: int) -> bytes:
        b = bytearray(data)
        b[bit // 8] ^= 1 << (bit % 8)
        return bytes(b)

    # 1. Single-bit flips of two valid signatures (indices into the differential file).
    for base in (1, 100):
        pk, msg, sig = vectors[base]
        fields = {
            "R": (256, lambda bit: (pk, msg, flip(sig[:32], bit) + sig[32:])),
            "S": (256, lambda bit: (pk, msg, sig[:32] + flip(sig[32:], bit))),
            "A": (256, lambda bit: (flip(pk, bit), msg, sig)),
        }
        msg_bits = range(8 * len(msg)) if len(msg) <= 8 else [
            *range(32), *range(8 * len(msg) - 32, 8 * len(msg))]
        for field, (count, mutate) in fields.items():
            for bit in range(count):
                got = verify(*mutate(bit))
                assert got != "ok"
                lines.append(f"mut {base} {field} {bit} {got}")
        for bit in msg_bits:
            got = verify(pk, flip(msg, bit), sig)
            assert got == "EquationFailed"
            lines.append(f"mut {base} M {bit} {got}")

    pk, msg, sig = vectors[7]
    r, s = sig[:32], int.from_bytes(sig[32:], "little")

    # 2. Non-canonical scalars. S + L is the classic malleated signature: it would pass an
    #    unreduced equation. A signature whose S is exactly L or L + 1 and whose reduced S
    #    satisfies the equation would need a SHA-512 preimage, so those use a valid R and A.
    for base in (0, 7, 64, 255, 299):
        bpk, bmsg, bsig = vectors[base]
        bs = int.from_bytes(bsig[32:], "little")
        raw(f"S+L(vector-{base})", bpk, bmsg, bsig[:32] + (bs + L).to_bytes(32, "little"),
            "SNonCanonical")
    raw("S+2L", pk, msg, r + (s + 2 * L).to_bytes(32, "little"), "SNonCanonical")
    for label, value in [("S=L", L), ("S=L+1", L + 1), ("S=2^256-1", 2**256 - 1),
                         ("S=2^253", 2**253)]:
        raw(label, pk, msg, r + value.to_bytes(32, "little"), "SNonCanonical")
    for label, value in [("S=L-1", L - 1), ("S=0", 0), ("S=1", 1)]:
        raw(label, pk, msg, r + value.to_bytes(32, "little"), "EquationFailed")

    # 3. Non-canonical y = p + j (j = 0..18 is every 255-bit value >= p), both sign bits.
    for j in range(19):
        for sign_bit in (0, 1):
            enc = encode_raw(P + j, sign_bit)
            raw(f"A:y=p+{j},sign={sign_bit}", enc, msg, sig, "PublicKeyNonCanonical")
            raw(f"R:y=p+{j},sign={sign_bit}", pk, msg, enc + sig[32:], "RNonCanonical")

    # 4. x = 0 with the sign bit set: y = 1 (identity) and y = p - 1 (order 2).
    for label, y in [("y=1", 1), ("y=p-1", P - 1)]:
        enc = encode_raw(y, 1)
        raw(f"A:x=0,sign=1,{label}", enc, msg, sig, "PublicKeyNonCanonical")
        raw(f"R:x=0,sign=1,{label}", pk, msg, enc + sig[32:], "RNonCanonical")

    # 5. y values with no point on the curve: the first eight from y = 2, both sign bits,
    #    plus the largest one below p.
    off_curve = []
    y = 2
    while len(off_curve) < 8:
        if recover_x(y, 0) is None:
            off_curve.append(y)
        y += 1
    y = P - 1
    while recover_x(y, 0) is not None:
        y -= 1
    off_curve.append(y)
    for y in off_curve:
        for sign_bit in (0, 1):
            enc = encode_raw(y, sign_bit)
            tag = f"y={y}" if y < 2**16 else f"y=p-{P - y}"
            raw(f"A:off-curve,{tag},sign={sign_bit}", enc, msg, sig, "PublicKeyNotOnCurve")
            raw(f"R:off-curve,{tag},sign={sign_bit}", pk, msg, enc + sig[32:], "RNotOnCurve")

    # 6. The 8 small-order points as A and as R.
    for pt in small_order_points():
        order = next(n for n in (1, 2, 4, 8) if mul(n, pt) == IDENTITY)
        enc = encode(pt)
        assert decode(enc) == (pt, None)
        raw(f"A:small-order({order})", enc, msg, sig, "PublicKeySmallOrder")
        raw(f"R:small-order({order})", pk, msg, enc + sig[32:], "RSmallOrder")

    # 7. Check order: the first failing check in the documented order wins.
    t8 = next(pt for pt in small_order_points()
              if mul(4, pt) != IDENTITY)  # an order-8 point
    nc = encode_raw(P + 1, 0)
    raw("order:A-noncanonical+R-noncanonical", nc, msg, nc + sig[32:], "PublicKeyNonCanonical")
    raw("order:A-small+S>=L", encode(t8), msg, r + L.to_bytes(32, "little"),
        "PublicKeySmallOrder")
    raw("order:R-small+S>=L", pk, msg, encode(t8) + L.to_bytes(32, "little"), "RSmallOrder")
    raw("order:R-off-curve+S>=L", pk, msg,
        encode_raw(off_curve[0], 0) + L.to_bytes(32, "little"), "RNotOnCurve")

    # 8. Other equation failures: wrong message, wrong key, swapped R.
    pk2, msg2, sig2 = vectors[8]
    raw("message+0x00", pk, msg + b"\x00", sig, "EquationFailed")
    raw("message-truncated", pk, msg[:-1], sig, "EquationFailed")
    raw("other-key", pk2, msg, sig, "EquationFailed")
    raw("other-R", pk, msg, sig2[:32] + sig[32:], "EquationFailed")
    raw("negated-R", pk, msg, encode(neg(decode(r)[0])) + sig[32:], "EquationFailed")
    raw("negated-A", encode(neg(decode(pk)[0])), msg, sig, "EquationFailed")

    # 9. A mixed-order key A' = [a]B + T (T of order 8): not small order, so it is decoded
    #    and accepted as a key. With S = r + k*a, the cofactorless equation holds exactly
    #    when [k]T = 0, that is when 8 divides k; a cofactored verifier accepts both.
    a, prefix = secret_scalar(stream("seed/mixed", 32))
    mixed = encode(add(mul(a, B), t8))
    want = {"EquationFailed": None, "ok": None}
    i = 0
    while None in want.values():
        m = stream(f"mixed/{i}", 16)
        signed = sign_with_scalar(a, prefix, mixed, m)
        k = sha512_int(signed[:32] + mixed + m) % L
        key = "ok" if k % 8 == 0 else "EquationFailed"
        if want[key] is None:
            want[key] = (m, signed, k % 8)
        i += 1
    for key, (m, signed, k8) in want.items():
        raw(f"mixed-order-key,k%8={k8}", mixed, m, signed, key)

    print("# Ed25519 negative corpus for origin-crypto. Generated by")
    print("# `python3 -I security/crypto/tools/ed25519_reference.py negative`; each expected")
    print("# result is the strict verdict of the independent pure-Python reference.")
    print("#   mut <vector> <field> <bit> <expected>: flip one bit of differential vector")
    print("#       <vector> (0-based line among the data lines); field R or S (signature")
    print("#       halves), A (public key) or M (message); bits are little-endian per byte.")
    print("#   raw <expected> <public_key> <message|-> <signature> <label>")
    for line in lines:
        print(line)


def main() -> None:
    commands = {"constants": cmd_constants, "selftest": cmd_selftest,
                "vectors": cmd_vectors, "signvectors": cmd_signvectors, "negative": cmd_negative}
    if len(sys.argv) != 2 or sys.argv[1] not in commands:
        sys.exit(f"usage: {sys.argv[0]} {{{','.join(commands)}}}")
    commands[sys.argv[1]]()


if __name__ == "__main__":
    main()
