//! SHA-3 and SHAKE tests (CLAUDE.md §59).
//!
//! - Known-answer tests: the empty message and "abc". Each expected value was computed with
//!   Python's hashlib before being embedded.
//! - Differential: 304 vectors in `tests/vectors/sha3-differential.txt`, on which hashlib's
//!   OpenSSL and HACL* backends agreed.
//! - Streaming: arbitrary input splits must give the one-shot result, and any sequence of
//!   squeezes the one-shot output.
//! - Constants: the round constants, ρ offsets and π mapping are re-derived here a second
//!   way, independently of the const fns in `keccak.rs`.

use crate::keccak::{PI_SOURCE, RHO, ROUND_CONSTANTS};
use crate::{Sha3_256, Sha3_512, Shake128, Shake256, ShakeReader};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn shake128_reader(msg: &[u8]) -> ShakeReader {
    let mut h = Shake128::new();
    h.update(msg);
    h.finalize_xof()
}

fn shake256_reader(msg: &[u8]) -> ShakeReader {
    let mut h = Shake256::new();
    h.update(msg);
    h.finalize_xof()
}

fn shake128(msg: &[u8], len: usize) -> Vec<u8> {
    let mut out = vec![0; len];
    shake128_reader(msg).squeeze(&mut out);
    out
}

fn shake256(msg: &[u8], len: usize) -> Vec<u8> {
    let mut out = vec![0; len];
    shake256_reader(msg).squeeze(&mut out);
    out
}

#[test]
fn sha3_known_answers() {
    assert_eq!(
        hex(&Sha3_256::digest(b"")),
        "a7ffc6f8bf1ed76651c14756a061d662f580ff4de43b49fa82d80a4b80f8434a"
    );
    assert_eq!(
        hex(&Sha3_256::digest(b"abc")),
        "3a985da74fe225b2045c172d6bd390bd855f086e3e9d525b46bfe24511431532"
    );
    assert_eq!(
        hex(&Sha3_512::digest(b"")),
        "a69f73cca23a9ac5c8b567dc185a756e97c982164fe25859e0d1dcc1475c80a615b2123af1f5f94c11e3e9402c3ac558f500199d95b6d3e301758586281dcd26"
    );
    assert_eq!(
        hex(&Sha3_512::digest(b"abc")),
        "b751850b1a57168a5693cd924b6b096e08f621827444f70d884f5d0240d2712e10e116e9192af3c91a7ec57647e3934057340b4cf408d5a56592f8274eec53f0"
    );
}

#[test]
fn shake_known_answers() {
    let shake128_empty_64 = "7f9c2ba4e88f827d616045507605853ed73b8093f6efbc88eb1a6eacfa66ef263cb1eea988004b93103cfb0aeefd2a686e01fa4a58e8a3639ca8a1e3f9ae57e2";
    let shake256_empty_64 = "46b9dd2b0ba88d13233b3feb743eeb243fcd52ea62b81b82b50c27646ed5762fd75dc4ddd8c0f200cb05019d67b592f6fc821c49479ab48640292eacb3b7c4be";
    assert_eq!(hex(&shake128(b"", 32)), shake128_empty_64[..64]);
    assert_eq!(hex(&shake128(b"", 64)), shake128_empty_64);
    assert_eq!(hex(&shake256(b"", 32)), shake256_empty_64[..64]);
    assert_eq!(hex(&shake256(b"", 64)), shake256_empty_64);
    // 1000 output bytes cross 5 SHAKE128 and 7 SHAKE256 block boundaries. To keep this file
    // short they are compared through their SHA3-256, which hashlib also computed.
    assert_eq!(
        hex(&Sha3_256::digest(&shake128(b"", 1000))),
        "4a0114f96eff74f864206cc3dbbade4d666c75268d17211121d2ae385bae11cc"
    );
    assert_eq!(
        hex(&Sha3_256::digest(&shake256(b"", 1000))),
        "4af3c8f245b2c60ddb544450f7b4fdf926a5fee5d05dba4f2d1a6c9caad22155"
    );
}

#[test]
fn rates_and_output_lengths() {
    // FIPS 202 §6: r = 1600 − c bits, with c = 2d for SHA3-d and c = 2·(security) for SHAKE.
    assert_eq!((Sha3_256::RATE, Sha3_256::OUTPUT_LEN), (136, 32));
    assert_eq!((Sha3_512::RATE, Sha3_512::OUTPUT_LEN), (72, 64));
    assert_eq!(Shake128::RATE, 168);
    assert_eq!(Shake256::RATE, 136);
}

/// The message `tools/gen_sha3_vectors.py` hashes for length `n`.
fn vector_message(n: usize) -> Vec<u8> {
    (0..n)
        .map(|i| ((i * 31 + n * 7 + 13) & 0xFF) as u8)
        .collect()
}

