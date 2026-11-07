//! Ed25519 verification tests (CLAUDE.md §59; ONBF §13 items 1, 2 and 4).
//!
//! The oracle is `tools/ed25519_reference.py`, an independent pure-Python RFC 8032
//! implementation: affine big-integer arithmetic, Euler's-criterion square roots and
//! hashlib's SHA-512, sharing no formulas with this crate.
//! - Known answers: RFC 8032 §7.1 TEST 1, 2, 3 and SHA(abc). Each was embedded only after
//!   the reference reproduced its public key and signature exactly from the RFC's secret
//!   key and message (`ed25519_reference.py selftest`).
//! - Differential: 300 reference signatures in `tests/vectors/ed25519-differential.txt`.
//! - Negative corpus: `tests/vectors/ed25519-negative.txt`, 1,608 single-bit mutations and
//!   157 constructed cases; each must give exactly the reference's verdict.
//! - Properties: valid signatures verify; mutated ones and S + L never do.
//! - Constants: every generated value is re-derived here from its definition.
//! - Internals: field, scalar and group operations against simpler definitions.

use crate::ed25519::constants::{BASE_T, BASE_X, BASE_Y, D, D2, FOUR_P, L, SQRT_M1};
use crate::ed25519::field::Fe;
use crate::ed25519::point::Point;
use crate::ed25519::scalar;
use crate::{Ed25519Error, Sha512, ed25519_verify as verify};

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

fn arr<const N: usize>(s: &str) -> [u8; N] {
    unhex(s).try_into().unwrap()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

struct Vector {
    pk: [u8; 32],
    msg: Vec<u8>,
    sig: [u8; 64],
}

fn data_lines(file: &str) -> impl Iterator<Item = &str> {
    file.lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
}

fn differential() -> Vec<Vector> {
    data_lines(include_str!("../tests/vectors/ed25519-differential.txt"))
        .map(|line| {
            let cols: Vec<&str> = line.split_whitespace().collect();
            assert_eq!(cols.len(), 3, "{line}");
            Vector {
                pk: arr(cols[0]),
                msg: unhex(cols[1]),
                sig: arr(cols[2]),
            }
        })
        .collect()
}

const NEGATIVE: &str = include_str!("../tests/vectors/ed25519-negative.txt");

fn expected(token: &str) -> Result<(), Ed25519Error> {
    use Ed25519Error::*;
    Err(match token {
        "ok" => return Ok(()),
        "PublicKeyNonCanonical" => PublicKeyNonCanonical,
        "PublicKeyNotOnCurve" => PublicKeyNotOnCurve,
        "PublicKeySmallOrder" => PublicKeySmallOrder,
        "RNonCanonical" => RNonCanonical,
        "RNotOnCurve" => RNotOnCurve,
        "RSmallOrder" => RSmallOrder,
        "SNonCanonical" => SNonCanonical,
        "EquationFailed" => EquationFailed,
        other => panic!("unknown expectation {other}"),
    })
}

/// RFC 8032 §7.1: (name, public key, message, signature).
const RFC8032: [(&str, &str, &str, &str); 4] = [
    (
        "TEST 1",
        "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
        "-",
        "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
    ),
    (
        "TEST 2",
        "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c",
        "72",
        "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00",
    ),
    (
        "TEST 3",
        "fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025",
        "af82",
        "6291d657deec24024827e69c3abe01a30ce548a284743a445e3680d7db5ac3ac18ff9b538d16f290ae67f760984dc6594a7c15e9716ed28dc027beceea1ec40a",
    ),
    (
        "TEST SHA(abc)",
        "ec172b93ad5e563bf4932c70e1245034c35467ef2efd4d64ebf819683467e2bf",
        "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f",
        "dc2a4459e7369633a52b1bf277839a00201009a3efbf3ecb69bea2186c26b58909351fc9ac90b3ecfdfbc7c66431e0303dca179c138ac17ad9bef1177331a704",
    ),
];

#[test]
fn rfc8032_known_answers() {
    for (name, pk, msg, sig) in RFC8032 {
        let (pk, msg, sig) = (arr(pk), unhex(msg), arr(sig));
        assert_eq!(verify(&pk, &msg, &sig), Ok(()), "{name}");
        let mut longer = msg.clone();
        longer.push(0);
        assert_eq!(
            verify(&pk, &longer, &sig),
            Err(Ed25519Error::EquationFailed),
            "{name} with an extra byte"
        );
    }
    // The SHA(abc) message is SHA-512("abc").
    assert_eq!(hex(&Sha512::digest(b"abc")), RFC8032[3].2);
    // A signature does not verify under another vector's key.
    let (pk2, sig1) = (arr(RFC8032[1].1), arr(RFC8032[0].3));
    assert_eq!(verify(&pk2, &[], &sig1), Err(Ed25519Error::EquationFailed));
}

#[test]
fn differential_vectors_verify() {
    let vectors = differential();
    assert_eq!(vectors.len(), 300);
    for (i, v) in vectors.iter().enumerate() {
        assert_eq!(verify(&v.pk, &v.msg, &v.sig), Ok(()), "vector {i}");
    }
    let lengths: Vec<usize> = vectors.iter().map(|v| v.msg.len()).collect();
    assert!((0..=255).all(|n| lengths.contains(&n)));
    assert_eq!(lengths.iter().max(), Some(&4096));
}

#[test]
fn negative_corpus_gives_the_exact_error() {
    let vectors = differential();
    let (mut mutations, mut constructed) = (0, 0);
    for line in data_lines(NEGATIVE) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        match cols[0] {
            "mut" => {
                let base = &vectors[cols[1].parse::<usize>().unwrap()];
                let bit: usize = cols[3].parse().unwrap();
                let (mut pk, mut msg, mut sig) = (base.pk, base.msg.clone(), base.sig);
                let flip = |bytes: &mut [u8]| bytes[bit / 8] ^= 1 << (bit % 8);
                match cols[2] {
                    "R" => flip(&mut sig[..32]),
                    "S" => flip(&mut sig[32..]),
                    "A" => flip(&mut pk),
                    "M" => flip(&mut msg),
                    field => panic!("unknown field {field}"),
                }
                assert_eq!(verify(&pk, &msg, &sig), expected(cols[4]), "{line}");
                mutations += 1;
            }
            "raw" => {
                assert_eq!(cols.len(), 6, "{line}");
                let (pk, msg, sig) = (arr(cols[2]), unhex(cols[3]), arr(cols[4]));
                assert_eq!(verify(&pk, &msg, &sig), expected(cols[1]), "{}", cols[5]);
                constructed += 1;
            }
            kind => panic!("unknown line kind {kind}"),
        }
    }
    assert_eq!((mutations, constructed), (1608, 157));
}

