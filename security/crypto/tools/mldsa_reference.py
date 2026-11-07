#!/usr/bin/env python3
"""Independent pure-Python ML-DSA (FIPS 204, pure ML-DSA-65 and ML-DSA-87) for origin-crypto's tests.

Written from FIPS 204 (August 2024) with Python integers and the standard library's
hashlib.shake_128 / shake_256 (H = SHAKE256, G = SHAKE128). KeyGen, Sign (deterministic and
hedged with a caller-supplied rnd) and Verify, external and internal (Algorithms 1-8).
HashML-DSA (pre-hash) is not implemented.

Formulations differ from the Rust verifier where that is practical, so the two are not the
same code twice:
  - NTT: a recursive CRT split, X^(2m) - r^2 = (X^m - r)(X^m + r), and its CRT inverse
    (halving at every level), both checked in `selftest` against the O(n^2) evaluation
    definition NTT(w)[i] = w(zeta^(2 BitRev8(i) + 1)). The Rust code transcribes the
    iterative Algorithms 41 and 42 with a zeta table and Barrett reduction; here every
    reduction is Python's %.
  - Bit packing: a polynomial is one big little-endian integer, not a byte accumulator.
  - A_hat is materialized (and cached per rho); the Rust code samples each entry on demand
    and multiplies it in as it is sampled.
  - Hint decoding is a literal transcription of Algorithm 21.

Strict verification, in this order; the Rust verifier must give the same result:
  1. |ctx| > 255                                             -> ContextTooLong
  2. HintBitUnpack (Algorithm 21), in its own order, polynomial by polynomial:
       count y[w+i] below the previous count or above omega   -> HintCountOutOfRange
       indices of polynomial i not strictly increasing        -> HintIndexNotIncreasing
     then, after all k polynomials:
       a non-zero byte among the unused index bytes           -> HintPaddingNonZero
  3. ||z||_inf >= gamma1 - beta                               -> ZOutOfRange
  4. c_tilde != H(mu || w1Encode(UseHint(h, w'_approx)))     -> ChallengeMismatch
FIPS 204 Algorithm 8 returns false at step 3 for a malformed hint, and step 13 is the
conjunction of the z bound and the c_tilde comparison; checking the z bound first is this
reference's (and the Rust code's) choice.

Usage: python3 -I mldsa_reference.py <command>
  selftest    check parameters, NTT, rounding and hint lemmas, and Sign -> Verify round trips
              plus mutations; exit 1 on a failure
  vectors     print tests/vectors/mldsa-differential.txt
  negative    print tests/vectors/mldsa-negative.txt
  crosscheck  compare against a locally installed OpenSSL >= 3.5 (`openssl` on PATH): keys
              from the same seeds, deterministic and fixed-entropy signatures byte for byte,
              and OpenSSL's verdict on every differential vector and part of the negative
              corpus. Optional; the committed files never depend on it.
Host-only engineering tool; nothing here ships.
"""

import functools
import hashlib
import os
import shutil
import subprocess
import sys
import tempfile

# ---------------------------------------------------------------------------------------
# Parameters (FIPS 204 §4, Table 1)

Q = 8380417
N = 256
D = 13
ZETA = 1753


