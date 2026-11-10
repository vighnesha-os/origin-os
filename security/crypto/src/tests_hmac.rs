//! HMAC-SHA-512 tests (CLAUDE.md §59).
//!
//! - Known answers: RFC 4231's seven test cases, from the RFC's own text. Each expected MAC was
//!   confirmed against Python's `hmac` before being embedded.
//! - Differential: 273 vectors in `tests/vectors/hmac-sha512-differential.txt`, on which
//!   Python's `hmac` and RFC 2104's formula written out over hashlib's SHA-512 agreed.
//! - The definition: the MAC rebuilt in this file from RFC 2104's formula and the public hash.
//! - Streaming: every split of a message must give the one-shot MAC.
//! - The secret-material paths of SHA-512 that HMAC is built on must hash what the public
//!   ones hash, and leave a hasher that has absorbed nothing.

use crate::block::BlockBuffer;
use crate::wipe::{wipe, wipe_words};
use crate::{HmacSha512, Sha512};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The key `tools/gen_hmac_vectors.py` uses for length `k`.
fn vector_key(k: usize) -> Vec<u8> {
    (0..k)
        .map(|i| ((i * 17 + k * 3 + 5) & 0xFF) as u8)
        .collect()
}

/// The message `tools/gen_hmac_vectors.py` uses for length `n`.
fn vector_message(n: usize) -> Vec<u8> {
    (0..n)
        .map(|i| ((i * 31 + n * 7 + 13) & 0xFF) as u8)
        .collect()
}

