//! SHA-2 tests (CLAUDE.md §59; ONBF §13 items 1–2).
//!
//! - Known-answer tests: the FIPS 180-4 example messages. Each expected digest was checked
//!   against two reference implementations (hashlib and shasum) before being embedded.
//! - Differential: 310 vectors in `tests/vectors/sha2-differential.txt`, on which hashlib
//!   and shasum agreed.
//! - Streaming: arbitrary splits must give the one-shot digest.
//! - Constants: the SHA-256 tables are re-derived here from their definition, independently
//!   of the Python generator, and checked against the SHA-512 tables.

use crate::constants::{H256, H512, K256, K512};
use crate::{Sha256, Sha512};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

const ABC448: &str = "abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
const ABC896: &str = "abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu";

#[test]
fn sha256_known_answers() {
    let cases: [(&[u8], &str); 4] = [
        (
            b"",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ),
        (
            b"abc",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        ),
        (
            ABC448.as_bytes(),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
        ),
        (
            ABC896.as_bytes(),
            "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1",
        ),
    ];
    for (msg, want) in cases {
        assert_eq!(hex(&Sha256::digest(msg)), want, "{} bytes", msg.len());
    }
    let million_a = vec![b'a'; 1_000_000];
    assert_eq!(
        hex(&Sha256::digest(&million_a)),
        "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
    );
}

#[test]
fn sha512_known_answers() {
    let cases: [(&[u8], &str); 4] = [
        (
            b"",
            "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e",
        ),
        (
            b"abc",
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f",
        ),
        (
            ABC448.as_bytes(),
            "204a8fc6dda82f0a0ced7beb8e08a41657c16ef468b228a8279be331a703c33596fd15c13b1b07f9aa1d3bea57789ca031ad85c7a71dd70354ec631238ca3445",
        ),
        (
            ABC896.as_bytes(),
            "8e959b75dae313da8cf4f72814fc143f8f7779c6eb9f7fa17299aeadb6889018501d289e4900f7e4331b99dec4b5433ac7d329eeb6dd26545e96e55b874be909",
        ),
    ];
    for (msg, want) in cases {
        assert_eq!(hex(&Sha512::digest(msg)), want, "{} bytes", msg.len());
    }
    let million_a = vec![b'a'; 1_000_000];
    assert_eq!(
        hex(&Sha512::digest(&million_a)),
        "e718483d0ce769644e2e42c7bc15b4638e1f98b13b2044285632a803afa973ebde0ff244877ea60a4cb0432ce577c31beb009c5c2c49aa2e4eadb217ad8cc09b"
    );
}

/// The message `tools/gen_sha2_vectors.py` hashes for length `n`.
fn vector_message(n: usize) -> Vec<u8> {
    (0..n)
        .map(|i| ((i * 31 + n * 7 + 13) & 0xFF) as u8)
        .collect()
}

#[test]
fn differential_vectors_match_two_reference_implementations() {
    let file = include_str!("../tests/vectors/sha2-differential.txt");
    let mut count = 0;
    for line in file
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let mut cols = line.split_whitespace();
        let n: usize = cols.next().unwrap().parse().unwrap();
        let (want256, want512) = (cols.next().unwrap(), cols.next().unwrap());
        let m = vector_message(n);
        assert_eq!(hex(&Sha256::digest(&m)), want256, "SHA-256, length {n}");
        assert_eq!(hex(&Sha512::digest(&m)), want512, "SHA-512, length {n}");
        count += 1;
    }
    assert_eq!(count, 310);
}

