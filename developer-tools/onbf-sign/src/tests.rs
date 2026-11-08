//! Known answers and differential tests for the host signer.
//!
//! - Ed25519: the RFC 8032 §7.1 vectors and 296 more from `ed25519_reference.py signvectors`
//!   (`tests/vectors/ed25519-sign.txt`), byte for byte: keys and signatures.
//! - ML-DSA-87: every key and signature in `mldsa-differential.txt`, byte for byte, from an
//!   independent pure-Python FIPS 204 implementation whose output matched OpenSSL 4.1.0.
//! - Independent verification: everything signed here must pass `origin-crypto`'s strict
//!   verifiers (separate arithmetic).
//! - The arithmetic helpers against simple definitions, exhaustively where they are small.

use origin_crypto::{ed25519_verify, mldsa87_verify};

use crate::ed25519;
use crate::ed25519::testing::{L, bytes_from_words, reduce_512, scalar_mul_add, words_from_bytes};
use crate::mldsa87::testing::{
    ALPHA, DECOMPOSE_M, GAMMA2, N, Poly, Q, any_at_least, centered, decompose, equal_mask,
    expand_mask, from_signed, inverse_ntt, make_hint, mul, ntt, pointwise, power2round, reduce,
    sample_in_ball,
};
use crate::mldsa87::{SignError, SigningKey};

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

fn arr<const M: usize>(s: &str) -> [u8; M] {
    unhex(s).try_into().unwrap()
}

/// A small deterministic generator (a counter hashed with SHA-512) for the property tests.
fn rng_bytes(label: &str, counter: u64, out: &mut [u8]) {
    use origin_crypto::Sha512;
    let mut filled = 0;
    let mut block = 0u64;
    while filled < out.len() {
        let mut h = Sha512::new();
        h.update(label.as_bytes());
        h.update(&counter.to_le_bytes());
        h.update(&block.to_le_bytes());
        let chunk = h.finalize();
        let take = (out.len() - filled).min(chunk.len());
        out[filled..filled + take].copy_from_slice(&chunk[..take]);
        filled += take;
        block += 1;
    }
}

// ---------------------------------------------------------------- Ed25519

const ED25519_VECTORS: &str = include_str!("../tests/vectors/ed25519-sign.txt");

fn ed25519_vectors() -> impl Iterator<Item = ([u8; 32], [u8; 32], Vec<u8>, [u8; 64])> {
    ED25519_VECTORS
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .map(|line| {
            let cols: Vec<&str> = line.split_whitespace().collect();
            assert_eq!(cols.len(), 4, "{line}");
            (arr(cols[0]), arr(cols[1]), unhex(cols[2]), arr(cols[3]))
        })
}

