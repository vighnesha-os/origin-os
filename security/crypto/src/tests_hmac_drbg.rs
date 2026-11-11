//! HMAC_DRBG tests (CLAUDE.md §59; ADR-0016).
//!
//! - Known answers from a published text: the six SHA-512 nonces of RFC 6979 appendix A.2.5 to
//!   A.2.7, which that RFC's §3.3 defines as HMAC_DRBG output. They cover instantiation and one
//!   request, of one and of two output blocks. Each was reproduced by an independent
//!   computation in Python before being embedded.
//! - Differential: the corpus in `tests/vectors/hmac-drbg-differential.txt`, written by
//!   `tools/hmac_drbg_reference.py`, an independent implementation with the same author. It
//!   covers reseeding, additional input, personalization strings, requests with prediction
//!   resistance, and the lengths around every block boundary.
//! - The state machine: what is refused, in which order, and that a refusal changes nothing.
//!
//! NIST's own validation vectors, which give published answers for reseeding, additional input
//! and prediction resistance, are run by `tests/kat/drbg.rs`.

use crate::{DrbgError, HmacDrbgSha512, HmacSha512, Sha512};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The bytes of a hexadecimal string; `-` is the empty string. An odd number of digits is a
/// number whose first digit stands alone, as RFC 6979 prints its 521-bit values.
fn unhex(text: &str) -> Vec<u8> {
    if text == "-" {
        return Vec::new();
    }
    let digits: Vec<u8> = text
        .chars()
        .map(|c| c.to_digit(16).expect("a hexadecimal digit") as u8)
        .collect();
    let mut bytes = Vec::new();
    let odd = digits.len() % 2;
    if odd == 1 {
        bytes.push(digits[0]);
    }
    bytes.extend(digits[odd..].chunks(2).map(|pair| pair[0] << 4 | pair[1]));
    bytes
}

/// `value` as a big-endian number of exactly `len` bytes.
fn left_pad(value: &[u8], len: usize) -> Vec<u8> {
    assert!(value.len() <= len);
    let mut padded = vec![0; len - value.len()];
    padded.extend_from_slice(value);
    padded
}

/// A deterministic byte string for a test: SHA-512 in counter mode over `label`.
fn bytes_for(label: &str, len: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut counter = 0u32;
    while out.len() < len {
        let mut hash = Sha512::new();
        hash.update(label.as_bytes());
        hash.update(&counter.to_be_bytes());
        out.extend_from_slice(&hash.finalize());
        counter += 1;
    }
    out.truncate(len);
    out
}

/// A generator instantiated from `label`, with the shortest seed.
fn instantiated(label: &str) -> HmacDrbgSha512 {
    let mut drbg = HmacDrbgSha512::new();
    seed(&mut drbg, label);
    drbg
}

fn seed(drbg: &mut HmacDrbgSha512, label: &str) {
    drbg.instantiate(
        &bytes_for(&format!("{label} entropy"), 32),
        &bytes_for(&format!("{label} nonce"), 16),
        &[],
    )
    .unwrap();
}

/// The next `len` bytes of `drbg`.
fn next(drbg: &mut HmacDrbgSha512, len: usize) -> Vec<u8> {
    let mut out = vec![0; len];
    drbg.generate(&mut out, &[]).unwrap();
    out
}

// ------------------------------------------------------------------ RFC 6979