#[test]
fn streaming_matches_one_shot_for_every_split() {
    let data = vector_message(700);
    for len in [
        0, 1, 55, 56, 63, 64, 65, 111, 112, 127, 128, 129, 255, 256, 700,
    ] {
        let msg = &data[..len];
        let want256 = Sha256::digest(msg);
        let want512 = Sha512::digest(msg);
        // Every two-piece split, and fixed-size pieces of 1, 7, 64 and 129 bytes.
        for cut in 0..=len {
            let mut h = Sha256::new();
            h.update(&msg[..cut]);
            h.update(&msg[cut..]);
            assert_eq!(h.finalize(), want256, "SHA-256 len {len} cut {cut}");
            let mut h = Sha512::new();
            h.update(&msg[..cut]);
            h.update(&msg[cut..]);
            assert_eq!(h.finalize(), want512, "SHA-512 len {len} cut {cut}");
        }
        for piece in [1, 7, 64, 129] {
            let mut a = Sha256::new();
            let mut b = Sha512::new();
            for chunk in msg.chunks(piece) {
                a.update(chunk);
                b.update(&[]);
                b.update(chunk);
            }
            assert_eq!(a.finalize(), want256, "SHA-256 len {len} piece {piece}");
            assert_eq!(b.finalize(), want512, "SHA-512 len {len} piece {piece}");
        }
    }
}

#[test]
fn clone_forks_the_stream() {
    let mut h = Sha256::new();
    h.update(b"ab");
    let fork = h.clone();
    h.update(b"c");
    assert_eq!(h.finalize(), Sha256::digest(b"abc"));
    assert_eq!(fork.finalize(), Sha256::digest(b"ab"));
}

#[test]
fn debug_output_reveals_no_state() {
    let mut h = Sha512::new();
    h.update(b"secret");
    assert_eq!(format!("{h:?}"), "Sha512 { .. }");
    assert_eq!(format!("{:?}", Sha256::new()), "Sha256 { .. }");
}

fn first_primes(count: usize) -> Vec<u128> {
    let mut found: Vec<u128> = Vec::new();
    let mut n = 2u128;
    while found.len() < count {
        if found
            .iter()
            .take_while(|&&p| p * p <= n)
            .all(|&p| !n.is_multiple_of(p))
        {
            found.push(n);
        }
        n += 1;
    }
    found
}

/// Largest x with x³ ≤ n, by binary search (n < 2^108 here, so x³ fits in u128).
fn icbrt(n: u128) -> u128 {
    let (mut lo, mut hi) = (0u128, 1u128 << 36);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if mid * mid * mid <= n {
            lo = mid
        } else {
            hi = mid - 1
        }
    }
    lo
}

#[test]
fn sha256_constants_rederived_from_their_definition() {
    let primes = first_primes(64);
    for (i, &p) in primes.iter().enumerate() {
        let k = (icbrt(p << 96) & 0xFFFF_FFFF) as u32;
        assert_eq!(K256[i], k, "K256[{i}] (cube root of {p})");
    }
    for (i, &p) in primes.iter().take(8).enumerate() {
        let h = ((p << 64).isqrt() & 0xFFFF_FFFF) as u32;
        assert_eq!(H256[i], h, "H256[{i}] (square root of {p})");
    }
}

#[test]
fn sha512_constants_extend_the_sha256_constants() {
    for i in 0..64 {
        assert_eq!((K512[i] >> 32) as u32, K256[i], "K512[{i}] high half");
    }
    for i in 0..8 {
        assert_eq!((H512[i] >> 32) as u32, H256[i], "H512[{i}] high half");
    }
    // The 16 SHA-512-only constants are covered by the known-answer tests.
    assert_eq!(K512.len(), 80);
}

/// Throughput of the portable code on the build host. Indicative only: the target is
/// x86-64, and SHA-NI is not used yet. Run with
/// `cargo test -p origin-crypto --release -- --ignored --nocapture throughput`.
#[test]
#[ignore = "benchmark; run explicitly in release mode"]
fn throughput() {
    use std::time::Instant;
    let data = vec![0x5Au8; 64 << 20];
    for (name, f) in [
        (
            "SHA-256",
            (|d: &[u8]| Sha256::digest(d)[0]) as fn(&[u8]) -> u8,
        ),
        ("SHA-512", |d: &[u8]| Sha512::digest(d)[0]),
    ] {
        let start = Instant::now();
        let mut sink = 0u8;
        for _ in 0..4 {
            sink ^= f(std::hint::black_box(&data));
        }
        let secs = start.elapsed().as_secs_f64();
        println!("{name}: {:.0} MiB/s (sink {sink})", 256.0 / secs);
    }
}