/// RFC 8032 §7.1 TEST 1, 2, 3 and SHA(abc) come first in the file, so a bad vector
/// is spotted before the differential ones.
#[test]
fn ed25519_rfc8032_known_answers() {
    let first: Vec<_> = ed25519_vectors().take(4).collect();
    assert_eq!(first.len(), 4);
    assert_eq!(
        hex_of(&first[0].0),
        "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60"
    );
    for (seed, public, message, signature) in first {
        assert_eq!(ed25519::public_key(&seed), public);
        assert_eq!(ed25519::sign(&seed, &message), signature);
    }
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn ed25519_matches_the_reference_byte_for_byte() {
    let mut count = 0;
    for (seed, public, message, signature) in ed25519_vectors() {
        assert_eq!(ed25519::public_key(&seed), public, "vector {count}");
        assert_eq!(ed25519::sign(&seed, &message), signature, "vector {count}");
        // The independent, strict verifier agrees.
        assert_eq!(
            ed25519_verify(&public, &message, &signature),
            Ok(()),
            "vector {count}"
        );
        count += 1;
    }
    assert_eq!(count, 300, "the vector file changed");
}

#[test]
fn ed25519_signatures_verify_for_many_keys() {
    for i in 0..200u64 {
        let mut seed = [0u8; 32];
        rng_bytes("ed25519 seed", i, &mut seed);
        let mut message = vec![0u8; (i % 300) as usize];
        rng_bytes("ed25519 message", i, &mut message);
        let public = ed25519::public_key(&seed);
        let signature = ed25519::sign(&seed, &message);
        assert_eq!(ed25519_verify(&public, &message, &signature), Ok(()), "{i}");
        // Signing is deterministic (RFC 8032 §5.1.6).
        assert_eq!(ed25519::sign(&seed, &message), signature);
        let mut changed = message.clone();
        changed.push(0);
        assert!(ed25519_verify(&public, &changed, &signature).is_err());
    }
}

/// `value` little-endian in 64 bytes.
fn wide_bytes(value: &[u64; 8]) -> [u8; 64] {
    let mut out = [0u8; 64];
    for (chunk, word) in out.as_chunks_mut::<8>().0.iter_mut().zip(value) {
        *chunk = word.to_le_bytes();
    }
    out
}

#[test]
fn scalar_reduction_handles_the_edges() {
    let l: [u64; 8] = [L[0], L[1], L[2], L[3], 0, 0, 0, 0];
    assert_eq!(reduce_512(&[0; 64]), [0; 4]);
    // L reduces to 0, L + 1 to 1, L − 1 to itself.
    assert_eq!(reduce_512(&wide_bytes(&l)), [0; 4]);
    let mut plus_one = l;
    plus_one[0] += 1;
    assert_eq!(reduce_512(&wide_bytes(&plus_one)), [1, 0, 0, 0]);
    let mut minus_one = l;
    minus_one[0] -= 1;
    assert_eq!(
        reduce_512(&wide_bytes(&minus_one)),
        [L[0] - 1, L[1], L[2], L[3]]
    );
    // The largest input, against a value computed outside this crate (Python: (2**512 - 1) % L).
    assert_eq!(
        reduce_512(&[0xFF; 64]),
        [
            0xa406_11e3_449c_0f00,
            0xd00e_1ba7_6885_9347,
            0xceec_73d2_17f5_be65,
            0x0399_411b_7c30_9a3d
        ]
    );
}

#[test]
fn scalar_mul_add_matches_small_arithmetic() {
    for i in 0..500u64 {
        let mut seed = [0u8; 40];
        rng_bytes("scalars", i, &mut seed);
        let k = u64::from_le_bytes(seed[..8].try_into().unwrap()) >> 1;
        let a_small = u64::from_le_bytes(seed[8..16].try_into().unwrap()) >> 1;
        let r = u64::from_le_bytes(seed[16..24].try_into().unwrap()) >> 1;
        let expected = u128::from(k) * u128::from(a_small) + u128::from(r);
        let mut a_bytes = [0u8; 32];
        a_bytes[..8].copy_from_slice(&a_small.to_le_bytes());
        let got = scalar_mul_add(&[k, 0, 0, 0], &a_bytes, &[r, 0, 0, 0]);
        let mut want = [0u8; 32];
        want[..16].copy_from_slice(&expected.to_le_bytes());
        assert_eq!(got, want, "{i}");
    }
    // k·a + r wrapping past L: (L − 1)·1 + 1 ≡ 0.
    let mut one = [0u8; 32];
    one[0] = 1;
    let got = scalar_mul_add(&[L[0] - 1, L[1], L[2], L[3]], &one, &[1, 0, 0, 0]);
    assert_eq!(got, [0u8; 32]);
    // Word conversions round trip.
    let w = [1u64, 2, 3, 4];
    assert_eq!(words_from_bytes(&bytes_from_words(&w)), w);
}

// ---------------------------------------------------------------- ML-DSA-87

const MLDSA_VECTORS: &str =
    include_str!("../../../security/crypto/tests/vectors/mldsa-differential.txt");

struct MlDsaVector {
    xi: [u8; 32],
    public: Vec<u8>,
    rnd: [u8; 32],
    ctx: Vec<u8>,
    msg: Vec<u8>,
    sig: Vec<u8>,
}

type MlDsaKeys = Vec<([u8; 32], Vec<u8>)>;

/// The ML-DSA-87 vectors of `mldsa-differential.txt`.
fn mldsa87_vectors() -> (MlDsaKeys, Vec<MlDsaVector>) {
    let mut keys = Vec::new();
    let mut signatures = Vec::new();
    for line in MLDSA_VECTORS
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols[1] != "87" {
            continue;
        }
        match cols[0] {
            "key" => {
                assert_eq!(cols[2], keys.len().to_string());
                keys.push((arr::<32>(cols[3]), unhex(cols[4])));
            }
            "sig" => {
                let key: usize = cols[3].parse().unwrap();
                let (xi, public) = keys[key].clone();
                let rnd = if cols[4] == "-" {
                    [0u8; 32]
                } else {
                    arr::<32>(cols[4])
                };
                signatures.push(MlDsaVector {
                    xi,
                    public,
                    rnd,
                    ctx: unhex(cols[5]),
                    msg: unhex(cols[6]),
                    sig: unhex(cols[7]),
                });
            }
            kind => panic!("unknown line kind {kind}"),
        }
    }
    (keys, signatures)
}