/// RFC 6979 §2.3.4, `bits2octets`, for a SHA-512 digest and a group order `q` of `qlen` bits:
/// the digest's leftmost `qlen` bits (all of it when `qlen` is larger), reduced modulo `q`, as
/// `ceil(qlen / 8)` bytes. Every `q` here is a whole number of bytes or longer than the digest,
/// so "leftmost bits" is "leftmost bytes", and one subtraction reduces.
fn bits2octets(digest: &[u8; 64], q: &[u8], qlen: usize) -> Vec<u8> {
    let octets = qlen.div_ceil(8);
    let z1 = if qlen >= 512 {
        left_pad(digest, octets)
    } else {
        assert_eq!(qlen % 8, 0);
        digest[..octets].to_vec()
    };
    let q = left_pad(q, octets);
    if z1 < q {
        return z1;
    }
    // z1 - q, big-endian.
    let mut z2 = vec![0u8; octets];
    let mut borrow = 0i16;
    for i in (0..octets).rev() {
        let difference = i16::from(z1[i]) - i16::from(q[i]) - borrow;
        z2[i] = difference.rem_euclid(256) as u8;
        borrow = i16::from(difference < 0);
    }
    assert_eq!(borrow, 0);
    z2
}

/// A big-endian number shifted right by `bits` (fewer than 8).
fn shifted_right(value: &[u8], bits: usize) -> Vec<u8> {
    assert!(bits < 8);
    let mut out = vec![0u8; value.len()];
    let mut carry = 0u16;
    for (dst, &byte) in out.iter_mut().zip(value) {
        let wide = carry << 8 | u16::from(byte);
        *dst = (wide >> bits) as u8;
        carry = wide & ((1 << bits) - 1);
    }
    out
}

/// RFC 6979 appendix A.2.5 (P-256), A.2.6 (P-384) and A.2.7 (P-521): the group order `q`, the
/// private key `x`, and `k` for SHA-512 with the messages "sample" and "test".
///
/// §3.3 of the RFC gives the generation of `k` as an HMAC_DRBG instantiated with
/// `int2octets(x)` as its entropy input and `bits2octets(H(m))` as its nonce, with no
/// personalization string, asked for `qlen` bits. P-521 needs 521 bits, so two output blocks.
#[test]
fn rfc_6979_known_answers() {
    struct Curve {
        qlen: usize,
        q: &'static str,
        x: &'static str,
        k_sample: &'static str,
        k_test: &'static str,
    }
    let curves = [
        Curve {
            qlen: 256,
            q: "FFFFFFFF00000000FFFFFFFFFFFFFFFFBCE6FAADA7179E84F3B9CAC2FC632551",
            x: "C9AFA9D845BA75166B5C215767B1D6934E50C3DB36E89B127B8A622B120F6721",
            k_sample: "5FA81C63109BADB88C1F367B47DA606DA28CAD69AA22C4FE6AD7DF73A7173AA5",
            k_test: "6915D11632ACA3C40D5D51C08DAF9C555933819548784480E93499000D9F0B7F",
        },
        Curve {
            qlen: 384,
            q: "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFC7634D81F4372DDF581A0DB248B0A77AECEC196ACCC52973",
            x: "6B9D3DAD2E1B8C1C05B19875B6659F4DE23C3B667BF297BA9AA47740787137D896D5724E4C70A825F872C9EA60D2EDF5",
            k_sample: "92FC3C7183A883E24216D1141F1A8976C5B0DD797DFA597E3D7B32198BD35331A4E966532593A52980D0E3AAA5E10EC3",
            k_test: "3780C4F67CB15518B6ACAE34C9F83568D2E12E47DEAB6C50A4E4EE5319D1E8CE0E2CC8A136036DC4B9C00E6888F66B6C",
        },
        Curve {
            qlen: 521,
            q: "1FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFA51868783BF2F966B7FCC0148F709A5D03BB5C9B8899C47AEBB6FB71E91386409",
            x: "0FAD06DAA62BA3B25D2FB40133DA757205DE67F5BB0018FEE8C86E1B68C7E75CAA896EB32F1F47C70855836A6D16FCC1466F6D8FBEC67DB89EC0C08B0E996B83538",
            k_sample: "1DAE2EA071F8110DC26882D4D5EAE0621A3256FC8847FB9022E2B7D28E6F10198B1574FDD03A9053C08A1854A168AA5A57470EC97DD5CE090124EF52A2F7ECBFFD3",
            k_test: "16200813020EC986863BEDFC1B121F605C1215645018AEA1A7B215A564DE9EB1B38A67AA1128B80CE391C4FB71187654AAA3431027BFC7F395766CA988C964DC56D",
        },
    ];
    for curve in curves {
        let octets = curve.qlen.div_ceil(8);
        let q = unhex(curve.q);
        let x = left_pad(&unhex(curve.x), octets);
        for (message, k) in [("sample", curve.k_sample), ("test", curve.k_test)] {
            let nonce = bits2octets(&Sha512::digest(message.as_bytes()), &q, curve.qlen);
            let mut drbg = HmacDrbgSha512::new();
            drbg.instantiate(&x, &nonce, &[]).unwrap();
            let mut t = vec![0u8; octets];
            drbg.generate(&mut t, &[]).unwrap();
            // bits2int: the leftmost qlen bits of what was generated.
            let candidate = shifted_right(&t, octets * 8 - curve.qlen);
            assert_eq!(
                hex(&candidate),
                hex(&left_pad(&unhex(k), octets)),
                "RFC 6979, qlen {}, message {message:?}",
                curve.qlen
            );
        }
    }
}