/// The 8 small-order encodings the reference derived from the group structure, checked
/// independently here: each decodes, has order dividing 8, and the multiples of the
/// order-8 ones give back exactly the same 8 encodings.
#[test]
fn small_order_points_are_the_8_torsion_subgroup() {
    let encodings: Vec<[u8; 32]> = data_lines(NEGATIVE)
        .map(|l| l.split_whitespace().collect::<Vec<_>>())
        .filter(|c| c[0] == "raw" && c[5].starts_with("A:small-order"))
        .map(|c| arr(c[2]))
        .collect();
    assert_eq!(encodings.len(), 8);
    let points: Vec<Point> = encodings
        .iter()
        .map(|e| Point::decode(e).unwrap())
        .collect();
    assert!(points.iter().all(Point::has_small_order));
    let order8: Vec<&Point> = points
        .iter()
        .filter(|p| !p.double().double().equals(&Point::IDENTITY))
        .collect();
    assert_eq!(order8.len(), 4);
    for t in order8 {
        let mut multiples = Vec::new();
        let mut acc = Point::IDENTITY;
        for _ in 0..8 {
            multiples.push(acc.encode());
            acc = acc.add(t);
        }
        assert!(acc.equals(&Point::IDENTITY));
        multiples.sort();
        let mut want = encodings.clone();
        want.sort();
        assert_eq!(multiples, want);
    }
}

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
}

#[test]
fn mutated_signatures_never_verify() {
    let mut rng = Rng(0x0e1d_2551_9000_0001);
    for (i, v) in differential().iter().enumerate() {
        for trial in 0..8 {
            let (mut pk, mut msg, mut sig) = (v.pk, v.msg.clone(), v.sig);
            let mask = (rng.next() as u8).max(1);
            let field = trial % 4;
            match field {
                0 | 1 => sig[rng.below(64)] ^= mask,
                2 => pk[rng.below(32)] ^= mask,
                _ if msg.is_empty() => msg.push(mask),
                _ => {
                    let at = rng.below(msg.len());
                    msg[at] ^= mask;
                }
            }
            assert!(
                verify(&pk, &msg, &sig).is_err(),
                "vector {i} trial {trial} field {field} still verifies"
            );
        }
    }
}