#[test]
fn mldsa87_keys_match_the_reference_byte_for_byte() {
    let (keys, _) = mldsa87_vectors();
    assert_eq!(keys.len(), 13);
    for (index, (xi, public)) in keys.iter().enumerate() {
        let key = SigningKey::from_seed(xi);
        assert_eq!(
            key.public_key().as_slice(),
            public.as_slice(),
            "key {index}"
        );
    }
}

#[test]
fn mldsa87_signatures_match_the_reference_byte_for_byte() {
    let (_, vectors) = mldsa87_vectors();
    assert_eq!(vectors.len(), 52, "the vector file changed");
    let (mut deterministic, mut hedged) = (0, 0);
    for (index, v) in vectors.iter().enumerate() {
        let key = SigningKey::from_seed(&v.xi);
        let sig = key.sign(&v.msg, &v.ctx, &v.rnd).unwrap();
        assert_eq!(sig.as_slice(), v.sig.as_slice(), "vector {index}");
        let public: &[u8; 2592] = v.public.as_slice().try_into().unwrap();
        // The independent, strict verifier agrees.
        assert_eq!(
            mldsa87_verify(public, &v.msg, &v.ctx, &sig),
            Ok(()),
            "vector {index}"
        );
        if v.rnd == [0u8; 32] {
            deterministic += 1;
        } else {
            hedged += 1;
        }
    }
    assert!(deterministic > 0 && hedged > 0);
}

#[test]
fn mldsa87_signatures_verify_for_many_keys_contexts_and_messages() {
    for i in 0..40u64 {
        let mut seed = [0u8; 32];
        rng_bytes("mldsa seed", i, &mut seed);
        let key = SigningKey::from_seed(&seed);
        let mut message = vec![0u8; (i * 37 % 2000) as usize];
        rng_bytes("mldsa message", i, &mut message);
        let mut context = vec![0u8; (i * 11 % 256) as usize];
        rng_bytes("mldsa context", i, &mut context);
        let mut rnd = [0u8; 32];
        rng_bytes("mldsa rnd", i, &mut rnd);
        let sig = key.sign(&message, &context, &rnd).unwrap();
        assert_eq!(
            mldsa87_verify(key.public_key(), &message, &context, &sig),
            Ok(()),
            "{i}"
        );
        // A different context never verifies.
        let other = [context.as_slice(), b"x"].concat();
        if other.len() <= 255 {
            assert!(mldsa87_verify(key.public_key(), &message, &other, &sig).is_err());
        }
    }
}

#[test]
fn a_context_over_255_bytes_is_refused() {
    let key = SigningKey::from_seed(&[1; 32]);
    assert_eq!(
        key.sign(b"m", &[0u8; 256], &[0; 32]).map(|_| ()),
        Err(SignError::ContextTooLong)
    );
    assert!(key.sign(b"m", &[0u8; 255], &[0; 32]).is_ok());
}

// ------------------------------------------------- the ML-DSA arithmetic, against definitions

#[test]
fn reduction_matches_the_remainder() {
    let mut x = 1u64;
    for _ in 0..200_000 {
        x = x.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(7);
        let value = x % (1 << 46);
        assert_eq!(u64::from(reduce(value)), value % u64::from(Q), "{value}");
    }
    assert_eq!(reduce(0), 0);
    assert_eq!(
        reduce(u64::from(Q - 1) * u64::from(Q - 1)),
        mul(Q - 1, Q - 1)
    );
    assert_eq!(
        u64::from(reduce((1 << 46) - 1)),
        ((1u64 << 46) - 1) % u64::from(Q)
    );
}