/// The helpers above, against values worked by hand, so a known-answer test cannot pass
/// because a helper is wrong in a way that hides a wrong generator.
#[test]
fn the_rfc_6979_helpers_do_what_they_say() {
    assert_eq!(unhex("-"), Vec::<u8>::new());
    assert_eq!(unhex("0a1B"), [0x0a, 0x1b]);
    assert_eq!(unhex("1FF"), [0x01, 0xff]);
    assert_eq!(left_pad(&[1, 2], 4), [0, 0, 1, 2]);
    assert_eq!(shifted_right(&[0x01, 0x80, 0xff], 7), [0x00, 0x03, 0x01]);
    assert_eq!(shifted_right(&[0xab, 0xcd], 0), [0xab, 0xcd]);
    // A digest whose leftmost bytes are below q, equal to it and above it.
    let q = [0x80, 0x00, 0x00, 0x05];
    let mut digest = [0u8; 64];
    digest[..4].copy_from_slice(&[0x80, 0x00, 0x00, 0x04]);
    assert_eq!(bits2octets(&digest, &q, 32), [0x80, 0x00, 0x00, 0x04]);
    digest[..4].copy_from_slice(&[0x80, 0x00, 0x00, 0x05]);
    assert_eq!(bits2octets(&digest, &q, 32), [0, 0, 0, 0]);
    digest[..4].copy_from_slice(&[0x81, 0x00, 0x00, 0x04]);
    assert_eq!(bits2octets(&digest, &q, 32), [0x00, 0xff, 0xff, 0xff]);
}

// ------------------------------------------------------------------ the differential corpus

/// Checks `got` against a corpus output: `=` and the bytes, or `#` and their SHA-512 digest.
fn assert_output(got: &[u8], want: &str, context: &str) {
    match want.split_at(1) {
        ("=", bytes) => assert_eq!(hex(got), bytes, "{context}"),
        ("#", digest) => assert_eq!(hex(&Sha512::digest(got)), digest, "{context}"),
        _ => panic!("{context}: an output is `=` or `#`"),
    }
}