class Params:
    def __init__(self, name, k, l, eta, tau, lam, gamma1, gamma2, omega):
        self.name, self.k, self.l, self.eta, self.tau = name, k, l, eta, tau
        self.lam, self.gamma1, self.gamma2, self.omega = lam, gamma1, gamma2, omega
        self.beta = tau * eta
        self.ctilde_bytes = lam // 4
        self.z_bits = (2 * gamma1 - 1).bit_length()  # bitlen(a + b) for a = g1 - 1, b = g1
        self.w1_bits = ((Q - 1) // (2 * gamma2) - 1).bit_length()
        self.pk_bytes = 32 + 32 * k * ((Q - 1).bit_length() - D)
        self.sig_bytes = self.ctilde_bytes + l * 32 * (1 + (gamma1 - 1).bit_length()) + omega + k


ML_DSA_65 = Params("ML-DSA-65", k=6, l=5, eta=4, tau=49, lam=192, gamma1=2**19,
                   gamma2=(Q - 1) // 32, omega=55)
ML_DSA_87 = Params("ML-DSA-87", k=8, l=7, eta=2, tau=60, lam=256, gamma1=2**19,
                   gamma2=(Q - 1) // 32, omega=75)
SETS = {"65": ML_DSA_65, "87": ML_DSA_87}


def H(data: bytes, n: int) -> bytes:
    return hashlib.shake_256(data).digest(n)


def G(data: bytes, n: int) -> bytes:
    return hashlib.shake_128(data).digest(n)


# ---------------------------------------------------------------------------------------
# NTT: recursive CRT formulation (see the module docstring)

def ntt(f: list[int]) -> list[int]:
    """NTT(f)[i] = f(zeta^(2 BitRev8(i) + 1)) for f in Z_q[X]/(X^256 + 1)."""
    return _ntt([c % Q for c in f], 256)


def _ntt(f: list[int], e: int) -> list[int]:
    """The values of f, a residue modulo X^len - zeta^e, at the len roots of that modulus.

    X^len - zeta^e = (X^h - r)(X^h + r) with h = len/2, r = zeta^(e/2), and
    X^h + r = X^h - zeta^(e/2 + 256) because zeta^256 = -1. Modulo X^h - r the residue is
    f_lo + r f_hi; modulo X^h + r it is f_lo - r f_hi.
    """
    if len(f) == 1:
        return f
    h, s = len(f) // 2, e // 2
    r = pow(ZETA, s, Q)
    lo = [(a + r * b) % Q for a, b in zip(f[:h], f[h:])]
    hi = [(a - r * b) % Q for a, b in zip(f[:h], f[h:])]
    return _ntt(lo, s) + _ntt(hi, s + 256)


def intt(v: list[int]) -> list[int]:
    return _intt(list(v), 256)


def _intt(v: list[int], e: int) -> list[int]:
    """CRT reconstruction, the inverse of _ntt: from lo = f_lo + r f_hi and
    hi = f_lo - r f_hi, f_lo = (lo + hi)/2 and f_hi = (lo - hi)/(2r)."""
    if len(v) == 1:
        return v
    h, s = len(v) // 2, e // 2
    r = pow(ZETA, s, Q)
    lo, hi = _intt(v[:h], s), _intt(v[h:], s + 256)
    half, inv_2r = pow(2, -1, Q), pow(2 * r, -1, Q)
    return ([(a + b) * half % Q for a, b in zip(lo, hi)]
            + [(a - b) * inv_2r % Q for a, b in zip(lo, hi)])


def brv8(i: int) -> int:
    return int(f"{i:08b}"[::-1], 2)


def ntt_naive(f: list[int]) -> list[int]:
    """The evaluation definition, O(n^2)."""
    powers = [pow(ZETA, t, Q) for t in range(512)]
    return [sum(c * powers[(2 * brv8(i) + 1) * j % 512] for j, c in enumerate(f)) % Q
            for i in range(N)]


def mul_schoolbook(a: list[int], b: list[int]) -> list[int]:
    """a * b in Z_q[X]/(X^256 + 1): X^256 = -1."""
    c = [0] * (2 * N)
    for i, x in enumerate(a):
        for j, y in enumerate(b):
            c[i + j] += x * y
    return [(c[i] - c[i + N]) % Q for i in range(N)]


def pointwise(a: list[int], b: list[int]) -> list[int]:
    return [x * y % Q for x, y in zip(a, b)]


def poly_add(a: list[int], b: list[int]) -> list[int]:
    return [(x + y) % Q for x, y in zip(a, b)]


def poly_sub(a: list[int], b: list[int]) -> list[int]:
    return [(x - y) % Q for x, y in zip(a, b)]


# ---------------------------------------------------------------------------------------
# Conversions and encodings (FIPS 204 §7.1-§7.3), as big little-endian integers

def pack_bits(values: list[int], bits: int) -> bytes:
    acc = 0
    for i, v in enumerate(values):
        assert 0 <= v < 1 << bits, (v, bits)
        acc |= v << (bits * i)
    return acc.to_bytes(len(values) * bits // 8, "little")


def unpack_bits(data: bytes, bits: int, count: int) -> list[int]:
    assert len(data) * 8 == bits * count
    acc = int.from_bytes(data, "little")
    return [(acc >> (bits * i)) & ((1 << bits) - 1) for i in range(count)]


def simple_bit_pack(w: list[int], b: int) -> bytes:  # Algorithm 16
    return pack_bits(w, b.bit_length())


def bit_pack(w: list[int], a: int, b: int) -> bytes:  # Algorithm 17
    return pack_bits([b - c for c in w], (a + b).bit_length())


def simple_bit_unpack(v: bytes, b: int) -> list[int]:  # Algorithm 18
    return unpack_bits(v, b.bit_length(), N)


def bit_unpack(v: bytes, a: int, b: int) -> list[int]:  # Algorithm 19
    return [b - c for c in unpack_bits(v, (a + b).bit_length(), N)]


def hint_bit_pack(h: list[list[int]], p: Params) -> bytes:  # Algorithm 20
    y = [0] * (p.omega + p.k)
    index = 0
    for i in range(p.k):
        for j in range(N):
            if h[i][j] != 0:
                y[index] = j
                index += 1
        y[p.omega + i] = index
    return bytes(y)


def hint_bit_unpack(y: bytes, p: Params) -> tuple[list[list[int]] | None, str | None]:
    """Algorithm 21, line by line. Returns (h, None) or (None, error name)."""
    omega, k = p.omega, p.k
    assert len(y) == omega + k
    h = [[0] * N for _ in range(k)]
    index = 0
    for i in range(k):
        if y[omega + i] < index or y[omega + i] > omega:
            return None, "HintCountOutOfRange"
        first = index
        while index < y[omega + i]:
            if index > first:
                if y[index - 1] >= y[index]:
                    return None, "HintIndexNotIncreasing"
            h[i][y[index]] = 1
            index += 1
    for i in range(index, omega):
        if y[i] != 0:
            return None, "HintPaddingNonZero"
    return h, None


def pk_encode(rho: bytes, t1: list[list[int]]) -> bytes:  # Algorithm 22
    return rho + b"".join(simple_bit_pack(t, 2**10 - 1) for t in t1)


def pk_decode(pk: bytes, p: Params) -> tuple[bytes, list[list[int]]]:  # Algorithm 23
    assert len(pk) == p.pk_bytes
    rho, rest = pk[:32], pk[32:]
    return rho, [simple_bit_unpack(rest[320 * i:320 * (i + 1)], 2**10 - 1) for i in range(p.k)]


def z_bytes(p: Params) -> int:
    return 32 * (1 + (p.gamma1 - 1).bit_length())


def sig_encode(ctilde: bytes, z: list[list[int]], h: list[list[int]], p: Params) -> bytes:
    """Algorithm 26; z is given as centred integers (z mod+- q)."""
    out = ctilde + b"".join(bit_pack(zi, p.gamma1 - 1, p.gamma1) for zi in z)
    return out + hint_bit_pack(h, p)


def sig_decode(sig: bytes, p: Params):  # Algorithm 27
    assert len(sig) == p.sig_bytes
    ct, zb = p.ctilde_bytes, z_bytes(p)
    ctilde = sig[:ct]
    z = [bit_unpack(sig[ct + zb * i: ct + zb * (i + 1)], p.gamma1 - 1, p.gamma1)
         for i in range(p.l)]
    h, err = hint_bit_unpack(sig[ct + zb * p.l:], p)
    return ctilde, z, h, err


def w1_encode(w1: list[list[int]], p: Params) -> bytes:  # Algorithm 28
    return b"".join(simple_bit_pack(w, (Q - 1) // (2 * p.gamma2) - 1) for w in w1)


# ---------------------------------------------------------------------------------------
# Sampling (FIPS 204 §7.3)

def sample_in_ball(rho: bytes, tau: int) -> list[int]:  # Algorithm 29
    n = 272
    while True:
        stream = H(rho, n)
        signs = int.from_bytes(stream[:8], "little")
        c = [0] * N
        pos, exhausted = 8, False
        for i in range(N - tau, N):
            while True:
                if pos >= n:
                    exhausted = True
                    break
                j = stream[pos]
                pos += 1
                if j <= i:
                    break
            if exhausted:
                break
            c[i] = c[j]
            c[j] = -1 if (signs >> (i + tau - N)) & 1 else 1
        if not exhausted:
            return c
        n *= 2  # a prefix of the same stream; retry with more of it


def coeff_from_three_bytes(b0: int, b1: int, b2: int) -> int | None:  # Algorithm 14
    if b2 > 127:
        b2 -= 128
    z = 2**16 * b2 + 2**8 * b1 + b0
    return z if z < Q else None


def coeff_from_half_byte(b: int, eta: int) -> int | None:  # Algorithm 15
    if eta == 2 and b < 15:
        return 2 - (b % 5)
    if eta == 4 and b < 9:
        return 4 - b
    return None


def rej_ntt_poly(rho: bytes) -> list[int]:  # Algorithm 30
    assert len(rho) == 34
    n = 840
    while True:
        stream = G(rho, n)
        a = []
        for i in range(0, n, 3):
            c = coeff_from_three_bytes(stream[i], stream[i + 1], stream[i + 2])
            if c is not None:
                a.append(c)
                if len(a) == N:
                    return a
        n *= 2


def rej_bounded_poly(rho: bytes, eta: int) -> list[int]:  # Algorithm 31
    assert len(rho) == 66
    n = 512
    while True:
        stream = H(rho, n)
        a = []
        for z in stream:
            z0 = coeff_from_half_byte(z % 16, eta)
            z1 = coeff_from_half_byte(z // 16, eta)
            if z0 is not None:
                a.append(z0)
            if z1 is not None and len(a) < N:
                a.append(z1)
            if len(a) == N:
                return a
        n *= 2


@functools.lru_cache(maxsize=64)
def expand_a(rho: bytes, k: int, l: int) -> tuple[tuple[tuple[int, ...], ...], ...]:
    """Algorithm 32: A_hat[r][s] = RejNTTPoly(rho || s || r), column byte first."""
    return tuple(tuple(tuple(rej_ntt_poly(rho + bytes([s, r]))) for s in range(l))
                 for r in range(k))


def expand_s(rho: bytes, p: Params):  # Algorithm 33
    s1 = [rej_bounded_poly(rho + r.to_bytes(2, "little"), p.eta) for r in range(p.l)]
    s2 = [rej_bounded_poly(rho + (r + p.l).to_bytes(2, "little"), p.eta) for r in range(p.k)]
    return s1, s2


def expand_mask(rho: bytes, mu: int, p: Params) -> list[list[int]]:  # Algorithm 34
    c = 1 + (p.gamma1 - 1).bit_length()
    return [bit_unpack(H(rho + (mu + r).to_bytes(2, "little"), 32 * c), p.gamma1 - 1, p.gamma1)
            for r in range(p.l)]


def mat_vec(a_hat, v_hat: list[list[int]]) -> list[list[int]]:
    """A_hat o v_hat: row i is sum_j A_hat[i][j] * v_hat[j], coefficient-wise mod q."""
    out = []
    for row in a_hat:
        acc = [0] * N
        for a, v in zip(row, v_hat):
            acc = [s + x * y for s, x, y in zip(acc, a, v)]
        out.append([s % Q for s in acc])
    return out


# ---------------------------------------------------------------------------------------
# Rounding (FIPS 204 §7.4)

def mod_pm(r: int, alpha: int) -> int:
    """The representative of r mod alpha in (-alpha/2, alpha/2] (or [-(alpha-1)/2,
    (alpha-1)/2] for odd alpha)."""
    r %= alpha
    return r - alpha if r > alpha // 2 else r


def power2round(r: int) -> tuple[int, int]:  # Algorithm 35
    rp = r % Q
    r0 = mod_pm(rp, 2**D)
    return (rp - r0) // 2**D, r0


def decompose(r: int, gamma2: int) -> tuple[int, int]:  # Algorithm 36
    rp = r % Q
    r0 = mod_pm(rp, 2 * gamma2)
    if rp - r0 == Q - 1:
        return 0, r0 - 1
    return (rp - r0) // (2 * gamma2), r0


def high_bits(r: int, gamma2: int) -> int:  # Algorithm 37
    return decompose(r, gamma2)[0]


def low_bits(r: int, gamma2: int) -> int:  # Algorithm 38
    return decompose(r, gamma2)[1]


def make_hint(z: int, r: int, gamma2: int) -> int:  # Algorithm 39
    return int(high_bits(r, gamma2) != high_bits(r + z, gamma2))


def use_hint(h: int, r: int, gamma2: int) -> int:  # Algorithm 40
    m = (Q - 1) // (2 * gamma2)
    r1, r0 = decompose(r, gamma2)
    if h == 1 and r0 > 0:
        return (r1 + 1) % m
    if h == 1 and r0 <= 0:
        return (r1 - 1) % m
    return r1


def inf_norm(w: list[int]) -> int:
    return max(abs(mod_pm(c, Q)) for c in w)


# ---------------------------------------------------------------------------------------
# ML-DSA internal and external functions (FIPS 204 §5, §6)

def keygen_internal(xi: bytes, p: Params):  # Algorithm 6
    assert len(xi) == 32
    seed = H(xi + bytes([p.k, p.l]), 128)
    rho, rho_p, key = seed[:32], seed[32:96], seed[96:]
    a_hat = expand_a(rho, p.k, p.l)
    s1, s2 = expand_s(rho_p, p)
    t = [poly_add(intt(row), s) for row, s in zip(mat_vec(a_hat, [ntt(s) for s in s1]), s2)]
    t1 = [[power2round(c)[0] for c in ti] for ti in t]
    t0 = [[power2round(c)[1] for c in ti] for ti in t]
    pk = pk_encode(rho, t1)
    sk = (rho, key, H(pk, 64), s1, s2, t0)  # kept structured; skEncode is not needed here
    return pk, sk


def sign_internal(sk, m_prime: bytes, rnd: bytes, p: Params) -> bytes:  # Algorithm 7
    assert len(rnd) == 32
    rho, key, tr, s1, s2, t0 = sk
    s1_hat, s2_hat, t0_hat = ([ntt(x) for x in v] for v in (s1, s2, t0))
    a_hat = expand_a(rho, p.k, p.l)
    mu = H(tr + m_prime, 64)
    rho_pp = H(key + rnd + mu, 64)
    kappa = 0
    while True:
        y = expand_mask(rho_pp, kappa, p)
        kappa += p.l
        w = [intt(row) for row in mat_vec(a_hat, [ntt(yi) for yi in y])]
        w1 = [[high_bits(c, p.gamma2) for c in wi] for wi in w]
        ctilde = H(mu + w1_encode(w1, p), p.ctilde_bytes)
        c_hat = ntt(sample_in_ball(ctilde, p.tau))
        cs1 = [intt(pointwise(c_hat, s)) for s in s1_hat]
        cs2 = [intt(pointwise(c_hat, s)) for s in s2_hat]
        z = [poly_add(yi, ci) for yi, ci in zip(y, cs1)]
        w_minus = [poly_sub(wi, ci) for wi, ci in zip(w, cs2)]
        r0 = [[low_bits(c, p.gamma2) for c in wi] for wi in w_minus]
        if (max(inf_norm(zi) for zi in z) >= p.gamma1 - p.beta
                or max(max(abs(c) for c in ri) for ri in r0) >= p.gamma2 - p.beta):
            continue
        ct0 = [intt(pointwise(c_hat, t)) for t in t0_hat]
        h = [[make_hint(-a % Q, (b + a) % Q, p.gamma2) for a, b in zip(ci, wi)]
             for ci, wi in zip(ct0, w_minus)]
        if max(inf_norm(ci) for ci in ct0) >= p.gamma2 or sum(map(sum, h)) > p.omega:
            continue
        return sig_encode(ctilde, [[mod_pm(c, Q) for c in zi] for zi in z], h, p)


def verify_internal(pk: bytes, m_prime: bytes, sig: bytes, p: Params) -> str:
    """Algorithm 8 with the check order of the module docstring. Returns "ok" or the error
    name the Rust verifier must return."""
    rho, t1 = pk_decode(pk, p)
    ctilde, z, h, err = sig_decode(sig, p)
    if err:
        return err
    if max(inf_norm(zi) for zi in z) >= p.gamma1 - p.beta:
        return "ZOutOfRange"
    a_hat = expand_a(rho, p.k, p.l)
    tr = H(pk, 64)
    mu = H(tr + m_prime, 64)
    c_hat = ntt(sample_in_ball(ctilde, p.tau))
    az = mat_vec(a_hat, [ntt(zi) for zi in z])
    ct1 = [pointwise(c_hat, ntt([c * 2**D for c in t])) for t in t1]
    w_approx = [intt(poly_sub(a, b)) for a, b in zip(az, ct1)]
    w1 = [[use_hint(hb, c, p.gamma2) for hb, c in zip(hi, wi)] for hi, wi in zip(h, w_approx)]
    return "ok" if H(mu + w1_encode(w1, p), p.ctilde_bytes) == ctilde else "ChallengeMismatch"


def m_prime_pure(msg: bytes, ctx: bytes) -> bytes:
    """M' = IntegerToBytes(0, 1) || IntegerToBytes(|ctx|, 1) || ctx || M (Algorithms 2, 3)."""
    assert len(ctx) <= 255
    return bytes([0, len(ctx)]) + ctx + msg


def sign(sk, msg: bytes, ctx: bytes, rnd: bytes, p: Params) -> bytes:  # Algorithm 2
    """rnd = 32 zero bytes is the deterministic variant; any other value is hedged with a
    fixed (caller-chosen) rnd."""
    return sign_internal(sk, m_prime_pure(msg, ctx), rnd, p)


def verify(pk: bytes, msg: bytes, sig: bytes, ctx: bytes, p: Params) -> str:  # Algorithm 3
    if len(ctx) > 255:
        return "ContextTooLong"
    return verify_internal(pk, m_prime_pure(msg, ctx), sig, p)


# ---------------------------------------------------------------------------------------
# Deterministic test data

def stream(label: str, n: int) -> bytes:
    """n deterministic pseudo-random bytes: SHA-512 in counter mode over a label."""
    out = b""
    counter = 0
    while len(out) < n:
        out += hashlib.sha512(f"origin-crypto/mldsa/{label}/{counter}".encode()).digest()
        counter += 1
    return out[:n]


DETERMINISTIC = bytes(32)
ROLES = ["publisher", "notary", "origin-driver-attest", "origin-kernel", "origin-release"]
KEYS_PER_SET = 13
SIGS_PER_KEY = 4
# (message length, context length) pairs that must appear, then cycles of both lists.
EDGE_CASES = [(0, 0), (0, 255), (0, 1), (1, 0), (4096, 255), (4096, 0), (255, 254), (136, 32)]
MSG_LENGTHS = [1, 2, 3, 5, 8, 16, 31, 32, 33, 47, 48, 63, 64, 65, 100, 127, 128, 135, 136, 137,
               167, 168, 169, 200, 255, 256, 257, 271, 272, 300, 383, 384, 500, 511, 512, 513,
               640, 777, 1000, 1023, 1024, 1025, 1500, 2047, 2048, 3000, 4095]  # fmt: skip
CTX_LENGTHS = [0, 1, 254, 2, 32, 31, 33, 64, 100, 127, 128, 200, 16, 8, 3, 255]


class Vector:
    def __init__(self, set_name, index, key_index, xi, pk, sk, rnd, ctx, msg, sig):
        self.set_name, self.index, self.key_index, self.xi = set_name, index, key_index, xi
        self.pk, self.sk, self.rnd, self.ctx, self.msg, self.sig = pk, sk, rnd, ctx, msg, sig


@functools.lru_cache(maxsize=None)
def key_for(set_name: str, key_index: int):
    """Key i of both parameter sets comes from the same seed xi, so the two sets' keys differ
    only through KeyGen's (k, l) domain separation."""
    xi = stream(f"xi/{key_index}", 32)
    pk, sk = keygen_internal(xi, SETS[set_name])
    return xi, pk, sk


@functools.lru_cache(maxsize=None)
def differential_vectors(set_name: str) -> tuple[Vector, ...]:
    p = SETS[set_name]
    vectors = []
    for index in range(KEYS_PER_SET * SIGS_PER_KEY):
        key_index = index // SIGS_PER_KEY
        xi, pk, sk = key_for(set_name, key_index)
        if index < len(ROLES):
            # The ONBF statement contexts (ONBF §7.1), over statement-sized messages.
            ctx = f"origin/onbf/1/{ROLES[index]}".encode()
            msg = stream(f"msg/{set_name}/{index}", 150 + 7 * index)
        else:
            j = index - len(ROLES)
            msg_len, ctx_len = (EDGE_CASES[j] if j < len(EDGE_CASES) else
                                (MSG_LENGTHS[j % len(MSG_LENGTHS)], CTX_LENGTHS[j % len(CTX_LENGTHS)]))
            ctx = stream(f"ctx/{set_name}/{index}", ctx_len)
            msg = stream(f"msg/{set_name}/{index}", msg_len)
        rnd = DETERMINISTIC if index % 2 == 0 else stream(f"rnd/{set_name}/{index}", 32)
        sig = sign(sk, msg, ctx, rnd, p)
        assert len(sig) == p.sig_bytes and len(pk) == p.pk_bytes
        assert verify(pk, msg, sig, ctx, p) == "ok"
        vectors.append(Vector(set_name, index, key_index, xi, pk, sk, rnd, ctx, msg, sig))
    msg_lens = {len(v.msg) for v in vectors}
    ctx_lens = {len(v.ctx) for v in vectors}
    assert {0, 4096} <= msg_lens and {0, 1, 255} <= ctx_lens, (msg_lens, ctx_lens)
    assert any(len(v.msg) == 0 and len(v.ctx) == 0 for v in vectors)
    assert any(len(v.msg) == 0 and len(v.ctx) == 255 for v in vectors)
    return tuple(vectors)


def hx(data: bytes) -> str:
    return data.hex() if data else "-"


# ---------------------------------------------------------------------------------------
# Self-checks

def is_prime(n: int) -> bool:
    return n > 1 and all(n % d for d in range(2, int(n**0.5) + 1))


def check_parameters() -> None:
    assert is_prime(Q) and Q == 2**23 - 2**13 + 1
    # zeta is a primitive 512th root of unity: zeta^512 = 1 and zeta^256 = -1.
    assert pow(ZETA, 512, Q) == 1 and pow(ZETA, 256, Q) == Q - 1
    assert pow(256, -1, Q) == 8347681
    for p in SETS.values():
        assert (p.pk_bytes, p.sig_bytes) in [(1952, 3309), (2592, 4627)]
        assert p.w1_bits == 4 and p.z_bits == 20
    assert ML_DSA_65.beta == 196 and ML_DSA_87.beta == 120


class Rng:
    """xorshift64 for self-test inputs."""

    def __init__(self, seed: int):
        self.s = seed

    def next(self) -> int:
        s = self.s
        s ^= (s << 13) & (2**64 - 1)
        s ^= s >> 7
        s ^= (s << 17) & (2**64 - 1)
        self.s = s
        return s

    def poly(self) -> list[int]:
        return [self.next() % Q for _ in range(N)]


def cmd_selftest() -> None:
    check_parameters()
    rng = Rng(0x0F1B_5204_0000_0001)
    # NTT against its definition, inverse, and the convolution theorem.
    for _ in range(3):
        a, b = rng.poly(), rng.poly()
        assert ntt(a) == ntt_naive(a)
        assert intt(ntt(a)) == a
        assert intt(pointwise(ntt(a), ntt(b))) == mul_schoolbook(a, b)
    unit = [1] + [0] * (N - 1)
    assert ntt(unit) == [1] * N
    x = [0, 1] + [0] * (N - 2)
    assert ntt(x) == [pow(ZETA, 2 * brv8(i) + 1, Q) for i in range(N)]
    print("ntt: matches the evaluation definition, inverts, multiplies negacyclically")
    # Rounding: Decompose reassembles, ranges hold, and UseHint(MakeHint(z, r), r) =
    # HighBits(r + z) for |z| <= gamma2 (the FIPS 204 hint lemma).
    g2 = ML_DSA_65.gamma2
    specials = [0, 1, g2 - 1, g2, g2 + 1, 2 * g2, Q - g2 - 1, Q - g2, Q - g2 + 1, Q - 2, Q - 1]
    for r in specials + [rng.next() % Q for _ in range(20000)]:
        r1, r0 = decompose(r, g2)
        assert 0 <= r1 < 16 and -g2 <= r0 <= g2
        assert (r1 * 2 * g2 + r0) % Q == r
        t1, t0 = power2round(r)
        assert 0 <= t1 < 1024 and -(2**12) < t0 <= 2**12 and t1 * 2**D + t0 == r
        z = rng.next() % (2 * g2 + 1) - g2
        assert use_hint(make_hint(z % Q, r, g2), r, g2) == high_bits(r + z, g2)
    print("rounding: Decompose, Power2Round and the hint lemma hold")
    # Hint encoding round trip.
    p = ML_DSA_87
    h = [[0] * N for _ in range(p.k)]
    for i in range(p.omega):
        h[rng.next() % p.k][rng.next() % N] = 1
    assert hint_bit_unpack(hint_bit_pack(h, p), p) == (h, None)
    # Sign -> Verify round trips, deterministic and hedged, and rejected mutations.
    for set_name, p in SETS.items():
        xi, pk, sk = key_for(set_name, 0)
        for mode, rnd in [("deterministic", DETERMINISTIC), ("hedged", stream("selftest", 32))]:
            for msg, ctx in [(b"", b""), (b"Origin OS", b"origin/onbf/1/publisher"),
                             (stream("selftest/m", 333), stream("selftest/c", 255))]:
                sig = sign(sk, msg, ctx, rnd, p)
                assert verify(pk, msg, sig, ctx, p) == "ok"
                assert sign(sk, msg, ctx, rnd, p) == sig  # the same rnd, the same signature
                assert verify(pk, msg + b"\0", sig, ctx, p) == "ChallengeMismatch"
                assert verify(pk, msg, sig, ctx + b"\0", p) != "ok"
                assert verify(pk, msg, sig, ctx + bytes(256), p) == "ContextTooLong"
                for bit in (0, 8 * p.ctilde_bytes + 5, 8 * (p.sig_bytes - p.k) - 1, 8 * len(sig) - 1):
                    bad = bytearray(sig)
                    bad[bit // 8] ^= 1 << (bit % 8)
                    assert verify(pk, msg, bytes(bad), ctx, p) != "ok"
                bad_pk = bytearray(pk)
                bad_pk[40] ^= 1
                assert verify(bytes(bad_pk), msg, sig, ctx, p) == "ChallengeMismatch"
            print(f"{p.name} {mode}: Sign -> Verify round trips; mutations rejected")
        other = sign(key_for(set_name, 1)[2], b"x", b"", DETERMINISTIC, p)
        assert verify(pk, b"x", other, b"", p) == "ChallengeMismatch"
    # Deterministic and hedged signatures differ, and the sets' keys from one seed differ.
    _, pk65, sk65 = key_for("65", 0)
    assert sign(sk65, b"m", b"", DETERMINISTIC, ML_DSA_65) != sign(
        sk65, b"m", b"", stream("selftest", 32), ML_DSA_65)
    assert key_for("87", 0)[1][:32] != pk65[:32]
    print("selftest passed")


# ---------------------------------------------------------------------------------------
# Commands

def cmd_vectors() -> None:
    check_parameters()
    print("# ML-DSA differential vectors for origin-crypto (FIPS 204, pure ML-DSA-65 and")
    print("# ML-DSA-87). Generated by `python3 -I security/crypto/tools/mldsa_reference.py")
    print("# vectors`: an independent pure-Python FIPS 204 implementation generated each key")
    print("# from its seed xi (ML-DSA.KeyGen_internal), signed (ML-DSA.Sign, deterministic")
    print("# when rnd is '-', otherwise hedged with the listed rnd) and verified strictly.")
    print("# Key i of both sets uses the same xi. Inputs are SHA-512 counter-mode streams.")
    print("# `mldsa_reference.py crosscheck` compares every key and signature here byte for")
    print("# byte with a locally installed OpenSSL (>= 3.5); regenerating needs no OpenSSL.")
    print("#   key <set> <key> <xi> <public_key>")
    print("#   sig <set> <vector> <key> <rnd|-> <ctx|-> <message|-> <signature>")
    for set_name in SETS:
        vectors = differential_vectors(set_name)
        for key_index in range(KEYS_PER_SET):
            xi, pk, _ = key_for(set_name, key_index)
            print(f"key {set_name} {key_index} {xi.hex()} {pk.hex()}")
        for v in vectors:
            rnd = "-" if v.rnd == DETERMINISTIC else v.rnd.hex()
            print(f"sig {set_name} {v.index} {v.key_index} {rnd} {hx(v.ctx)} {hx(v.msg)} "
                  f"{v.sig.hex()}")


class Case:
    """One negative-corpus line: a differential vector with edits applied."""

    def __init__(self, base: Vector):
        self.base = base
        self.pk, self.msg, self.ctx = base.pk, base.msg, base.ctx
        self.sig = bytearray(base.sig)
        self.edits: list[str] = []

    def xor(self, offset: int, data: bytes) -> "Case":
        for i, b in enumerate(data):
            self.sig[offset + i] ^= b
        self.edits.append(f"sig^{offset}:{data.hex()}")
        return self

    def put(self, offset: int, data: bytes) -> "Case":
        self.sig[offset:offset + len(data)] = data
        self.edits.append(f"sig={offset}:{data.hex()}")
        return self

    def with_ctx(self, ctx: bytes) -> "Case":
        self.ctx = ctx
        self.edits.append(f"ctx={hx(ctx)}")
        return self

    def with_msg(self, msg: bytes) -> "Case":
        self.msg = msg
        self.edits.append(f"msg={hx(msg)}")
        return self

    def with_pk(self, pk: bytes) -> "Case":
        self.pk = pk
        self.edits.append(f"pk={pk.hex()}")
        return self


def z_offset(p: Params, poly: int, coeff: int) -> tuple[int, int]:
    """The bit offset of z[poly][coeff]'s 20-bit field in the signature."""
    return 8 * (p.ctilde_bytes + z_bytes(p) * poly) + p.z_bits * coeff, p.z_bits


def set_z(case: Case, p: Params, poly: int, coeff: int, value: int) -> Case:
    """Overwrites z[poly][coeff] with `value` (encoded as gamma1 - value), as the byte edit
    that covers the field."""
    bit, width = z_offset(p, poly, coeff)
    first, last = bit // 8, (bit + width - 1) // 8
    span = int.from_bytes(case.sig[first:last + 1], "little")
    shift = bit - 8 * first
    field = p.gamma1 - value
    assert 0 <= field < 1 << width
    span = (span & ~(((1 << width) - 1) << shift)) | (field << shift)
    return case.put(first, span.to_bytes(last - first + 1, "little"))


def hint_region(p: Params) -> int:
    return p.ctilde_bytes + z_bytes(p) * p.l


def hint_counts(sig: bytes, p: Params) -> list[int]:
    start = hint_region(p)
    return list(sig[start + p.omega:start + p.omega + p.k])


def cmd_negative() -> None:
    check_parameters()
    lines: list[str] = []

    def emit(set_name: str, case: Case, label: str, expect: str | None = None) -> None:
        p = SETS[set_name]
        got = verify(case.pk, case.msg, bytes(case.sig), case.ctx, p)
        if expect is not None:
            assert got == expect, f"{set_name} {label}: expected {expect}, reference says {got}"
        assert got != "ok", f"{set_name} {label}: a negative case verifies"
        edits = ",".join(case.edits) if case.edits else "none"
        lines.append(f"neg {set_name} {case.base.index} {got} {edits} {label}")

    for set_name, p in SETS.items():
        vectors = differential_vectors(set_name)
        base = vectors[0]
        ct, hr, omega, k = p.ctilde_bytes, hint_region(p), p.omega, p.k

        # 1. Every bit of c_tilde.
        for bit in range(8 * ct):
            emit(set_name, Case(base).xor(bit // 8, bytes([1 << (bit % 8)])),
                 f"flip:c_tilde:bit{bit}", "ChallengeMismatch")

        # 2. Bits of z: low, middle and the top bits of the 20-bit field, at the first, a
        #    middle and the last coefficient of every polynomial. A high bit moves z by up to
        #    2^19 and usually out of range.
        for poly in range(p.l):
            for coeff in (0, 1, 127, 128, 254, 255):
                for b in (0, 1, 9, 17, 18, 19):
                    bit = z_offset(p, poly, coeff)[0] + b
                    emit(set_name, Case(base).xor(bit // 8, bytes([1 << (bit % 8)])),
                         f"flip:z[{poly}][{coeff}]:bit{b}")

        # 3. Every bit of the hint encoding (omega index bytes, then k counts).
        for bit in range(8 * (omega + k)):
            at = hr * 8 + bit
            region = "index" if bit < 8 * omega else "count"
            emit(set_name, Case(base).xor(at // 8, bytes([1 << (at % 8)])),
                 f"flip:h:{region}:byte{bit // 8}:bit{bit % 8}")

        # 4. Constructed hint encodings. Pick a vector whose hints include a polynomial with
        #    at least two indices and a polynomial after one with hints.
        hb = next(v for v in vectors if sum(hint_counts(v.sig, p)) >= 8
                  and any(b - a >= 2 for a, b in zip([0] + hint_counts(v.sig, p),
                                                    hint_counts(v.sig, p))))
        counts = hint_counts(hb.sig, p)
        total = counts[-1]
        idx = list(hb.sig[hr:hr + omega])
        starts = [0] + counts[:-1]
        multi = next(i for i in range(k) if counts[i] - starts[i] >= 2)
        later = next(i for i in range(1, k) if starts[i] > 0 and counts[i] - starts[i] >= 1)

        def hints(index_bytes: list[int], count_bytes: list[int]) -> bytes:
            assert len(index_bytes) == omega and len(count_bytes) == k
            return bytes(index_bytes) + bytes(count_bytes)

        def hcase(index_bytes, count_bytes) -> Case:
            return Case(hb).put(hr, hints(index_bytes, count_bytes))

        # 4a. Counts.
        if any(c > 0 for c in counts[:-1]):
            i = next(i for i in range(1, k) if counts[i - 1] > 0)
            c2 = counts[:]
            c2[i] = counts[i - 1] - 1
            emit(set_name, hcase(idx, c2), f"hint:count[{i}]-below-count[{i - 1}]",
                 "HintCountOutOfRange")
        for value in (omega + 1, 255):
            c2 = counts[:]
            c2[-1] = value
            emit(set_name, hcase(idx, c2), f"hint:last-count={value}>omega",
                 "HintCountOutOfRange")
        c2 = counts[:]
        c2[0] = omega + 1
        emit(set_name, hcase(idx, c2), "hint:first-count=omega+1", "HintCountOutOfRange")
        # Exactly omega hints is valid: omega distinct indices spread over the polynomials.
        full_idx = [(7 * j) % N for j in range(omega)]
        per = omega // k
        full_counts = [per * (i + 1) for i in range(k - 1)] + [omega]
        for i in range(k):
            lo, hi = (0 if i == 0 else full_counts[i - 1]), full_counts[i]
            full_idx[lo:hi] = sorted(full_idx[lo:hi])
        emit(set_name, hcase(full_idx, full_counts), "hint:exactly-omega-hints-decode",
             "ChallengeMismatch")
        # All omega hints in the last polynomial.
        emit(set_name, hcase(sorted((5 * j + 1) % N for j in range(omega)), [0] * (k - 1) + [omega]),
             "hint:omega-hints-in-last-poly-decode", "ChallengeMismatch")
        # omega hints in polynomial 0 and a further count of omega + 1.
        emit(set_name, hcase(list(range(omega)), [omega] * (k - 1) + [omega + 1]),
             "hint:omega-then-omega+1", "HintCountOutOfRange")
        # No hints at all decodes.
        emit(set_name, hcase([0] * omega, [0] * k), "hint:none-decode", "ChallengeMismatch")
        # 4b. Index order within a polynomial.
        s = starts[multi]
        dup = idx[:]
        dup[s + 1] = dup[s]
        emit(set_name, hcase(dup, counts), f"hint:duplicate-index-in-poly{multi}",
             "HintIndexNotIncreasing")
        swp = idx[:]
        swp[s], swp[s + 1] = swp[s + 1], swp[s]
        emit(set_name, hcase(swp, counts), f"hint:swapped-indices-in-poly{multi}",
             "HintIndexNotIncreasing")
        last = idx[:]
        e = counts[multi] - 1
        last[e] = last[e - 1]
        emit(set_name, hcase(last, counts), f"hint:last-index-of-poly{multi}-repeats",
             "HintIndexNotIncreasing")
        # Across a polynomial boundary the order restarts, so a smaller first index is valid.
        cross = idx[:]
        cross[starts[later]] = 0 if idx[starts[later]] != 0 else 1
        if counts[later] - starts[later] >= 2:
            assert cross[starts[later]] < cross[starts[later] + 1]
        emit(set_name, hcase(cross, counts),
             f"hint:poly{later}-first-index-below-previous-poly-decodes", "ChallengeMismatch")
        # 4c. Padding.
        if total < omega:
            for at in sorted({total, omega - 1}):
                pad = idx[:]
                pad[at] = 1
                emit(set_name, hcase(pad, counts), f"hint:padding-byte{at}=1",
                     "HintPaddingNonZero")
            pad = idx[:]
            pad[omega - 1] = 0xFF
            emit(set_name, hcase(pad, counts), "hint:last-padding-byte=ff", "HintPaddingNonZero")
        # 4d. Order of the checks: the first failure in Algorithm 21's order wins.
        c_bad = counts[:]
        c_bad[-1] = omega + 1
        emit(set_name, hcase(dup, c_bad), "order:index-order(earlier-poly)+count(last-poly)",
             "HintIndexNotIncreasing" if multi < k - 1 else "HintCountOutOfRange")
        c_bad0 = counts[:]
        c_bad0[0] = omega + 1
        emit(set_name, hcase(dup, c_bad0), "order:count(poly0)+index-order", "HintCountOutOfRange")
        if total < omega:
            pad = dup[:]
            pad[omega - 1] = 1
            emit(set_name, hcase(pad, counts), "order:index-order+padding",
                 "HintIndexNotIncreasing")
            pad = idx[:]
            pad[omega - 1] = 1
            emit(set_name, hcase(pad, c_bad0), "order:count+padding", "HintCountOutOfRange")
        emit(set_name, set_z(hcase(dup, counts), p, 0, 0, p.gamma1),
             "order:hint+z-out-of-range", "HintIndexNotIncreasing")
        emit(set_name, hcase(dup, counts).with_ctx(bytes(256)), "order:ctx-256+hint",
             "ContextTooLong")

        # 5. The z bound, ||z||_inf < gamma1 - beta, at its boundary, at the first, a middle
        #    and the last coefficient.
        bound = p.gamma1 - p.beta
        for poly, coeff in ((0, 0), (p.l // 2, 100), (p.l - 1, 255)):
            for value, want in ((bound - 1, "ChallengeMismatch"), (-(bound - 1), "ChallengeMismatch"),
                                (bound, "ZOutOfRange"), (-bound, "ZOutOfRange"),
                                (p.gamma1, "ZOutOfRange"), (-p.gamma1 + 1, "ZOutOfRange")):
                emit(set_name, set_z(Case(base), p, poly, coeff, value),
                     f"z[{poly}][{coeff}]={value}", want)
        emit(set_name, set_z(Case(base), p, 0, 0, bound).with_msg(base.msg + b"\0"),
             "order:z-out-of-range+wrong-message", "ZOutOfRange")

        # 6. Context strings.
        for v in vectors[:5]:
            emit(set_name, Case(v).with_ctx(v.ctx[:-1]), "ctx:truncated", "ChallengeMismatch")
            emit(set_name, Case(v).with_ctx(v.ctx + b"\0"), "ctx:extra-byte", "ChallengeMismatch")
            emit(set_name, Case(v).with_ctx(b""), "ctx:empty", "ChallengeMismatch")
            role = v.ctx.replace(b"origin/onbf/1/", b"")
            other = next(r for r in ROLES if r.encode() != role)
            emit(set_name, Case(v).with_ctx(f"origin/onbf/1/{other}".encode()),
                 f"ctx:role-{role.decode()}-as-{other}", "ChallengeMismatch")
        v255 = next(v for v in vectors if len(v.ctx) == 255)
        v0 = next(v for v in vectors if len(v.ctx) == 0)
        flipped = bytearray(v255.ctx)
        flipped[254] ^= 0x80
        emit(set_name, Case(v255).with_ctx(bytes(flipped)), "ctx:255-last-bit-flipped",
             "ChallengeMismatch")
        emit(set_name, Case(v255).with_ctx(v255.ctx + b"\0"), "ctx:256-bytes", "ContextTooLong")
        emit(set_name, Case(v0).with_ctx(bytes(256)), "ctx:256-zero-bytes", "ContextTooLong")
        emit(set_name, Case(v0).with_ctx(bytes(1000)), "ctx:1000-bytes", "ContextTooLong")
        emit(set_name, Case(v0).with_ctx(b"\0"), "ctx:empty-vs-one-zero-byte", "ChallengeMismatch")

        # 7. Messages.
        emit(set_name, Case(base).with_msg(base.msg + b"\0"), "msg:extra-byte", "ChallengeMismatch")
        long = next(v for v in vectors if len(v.msg) >= 100)
        emit(set_name, Case(long).with_msg(long.msg[:-1]), "msg:truncated", "ChallengeMismatch")
        m = bytearray(long.msg)
        m[len(m) // 2] ^= 0x01
        emit(set_name, Case(long).with_msg(bytes(m)), "msg:bit-flipped", "ChallengeMismatch")
        # M' framing: (ctx = c, M = x || m) must not verify as (ctx = c || x, M = m).
        vm = next(v for v in vectors if 0 < len(v.ctx) < 255 and len(v.msg) > 0)
        emit(set_name, Case(vm).with_ctx(vm.ctx + vm.msg[:1]).with_msg(vm.msg[1:]),
             "msg:first-byte-moved-into-ctx", "ChallengeMismatch")

        # 8. Keys: another key of the same set, and bit flips in rho and t1.
        emit(set_name, Case(base).with_pk(vectors[SIGS_PER_KEY].pk), "pk:other-key",
             "ChallengeMismatch")
        for byte in (0, 31, 32, p.pk_bytes - 1):
            bad = bytearray(base.pk)
            bad[byte] ^= 0x01
            emit(set_name, Case(base).with_pk(bytes(bad)), f"pk:flip-byte{byte}-bit0",
                 "ChallengeMismatch")

    # 9. Keys and signatures of the other parameter set, cut or extended to this set's
    #    lengths. Key 0 of both sets comes from the same seed.
    v65, v87 = differential_vectors("65")[0], differential_vectors("87")[0]
    emit("65", Case(v65).with_pk(v87.pk[:ML_DSA_65.pk_bytes]),
         "pk:ML-DSA-87-key-same-seed-truncated", "ChallengeMismatch")
    emit("87", Case(v87).with_pk(v65.pk + v87.pk[ML_DSA_65.pk_bytes:]),
         "pk:ML-DSA-65-key-same-seed-extended", "ChallengeMismatch")
    emit("65", Case(v65).put(0, v87.sig[:ML_DSA_65.sig_bytes]),
         "sig:ML-DSA-87-signature-truncated")

    print("# ML-DSA negative corpus for origin-crypto. Generated by")
    print("# `python3 -I security/crypto/tools/mldsa_reference.py negative`; each expected")
    print("# result is the strict verdict of the independent pure-Python reference.")
    print("#   neg <set> <vector> <expected> <edits|none> <label>")
    print("# <vector> is a differential vector of the same set (its index in the sig lines).")
    print("# Edits, comma-separated, applied in order to that vector's key, context, message")
    print("# and signature:")
    print("#   sig^OFF:HEX  XOR HEX into the signature at byte offset OFF")
    print("#   sig=OFF:HEX  overwrite the signature at byte offset OFF with HEX")
    print("#   ctx=HEX|-    replace the context string; msg=HEX|- replace the message")
    print("#   pk=HEX       replace the public key")
    for line in lines:
        print(line)


# ---------------------------------------------------------------------------------------
# Optional cross-check against a locally installed OpenSSL (>= 3.5 has ML-DSA)

def cmd_crosscheck() -> None:
    check_parameters()
    openssl = shutil.which("openssl")
    if openssl is None:
        sys.exit("crosscheck: no openssl on PATH")
    version = subprocess.run([openssl, "version"], capture_output=True, text=True).stdout.strip()
    print(f"crosscheck against {version} ({openssl})")
    failures = 0
    counts = {"keys": 0, "signatures": 0, "verified": 0, "rejected": 0}

    with tempfile.TemporaryDirectory() as tmp:
        def path(name: str) -> str:
            return os.path.join(tmp, name)

        def run(args: list[str]) -> subprocess.CompletedProcess:
            return subprocess.run([openssl, *args], capture_output=True)

        def write(name: str, data: bytes) -> str:
            with open(path(name), "wb") as f:
                f.write(data)
            return path(name)

        def read(name: str) -> bytes:
            with open(path(name), "rb") as f:
                return f.read()

        def ctx_opts(ctx: bytes) -> list[str]:
            return ["-pkeyopt", f"hexcontext-string:{ctx.hex()}"] if ctx else []

        def spki(p: Params, pk: bytes, prefix: bytes) -> str:
            der = write("pub.der", prefix + pk)
            out = run(["pkey", "-pubin", "-inform", "DER", "-in", der, "-out", path("pub.pem")])
            assert out.returncode == 0, out.stderr
            return path("pub.pem")

        def ossl_verify(pub: str, msg: bytes, sig: bytes, ctx: bytes) -> bool:
            out = run(["pkeyutl", "-verify", "-pubin", "-inkey", pub, "-rawin",
                       "-in", write("msg.bin", msg), "-sigfile", write("sig.bin", sig),
                       *ctx_opts(ctx)])
            return out.returncode == 0

        for set_name, p in SETS.items():
            alg = p.name
            prefix = None
            for key_index in range(KEYS_PER_SET):
                xi, pk, _ = key_for(set_name, key_index)
                out = run(["genpkey", "-algorithm", alg, "-pkeyopt", f"hexseed:{xi.hex()}",
                           "-out", path(f"sk{key_index}.pem")])
                assert out.returncode == 0, out.stderr
                out = run(["pkey", "-in", path(f"sk{key_index}.pem"), "-pubout", "-outform",
                           "DER", "-out", path("pk.der")])
                der = read("pk.der")
                prefix = der[:-p.pk_bytes]
                counts["keys"] += 1
                if der[-p.pk_bytes:] != pk:
                    failures += 1
                    print(f"  {alg} key {key_index}: public key MISMATCH")
            for v in differential_vectors(set_name):
                opts = (["-pkeyopt", "deterministic:1"] if v.rnd == DETERMINISTIC
                        else ["-pkeyopt", f"hextest-entropy:{v.rnd.hex()}"])
                out = run(["pkeyutl", "-sign", "-inkey", path(f"sk{v.key_index}.pem"), "-rawin",
                           "-in", write("msg.bin", v.msg), "-out", path("osig.bin"),
                           *opts, *ctx_opts(v.ctx)])
                assert out.returncode == 0, out.stderr
                counts["signatures"] += 1
                if read("osig.bin") != v.sig:
                    failures += 1
                    print(f"  {alg} vector {v.index}: signature MISMATCH")
                pub = spki(p, v.pk, prefix)
                if ossl_verify(pub, v.msg, v.sig, v.ctx):
                    counts["verified"] += 1
                else:
                    failures += 1
                    print(f"  {alg} vector {v.index}: OpenSSL rejects the reference signature")
            # OpenSSL must reject mutated signatures; a sample of the negative corpus kinds.
            base = differential_vectors(set_name)[0]
            pub = spki(p, base.pk, prefix)
            mutations = [(base.msg + b"\0", base.sig, base.ctx),
                         (base.msg, base.sig, base.ctx + b"\0")]
            for bit in (0, 8 * p.ctilde_bytes + 3, 8 * hint_region(p) + 1,
                        8 * (p.sig_bytes - 1) + 7):
                bad = bytearray(base.sig)
                bad[bit // 8] ^= 1 << (bit % 8)
                mutations.append((base.msg, bytes(bad), base.ctx))
            for msg, sig, ctx in mutations:
                if ossl_verify(pub, msg, sig, ctx):
                    failures += 1
                    print(f"  {alg}: OpenSSL accepts a mutation the reference rejects")
                else:
                    counts["rejected"] += 1
    print(f"  {counts['keys']} keys and {counts['signatures']} signatures compared byte for "
          f"byte; OpenSSL verified {counts['verified']} reference signatures and rejected "
          f"{counts['rejected']} mutations")
    print("crosscheck passed" if failures == 0 else f"crosscheck FAILED ({failures})")
    sys.exit(1 if failures else 0)


def main() -> None:
    commands = {"selftest": cmd_selftest, "vectors": cmd_vectors, "negative": cmd_negative,
                "crosscheck": cmd_crosscheck}
    if len(sys.argv) != 2 or sys.argv[1] not in commands:
        sys.exit(f"usage: {sys.argv[0]} {{{','.join(commands)}}}")
    commands[sys.argv[1]]()


if __name__ == "__main__":
    main()
