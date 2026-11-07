//! ML-DSA verification tests (CLAUDE.md §59; ONBF §13 items 1, 2 and 4).
//!
//! The oracle is `tools/mldsa_reference.py`, an independent pure-Python FIPS 204
//! implementation (KeyGen, deterministic and hedged Sign, Verify) with a recursive CRT NTT,
//! big-integer bit packing and a materialized Â, sharing no code with this crate. Its output
//! was compared byte for byte with OpenSSL 4.1.0's ML-DSA (`mldsa_reference.py crosscheck`:
//! every key from its seed and every differential signature identical, and OpenSSL verifies
//! them all); that comparison is a development check and no test here depends on OpenSSL.
//! - Differential: 52 signatures per parameter set in `tests/vectors/mldsa-differential.txt`
//!   (13 keys, messages of 0–4096 bytes, contexts of 0–255 bytes including the five ONBF
//!   role contexts, deterministic and hedged).
//! - Negative corpus: `tests/vectors/mldsa-negative.txt`, single-bit flips over c̃, z and the
//!   whole hint encoding plus constructed cases (every kind of malformed hint and the order
//!   of the checks, the z bound at γ1 − β − 1 and γ1 − β, contexts, messages and keys,
//!   including keys of the other parameter set); each must give exactly the reference's
//!   verdict.
//! - Properties: random mutations never verify; contexts over 255 bytes are refused first.
//! - Internals: the field against integer arithmetic, the zeta table and 256^−1 from ζ =
//!   1753, NTT against its evaluation definition, its inverse, and schoolbook negacyclic
//!   multiplication; Decompose and UseHint exhaustively over Z_q against their definitions;
//!   the hint lemma; Power2Round's range (why t1·2^d needs no reduction); SampleInBall's
//!   shape; the stack budget.

use crate::mldsa::testing::*;
use crate::{
    MLDSA65_PUBLIC_KEY_BYTES, MLDSA65_SIGNATURE_BYTES, MLDSA87_PUBLIC_KEY_BYTES,
    MLDSA87_SIGNATURE_BYTES, MlDsaError, Shake128, mldsa65_verify, mldsa87_verify,
};