/// S + L is the classic malleated signature: it satisfies the equation before reduction.
#[test]
fn s_plus_l_is_rejected_for_every_vector() {
    for (i, v) in differential().iter().enumerate() {
        let mut sig = v.sig;
        let mut carry = 0u128;
        for (chunk, l) in sig[32..].as_chunks_mut::<8>().0.iter_mut().zip(L) {
            let sum = u128::from(u64::from_le_bytes(*chunk)) + u128::from(l) + carry;
            *chunk = (sum as u64).to_le_bytes();
            carry = sum >> 64;
        }
        assert_eq!(carry, 0, "S + L fits 256 bits because S < L < 2^253");
        assert_eq!(
            verify(&v.pk, &v.msg, &sig),
            Err(Ed25519Error::SNonCanonical),
            "vector {i}"
        );
    }
}

#[test]
fn error_debug_names_the_check_only() {
    assert_eq!(format!("{:?}", Ed25519Error::RSmallOrder), "RSmallOrder");
    assert_eq!(
        format!("{:?}", Ed25519Error::EquationFailed),
        "EquationFailed"
    );
}

// ---------------------------------------------------------------------------------------
// Constants and internals

fn small(n: u64) -> Fe {
    Fe([n, 0, 0, 0, 0])
}

/// `x^e` by plain square-and-multiply over the little-endian exponent bytes.
fn pow(x: &Fe, e: &[u8; 32]) -> Fe {
    let mut acc = Fe::ONE;
    for byte in e.iter().rev() {
        for bit in (0..8).rev() {
            acc = acc.square();
            if byte >> bit & 1 == 1 {
                acc = acc.mul(x);
            }
        }
    }
    acc
}

/// 2^n − c as 32 little-endian bytes, for 8 ≤ n < 256 and 0 < c < 256.
fn two_pow_minus(n: u32, c: u8) -> [u8; 32] {
    let mut out = [0u8; 32];
    // 2^n − 1 is n one bits; then subtract c − 1 from the low byte.
    for i in 0..n as usize {
        out[i / 8] |= 1 << (i % 8);
    }
    out[0] -= c - 1;
    out
}

#[test]
fn constants_rederived_from_their_definitions() {
    // p = 2^255 − 19: −1 encodes as p − 1 = 2^255 − 20, and p itself reduces to 0.
    assert_eq!(Fe::ONE.neg().to_bytes(), two_pow_minus(255, 20));
    assert!(Fe::from_bytes(&two_pow_minus(255, 19)).is_zero());
    // 4p as limbs: four times the 51-bit limbs of p.
    let p_limbs = [
        (1u64 << 51) - 19,
        (1 << 51) - 1,
        (1 << 51) - 1,
        (1 << 51) - 1,
        (1 << 51) - 1,
    ];
    assert_eq!(FOUR_P, p_limbs.map(|l| 4 * l));
    // d = −121665/121666: d·121666 + 121665 = 0. 2d is d + d. d is not a square, which
    // makes the addition law complete: d^((p−1)/2) = −1.
    assert!(D.mul(&small(121666)).add(&small(121665)).is_zero());
    assert!(D2.equals(&D.add(&D)));
    assert!(pow(&D, &two_pow_minus(254, 10)).equals(&Fe::ONE.neg()));
    // sqrt(−1) = 2^((p−1)/4) = 2^(2^253 − 5), and it squares to −1.
    assert!(pow(&small(2), &two_pow_minus(253, 5)).equals(&SQRT_M1));
    assert!(SQRT_M1.square().equals(&Fe::ONE.neg()));
    // B: y = 4/5, x even, on −x² + y² = 1 + d·x²·y², T = x·y.
    assert!(BASE_Y.mul(&small(5)).equals(&small(4)));
    assert!(!BASE_X.is_odd());
    let (xx, yy) = (BASE_X.square(), BASE_Y.square());
    assert!(yy.sub(&xx).equals(&Fe::ONE.add(&D.mul(&xx).mul(&yy))));
    assert!(BASE_T.equals(&BASE_X.mul(&BASE_Y)));
    // L = 2^252 + 27742317777372353535851937790883648493, and [L]B = identity ≠ B.
    let c: u128 = 27742317777372353535851937790883648493;
    assert_eq!(L, [c as u64, (c >> 64) as u64, 0, 1 << 60]);
    let lb = Point::base_plus_mul(&L, &[0; 4], &Point::IDENTITY);
    assert!(lb.equals(&Point::IDENTITY));
    assert!(!Point::BASE.equals(&Point::IDENTITY));
    // B round-trips through its encoding.
    let enc = Point::BASE.encode();
    assert!(Point::decode(&enc).unwrap().equals(&Point::BASE));
    assert_eq!(hex(&enc), format!("58{}", "66".repeat(31)));
}