/// RFC 2104 §2, written out over the public hash: `H((K0 ^ opad) || H((K0 ^ ipad) || text))`.
fn by_definition(key: &[u8], text: &[u8]) -> [u8; 64] {
    let mut k0 = [0u8; 128];
    if key.len() > 128 {
        k0[..64].copy_from_slice(&Sha512::digest(key));
    } else {
        k0[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha512::new();
    inner.update(&k0.map(|b| b ^ 0x36));
    inner.update(text);
    let mut outer = Sha512::new();
    outer.update(&k0.map(|b| b ^ 0x5c));
    outer.update(&inner.finalize());
    outer.finalize()
}

/// RFC 4231 §4.2 to §4.8: the key, the data and the HMAC-SHA-512 value of each test case. Test
/// case 5's value is the RFC's truncation to 128 bits.
#[test]
fn rfc_4231_known_answers() {
    let cases: [(&[u8], &[u8], &str); 7] = [
        (
            &[0x0b; 20],
            b"Hi There",
            "87aa7cdea5ef619d4ff0b4241a1d6cb02379f4e2ce4ec2787ad0b30545e17cdedaa833b7d6b8a702038b274eaea3f4e4be9d914eeb61f1702e696c203a126854",
        ),
        (
            b"Jefe",
            b"what do ya want for nothing?",
            "164b7a7bfcf819e2e395fbe73b56e0a387bd64222e831fd610270cd7ea2505549758bf75c05a994a6d034f65f8f0e6fdcaeab1a34d4a6b4b636e070a38bce737",
        ),
        (
            &[0xaa; 20],
            &[0xdd; 50],
            "fa73b0089d56a284efb0f0756c890be9b1b5dbdd8ee81a3655f83e33b2279d39bf3e848279a722c806b485a47e67c807b946a337bee8942674278859e13292fb",
        ),
        (
            &[
                0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
                0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19,
            ],
            &[0xcd; 50],
            "b0ba465637458c6990e5a8c5f61d4af7e576d97ff94b872de76f8050361ee3dba91ca5c11aa25eb4d679275cc5788063a5f19741120c4f2de2adebeb10a298dd",
        ),
        (
            &[0x0c; 20],
            b"Test With Truncation",
            "415fad6271580a531d4179bc891d87a6",
        ),
        (
            &[0xaa; 131],
            b"Test Using Larger Than Block-Size Key - Hash Key First",
            "80b24263c7c1a3ebb71493c1dd7be8b49b46d1f41b4aeec1121b013783f8f3526b56d037e05f2598bd0fd2215d6a1e5295e64f73f63f0aec8b915a985d786598",
        ),
        (
            &[0xaa; 131],
            b"This is a test using a larger than block-size key and a larger than block-size data. The key needs to be hashed before being used by the HMAC algorithm.",
            "e37b6a775dc87dbaa4dfa9f96e5e3ffddebd71f8867289865df5a32d20cdc944b6022cac3c4982b10d5eeb55c3e4de15134676fb6de0446065c97440fa8c6a58",
        ),
    ];
    for (number, (key, data, want)) in cases.into_iter().enumerate() {
        let mac = hex(&HmacSha512::mac(key, data));
        // Only test case 5 is truncated, to 128 bits.
        assert_eq!(want.len() == 32, number == 4);
        assert_eq!(
            &mac[..want.len()],
            want,
            "RFC 4231 test case {}",
            number + 1
        );
    }
}

#[test]
fn differential_vectors_match_two_reference_computations() {
    let file = include_str!("../tests/vectors/hmac-sha512-differential.txt");
    let mut count = 0;
    for line in file
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let mut cols = line.split_whitespace();
        let k: usize = cols.next().unwrap().parse().unwrap();
        let n: usize = cols.next().unwrap().parse().unwrap();
        let want = cols.next().unwrap();
        assert_eq!(
            hex(&HmacSha512::mac(&vector_key(k), &vector_message(n))),
            want,
            "key of {k} bytes, message of {n} bytes"
        );
        count += 1;
    }
    assert_eq!(count, 273);
}

/// Every key length from none to past two blocks, against the formula written out here: the
/// block-length boundary, where a key starts being hashed, is at 128 and 129.
#[test]
fn every_key_length_matches_the_definition() {
    for k in 0..=260 {
        let key = vector_key(k);
        for n in [0, 1, 111, 112, 128, 300] {
            let text = vector_message(n);
            assert_eq!(
                HmacSha512::mac(&key, &text),
                by_definition(&key, &text),
                "key of {k} bytes, message of {n} bytes"
            );
        }
    }
}

#[test]
fn streaming_matches_one_shot_for_every_split() {
    let data = vector_message(400);
    for k in [0, 20, 64, 128, 129] {
        let key = vector_key(k);
        for len in [
            0, 1, 110, 111, 112, 113, 127, 128, 129, 239, 240, 241, 256, 400,
        ] {
            let text = &data[..len];
            let want = HmacSha512::mac(&key, text);
            for cut in 0..=len {
                let mut mac = HmacSha512::new(&key);
                mac.update(&text[..cut]);
                mac.update(&text[cut..]);
                assert_eq!(mac.finalize(), want, "key {k}, length {len}, cut {cut}");
            }
            for piece in [1, 7, 64, 129] {
                let mut mac = HmacSha512::new(&key);
                for chunk in text.chunks(piece) {
                    mac.update(&[]);
                    mac.update(chunk);
                }
                assert_eq!(mac.finalize(), want, "key {k}, length {len}, piece {piece}");
            }
        }
    }
}

/// A key and its hash are different keys only above the block length: RFC 2104 hashes a key
/// "longer than B bytes", so a 129-byte key and its 64-byte digest give one MAC, and a
/// 128-byte key and its digest do not.
#[test]
fn only_a_key_longer_than_a_block_is_hashed() {
    let text = b"the same message";
    let long = vector_key(129);
    assert_eq!(
        HmacSha512::mac(&long, text),
        HmacSha512::mac(&Sha512::digest(&long), text)
    );
    let block = vector_key(128);
    assert_ne!(
        HmacSha512::mac(&block, text),
        HmacSha512::mac(&Sha512::digest(&block), text)
    );
    // A key is filled with zeros to the block length, so trailing zero bytes do not change it.
    let mut padded = vector_key(20);
    let short = HmacSha512::mac(&padded, text);
    padded.resize(128, 0);
    assert_eq!(HmacSha512::mac(&padded, text), short);
}

#[test]
fn the_key_and_the_message_both_matter() {
    let mac = HmacSha512::mac(b"key", b"message");
    assert_ne!(mac, HmacSha512::mac(b"kez", b"message"));
    assert_ne!(mac, HmacSha512::mac(b"key", b"messagf"));
    assert_ne!(mac, HmacSha512::mac(b"", b"message"));
    assert_ne!(mac, HmacSha512::mac(b"key", b""));
    // Not the plain hash of either arrangement.
    assert_ne!(mac, Sha512::digest(b"keymessage"));
}

#[test]
fn debug_output_reveals_nothing() {
    let mut mac = HmacSha512::new(b"a secret key");
    mac.update(b"a secret message");
    assert_eq!(format!("{mac:?}"), "HmacSha512 { .. }");
}

/// The paths HMAC hashes through give the digests the public paths give, at every split, and
/// leave a hasher that has absorbed nothing.
#[test]
fn the_secret_paths_of_sha512_hash_what_the_public_ones_do() {
    let data = vector_message(700);
    for len in [0, 1, 111, 112, 127, 128, 129, 239, 240, 255, 256, 700] {
        let text = &data[..len];
        let want = Sha512::digest(text);
        for cut in 0..=len {
            let mut hash = Sha512::new();
            hash.update_secret(&text[..cut]);
            hash.update_secret(&text[cut..]);
            let mut out = [0u8; 64];
            hash.finalize_secret(&mut out);
            assert_eq!(out, want, "length {len}, cut {cut}");
            // The hasher is as new: it hashes the next message alone.
            hash.update_secret(b"abc");
            hash.finalize_secret(&mut out);
            assert_eq!(out, Sha512::digest(b"abc"), "reuse after length {len}");
        }
    }
}

/// `wipe` leaves zeros where the caller can still see them. That a compiler keeps the write
/// when nothing reads the buffer afterwards cannot be shown from safe code: `crate::wipe` says
/// what is promised.
#[test]
fn wiped_buffers_read_as_zero() {
    let mut bytes = [0xA5u8; 97];
    wipe(&mut bytes);
    assert!(bytes.iter().all(|&b| b == 0));
    let mut words = [u64::MAX; 16];
    wipe_words(&mut words);
    assert!(words.iter().all(|&w| w == 0));
    wipe(&mut []);
    wipe_words(&mut []);

    // A block buffer forgets a partial block: what it then pads is the empty message.
    let mut buffer = BlockBuffer::<128>::new();
    buffer.update(&[0x5A; 100], |_| panic!("no whole block was fed"));
    buffer.wipe();
    let mut padded = [0xFFu8; 128];
    buffer.finish_in_place([0u8; 16], |block| padded = *block);
    let mut empty = [0u8; 128];
    empty[0] = 0x80;
    assert_eq!(padded, empty);
}

/// Throughput on the build host. Indicative only: the target is x86-64. Run with
/// `cargo test -p origin-crypto --release -- --ignored --nocapture hmac_throughput`.
#[test]
#[ignore = "benchmark; run explicitly in release mode"]
fn hmac_throughput() {
    use std::time::Instant;
    let key = vector_key(64);
    let short = vector_message(64);
    let start = Instant::now();
    let rounds = 200_000u32;
    let mut sink = 0u8;
    for _ in 0..rounds {
        sink ^= HmacSha512::mac(std::hint::black_box(&key), std::hint::black_box(&short))[0];
    }
    let nanos = start.elapsed().as_nanos() / u128::from(rounds);
    println!(
        "HMAC-SHA-512, 64-byte key and message, key set up each time: {nanos} ns (sink {sink})"
    );
    let data = vec![0x5Au8; 64 << 20];
    let start = Instant::now();
    let sink = HmacSha512::mac(&key, std::hint::black_box(&data))[0];
    let secs = start.elapsed().as_secs_f64();
    println!(
        "HMAC-SHA-512, 64 MiB: {:.0} MiB/s (sink {sink})",
        64.0 / secs
    );
}