#[test]
fn differential_corpus_matches_the_reference() {
    let file = include_str!("../tests/vectors/hmac-drbg-differential.txt");
    let mut drbg = HmacDrbgSha512::new();
    let (mut cases, mut reseeds, mut requests, mut predictions) = (0, 0, 0, 0);
    let mut name = "";
    for (number, line) in file.lines().enumerate() {
        let context = format!("line {}, case {name}", number + 1);
        let fields: Vec<&str> = line.split_whitespace().collect();
        match fields[..] {
            [] => {}
            ["#", ..] => {}
            ["case", case] => name = case,
            ["i", entropy, nonce, personalization] => {
                drbg.uninstantiate();
                drbg.instantiate(&unhex(entropy), &unhex(nonce), &unhex(personalization))
                    .expect(&context);
                cases += 1;
            }
            ["r", entropy, additional] => {
                drbg.reseed(&unhex(entropy), &unhex(additional))
                    .expect(&context);
                reseeds += 1;
            }
            ["g", len, additional, want] => {
                let mut out = vec![0xEE; len.parse().unwrap()];
                drbg.generate(&mut out, &unhex(additional)).expect(&context);
                assert_output(&out, want, &context);
                requests += 1;
            }
            ["p", len, entropy, additional, want] => {
                let mut out = vec![0xEE; len.parse().unwrap()];
                drbg.reseed_and_generate(&mut out, &unhex(entropy), &unhex(additional))
                    .expect(&context);
                assert_output(&out, want, &context);
                predictions += 1;
            }
            _ => panic!("{context}: not a corpus line"),
        }
    }
    // Pinned, so a case cannot silently go.
    assert_eq!((cases, reseeds, requests, predictions), (145, 74, 629, 79));
}

// ------------------------------------------------------------------ the state machine

#[test]
fn a_generator_with_no_state_answers_nothing() {
    let mut drbg = HmacDrbgSha512::new();
    assert!(!drbg.is_instantiated());
    assert!(!drbg.reseed_required());
    assert_eq!(drbg.requests_since_seed(), 0);
    let mut out = [0xEE; 40];
    assert_eq!(
        drbg.generate(&mut out, &[]),
        Err(DrbgError::NotInstantiated)
    );
    assert_eq!(drbg.reseed(&[7; 32], &[]), Err(DrbgError::NotInstantiated));
    assert_eq!(
        drbg.reseed_and_generate(&mut out, &[7; 32], &[]),
        Err(DrbgError::NotInstantiated)
    );
    assert_eq!(out, [0xEE; 40], "a refusal writes nothing");
    assert!(drbg.state_is_overwritten());
    assert!(HmacDrbgSha512::default().state_is_overwritten());
}

#[test]
fn instantiation_checks_its_inputs_in_order() {
    let mut drbg = HmacDrbgSha512::new();
    let (entropy, nonce) = ([3u8; 64], [4u8; 32]);
    // Too little entropy input is reported before a short nonce.
    assert_eq!(
        drbg.instantiate(&entropy[..31], &nonce[..15], &[]),
        Err(DrbgError::EntropyTooShort)
    );
    assert_eq!(
        drbg.instantiate(&[], &nonce, &[]),
        Err(DrbgError::EntropyTooShort)
    );
    assert_eq!(
        drbg.instantiate(&entropy[..32], &nonce[..15], &[]),
        Err(DrbgError::NonceTooShort)
    );
    assert_eq!(
        drbg.instantiate(&entropy[..32], &[], &[]),
        Err(DrbgError::NonceTooShort)
    );
    assert!(!drbg.is_instantiated());
    assert!(drbg.state_is_overwritten(), "a refusal leaves no state");

    assert_eq!(drbg.instantiate(&entropy[..32], &nonce[..16], &[]), Ok(()));
    assert!(drbg.is_instantiated());
    assert!(!drbg.state_is_overwritten());
    assert_eq!(drbg.requests_since_seed(), 0);

    // A state is never replaced by a second instantiation.
    let mut twin = HmacDrbgSha512::new();
    twin.instantiate(&entropy[..32], &nonce[..16], &[]).unwrap();
    assert_eq!(
        drbg.instantiate(&[9; 32], &[9; 16], &[]),
        Err(DrbgError::AlreadyInstantiated)
    );
    assert_eq!(next(&mut drbg, 64), next(&mut twin, 64));
}