/// Pseudo-random field elements, including limbs at the top of the module invariant.
fn sample_elements() -> Vec<Fe> {
    let mut rng = Rng(0x5eed_f1e1_d000_0019);
    let top = (1u64 << 51) + (1 << 18) - 1;
    let mut out = vec![Fe::ZERO, Fe::ONE, Fe::ONE.neg(), Fe([top; 5]), SQRT_M1, D];
    for _ in 0..200 {
        let mut limbs = [0u64; 5];
        for l in &mut limbs {
            *l = rng.next() & ((1 << 51) - 1);
        }
        out.push(Fe(limbs));
    }
    out
}

fn canonical(x: &Fe) -> Fe {
    Fe::from_bytes(&x.to_bytes())
}

#[test]
fn field_operations_agree_with_their_definitions() {
    let xs = sample_elements();
    for (i, x) in xs.iter().enumerate() {
        let y = &xs[(i * 7 + 3) % xs.len()];
        assert!(x.square().equals(&x.mul(x)), "square {i}");
        // Unreduced limbs give the same products as canonical ones.
        assert!(x.mul(y).equals(&canonical(x).mul(&canonical(y))), "mul {i}");
        assert!(x.add(y).sub(y).equals(x), "add/sub {i}");
        assert!(x.add(&x.neg()).is_zero(), "neg {i}");
        assert!(x.mul(y).equals(&y.mul(x)), "commutative {i}");
        if !x.is_zero() {
            assert!(x.invert().mul(x).equals(&Fe::ONE), "invert {i}");
            assert!(
                x.invert().equals(&pow(x, &two_pow_minus(255, 21))),
                "p − 2 {i}"
            );
            assert!(
                x.pow_p58().equals(&pow(x, &two_pow_minus(252, 3))),
                "p58 {i}"
            );
        }
        // Square roots: x² always has one, ±x; v·u⁻¹ form via sqrt_ratio(x²·v, v).
        let v = y.add(&Fe::ONE);
        if !v.is_zero() {
            let r = Fe::sqrt_ratio(&x.square().mul(&v), &v).unwrap();
            assert!(r.equals(x) || r.equals(&x.neg()), "sqrt {i}");
        }
    }
    // 2 is not a square mod p (p ≡ 5 mod 8).
    assert!(Fe::sqrt_ratio(&small(2), &Fe::ONE).is_none());
}

#[test]
fn field_matches_integer_arithmetic_below_2_128() {
    let mut rng = Rng(0x1234_5678_9abc_def1);
    let fe = |v: u128| {
        let mut b = [0u8; 32];
        b[..16].copy_from_slice(&v.to_le_bytes());
        Fe::from_bytes(&b)
    };
    for _ in 0..500 {
        let (a, b) = (rng.next(), rng.next());
        let want = u128::from(a) * u128::from(b);
        let mut bytes = [0u8; 32];
        bytes[..16].copy_from_slice(&want.to_le_bytes());
        assert_eq!(fe(a.into()).mul(&fe(b.into())).to_bytes(), bytes);
    }
    // 2^254 · 2 = 2^255 ≡ 19.
    let mut b = [0u8; 32];
    b[31] = 0x40;
    assert!(Fe::from_bytes(&b).mul(&small(2)).equals(&small(19)));
}

#[test]
fn encodings_at_and_above_p_reduce() {
    // p + j for j = 0..18 covers every 255-bit value ≥ p.
    for j in 0..19u8 {
        let mut p_plus_j = two_pow_minus(255, 19);
        p_plus_j[0] += j;
        let mut want = [0u8; 32];
        want[0] = j;
        assert_eq!(Fe::from_bytes(&p_plus_j).to_bytes(), want, "p + {j}");
    }
    // Bit 255 is ignored by from_bytes.
    let mut b = [0u8; 32];
    b[0] = 5;
    b[31] = 0x80;
    assert_eq!(Fe::from_bytes(&b).to_bytes()[..], small(5).to_bytes()[..]);
}