fn unhex(s: &str) -> Vec<u8> {
    if s == "-" {
        return Vec::new();
    }
    assert!(s.len().is_multiple_of(2), "odd hex length");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn data_lines(file: &str) -> impl Iterator<Item = &str> {
    file.lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Set {
    MlDsa65,
    MlDsa87,
}

impl Set {
    fn parse(s: &str) -> Set {
        match s {
            "65" => Set::MlDsa65,
            "87" => Set::MlDsa87,
            other => panic!("unknown parameter set {other}"),
        }
    }
    fn pk_bytes(self) -> usize {
        match self {
            Set::MlDsa65 => MLDSA65_PUBLIC_KEY_BYTES,
            Set::MlDsa87 => MLDSA87_PUBLIC_KEY_BYTES,
        }
    }
    fn sig_bytes(self) -> usize {
        match self {
            Set::MlDsa65 => MLDSA65_SIGNATURE_BYTES,
            Set::MlDsa87 => MLDSA87_SIGNATURE_BYTES,
        }
    }
}

/// Verifies with the public function of `set`; the lengths must be that set's.
fn verify(set: Set, pk: &[u8], msg: &[u8], ctx: &[u8], sig: &[u8]) -> Result<(), MlDsaError> {
    match set {
        Set::MlDsa65 => mldsa65_verify(pk.try_into().unwrap(), msg, ctx, sig.try_into().unwrap()),
        Set::MlDsa87 => mldsa87_verify(pk.try_into().unwrap(), msg, ctx, sig.try_into().unwrap()),
    }
}

#[derive(Clone)]
struct Vector {
    set: Set,
    index: usize,
    key: usize,
    hedged: bool,
    pk: Vec<u8>,
    ctx: Vec<u8>,
    msg: Vec<u8>,
    sig: Vec<u8>,
}

const DIFFERENTIAL: &str = include_str!("../tests/vectors/mldsa-differential.txt");
const NEGATIVE: &str = include_str!("../tests/vectors/mldsa-negative.txt");

fn differential() -> Vec<Vector> {
    let mut keys = std::collections::HashMap::new();
    let mut vectors = Vec::new();
    for line in data_lines(DIFFERENTIAL) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        match cols[0] {
            "key" => {
                assert_eq!(cols.len(), 5, "{line}");
                let set = Set::parse(cols[1]);
                let pk = unhex(cols[4]);
                assert_eq!(pk.len(), set.pk_bytes());
                assert_eq!(unhex(cols[3]).len(), 32, "xi");
                keys.insert((set, cols[2].parse::<usize>().unwrap()), pk);
            }
            "sig" => {
                assert_eq!(cols.len(), 8, "{}", &line[..40]);
                let set = Set::parse(cols[1]);
                let key = cols[3].parse().unwrap();
                let sig = unhex(cols[7]);
                assert_eq!(sig.len(), set.sig_bytes());
                vectors.push(Vector {
                    set,
                    index: cols[2].parse().unwrap(),
                    key,
                    hedged: cols[4] != "-",
                    pk: keys[&(set, key)].clone(),
                    ctx: unhex(cols[5]),
                    msg: unhex(cols[6]),
                    sig,
                });
            }
            kind => panic!("unknown line kind {kind}"),
        }
    }
    vectors
}

fn vector(vectors: &[Vector], set: Set, index: usize) -> &Vector {
    let v = vectors
        .iter()
        .find(|v| v.set == set && v.index == index)
        .unwrap();
    assert_eq!(v.index, index);
    v
}

fn expected(token: &str) -> Result<(), MlDsaError> {
    use MlDsaError::*;
    Err(match token {
        "ok" => return Ok(()),
        "ContextTooLong" => ContextTooLong,
        "HintCountOutOfRange" => HintCountOutOfRange,
        "HintIndexNotIncreasing" => HintIndexNotIncreasing,
        "HintPaddingNonZero" => HintPaddingNonZero,
        "ZOutOfRange" => ZOutOfRange,
        "ChallengeMismatch" => ChallengeMismatch,
        other => panic!("unknown expectation {other}"),
    })
}

#[test]
fn differential_vectors_verify() {
    let vectors = differential();
    for set in [Set::MlDsa65, Set::MlDsa87] {
        let of_set: Vec<&Vector> = vectors.iter().filter(|v| v.set == set).collect();
        assert_eq!(of_set.len(), 52);
        for v in &of_set {
            assert_eq!(
                verify(set, &v.pk, &v.msg, &v.ctx, &v.sig),
                Ok(()),
                "{set:?} vector {}",
                v.index
            );
        }
        // Coverage the generator promises.
        let has = |f: &dyn Fn(&&Vector) -> bool| of_set.iter().any(f);
        assert!(has(&|v| v.msg.is_empty() && v.ctx.is_empty()));
        assert!(has(&|v| v.msg.is_empty() && v.ctx.len() == 255));
        assert!(has(&|v| v.msg.len() == 4096 && v.ctx.len() == 255));
        assert!(has(&|v| v.hedged) && has(&|v| !v.hedged));
        assert_eq!(of_set.iter().map(|v| v.key).max(), Some(12));
        for role in [
            "publisher",
            "notary",
            "origin-driver-attest",
            "origin-kernel",
            "origin-release",
        ] {
            let ctx = format!("origin/onbf/1/{role}");
            assert!(has(&|v| v.ctx == ctx.as_bytes()), "{role}");
        }
    }
}

/// Applies one negative-corpus edit list (see the file header) to a copy of `base`.
fn apply_edits(base: &Vector, edits: &str) -> Vector {
    let mut v = base.clone();
    if edits == "none" {
        return v;
    }
    for edit in edits.split(',') {
        let (target, value) = edit.split_at(3);
        match (target, &value[..1]) {
            ("sig", "^") | ("sig", "=") => {
                let (offset, hex) = value[1..].split_once(':').unwrap();
                let offset: usize = offset.parse().unwrap();
                for (i, byte) in unhex(hex).into_iter().enumerate() {
                    let slot = &mut v.sig[offset + i];
                    if value.starts_with('^') {
                        *slot ^= byte;
                    } else {
                        *slot = byte;
                    }
                }
            }
            ("ctx", "=") => v.ctx = unhex(&value[1..]),
            ("msg", "=") => v.msg = unhex(&value[1..]),
            ("pk=", _) => v.pk = unhex(&edit[3..]),
            _ => panic!("unknown edit {edit}"),
        }
    }
    v
}

#[test]
fn negative_corpus_gives_the_exact_error() {
    let vectors = differential();
    let mut seen = std::collections::BTreeMap::<(&str, &str), usize>::new();
    for line in data_lines(NEGATIVE) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        assert_eq!(cols[0], "neg");
        assert!(cols.len() >= 6, "{line}");
        let set = Set::parse(cols[1]);
        let base = vector(&vectors, set, cols[2].parse().unwrap());
        let v = apply_edits(base, cols[4]);
        let label = cols[5..].join(" ");
        assert_eq!(
            verify(set, &v.pk, &v.msg, &v.ctx, &v.sig),
            expected(cols[3]),
            "{set:?} {label}"
        );
        *seen.entry((cols[1], cols[3])).or_default() += 1;
    }
    // The corpus is the size the generator wrote, with every error kind for both sets.
    let total: usize = seen.values().sum();
    assert_eq!(total, NEGATIVE_CASES, "{seen:?}");
    for set in ["65", "87"] {
        for kind in [
            "ContextTooLong",
            "HintCountOutOfRange",
            "HintIndexNotIncreasing",
            "HintPaddingNonZero",
            "ZOutOfRange",
            "ChallengeMismatch",
        ] {
            assert!(seen.contains_key(&(set, kind)), "{set} {kind}");
        }
    }
}

/// Data lines in `tests/vectors/mldsa-negative.txt`.
const NEGATIVE_CASES: usize = 2631;

/// A deterministic xorshift64 stream for the property tests.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn coeff(&mut self) -> u32 {
        (self.next() % u64::from(Q)) as u32
    }
    fn poly(&mut self) -> Poly {
        std::array::from_fn(|_| self.coeff())
    }
}