/// FIPS 204's definitions, written directly, compared on every value below q.
#[test]
fn decompose_is_exact_for_every_coefficient() {
    let mod_pm = |x: i64, alpha: i64| {
        let m = x.rem_euclid(alpha);
        if m > alpha / 2 { m - alpha } else { m }
    };
    for r in 0..Q {
        // The division by α behind `decompose`, for the largest argument it sees.
        let a = u64::from(r) + u64::from(GAMMA2) - 1;
        assert_eq!((a * DECOMPOSE_M) >> 46, a / u64::from(ALPHA), "{r}");

        let r0_def = mod_pm(i64::from(r), i64::from(2 * GAMMA2));
        let (r1, r0) = if i64::from(r) - r0_def == i64::from(Q) - 1 {
            (0, r0_def - 1)
        } else {
            ((i64::from(r) - r0_def) / i64::from(2 * GAMMA2), r0_def)
        };
        let got = decompose(r);
        assert_eq!((i64::from(got.0), i64::from(got.1)), (r1, r0), "{r}");

        // Power2Round's definition (Algorithm 35).
        let r0_def = mod_pm(i64::from(r), 1 << 13);
        let (high, low) = power2round(r);
        assert_eq!(i64::from(high), (i64::from(r) - r0_def) >> 13, "{r}");
        assert_eq!(i64::from(low), r0_def.rem_euclid(i64::from(Q)), "{r}");
    }
}

#[test]
fn signed_representatives_and_masks_are_exact() {
    for x in [0, 1, Q / 2, Q / 2 + 1, Q - 1, 1234, 4_190_208, 4_190_209] {
        let c = centered(x);
        assert_eq!(from_signed(c), x);
        assert!(c.unsigned_abs() <= (Q - 1) / 2, "{x}");
    }
    assert_eq!(equal_mask(5, 5), u32::MAX);
    assert_eq!(equal_mask(5, 6), 0);
    assert_eq!(equal_mask(0, 0x8000_0000), 0);
    let mut poly: Poly = [0; N];
    assert_eq!(any_at_least(&poly, 1), 0);
    poly[17] = Q - 1; // −1, magnitude 1
    assert_eq!(any_at_least(&poly, 1), u32::MAX);
    assert_eq!(any_at_least(&poly, 2), 0);
}

/// `MakeHint` is true exactly when the high bits change (Algorithm 39).
#[test]
fn hints_mark_exactly_the_changed_high_bits() {
    let mut x = 7u64;
    for _ in 0..50_000 {
        x = x.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(11);
        let r = (x % u64::from(Q)) as u32;
        let z = ((x >> 32) % u64::from(Q)) as u32;
        let changed = decompose(r).0 != decompose((r + z) % Q).0;
        assert_eq!(make_hint(z, r), u32::from(changed), "{r} {z}");
    }
}

/// The NTT against schoolbook multiplication in Z_q[X]/(X^256 + 1).
#[test]
fn ntt_multiplication_matches_the_schoolbook() {
    let mut poly_a: Poly = [0; N];
    let mut poly_b: Poly = [0; N];
    let mut x = 3u64;
    for trial in 0..20 {
        for p in [&mut poly_a, &mut poly_b] {
            for c in p.iter_mut() {
                x = x.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(trial);
                *c = (x % u64::from(Q)) as u32;
            }
        }
        let mut schoolbook = [0i128; 2 * N];
        for i in 0..N {
            for j in 0..N {
                schoolbook[i + j] += i128::from(poly_a[i]) * i128::from(poly_b[j]);
            }
        }
        let mut expected: Poly = [0; N];
        for i in 0..N {
            let value = (schoolbook[i] - schoolbook[i + N]).rem_euclid(i128::from(Q));
            expected[i] = value as u32;
        }
        let (mut a_hat, mut b_hat) = (poly_a, poly_b);
        ntt(&mut a_hat);
        ntt(&mut b_hat);
        let mut product = pointwise(&a_hat, &b_hat);
        inverse_ntt(&mut product);
        assert_eq!(product, expected, "trial {trial}");

        // The transform is its own inverse pair.
        let mut round = poly_a;
        ntt(&mut round);
        inverse_ntt(&mut round);
        assert_eq!(round, poly_a);
    }
}

#[test]
fn expand_mask_has_the_range_fips_204_gives_it() {
    let mut y = [[0u32; N]; 7];
    expand_mask(&[7; 64], 0, &mut y);
    for poly in &y {
        for &c in poly {
            let v = centered(c);
            assert!((-(1i32 << 19) + 1..=(1 << 19)).contains(&v), "{v}");
        }
    }
}

#[test]
fn the_challenge_has_exactly_tau_nonzero_coefficients() {
    let c = sample_in_ball(&[42; 64]);
    let nonzero = c.iter().filter(|&&x| x != 0).count();
    assert_eq!(nonzero, 60);
    assert!(c.iter().all(|&x| x == 0 || x == 1 || x == Q - 1));
}