/// q·L + r as 64 little-endian bytes.
fn mul_add_l(q: [u64; 4], r: [u64; 4]) -> [u8; 64] {
    let mut acc = [0u128; 9];
    for (i, &qi) in q.iter().enumerate() {
        for (j, &lj) in L.iter().enumerate() {
            let prod = u128::from(qi) * u128::from(lj);
            acc[i + j] += prod & u128::from(u64::MAX);
            acc[i + j + 1] += prod >> 64;
        }
    }
    for (i, &ri) in r.iter().enumerate() {
        acc[i] += u128::from(ri);
    }
    let mut out = [0u8; 64];
    let mut carry = 0u128;
    for (i, chunk) in out.as_chunks_mut::<8>().0.iter_mut().enumerate() {
        let v = acc[i] + carry;
        *chunk = (v as u64).to_le_bytes();
        carry = v >> 64;
    }
    assert_eq!(acc[8] + carry, 0, "q·L + r must fit 512 bits");
    out
}

#[test]
fn scalar_reduction_and_canonicity() {
    let mut rng = Rng(0xfeed_beef_cafe_0001);
    let mut l_minus_1 = L;
    l_minus_1[0] -= 1;
    for trial in 0..300 {
        // r < L: random below 2^252, or L − 1, or 0.
        let r = match trial {
            0 => l_minus_1,
            1 => [0; 4],
            _ => [
                rng.next(),
                rng.next(),
                rng.next(),
                rng.next() & ((1 << 60) - 1),
            ],
        };
        // q < 2^195 (top word below 8), so q·L + r < 2^448 fits the 64 bytes.
        let q = [rng.next(), rng.next(), rng.next(), rng.next() & 7];
        assert_eq!(scalar::reduce_wide(&mul_add_l(q, r)), r, "trial {trial}");
        assert_eq!(scalar::reduce_wide(&mul_add_l([0; 4], r)), r);
    }
    assert_eq!(
        scalar::reduce_wide(&mul_add_l([1, 0, 0, 0], [0; 4])),
        [0; 4]
    );
    let bytes = |w: [u64; 4]| {
        let mut b = [0u8; 32];
        for (chunk, word) in b.as_chunks_mut::<8>().0.iter_mut().zip(w) {
            *chunk = word.to_le_bytes();
        }
        b
    };
    assert_eq!(scalar::canonical(&bytes(l_minus_1)), Some(l_minus_1));
    assert_eq!(scalar::canonical(&bytes([0; 4])), Some([0; 4]));
    assert_eq!(scalar::canonical(&bytes(L)), None);
    assert_eq!(scalar::canonical(&[0xff; 32]), None);
}

#[test]
fn group_law_sanity() {
    let b = Point::BASE;
    let b2 = b.double();
    assert!(b.add(&b).equals(&b2), "add(P, P) = double(P)");
    assert!(b.add(&Point::IDENTITY).equals(&b));
    assert!(b.add(&b.neg()).equals(&Point::IDENTITY));
    assert!(b2.add(&b).equals(&b.add(&b2)));
    // [3]B by the double-scalar routine, both ways.
    let three = b2.add(&b);
    assert!(Point::base_plus_mul(&[3, 0, 0, 0], &[0; 4], &b).equals(&three));
    assert!(Point::base_plus_mul(&[1, 0, 0, 0], &[2, 0, 0, 0], &b).equals(&three));
    assert!(!b.has_small_order());
}

/// Verifications per second on the build host. Indicative only: the target is x86-64.
/// Run with
/// `cargo test -p origin-crypto --release -- --ignored --nocapture ed25519_throughput`.
#[test]
#[ignore = "benchmark; run explicitly in release mode"]
fn ed25519_throughput() {
    use std::hint::black_box;
    use std::time::Instant;
    let vectors = differential();
    let set = &vectors[..64];
    let start = Instant::now();
    let mut count = 0u64;
    while start.elapsed().as_secs_f64() < 3.0 {
        for v in set {
            verify(black_box(&v.pk), black_box(&v.msg), black_box(&v.sig)).unwrap();
            count += 1;
        }
    }
    let secs = start.elapsed().as_secs_f64();
    println!(
        "Ed25519 verify: {:.0} verifications/s ({count} in {secs:.2} s, messages of 0-63 bytes)",
        count as f64 / secs
    );
}