#[test]
fn mutated_inputs_never_verify() {
    let mut rng = Rng(0x0f1b_5204_ae5e_0001);
    for v in differential() {
        for trial in 0..10 {
            let mut m = v.clone();
            let mask = (rng.next() as u8).max(1);
            match trial % 5 {
                0..=2 => {
                    let at = rng.below(m.sig.len());
                    m.sig[at] ^= mask;
                }
                3 => {
                    let at = rng.below(m.pk.len());
                    m.pk[at] ^= mask;
                }
                _ if trial % 2 == 0 && !m.msg.is_empty() => {
                    let at = rng.below(m.msg.len());
                    m.msg[at] ^= mask;
                }
                _ if !m.ctx.is_empty() => {
                    let at = rng.below(m.ctx.len());
                    m.ctx[at] ^= mask;
                }
                _ => m.msg.push(mask),
            }
            assert!(
                verify(v.set, &m.pk, &m.msg, &m.ctx, &m.sig).is_err(),
                "{:?} vector {} trial {trial} still verifies",
                v.set,
                v.index
            );
        }
    }
}

#[test]
fn contexts_over_255_bytes_are_refused_first() {
    let vectors = differential();
    for set in [Set::MlDsa65, Set::MlDsa87] {
        let v = vectors.iter().find(|v| v.set == set).unwrap();
        for len in [256, 257, 300, 511, 4096] {
            let ctx = vec![0u8; len];
            assert_eq!(
                verify(set, &v.pk, &v.msg, &ctx, &v.sig),
                Err(MlDsaError::ContextTooLong)
            );
        }
        // Even with every other input malformed.
        let bad = vec![0xffu8; set.sig_bytes()];
        assert_eq!(
            verify(set, &v.pk, &v.msg, &[1; 256], &bad),
            Err(MlDsaError::ContextTooLong)
        );
    }
}

#[test]
fn error_debug_names_the_check_only() {
    assert_eq!(format!("{:?}", MlDsaError::ZOutOfRange), "ZOutOfRange");
    assert_eq!(
        format!("{:?}", MlDsaError::ChallengeMismatch),
        "ChallengeMismatch"
    );
}

// ---------------------------------------------------------------------------------------
// Parameters and constants

/// FIPS 204 Table 1 relations and the Table 2 sizes, from their definitions.
#[test]
fn parameter_sets_match_their_definitions() {
    fn check<P: ParameterSet>(k: usize, l: usize, pk: usize, sig: usize) {
        let bitlen = |x: u32| 32 - x.leading_zeros() as usize;
        assert_eq!((P::K, P::L), (k, l));
        assert_eq!(P::BETA, P::TAU as u32 * P::ETA);
        assert_eq!(P::CTILDE_BYTES, P::LAMBDA / 4);
        assert_eq!(P::GAMMA2, (Q - 1) / 32);
        assert_eq!(P::GAMMA1, 1 << 19);
        let d = 13;
        assert_eq!(T1_ROW_BYTES, 32 * (bitlen(Q - 1) - d));
        assert_eq!(Z_POLY_BYTES, 32 * (1 + bitlen(P::GAMMA1 - 1)));
        assert_eq!(W1_POLY_BYTES, 32 * bitlen((Q - 1) / (2 * P::GAMMA2) - 1));
        assert_eq!(P::PUBLIC_KEY_BYTES, 32 + 32 * k * (bitlen(Q - 1) - d));
        assert_eq!(
            P::SIGNATURE_BYTES,
            P::LAMBDA / 4 + l * 32 * (1 + bitlen(P::GAMMA1 - 1)) + P::OMEGA + k
        );
        assert_eq!((P::PUBLIC_KEY_BYTES, P::SIGNATURE_BYTES), (pk, sig));
    }
    check::<MlDsa65>(6, 5, 1952, 3309);
    check::<MlDsa87>(8, 7, 2592, 4627);
    assert_eq!(
        (MlDsa65::TAU, MlDsa65::ETA, MlDsa65::OMEGA, MlDsa65::LAMBDA),
        (49, 4, 55, 192)
    );
    assert_eq!(
        (MlDsa87::TAU, MlDsa87::ETA, MlDsa87::OMEGA, MlDsa87::LAMBDA),
        (60, 2, 75, 256)
    );
}

/// `x^e mod q` by repeated multiplication in `u64`, independent of the crate's `pow_mod`.
fn pow_slow(x: u64, e: u32) -> u64 {
    (0..e).fold(1, |acc, _| acc * x % u64::from(Q))
}

#[test]
fn constants_rederived_from_their_definitions() {
    // q = 2^23 − 2^13 + 1 is prime (trial division).
    assert_eq!(Q, (1 << 23) - (1 << 13) + 1);
    assert!((2..=2895u32).all(|d| !Q.is_multiple_of(d)) && 2896 * 2896 > Q);
    // ζ = 1753 is a primitive 512th root of unity: ζ^512 = 1 and ζ^256 = −1 (so its order
    // divides 512 but not 256).
    let z = u64::from(ZETA);
    assert_eq!(pow_slow(z, 512), 1);
    assert_eq!(pow_slow(z, 256), u64::from(Q - 1));
    // The table: zetas[m] = ζ^BitRev8(m), from a power table built by multiplication.
    let mut powers = vec![1u64; 256];
    for e in 1..256 {
        powers[e] = powers[e - 1] * z % u64::from(Q);
    }
    for (m, &zeta) in ZETAS.iter().enumerate() {
        assert_eq!(
            u64::from(zeta),
            powers[usize::from((m as u8).reverse_bits())],
            "zetas[{m}]"
        );
    }
    assert_eq!(ZETAS[0], 1);
    // f = 256^−1 mod q.
    assert_eq!(u64::from(N_INV) * 256 % u64::from(Q), 1);
}