#[test]
fn reseeding_checks_its_entropy_input() {
    let mut drbg = instantiated("reseed checks");
    let mut twin = instantiated("reseed checks");
    assert_eq!(drbg.reseed(&[5; 31], &[]), Err(DrbgError::EntropyTooShort));
    assert_eq!(
        drbg.reseed(&[], &[1, 2, 3]),
        Err(DrbgError::EntropyTooShort)
    );
    let mut out = [0xEE; 16];
    assert_eq!(
        drbg.reseed_and_generate(&mut out, &[5; 31], &[]),
        Err(DrbgError::EntropyTooShort)
    );
    assert_eq!(out, [0xEE; 16]);
    // The refusals changed nothing.
    assert_eq!(next(&mut drbg, 64), next(&mut twin, 64));
    // A reseed does.
    drbg.reseed(&[5; 32], &[]).unwrap();
    assert_ne!(next(&mut drbg, 64), next(&mut twin, 64));
}

#[test]
fn the_reseed_interval_is_enforced() {
    assert!(HmacDrbgSha512::with_reseed_interval(0).is_none());
    assert!(HmacDrbgSha512::with_reseed_interval((1 << 48) + 1).is_none());
    assert!(HmacDrbgSha512::with_reseed_interval(u64::MAX).is_none());
    assert!(HmacDrbgSha512::with_reseed_interval(1 << 48).is_some());
    assert_eq!(HmacDrbgSha512::MAX_RESEED_INTERVAL, 1 << 48);

    for interval in [1u64, 2, 3, 10] {
        let mut drbg = HmacDrbgSha512::with_reseed_interval(interval).unwrap();
        let mut twin = HmacDrbgSha512::new();
        seed(&mut drbg, "interval");
        seed(&mut twin, "interval");
        // Exactly `interval` requests are answered, and they are the ones a generator with
        // the longest interval gives.
        for request in 0..interval {
            assert!(!drbg.reseed_required());
            assert_eq!(drbg.requests_since_seed(), request);
            assert_eq!(
                next(&mut drbg, 24),
                next(&mut twin, 24),
                "request {request}"
            );
        }
        assert!(drbg.reseed_required());
        assert_eq!(drbg.requests_since_seed(), interval);
        // The next is refused, as often as it is asked, and writes nothing. A request of no
        // bytes is refused too, and one with additional input.
        for _ in 0..3 {
            let mut out = [0xEE; 24];
            assert_eq!(drbg.generate(&mut out, &[]), Err(DrbgError::ReseedRequired));
            assert_eq!(out, [0xEE; 24]);
            assert_eq!(
                drbg.generate(&mut [], &[1, 2, 3]),
                Err(DrbgError::ReseedRequired)
            );
        }
        // The refusals changed nothing: after the same reseed, both give the same bytes.
        drbg.reseed(&[6; 32], b"more").unwrap();
        twin.reseed(&[6; 32], b"more").unwrap();
        assert!(!drbg.reseed_required());
        assert_eq!(drbg.requests_since_seed(), 0);
        for request in 0..interval {
            assert_eq!(
                next(&mut drbg, 24),
                next(&mut twin, 24),
                "request {request}"
            );
        }
        // "Reseed required" is answered by a request that brings its own entropy.
        let (mut out, mut twin_out) = ([0u8; 24], [0u8; 24]);
        assert_eq!(drbg.generate(&mut out, &[]), Err(DrbgError::ReseedRequired));
        drbg.reseed_and_generate(&mut out, &[8; 32], b"add")
            .unwrap();
        twin.reseed_and_generate(&mut twin_out, &[8; 32], b"add")
            .unwrap();
        assert_eq!(out, twin_out);
        assert_eq!(drbg.requests_since_seed(), 1);
    }
}