#[test]
fn differential_vectors_match_two_reference_implementations() {
    let file = include_str!("../tests/vectors/sha3-differential.txt");
    let mut count = 0;
    for line in file
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let cols: Vec<&str> = line.split_whitespace().collect();
        let [n, sha3_256, sha3_512, s128, s256] = cols[..] else {
            panic!("malformed line: {line}");
        };
        let n: usize = n.parse().unwrap();
        let m = vector_message(n);
        assert_eq!(hex(&Sha3_256::digest(&m)), sha3_256, "SHA3-256, length {n}");
        assert_eq!(hex(&Sha3_512::digest(&m)), sha3_512, "SHA3-512, length {n}");
        assert_eq!(hex(&shake128(&m, 200)), s128, "SHAKE128, length {n}");
        assert_eq!(hex(&shake256(&m, 200)), s256, "SHAKE256, length {n}");
        count += 1;
    }
    assert_eq!(count, 304);
}

/// The four functions behind one interface, so that each streaming test covers all of them.
trait Absorb: Clone {
    const RATE: usize;
    fn fresh() -> Self;
    fn absorb(&mut self, data: &[u8]);
    /// The digest, or the first 64 output bytes of an XOF.
    fn output(self) -> Vec<u8>;
}

impl Absorb for Sha3_256 {
    const RATE: usize = Self::RATE;
    fn fresh() -> Self {
        Self::new()
    }
    fn absorb(&mut self, data: &[u8]) {
        self.update(data);
    }
    fn output(self) -> Vec<u8> {
        self.finalize().to_vec()
    }
}

impl Absorb for Sha3_512 {
    const RATE: usize = Self::RATE;
    fn fresh() -> Self {
        Self::new()
    }
    fn absorb(&mut self, data: &[u8]) {
        self.update(data);
    }
    fn output(self) -> Vec<u8> {
        self.finalize().to_vec()
    }
}

impl Absorb for Shake128 {
    const RATE: usize = Self::RATE;
    fn fresh() -> Self {
        Self::new()
    }
    fn absorb(&mut self, data: &[u8]) {
        self.update(data);
    }
    fn output(self) -> Vec<u8> {
        let mut out = vec![0; 64];
        self.finalize_xof().squeeze(&mut out);
        out
    }
}

impl Absorb for Shake256 {
    const RATE: usize = Self::RATE;
    fn fresh() -> Self {
        Self::new()
    }
    fn absorb(&mut self, data: &[u8]) {
        self.update(data);
    }
    fn output(self) -> Vec<u8> {
        let mut out = vec![0; 64];
        self.finalize_xof().squeeze(&mut out);
        out
    }
}

/// Lengths around every rate (72, 136 and 168 bytes) and its double, plus small and odd ones.
const BOUNDARY_LENGTHS: [usize; 24] = [
    0, 1, 7, 8, 9, 71, 72, 73, 135, 136, 137, 143, 144, 145, 167, 168, 169, 271, 272, 273, 335,
    336, 337, 500,
];

fn check_streaming<H: Absorb>(name: &str) {
    let data = vector_message(BOUNDARY_LENGTHS[BOUNDARY_LENGTHS.len() - 1]);
    for len in BOUNDARY_LENGTHS {
        let msg = &data[..len];
        let mut one_shot = H::fresh();
        one_shot.absorb(msg);
        let want = one_shot.output();
        // Every two-piece split.
        for cut in 0..=len {
            let mut h = H::fresh();
            h.absorb(&msg[..cut]);
            h.absorb(&msg[cut..]);
            assert_eq!(h.output(), want, "{name} len {len} cut {cut}");
        }
        // Fixed-size pieces, with an empty update before each.
        for piece in [1, 7, H::RATE] {
            let mut h = H::fresh();
            for chunk in msg.chunks(piece) {
                h.absorb(&[]);
                h.absorb(chunk);
            }
            assert_eq!(h.output(), want, "{name} len {len} piece {piece}");
        }
    }
}

#[test]
fn streaming_matches_one_shot_for_every_split() {
    check_streaming::<Sha3_256>("SHA3-256");
    check_streaming::<Sha3_512>("SHA3-512");
    check_streaming::<Shake128>("SHAKE128");
    check_streaming::<Shake256>("SHAKE256");
}