#[test]
fn field_matches_integer_arithmetic() {
    let q = u64::from(Q);
    let mut edges = vec![0, 1, q - 1, q, q + 1, 2 * q - 1, 2 * q, (q - 1) * (q - 1)];
    edges.extend([
        u64::MAX,
        u64::MAX - 1,
        u64::MAX / q * q,
        u64::MAX / q * q - 1,
    ]);
    edges.extend((0..64).map(|b| 1u64 << b));
    let mut rng = Rng(0x0f1e_1d00_0838_0417);
    edges.extend((0..100_000).map(|_| rng.next()));
    edges.extend((0..100_000).map(|_| rng.next() >> 18)); // products of canonical values
    for x in edges {
        assert_eq!(u64::from(reduce(x)), x % q, "reduce({x})");
    }
    for _ in 0..100_000 {
        let (a, b) = (rng.coeff(), rng.coeff());
        let (wa, wb) = (u64::from(a), u64::from(b));
        assert_eq!(u64::from(mul(a, b)), wa * wb % q);
        assert_eq!(u64::from(add(a, b)), (wa + wb) % q);
        assert_eq!(u64::from(sub(a, b)), (wa + q - wb) % q);
    }
    for (a, b) in [(0, 0), (Q - 1, Q - 1), (0, Q - 1), (Q - 1, 0), (1, Q - 1)] {
        assert_eq!(u64::from(add(a, b)), (u64::from(a) + u64::from(b)) % q);
        assert_eq!(u64::from(sub(a, b)), (u64::from(a) + q - u64::from(b)) % q);
        assert_eq!(u64::from(mul(a, b)), u64::from(a) * u64::from(b) % q);
    }
}

// ---------------------------------------------------------------------------------------
// NTT

/// The evaluation definition: NTT(w)[i] = w(ζ^(2·BitRev8(i) + 1)), O(n^2).
fn ntt_by_definition(w: &Poly) -> Poly {
    let q = u64::from(Q);
    let mut powers = vec![1u64; 512];
    for e in 1..512 {
        powers[e] = powers[e - 1] * u64::from(ZETA) % q;
    }
    std::array::from_fn(|i| {
        let root = 2 * usize::from((i as u8).reverse_bits()) + 1;
        let sum = w
            .iter()
            .enumerate()
            .map(|(j, &c)| u64::from(c) * powers[root * j % 512] % q)
            .sum::<u64>();
        (sum % q) as u32
    })
}

/// a·b in Z_q[X]/(X^256 + 1) by schoolbook multiplication: X^256 = −1.
fn mul_schoolbook(a: &Poly, b: &Poly) -> Poly {
    let q = u64::from(Q);
    let mut c = [0u64; 2 * N];
    for (i, &x) in a.iter().enumerate() {
        for (j, &y) in b.iter().enumerate() {
            c[i + j] = (c[i + j] + u64::from(x) * u64::from(y)) % q;
        }
    }
    std::array::from_fn(|i| ((c[i] + q - c[i + N]) % q) as u32)
}

fn sample_polys() -> Vec<Poly> {
    let mut rng = Rng(0x0e77_0000_1753_0001);
    let mut out = vec![
        [0; N],
        [Q - 1; N],
        [1; N],
        std::array::from_fn(|i| i as u32),
    ];
    let mut x = [0; N];
    x[1] = 1;
    out.push(x);
    let mut top = [0; N];
    top[N - 1] = Q - 1;
    out.push(top);
    out.extend((0..40).map(|_| rng.poly()));
    out
}

#[test]
fn ntt_matches_its_definition_and_inverts() {
    for (i, w) in sample_polys().iter().enumerate() {
        let mut hat = *w;
        ntt(&mut hat);
        if i < 12 {
            assert_eq!(hat, ntt_by_definition(w), "poly {i}");
        }
        let mut back = hat;
        inverse_ntt(&mut back);
        assert_eq!(&back, w, "NTT⁻¹(NTT(w)) = w, poly {i}");
        let mut again = *w;
        inverse_ntt(&mut again);
        ntt(&mut again);
        assert_eq!(&again, w, "NTT(NTT⁻¹(w)) = w, poly {i}");
        assert!(hat.iter().all(|&c| c < Q));
    }
    // NTT(1) = 1 at every root, and NTT(X)[i] is the root itself.
    let mut one = [0; N];
    one[0] = 1;
    ntt(&mut one);
    assert_eq!(one, [1; N]);
}

#[test]
fn ntt_multiplication_is_negacyclic_convolution() {
    let polys = sample_polys();
    for (i, pair) in polys.windows(2).enumerate().take(30) {
        let (mut a, mut b) = (pair[0], pair[1]);
        ntt(&mut a);
        ntt(&mut b);
        let mut c: Poly = std::array::from_fn(|j| mul(a[j], b[j]));
        inverse_ntt(&mut c);
        assert_eq!(c, mul_schoolbook(&pair[0], &pair[1]), "pair {i}");
    }
    // X^255 · X = X^256 = −1.
    let (mut a, mut b) = ([0; N], [0; N]);
    a[N - 1] = 1;
    b[1] = 1;
    ntt(&mut a);
    ntt(&mut b);
    let mut c: Poly = std::array::from_fn(|j| mul(a[j], b[j]));
    inverse_ntt(&mut c);
    let mut minus_one = [0; N];
    minus_one[0] = Q - 1;
    assert_eq!(c, minus_one);
}