#[test]
fn a_request_is_limited_to_65536_bytes() {
    assert_eq!(HmacDrbgSha512::MAX_REQUEST_LEN, 65536);
    let mut drbg = instantiated("request limit");
    let mut twin = instantiated("request limit");
    let mut out = vec![0xEE; 65537];
    assert_eq!(drbg.generate(&mut out, &[]), Err(DrbgError::RequestTooLong));
    assert_eq!(
        drbg.reseed_and_generate(&mut out, &[1; 32], &[]),
        Err(DrbgError::RequestTooLong),
        "refused before the entropy is used"
    );
    assert!(out.iter().all(|&b| b == 0xEE));
    assert_eq!(drbg.requests_since_seed(), 0);
    // The refusals changed nothing, and the longest request is answered.
    out.truncate(65536);
    let mut twin_out = vec![0; 65536];
    drbg.generate(&mut out, &[]).unwrap();
    twin.generate(&mut twin_out, &[]).unwrap();
    assert_eq!(out, twin_out);
    assert!(out.iter().any(|&b| b != 0xEE));
}

/// The length and state checks come before the count: a request that could never be answered
/// is told so, not told to reseed.
#[test]
fn refusals_come_in_the_standards_order() {
    let mut drbg = HmacDrbgSha512::with_reseed_interval(1).unwrap();
    seed(&mut drbg, "order");
    next(&mut drbg, 1);
    assert!(drbg.reseed_required());
    assert_eq!(
        drbg.generate(&mut vec![0; 65537], &[]),
        Err(DrbgError::RequestTooLong)
    );
    assert_eq!(
        drbg.generate(&mut [0; 8], &[]),
        Err(DrbgError::ReseedRequired)
    );
    drbg.uninstantiate();
    assert_eq!(
        drbg.generate(&mut vec![0; 65537], &[]),
        Err(DrbgError::NotInstantiated)
    );
}

#[test]
fn the_count_follows_requests_and_seeds() {
    let mut drbg = instantiated("count");
    for request in 0..5 {
        assert_eq!(drbg.requests_since_seed(), request);
        // A request of no bytes is a request: the state moves on and the count with it.
        drbg.generate(&mut [], &[]).unwrap();
    }
    drbg.reseed(&[2; 32], &[]).unwrap();
    assert_eq!(drbg.requests_since_seed(), 0);
    drbg.reseed_and_generate(&mut [0; 8], &[2; 32], &[])
        .unwrap();
    assert_eq!(drbg.requests_since_seed(), 1);
    drbg.uninstantiate();
    assert_eq!(drbg.requests_since_seed(), 0);
}

/// A request of no bytes still moves the state on, as the standard's steps do.
#[test]
fn a_request_of_no_bytes_moves_the_state() {
    let mut drbg = instantiated("empty request");
    let mut twin = instantiated("empty request");
    drbg.generate(&mut [], &[]).unwrap();
    assert_ne!(next(&mut drbg, 64), next(&mut twin, 64));
}

#[test]
fn uninstantiation_overwrites_the_state() {
    let mut drbg = instantiated("uninstantiate");
    let first = next(&mut drbg, 64);
    assert!(!drbg.state_is_overwritten());
    drbg.uninstantiate();
    assert!(drbg.state_is_overwritten());
    assert!(!drbg.is_instantiated());
    assert_eq!(
        drbg.generate(&mut [0; 8], &[]),
        Err(DrbgError::NotInstantiated)
    );
    // Uninstantiating twice is harmless, and the generator can be given a state again, which
    // owes nothing to the old one.
    drbg.uninstantiate();
    seed(&mut drbg, "uninstantiate");
    assert_eq!(next(&mut drbg, 64), first);
}