#[test]
fn incremental_squeeze_matches_one_shot() {
    let msg = vector_message(100);
    let functions = [
        ("SHAKE128", shake128_reader as fn(&[u8]) -> ShakeReader),
        ("SHAKE256", shake256_reader),
    ];
    for (name, reader) in functions {
        let mut want = vec![0; 1000];
        reader(&msg).squeeze(&mut want);
        for piece in [1, 7, 136, 168, 500] {
            let mut r = reader(&msg);
            let mut got = vec![0; 1000];
            for chunk in got.chunks_mut(piece) {
                r.squeeze(chunk);
            }
            assert_eq!(got, want, "{name} piece {piece}");
        }
        // Mixed sizes, so that pieces start and end at every kind of offset.
        let mut r = reader(&msg);
        let mut got = vec![0; 1000];
        let mut rest = &mut got[..];
        for size in [1, 7, 136, 168, 500].into_iter().cycle() {
            if rest.is_empty() {
                break;
            }
            let (head, tail) = rest.split_at_mut(size.min(rest.len()));
            r.squeeze(head);
            rest = tail;
        }
        assert_eq!(got, want, "{name} mixed pieces");
    }
}

#[test]
fn empty_squeeze_is_a_no_op() {
    let want = shake256(b"abc", 300);
    let mut h = Shake256::new();
    h.update(b"abc");
    let mut r = h.finalize_xof();
    let mut got = vec![0; 300];
    r.squeeze(&mut []);
    r.squeeze(&mut got[..5]);
    r.squeeze(&mut []);
    // Up to the end of the first block exactly; an empty squeeze there must not advance.
    r.squeeze(&mut got[5..Shake256::RATE]);
    r.squeeze(&mut []);
    r.squeeze(&mut got[Shake256::RATE..]);
    r.squeeze(&mut []);
    assert_eq!(got, want);
}

#[test]
fn clone_forks_the_stream() {
    let mut h = Sha3_256::new();
    h.update(b"ab");
    let fork = h.clone();
    h.update(b"c");
    assert_eq!(h.finalize(), Sha3_256::digest(b"abc"));
    assert_eq!(fork.finalize(), Sha3_256::digest(b"ab"));

    let mut h = Shake128::new();
    h.update(b"ab");
    let fork = h.clone();
    h.update(b"c");
    let (mut a, mut b) = ([0u8; 40], [0u8; 40]);
    h.finalize_xof().squeeze(&mut a);
    fork.finalize_xof().squeeze(&mut b);
    assert_eq!(a.to_vec(), shake128(b"abc", 40));
    assert_eq!(b.to_vec(), shake128(b"ab", 40));

    // A cloned reader continues from the same position, independently of the original.
    let want = shake256(b"abc", 400);
    let mut h = Shake256::new();
    h.update(b"abc");
    let mut r = h.finalize_xof();
    let mut head = [0u8; 150];
    r.squeeze(&mut head);
    let mut fork = r.clone();
    let (mut a, mut b) = (vec![0u8; 250], vec![0u8; 250]);
    r.squeeze(&mut a);
    fork.squeeze(&mut b[..1]);
    fork.squeeze(&mut b[1..]);
    assert_eq!(head[..], want[..150]);
    assert_eq!(a, want[150..]);
    assert_eq!(b, want[150..]);
}

#[test]
fn debug_output_reveals_no_state() {
    let mut h = Sha3_256::new();
    h.update(b"secret");
    assert_eq!(format!("{h:?}"), "Sha3_256 { .. }");
    assert_eq!(format!("{:?}", Sha3_512::new()), "Sha3_512 { .. }");
    let mut x = Shake128::new();
    x.update(b"secret");
    assert_eq!(format!("{x:?}"), "Shake128 { .. }");
    let mut r = x.finalize_xof();
    r.squeeze(&mut [0; 3]);
    assert_eq!(format!("{r:?}"), "ShakeReader { .. }");
    assert_eq!(format!("{:?}", Shake256::new()), "Shake256 { .. }");
}

/// Product of two polynomials over GF(2), reduced modulo x^8 + x^6 + x^5 + x^4 + 1, the
/// feedback polynomial of FIPS 202 Algorithm 5 (each step multiplies R by x and replaces x^8
/// with x^6 + x^5 + x^4 + 1). Bit i is the coefficient of x^i.
fn gf2_mul_mod(a: u32, b: u32) -> u32 {
    let mut product = 0u32;
    for i in 0..8 {
        if (b >> i) & 1 == 1 {
            product ^= a << i;
        }
    }
    for i in (8..15).rev() {
        if (product >> i) & 1 == 1 {
            product ^= 0b1_0111_0001 << (i - 8);
        }
    }
    product
}

/// rc(t) as the constant term of x^t mod the feedback polynomial, by square-and-multiply.
/// Algorithm 5 instead steps the register t mod 255 times.
fn rc_by_powering(t: u32) -> u64 {
    let (mut result, mut base, mut e) = (1u32, 0b10u32, t);
    while e > 0 {
        if e & 1 == 1 {
            result = gf2_mul_mod(result, base);
        }
        base = gf2_mul_mod(base, base);
        e >>= 1;
    }
    u64::from(result & 1)
}