// ---------------------------------------------------------------------------------------
// Rounding

/// r mod± α: the representative in (−α/2, α/2] (FIPS 204 §2.3).
fn mod_pm(r: i64, alpha: i64) -> i64 {
    let r = r.rem_euclid(alpha);
    if r > alpha / 2 { r - alpha } else { r }
}

/// Algorithm 36, transcribed with signed integers.
fn decompose_by_definition(r: u32, gamma2: i64) -> (i64, i64) {
    let rp = i64::from(r).rem_euclid(i64::from(Q));
    let r0 = mod_pm(rp, 2 * gamma2);
    if rp - r0 == i64::from(Q) - 1 {
        (0, r0 - 1)
    } else {
        ((rp - r0) / (2 * gamma2), r0)
    }
}

/// Algorithm 40, transcribed.
fn use_hint_by_definition(h: bool, r: u32, gamma2: i64) -> i64 {
    let m = (i64::from(Q) - 1) / (2 * gamma2);
    let (r1, r0) = decompose_by_definition(r, gamma2);
    match (h, r0 > 0) {
        (true, true) => (r1 + 1).rem_euclid(m),
        (true, false) => (r1 - 1).rem_euclid(m),
        (false, _) => r1,
    }
}

/// Decompose and UseHint agree with their definitions on every element of Z_q.
#[test]
fn decompose_and_use_hint_match_their_definitions_exhaustively() {
    let g2 = i64::from(MlDsa65::GAMMA2);
    assert_eq!(MlDsa65::GAMMA2, MlDsa87::GAMMA2);
    for r in 0..Q {
        let (r1, r0) = decompose::<MlDsa65>(r);
        let want = decompose_by_definition(r, g2);
        assert_eq!((i64::from(r1), i64::from(r0)), want, "Decompose({r})");
        assert!(r1 < 16 && i64::from(r0) >= -g2 && i64::from(r0) <= g2);
        for h in [false, true] {
            assert_eq!(
                i64::from(use_hint::<MlDsa65>(h, r)),
                use_hint_by_definition(h, r, g2),
                "UseHint({h}, {r})"
            );
        }
    }
    // The wrap-around case: r − r0 = q − 1 gives r1 = 0 and r0 − 1.
    assert_eq!(decompose::<MlDsa65>(Q - 1), (0, -1));
}

/// FIPS 204's hint lemma: UseHint(MakeHint(z, r), r) = HighBits(r + z) when ‖z‖∞ ≤ γ2, with
/// MakeHint(z, r) = [HighBits(r) ≠ HighBits(r + z)] (Algorithm 39).
#[test]
fn hint_lemma_holds() {
    let g2 = MlDsa65::GAMMA2;
    let high = |r: u32| decompose::<MlDsa65>(r).0;
    let mut rng = Rng(0x41_6c67_3339_0040);
    for i in 0..300_000 {
        let r = if i < 2000 {
            (Q - 1000 + i) % Q
        } else {
            rng.coeff()
        };
        let z = (rng.next() % u64::from(2 * g2 + 1)) as u32; // z + γ2 ∈ [0, 2γ2]
        let rz = add(r, sub(z, g2)); // r + (z − γ2) mod q
        let hint = high(r) != high(rz);
        assert_eq!(use_hint::<MlDsa65>(hint, r), high(rz), "r {r}, z {z}");
    }
}

/// Power2Round (Algorithm 35) over all of Z_q: t1 ∈ [0, 1023], so the 10-bit pk fields hold
/// every t1 a key can have, and t1·2^13 ≤ q − 1. That is why `t1_times_2d` needs no
/// reduction.
#[test]
fn power2round_range_justifies_unreduced_t1() {
    let mut max_t1 = 0;
    for r in 0..Q {
        let r0 = mod_pm(i64::from(r), 1 << 13);
        let t1 = (i64::from(r) - r0) >> 13;
        assert!((0..1024).contains(&t1) && t1 * (1 << 13) + r0 == i64::from(r));
        max_t1 = max_t1.max(t1);
    }
    assert_eq!(max_t1, 1023);
    assert_eq!(1023 * (1 << 13), Q - 1);
    let mut out = [0; N];
    t1_times_2d(&[0xff; T1_ROW_BYTES], &mut out);
    assert_eq!(out, [Q - 1; N]);
    // Coefficient i occupies bits 10i..10i+10: set only coefficient 5 (bits 50–59).
    let mut row = [0u8; T1_ROW_BYTES];
    row[6] = 0b1111_1100;
    row[7] = 0b0000_1111;
    t1_times_2d(&row, &mut out);
    for (i, &c) in out.iter().enumerate() {
        assert_eq!(c, if i == 5 { 1023 << 13 } else { 0 }, "coefficient {i}");
    }
}