#[test]
fn equal_seeds_give_equal_bytes_and_any_other_seed_gives_others() {
    let entropy = bytes_for("sensitivity entropy", 48);
    let nonce = bytes_for("sensitivity nonce", 24);
    let personalization = bytes_for("sensitivity personalization", 20);
    let output = |entropy: &[u8], nonce: &[u8], personalization: &[u8], additional: &[u8]| {
        let mut drbg = HmacDrbgSha512::new();
        drbg.instantiate(entropy, nonce, personalization).unwrap();
        let mut out = [0u8; 96];
        drbg.generate(&mut out, additional).unwrap();
        out
    };
    let base = output(&entropy, &nonce, &personalization, b"additional");
    assert_eq!(
        base,
        output(&entropy, &nonce, &personalization, b"additional")
    );
    // One bit of any input, at its first, middle and last byte.
    for (which, len) in [(0, 48), (1, 24), (2, 20), (3, 10)] {
        for at in [0, len / 2, len - 1] {
            let mut inputs = [
                entropy.clone(),
                nonce.clone(),
                personalization.clone(),
                b"additional".to_vec(),
            ];
            inputs[which][at] ^= 0x10;
            let [e, n, p, a] = &inputs;
            assert_ne!(base, output(e, n, p, a), "input {which}, byte {at}");
        }
    }
    // With and without a personalization string, and with and without additional input.
    assert_ne!(base, output(&entropy, &nonce, &[], b"additional"));
    assert_ne!(base, output(&entropy, &nonce, &personalization, &[]));
}

/// The seed is `entropy_input || nonce || personalization_string` as one string: where the
/// parameters divide it does not matter (RFC 6979 §3.3 says as much of the first two). The
/// same holds for a reseed's `entropy_input || additional_input`. This is also what shows the
/// generator feeds HMAC the parts in order with nothing between them.
#[test]
fn the_seed_is_the_concatenation_of_its_parts() {
    let material = bytes_for("concatenation", 200);
    let output = |entropy: &[u8], nonce: &[u8], personalization: &[u8]| {
        let mut drbg = HmacDrbgSha512::new();
        drbg.instantiate(entropy, nonce, personalization).unwrap();
        next(&mut drbg, 80)
    };
    let base = output(&material[..32], &material[32..48], &material[48..]);
    for (a, b) in [
        (32, 200),
        (33, 49),
        (64, 128),
        (100, 116),
        (184, 200),
        (47, 63),
        (63, 190),
    ] {
        assert_eq!(
            base,
            output(&material[..a], &material[a..b], &material[b..]),
            "divided at {a} and {b}"
        );
    }
    // One byte moved from the end to the front is another seed.
    let mut rotated = material.clone();
    rotated.rotate_right(1);
    assert_ne!(
        base,
        output(&rotated[..32], &rotated[32..48], &rotated[48..])
    );

    let reseeded = |entropy: &[u8], additional: &[u8]| {
        let mut drbg = instantiated("concatenation");
        drbg.reseed(entropy, additional).unwrap();
        next(&mut drbg, 80)
    };
    let base = reseeded(&material[..32], &material[32..]);
    for at in [33, 47, 63, 64, 128, 200] {
        assert_eq!(
            base,
            reseeded(&material[..at], &material[at..]),
            "divided at {at}"
        );
    }
}

/// A request's bytes are the leftmost bytes of a longer request from the same state.
#[test]
fn a_shorter_request_is_the_start_of_a_longer_one() {
    let longest = next(&mut instantiated("leftmost"), 300);
    for len in [0, 1, 63, 64, 65, 127, 128, 129, 192, 299] {
        assert_eq!(
            next(&mut instantiated("leftmost"), len),
            longest[..len],
            "{len} bytes"
        );
    }
    // And two requests are not one request cut in two: the state moves between them.
    let mut drbg = instantiated("leftmost");
    let mut both = next(&mut drbg, 64);
    both.extend(next(&mut drbg, 64));
    assert_eq!(both[..64], longest[..64]);
    assert_ne!(both[64..], longest[64..128]);
}