#[test]
fn round_constants_rederived_independently() {
    // x^255 = 1 modulo the (primitive) feedback polynomial, which is why Algorithm 5 reduces t
    // modulo 255.
    assert_eq!(rc_by_powering(255), 1);
    for (ir, &rc) in ROUND_CONSTANTS.iter().enumerate() {
        let mut want = 0u64;
        for j in 0..=6u32 {
            want |= rc_by_powering(j + 7 * ir as u32) << ((1u32 << j) - 1);
        }
        assert_eq!(rc, want, "round constant {ir}");
    }
    assert_eq!(ROUND_CONSTANTS.len(), 24);
}

/// M^t (1, 0) modulo 5 with M = [[0, 1], [2, 3]], computed by matrix powering: the position
/// of step t of the ρ walk, whose update is (x, y) ← (y, 2x + 3y).
fn rho_position(t: u32) -> (usize, usize) {
    let mul = |a: [[usize; 2]; 2], b: [[usize; 2]; 2]| {
        let mut c = [[0; 2]; 2];
        for i in 0..2 {
            for j in 0..2 {
                c[i][j] = (a[i][0] * b[0][j] + a[i][1] * b[1][j]) % 5;
            }
        }
        c
    };
    let (mut power, mut base, mut e) = ([[1, 0], [0, 1]], [[0, 1], [2, 3]], t);
    while e > 0 {
        if e & 1 == 1 {
            power = mul(power, base);
        }
        base = mul(base, base);
        e >>= 1;
    }
    (power[0][0], power[1][0])
}

#[test]
fn rho_offsets_rederived_independently() {
    let mut seen = [false; 25];
    let mut offsets = std::collections::HashSet::new();
    for t in 0..24u32 {
        let (x, y) = rho_position(t);
        let lane = x + 5 * y;
        assert!(!seen[lane], "the walk revisits lane ({x}, {y})");
        seen[lane] = true;
        let want = ((t + 1) * (t + 2) / 2) % 64;
        assert_eq!(RHO[lane], want, "rho offset of lane ({x}, {y}), step {t}");
        offsets.insert(want);
    }
    // Lane (0, 0) is the only one the walk misses, and it is not rotated.
    assert!(!seen[0] && seen[1..].iter().all(|&s| s));
    assert_eq!(RHO[0], 0);
    // The 24 rotations are distinct and non-zero.
    assert_eq!(offsets.len(), 24);
    assert!(!offsets.contains(&0));
}

#[test]
fn pi_mapping_rederived_independently() {
    // The forward form of the Keccak reference: π moves lane (x, y) to (y, 2x + 3y). FIPS 202
    // Algorithm 3 states the inverse, A′[x, y] = A[(x + 3y) mod 5, x], which keccak.rs uses.
    for y in 0..5 {
        for x in 0..5 {
            let destination = y + 5 * ((2 * x + 3 * y) % 5);
            assert_eq!(PI_SOURCE[destination], x + 5 * y, "pi of lane ({x}, {y})");
        }
    }
    let mut sorted = PI_SOURCE;
    sorted.sort_unstable();
    assert!(sorted.iter().copied().eq(0..25), "pi is a permutation");
}

/// Throughput of the portable code on the build host. Indicative only: the target is
/// x86-64. Run with `cargo test -p origin-crypto --release -- --ignored --nocapture throughput`.
#[test]
#[ignore = "benchmark; run explicitly in release mode"]
fn throughput_sha3() {
    use std::hint::black_box;
    use std::time::Instant;
    const MIB: usize = 64;
    let data = vec![0x5Au8; MIB << 20];
    let report = |name: &str, start: Instant, sink: u8| {
        let secs = start.elapsed().as_secs_f64();
        println!("{name}: {:.0} MiB/s (sink {sink})", (4 * MIB) as f64 / secs);
    };

    let start = Instant::now();
    let mut sink = 0u8;
    for _ in 0..4 {
        sink ^= Sha3_256::digest(black_box(&data))[0];
    }
    report("SHA3-256", start, sink);

    let start = Instant::now();
    let mut sink = 0u8;
    for _ in 0..4 {
        let mut h = Shake256::new();
        h.update(black_box(&data));
        let mut out = [0u8; 32];
        h.finalize_xof().squeeze(&mut out);
        sink ^= out[0];
    }
    report("SHAKE256 absorb", start, sink);

    let mut out = vec![0u8; MIB << 20];
    let start = Instant::now();
    let mut sink = 0u8;
    for _ in 0..4 {
        let mut r = Shake256::new().finalize_xof();
        r.squeeze(black_box(&mut out));
        sink ^= out[out.len() - 1];
    }
    report("SHAKE256 squeeze", start, sink);
}