#[test]
fn w1_encoding_packs_hinted_high_bits() {
    let mut rng = Rng(0x0077_1e2c_0de0_0001);
    for _ in 0..50 {
        let w = rng.poly();
        let hints = [rng.next(), rng.next(), rng.next(), rng.next()];
        let mut packed = [0u8; W1_POLY_BYTES];
        w1_encode_row::<MlDsa87>(&w, &hints, &mut packed);
        for j in 0..N {
            let h = hints[j / 64] >> (j % 64) & 1 == 1;
            let nibble = packed[j / 2] >> (4 * (j % 2)) & 0xf;
            assert_eq!(
                u32::from(nibble),
                use_hint::<MlDsa87>(h, w[j]),
                "coefficient {j}"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------
// Decoding

/// A z polynomial whose coefficient `at` is `value` and every other is 0, packed as
/// BitPack(z, γ1 − 1, γ1).
fn pack_z(at: usize, value: i64) -> [u8; Z_POLY_BYTES] {
    let gamma1 = 1i64 << 19;
    let mut bits = vec![false; 20 * N];
    for i in 0..N {
        let field = gamma1 - if i == at { value } else { 0 };
        assert!((0..1 << 20).contains(&field));
        for b in 0..20 {
            bits[20 * i + b] = field >> b & 1 == 1;
        }
    }
    std::array::from_fn(|byte| (0..8).fold(0u8, |acc, b| acc | (u8::from(bits[8 * byte + b]) << b)))
}

#[test]
fn z_decoding_and_its_bound() {
    fn check<P: ParameterSet>() {
        let bound = i64::from(P::GAMMA1 - P::BETA);
        let gamma1 = i64::from(P::GAMMA1);
        let mut out = [0; N];
        for at in [0, 1, 127, 128, 254, 255] {
            for value in [0, 1, -1, bound - 1, -(bound - 1), 12345, -12345] {
                decode_z::<P>(&pack_z(at, value), &mut out).unwrap();
                for (i, &c) in out.iter().enumerate() {
                    let want = if i == at { value } else { 0 };
                    assert_eq!(i64::from(c), want.rem_euclid(i64::from(Q)), "z[{i}]");
                }
            }
            for value in [bound, -bound, bound + 1, gamma1, -gamma1 + 1] {
                assert_eq!(
                    decode_z::<P>(&pack_z(at, value), &mut out),
                    Err(MlDsaError::ZOutOfRange),
                    "z[{at}] = {value}"
                );
            }
        }
    }
    check::<MlDsa65>();
    check::<MlDsa87>();
}

/// HintBitUnpack on small hand-made encodings (ω = 75, k = 8), beside the corpus.
#[test]
fn hint_unpacking_rules() {
    const OMEGA: usize = 75;
    let encode = |positions: &[u8], counts: [u8; 8]| {
        let mut y = vec![0u8; OMEGA + 8];
        y[..positions.len()].copy_from_slice(positions);
        y[OMEGA..].copy_from_slice(&counts);
        y
    };
    let unpack = |y: &[u8]| {
        let mut masks = [[0u64; 4]; 8];
        hint_bit_unpack::<MlDsa87, 8>(y, &mut masks).map(|()| masks)
    };
    // Polynomial 0: {3, 200}; polynomial 2: {0, 255}; polynomial 7: {64}.
    let ok = encode(&[3, 200, 0, 255, 64], [2, 2, 4, 4, 4, 4, 4, 5]);
    let masks = unpack(&ok).unwrap();
    assert_eq!(masks[0], [1 << 3, 0, 0, 1 << 8]);
    assert_eq!(masks[1], [0; 4]);
    assert_eq!(masks[2], [1, 0, 0, 1 << 63]);
    assert_eq!(masks[7], [0, 1, 0, 0]);
    // No hints, and exactly ω hints, both decode.
    assert!(unpack(&encode(&[], [0; 8])).is_ok());
    let all: Vec<u8> = (0..OMEGA as u8).collect();
    assert!(unpack(&encode(&all, [75; 8])).is_ok());
    use MlDsaError::*;
    let cases: [(&[u8], [u8; 8], MlDsaError); 7] = [
        (&all, [75, 75, 75, 75, 75, 75, 75, 76], HintCountOutOfRange),
        (&[3, 200], [2, 1, 2, 2, 2, 2, 2, 2], HintCountOutOfRange),
        (&[3, 3], [2; 8], HintIndexNotIncreasing),
        (&[4, 3], [2; 8], HintIndexNotIncreasing),
        (&[3, 200, 1], [2; 8], HintPaddingNonZero),
        // Order: polynomial 0's bad order is found before polynomial 1's bad count.
        (&[4, 3], [2, 1, 2, 2, 2, 2, 2, 2], HintIndexNotIncreasing),
        // And a bad count before any padding.
        (&[3, 200, 1], [2, 1, 2, 2, 2, 2, 2, 2], HintCountOutOfRange),
    ];
    for (positions, counts, want) in cases {
        assert_eq!(
            unpack(&encode(positions, counts)),
            Err(want),
            "{positions:?} {counts:?}"
        );
    }
    let mut last_padding = encode(&[3], [1; 8]);
    last_padding[OMEGA - 1] = 1;
    assert_eq!(unpack(&last_padding), Err(HintPaddingNonZero));
}

// ---------------------------------------------------------------------------------------
// Sampling

#[test]
fn sample_in_ball_has_weight_tau_and_uses_all_of_c_tilde() {
    fn check<P: ParameterSet>(vectors: &[Vector], set: Set) {
        for v in vectors.iter().filter(|v| v.set == set) {
            let ctilde = &v.sig[..P::CTILDE_BYTES];
            let mut c = [0; N];
            sample_in_ball::<P>(ctilde, &mut c);
            assert_eq!(c.iter().filter(|&&x| x != 0).count(), P::TAU);
            assert!(c.iter().all(|&x| x == 0 || x == 1 || x == Q - 1));
            // FIPS 204 (final) seeds SampleInBall with all λ/4 bytes of c̃.
            let mut changed = ctilde.to_vec();
            *changed.last_mut().unwrap() ^= 1;
            let mut c2 = [0; N];
            sample_in_ball::<P>(&changed, &mut c2);
            assert_ne!(c, c2);
        }
    }
    let vectors = differential();
    check::<MlDsa65>(&vectors, Set::MlDsa65);
    check::<MlDsa87>(&vectors, Set::MlDsa87);
}

/// RejNTTPoly three bytes at a time, as Algorithm 30 is written.
fn rej_ntt_poly_by_definition(seed: &[u8; 34]) -> Poly {
    let mut g = Shake128::new();
    g.update(seed);
    let mut xof = g.finalize_xof();
    let mut out = [0; N];
    let mut j = 0;
    while j < N {
        let mut s = [0u8; 3];
        xof.squeeze(&mut s);
        let mut b2 = u32::from(s[2]);
        if b2 > 127 {
            b2 -= 128;
        }
        let z = (b2 << 16) + (u32::from(s[1]) << 8) + u32::from(s[0]);
        if z < Q {
            out[j] = z;
            j += 1;
        }
    }
    out
}

#[test]
fn streamed_matrix_entries_match_rej_ntt_poly() {
    let rho: [u8; 32] = std::array::from_fn(|i| i as u8 * 7 + 1);
    let mut rng = Rng(0x0e8a_0d00_0032);
    for (row, column) in [(0u8, 0u8), (0, 1), (1, 0), (7, 6), (5, 3)] {
        let mut seed = [0u8; 34];
        seed[..32].copy_from_slice(&rho);
        seed[32] = column;
        seed[33] = row;
        let entry = rej_ntt_poly_by_definition(&seed);
        // acc + Â[row][column] ∘ ẑ, from a random acc and ẑ.
        let (acc0, z) = (rng.poly(), rng.poly());
        let mut acc = acc0;
        add_matrix_entry_product(&mut acc, &rho, row, column, &z);
        for j in 0..N {
            let want = (u64::from(acc0[j]) + u64::from(entry[j]) * u64::from(z[j])) % 8380417;
            assert_eq!(u64::from(acc[j]), want, "({row}, {column})[{j}]");
        }
    }
}

// ---------------------------------------------------------------------------------------
// Stack and throughput

/// Runs every differential vector of both parameter sets on a thread created with
/// `stack_size(16 KiB)`, the smallest size the M1 host accepts (`PTHREAD_STACK_MIN` on macOS
/// arm64; Rust raises smaller requests to it). A larger peak overflows the guard page and
/// aborts the test process. macOS gives such a thread about 27.5 KiB in practice (measured
/// by `verify_peak_stack_measured`), so on this host the bound proved here is that one;
/// the calibrated measurement below gives the peak itself.
#[test]
fn verify_runs_on_a_16_kib_thread() {
    const STACK_BYTES: usize = 16 * 1024;
    let vectors = differential();
    let thread = std::thread::Builder::new()
        .stack_size(STACK_BYTES)
        .spawn(move || {
            vectors
                .iter()
                .filter(|v| verify(v.set, &v.pk, &v.msg, &v.ctx, &v.sig).is_ok())
                .count()
        })
        .unwrap();
    assert_eq!(thread.join().unwrap(), 104);
}

/// Set in a child process of `verify_peak_stack_measured` to `<workload>:<levels>`.
const STACK_PROBE_ENV: &str = "ORIGIN_MLDSA_STACK_PROBE";

/// `levels` nested frames, each holding a live 128-byte array, then `f`. Records the
/// deepest array address, so the padding's real size is measured, not assumed.
#[inline(never)]
fn burn(levels: usize, deepest: &std::cell::Cell<usize>, f: &dyn Fn() -> bool) -> bool {
    let mut pad = [0u8; 128];
    std::hint::black_box(&mut pad);
    deepest.set(deepest.get().min(pad.as_ptr() as usize));
    let ok = if levels == 0 {
        f()
    } else {
        burn(levels - 1, deepest, f)
    };
    std::hint::black_box(&mut pad);
    ok
}

/// The child's side: on a fresh thread, `levels` of padding and then the workload (nothing,
/// or every vector of one set through its public function); prints the padding's depth. A
/// stack overflow aborts the child instead.
fn stack_probe_child(spec: &str) {
    let (workload, levels) = spec.split_once(':').unwrap();
    let levels: usize = levels.parse().unwrap();
    let workload = workload.to_string();
    let vectors = differential();
    let (ok, depth) = std::thread::Builder::new()
        .stack_size(64 * 1024)
        .spawn(move || {
            let top = 0u8;
            let top = std::hint::black_box(&top) as *const u8 as usize;
            let deepest = std::cell::Cell::new(usize::MAX);
            let work = || match workload.as_str() {
                "none" => true,
                "65" => vectors.iter().filter(|v| v.set == Set::MlDsa65).all(|v| {
                    let (pk, sig) = (v.pk.as_slice().try_into(), v.sig.as_slice().try_into());
                    mldsa65_verify(pk.unwrap(), &v.msg, &v.ctx, sig.unwrap()).is_ok()
                }),
                "87" => vectors.iter().filter(|v| v.set == Set::MlDsa87).all(|v| {
                    let (pk, sig) = (v.pk.as_slice().try_into(), v.sig.as_slice().try_into());
                    mldsa87_verify(pk.unwrap(), &v.msg, &v.ctx, sig.unwrap()).is_ok()
                }),
                other => panic!("unknown workload {other}"),
            };
            let ok = burn(levels, &deepest, &work);
            (ok, top - deepest.get())
        })
        .unwrap()
        .join()
        .unwrap();
    println!("STACK-PROBE {ok} {depth}");
}

/// Runs the probe in a child process; the padding depth in bytes if the workload completed.
fn run_stack_probe(workload: &str, levels: usize) -> Option<usize> {
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "--nocapture", "--test-threads=1"])
        .arg("tests_mldsa::verify_peak_stack_measured")
        .env(STACK_PROBE_ENV, format!("{workload}:{levels}"))
        .output()
        .unwrap();
    // libtest prints "test <name> ... " before the test's own output, on the same line.
    let stdout = String::from_utf8_lossy(&out.stdout);
    let report = stdout.split("STACK-PROBE ").nth(1)?;
    let cols: Vec<&str> = report.split_whitespace().collect();
    (out.status.success() && cols[0] == "true").then(|| cols[1].parse().unwrap())
}

/// The deepest padding (bytes) under which `workload` completes, and the shallowest under
/// which it does not: a doubling search, then bisection over the padding levels.
fn padding_limits(workload: &str) -> (usize, usize) {
    let (mut ok, mut bad) = (0, 1);
    while run_stack_probe(workload, bad).is_some() {
        (ok, bad) = (bad, bad * 2);
        assert!(bad < 4096, "{workload}: no overflow found");
    }
    assert!(
        run_stack_probe(workload, ok).is_some(),
        "{workload} fails without padding"
    );
    while bad - ok > 1 {
        let mid = (ok + bad) / 2;
        if run_stack_probe(workload, mid).is_some() {
            ok = mid;
        } else {
            bad = mid;
        }
    }
    // The failing depth is the passing one plus one level, measured from passing runs.
    let ok_depth = run_stack_probe(workload, ok).unwrap();
    let level = match ok {
        0 => 0,
        _ => ok_depth - run_stack_probe(workload, ok - 1).unwrap(),
    };
    (ok_depth, ok_depth + level)
}

/// Measures the peak stack of `mldsa65_verify` and `mldsa87_verify` (with the closure that
/// calls them) without trusting the thread size the OS reports. Each probe runs in a child
/// process, which either completes or aborts on a stack overflow. With U the stack below
/// the probe thread's first frame, a workload of peak V completes under padding p exactly
/// when p + V ≤ U, so bisecting p for an empty workload and for each verifier bounds V to
/// within one padding level, whatever U is. The assertion is the budget: 24 KiB, inside
/// the Framekernel's 64 KiB stack. Run with
/// `cargo test -p origin-crypto --release -- --ignored --nocapture verify_peak_stack_measured`.
#[test]
#[ignore = "spawns child processes; run explicitly in release mode"]
fn verify_peak_stack_measured() {
    if let Ok(spec) = std::env::var(STACK_PROBE_ENV) {
        stack_probe_child(&spec);
        return;
    }
    let (none_ok, none_bad) = padding_limits("none");
    println!("usable stack below the probe frame: {none_ok} to {none_bad} bytes");
    for set in ["65", "87"] {
        let (ok, bad) = padding_limits(set);
        // none_ok ≤ U < none_bad and ok + V ≤ U < bad + V give none_ok − bad < V < none_bad − ok.
        let (low, high) = (none_ok.saturating_sub(bad), none_bad - ok);
        println!("ML-DSA-{set} verify peak stack: more than {low} and less than {high} bytes");
        assert!(
            high <= 24 * 1024,
            "ML-DSA-{set}: {high} bytes exceeds the 24 KiB budget"
        );
    }
}

/// Verifications per second on the build host. Indicative only: the target is x86-64.
/// Run with
/// `cargo test -p origin-crypto --release -- --ignored --nocapture mldsa_throughput`.
#[test]
#[ignore = "benchmark; run explicitly in release mode"]
fn mldsa_throughput() {
    use std::hint::black_box;
    use std::time::Instant;
    let vectors = differential();
    for set in [Set::MlDsa65, Set::MlDsa87] {
        let of_set: Vec<&Vector> = vectors
            .iter()
            .filter(|v| v.set == set && v.msg.len() <= 256)
            .collect();
        let start = Instant::now();
        let mut count = 0u64;
        while start.elapsed().as_secs_f64() < 3.0 {
            for v in &of_set {
                verify(
                    set,
                    black_box(&v.pk),
                    black_box(&v.msg),
                    black_box(&v.ctx),
                    black_box(&v.sig),
                )
                .unwrap();
                count += 1;
            }
        }
        let secs = start.elapsed().as_secs_f64();
        println!(
            "{set:?} verify: {:.0} verifications/s ({count} in {secs:.2} s, messages of 0-256 bytes)",
            count as f64 / secs
        );
    }
}