/// The generator's first two steps, worked here from the standard's formulas with the public
/// HMAC: an instantiation, then one request of one block with no additional input.
#[test]
fn the_first_request_matches_the_standards_steps_worked_by_hand() {
    let entropy = bytes_for("by hand entropy", 32);
    let nonce = bytes_for("by hand nonce", 16);
    let personalization = b"by hand";
    let seed_material = [entropy.as_slice(), &nonce, personalization].concat();
    let hmac = |key: &[u8], parts: &[&[u8]]| HmacSha512::mac(key, &parts.concat());

    // Instantiate: K = 00..00, V = 01..01, Update(seed_material).
    let (mut k, mut v) = ([0x00u8; 64], [0x01u8; 64]);
    k = hmac(&k, &[&v, &[0x00], &seed_material]);
    v = hmac(&k, &[&v]);
    k = hmac(&k, &[&v, &[0x01], &seed_material]);
    v = hmac(&k, &[&v]);
    // Generate 64 bytes: V = HMAC(K, V) is the output; then Update with nothing.
    v = hmac(&k, &[&v]);
    let first = v;
    k = hmac(&k, &[&v, &[0x00]]);
    v = hmac(&k, &[&v]);
    // The next request.
    v = hmac(&k, &[&v]);
    let second = v;

    let mut drbg = HmacDrbgSha512::new();
    drbg.instantiate(&entropy, &nonce, personalization).unwrap();
    assert_eq!(next(&mut drbg, 64), first);
    assert_eq!(next(&mut drbg, 64), second);
}

#[test]
fn debug_output_reveals_nothing() {
    let drbg = instantiated("debug");
    assert_eq!(format!("{drbg:?}"), "HmacDrbgSha512 { .. }");
    assert_eq!(format!("{:?}", DrbgError::ReseedRequired), "ReseedRequired");
}

/// Neither the generator nor a keyed MAC can be copied: this compiles only while neither is
/// `Clone` (so not `Copy` either). With a `Clone` implementation the two blanket
/// implementations below would both apply, and the call could not be resolved.
#[test]
fn keyed_state_cannot_be_cloned() {
    trait NotClone<Marker> {
        fn holds() {}
    }
    impl<T> NotClone<()> for T {}
    impl<T: Clone> NotClone<u8> for T {}
    <HmacDrbgSha512 as NotClone<_>>::holds();
    <HmacSha512 as NotClone<_>>::holds();
}

/// The state is a fixed 544 bytes on every 64-bit target: two SHA-512 states for the key, `V`,
/// the count and the interval. The Framekernel keeps one for each processor (ADR-0016).
#[test]
fn the_state_has_a_fixed_size() {
    assert_eq!(size_of::<HmacDrbgSha512>(), 544);
}

/// Cost on the build host. Indicative only: the target is x86-64. Run with
/// `cargo test -p origin-crypto --release -- --ignored --nocapture drbg_cost`.
#[test]
#[ignore = "benchmark; run explicitly in release mode"]
fn drbg_cost() {
    use std::hint::black_box;
    use std::time::Instant;
    let mut drbg = instantiated("cost");
    for (len, rounds) in [
        (8usize, 200_000u32),
        (64, 200_000),
        (128, 100_000),
        (4096, 5_000),
    ] {
        let mut out = vec![0u8; len];
        let start = Instant::now();
        for _ in 0..rounds {
            drbg.generate(black_box(&mut out), &[]).unwrap();
        }
        let nanos = start.elapsed().as_nanos() / u128::from(rounds);
        println!("generate {len} bytes: {nanos} ns");
    }
    let entropy = bytes_for("cost entropy", 8192);
    let rounds = 2_000u32;
    let start = Instant::now();
    for _ in 0..rounds {
        drbg.reseed(black_box(&entropy[..64]), &[]).unwrap();
    }
    println!(
        "reseed from 64 bytes: {} ns",
        start.elapsed().as_nanos() / u128::from(rounds)
    );
    let start = Instant::now();
    for _ in 0..rounds {
        drbg.uninstantiate();
        drbg.instantiate(black_box(&entropy), &entropy[..16], b"cost")
            .unwrap();
    }
    println!(
        "uninstantiate and instantiate from 8192 bytes: {} ns",
        start.elapsed().as_nanos() / u128::from(rounds)
    );
}
