//! OCBOR-1 and KRN manifest tests (ONBF §5; `docs/spec/ONBF-manifest-keys.md`).
//!
//! - RFC 8949 Appendix A examples: the ones OCBOR-1 accepts decode to the right items, and
//!   the ones its profile excludes are rejected for the right reason.
//! - The §13 item 4 corpus for manifests: non-canonical and duplicate-key CBOR, limits and
//!   UTF-8.
//! - Truncation and bit-flip sweeps.
//! - The KRN schema.

use crate::ocbor::{self, Cursor, Item, MAX_DOCUMENT, OcborError, OcborErrorKind as K};
use crate::*;

// ------------------------------------------------------------------ a test-only encoder

fn head(major: u8, arg: u64) -> Vec<u8> {
    let m = major << 5;
    match arg {
        0..=23 => vec![m | arg as u8],
        24..=0xFF => vec![m | 24, arg as u8],
        0x100..=0xFFFF => [&[m | 25][..], &(arg as u16).to_be_bytes()].concat(),
        0x1_0000..=0xFFFF_FFFF => [&[m | 26][..], &(arg as u32).to_be_bytes()].concat(),
        _ => [&[m | 27][..], &arg.to_be_bytes()].concat(),
    }
}
pub(crate) fn uint(v: u64) -> Vec<u8> {
    head(0, v)
}
pub(crate) fn bytes(b: &[u8]) -> Vec<u8> {
    [head(2, b.len() as u64), b.to_vec()].concat()
}
pub(crate) fn text(s: &str) -> Vec<u8> {
    [head(3, s.len() as u64), s.as_bytes().to_vec()].concat()
}
pub(crate) fn array(items: &[Vec<u8>]) -> Vec<u8> {
    [head(4, items.len() as u64), items.concat()].concat()
}
/// A map, with entries encoded in the order given (callers sort them).
pub(crate) fn map(entries: &[(u64, Vec<u8>)]) -> Vec<u8> {
    let mut out = head(5, entries.len() as u64);
    for (k, v) in entries {
        out.extend(uint(*k));
        out.extend(v);
    }
    out
}
fn unhex(s: &str) -> Vec<u8> {
    let s: String = s.split_whitespace().collect();
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}
fn kind_of(doc: &[u8]) -> Option<K> {
    ocbor::validate(doc).err().map(|e| e.kind)
}
fn first_item(doc: &[u8]) -> Item<'_> {
    Cursor::new(doc).next_item().unwrap()
}

// ------------------------------------------------------------------ RFC 8949 Appendix A

#[test]
fn rfc8949_examples_accepted() {
    let cases: [(&str, Item<'_>); 22] = [
        ("00", Item::Unsigned(0)),
        ("01", Item::Unsigned(1)),
        ("0a", Item::Unsigned(10)),
        ("17", Item::Unsigned(23)),
        ("1818", Item::Unsigned(24)),
        ("1819", Item::Unsigned(25)),
        ("1864", Item::Unsigned(100)),
        ("1903e8", Item::Unsigned(1000)),
        ("1a000f4240", Item::Unsigned(1_000_000)),
        ("1b000000e8d4a51000", Item::Unsigned(1_000_000_000_000)),
        ("1bffffffffffffffff", Item::Unsigned(u64::MAX)),
        ("3bffffffffffffffff", Item::Negative(u64::MAX)), // -18446744073709551616
        ("20", Item::Negative(0)),                        // -1
        ("29", Item::Negative(9)),                        // -10
        ("3863", Item::Negative(99)),                     // -100
        ("3903e7", Item::Negative(999)),                  // -1000
        ("f4", Item::Bool(false)),
        ("f5", Item::Bool(true)),
        ("f6", Item::Null),
        ("40", Item::Bytes(&[])),
        ("4401020304", Item::Bytes(&[1, 2, 3, 4])),
        ("60", Item::Text("")),
    ];
    for (hex, want) in cases {
        let doc = unhex(hex);
        assert_eq!(ocbor::validate(&doc), Ok(()), "{hex}");
        assert_eq!(first_item(&doc), want, "{hex}");
    }
    for (hex, want) in [
        ("6161", "a"),
        ("6449455446", "IETF"),
        ("62225c", "\"\\"),
        ("62c3bc", "\u{fc}"),
        ("63e6b0b4", "\u{6c34}"),
        ("64f0908591", "\u{10151}"),
    ] {
        let doc = unhex(hex);
        assert_eq!(ocbor::validate(&doc), Ok(()), "{hex}");
        assert_eq!(first_item(&doc), Item::Text(want), "{hex}");
    }
    for hex in [
        "80",
        "83010203",
        "8301820203820405",
        "98190102030405060708090a0b0c0d0e0f101112131415161718181819",
        "a0",
        "a201020304",
    ] {
        assert_eq!(ocbor::validate(&unhex(hex)), Ok(()), "{hex}");
    }
}

#[test]
fn rfc8949_examples_outside_the_profile_are_rejected() {
    let cases = [
        ("f90000", K::Float),                                     // 0.0, half precision
        ("fb3ff199999999999a", K::Float),                         // 1.1
        ("fa7f800000", K::Float),                                 // Infinity
        ("f7", K::Simple),                                        // undefined
        ("f0", K::Simple),                                        // simple(16)
        ("f8ff", K::Simple),                                      // simple(255)
        ("c074323031332d30332d32315432303a30343a30305a", K::Tag), // tag 0
        ("c249010000000000000000", K::Tag),                       // bignum 2^64
        ("5f42010243030405ff", K::Indefinite),                    // (_ h'0102', h'030405')
        ("7f657374726561646d696e67ff", K::Indefinite),
        ("9fff", K::Indefinite),                   // [_ ]
        ("bf61610161ff", K::Indefinite),           // {_ "a": 1}
        ("a26161016162820203", K::KeyNotUnsigned), // {"a": 1, "b": [2, 3]}
    ];
    for (hex, want) in cases {
        assert_eq!(kind_of(&unhex(hex)), Some(want), "{hex}");
    }
}

// ------------------------------------------------------------------ corpus (§13 item 4)

#[test]
fn non_canonical_encodings_are_rejected() {
    for hex in [
        "1817",               // 23 in one byte
        "190017",             // 23 in two bytes
        "1900ff",             // 255 in two bytes
        "1a0000ffff",         // 65535 in four bytes
        "1b00000000ffffffff", // 2^32-1 in eight bytes
        "3817",               // -24 in one byte
        "580100",             // a 1-byte string with a 1-byte length
        "780161",             // "a" with a 1-byte length
        "980100",             // a 1-element array with a 1-byte length
        "b80101 00",          // a 1-entry map with a 1-byte length
    ] {
        assert_eq!(kind_of(&unhex(hex)), Some(K::NonShortest), "{hex}");
    }
}

#[test]
fn malformed_items_are_rejected() {
    let cases = [
        ("", K::Empty),
        ("1c", K::Reserved),
        ("1d", K::Reserved),
        ("1e", K::Reserved),
        ("fc", K::Reserved),
        ("ff", K::Indefinite), // a stray break
        ("18", K::Truncated),
        ("1901", K::Truncated),
        ("6261", K::Truncated),
        ("8201", K::Truncated),
        ("a101", K::Truncated),
        ("0000", K::TrailingData),
        ("62c080", K::InvalidUtf8),   // overlong NUL
        ("63eda080", K::InvalidUtf8), // a UTF-16 surrogate
        ("61ff", K::InvalidUtf8),
    ];
    for (hex, want) in cases {
        assert_eq!(kind_of(&unhex(hex)), Some(want), "{hex:?}");
    }
    assert_eq!(
        ocbor::validate(&unhex("0000")),
        Err(OcborError {
            offset: 1,
            kind: K::TrailingData
        })
    );
}

#[test]
fn map_keys_must_be_unsigned_and_strictly_ascending() {
    let cases = [
        ("a2 0200 0100", K::KeysNotAscending),       // {2: 0, 1: 0}
        ("a2 0100 0100", K::KeysNotAscending),       // duplicate key
        ("a1 2000", K::KeyNotUnsigned),              // negative key
        ("a1 4100 00", K::KeyNotUnsigned),           // byte-string key
        ("a1 f400", K::KeyNotUnsigned),              // false as a key
        ("a1 01 a2 0500 0300", K::KeysNotAscending), // inside a nested map
    ];
    for (hex, want) in cases {
        assert_eq!(kind_of(&unhex(hex)), Some(want), "{hex}");
    }
    // Ascending keys across the 1-byte/2-byte boundary are fine.
    assert_eq!(
        ocbor::validate(&map(&[(23, uint(0)), (24, uint(0)), (1000, uint(0))])),
        Ok(())
    );
}

#[test]
fn limits_are_enforced() {
    // Depth: 16 nested containers are allowed, 17 are not, empty or not.
    let nest = |n: usize| [vec![0x81u8; n - 1], vec![0x80]].concat();
    assert_eq!(ocbor::validate(&nest(16)), Ok(()));
    assert_eq!(kind_of(&nest(17)), Some(K::Depth));
    let maps = |n: usize| [[0xA1u8, 0x00].repeat(n - 1), vec![0xA0]].concat();
    assert_eq!(ocbor::validate(&maps(16)), Ok(()));
    assert_eq!(kind_of(&maps(17)), Some(K::Depth));

    // Items per container: 65,536.
    let full = [head(4, 65_536), vec![0u8; 65_536]].concat();
    assert_eq!(ocbor::validate(&full), Ok(()));
    assert_eq!(kind_of(&head(4, 65_537)), Some(K::TooManyItems));
    assert_eq!(kind_of(&head(5, 65_537)), Some(K::TooManyItems));

    // Text: 64 KiB.
    let t = "x".repeat(64 * 1024);
    assert_eq!(ocbor::validate(&text(&t)), Ok(()));
    assert_eq!(kind_of(&head(3, 64 * 1024 + 1)), Some(K::TextTooLong));

    // Document: 1 MiB.
    let doc = vec![0u8; MAX_DOCUMENT + 1];
    assert_eq!(kind_of(&doc), Some(K::TooLarge));
}

#[test]
fn cursor_skips_nested_contents() {
    let doc = array(&[
        map(&[(1, array(&[uint(1), text("x")])), (2, bytes(b"ab"))]),
        uint(7),
    ]);
    assert_eq!(ocbor::validate(&doc), Ok(()));
    let mut c = Cursor::new(&doc);
    assert_eq!(c.next_item(), Ok(Item::Array(2)));
    let m = c.next_item().unwrap();
    c.skip_contents(m).unwrap();
    assert_eq!(c.next_item(), Ok(Item::Unsigned(7)));
    assert_eq!(c.position(), doc.len());
}

// ------------------------------------------------------------------ KRN manifest

/// The KRN manifest entries used by the tests, in key order. Entry 0x100 lies in the
/// test kernel image's RX segment.
pub(crate) fn krn_entries() -> Vec<(u64, Vec<u8>)> {
    vec![
        (0, uint(1)),
        (1, bytes(&[0x11; 32])),
        (
            2,
            map(&[
                (0, text("1.99.1")),
                (1, text("58cf8f9c35b355199727627d07ae34a4e2846d67")),
                (2, text("23.1.1")),
                (3, bytes(&[0x22; 32])),
            ]),
        ),
        (3, uint(0)),
        (4, map(&[(0, cbor_true()), (1, cbor_false())])),
        (5, uint(0x100)),
        (24, array(&[uint(1), uint(0)])),
    ]
}
fn cbor_true() -> Vec<u8> {
    vec![0xF5]
}
fn cbor_false() -> Vec<u8> {
    vec![0xF4]
}

pub(crate) fn krn_manifest_bytes() -> Vec<u8> {
    map(&krn_entries())
}

/// Builds a manifest from the standard entries after `edit`, sorting by key.
fn krn_with(edit: impl FnOnce(&mut Vec<(u64, Vec<u8>)>)) -> Vec<u8> {
    let mut e = krn_entries();
    edit(&mut e);
    e.sort_by_key(|(k, _)| *k);
    map(&e)
}
fn set(e: &mut Vec<(u64, Vec<u8>)>, key: u64, value: Vec<u8>) {
    match e.iter_mut().find(|(k, _)| *k == key) {
        Some(slot) => slot.1 = value,
        None => e.push((key, value)),
    }
}
fn remove(e: &mut Vec<(u64, Vec<u8>)>, key: u64) {
    e.retain(|(k, _)| *k != key);
}
fn decode_err(doc: &[u8]) -> ManifestError {
    KrnManifest::decode(doc).expect_err("expected a manifest error")
}
fn top(key: u64) -> FieldPath {
    FieldPath { key, sub: None }
}
fn sub(key: u64, s: u64) -> FieldPath {
    FieldPath { key, sub: Some(s) }
}

#[test]
fn krn_manifest_decodes() {
    let doc = krn_manifest_bytes();
    let m = KrnManifest::decode(&doc).unwrap();
    assert_eq!(m.build_id, [0x11; 32]);
    assert_eq!(m.toolchain.rustc, "1.99.1");
    assert_eq!(
        m.toolchain.rustc_commit,
        "58cf8f9c35b355199727627d07ae34a4e2846d67"
    );
    assert_eq!(m.toolchain.llvm, "23.1.1");
    assert_eq!(m.toolchain.flags_digest, [0x22; 32]);
    assert_eq!(m.cpu_features, CpuFeatures(0));
    assert_eq!(
        m.cet,
        Cet {
            shstk: true,
            ibt: false
        }
    );
    assert_eq!(m.entry, 0x100);
    assert_eq!(m.boot_info_abi, (1, 0));
}

#[test]
fn krn_required_keys_and_unknown_keys() {
    for key in [0, 1, 2, 3, 4, 5, 24] {
        let doc = krn_with(|e| remove(e, key));
        assert_eq!(
            decode_err(&doc),
            ManifestError::MissingKey(top(key)),
            "key {key}"
        );
    }
    for key in [6, 23, 25, 999] {
        let doc = krn_with(|e| set(e, key, uint(0)));
        assert_eq!(
            decode_err(&doc),
            ManifestError::UnknownKey(top(key)),
            "key {key}"
        );
    }
    // Extensions (>= 1000) are skipped, however complex.
    let ext = map(&[(1, array(&[uint(1), map(&[(2, text("x"))])]))]);
    let doc = krn_with(|e| set(e, 1000, ext.clone()));
    assert!(KrnManifest::decode(&doc).is_ok());
    let doc = krn_with(|e| set(e, u64::MAX, ext));
    assert!(KrnManifest::decode(&doc).is_ok());
    // ...including inside toolchain and cet.
    let tc = map(&[
        (0, text("1.99.1")),
        (1, text("58cf8f9c35b355199727627d07ae34a4e2846d67")),
        (2, text("23.1.1")),
        (3, bytes(&[0; 32])),
        (5000, text("note")),
    ]);
    assert!(KrnManifest::decode(&krn_with(|e| set(e, 2, tc))).is_ok());
    let cet = map(&[(0, cbor_true()), (1, cbor_false()), (2, uint(0))]);
    assert_eq!(
        decode_err(&krn_with(|e| set(e, 4, cet))),
        ManifestError::UnknownKey(sub(4, 2))
    );
}

#[test]
fn krn_field_values_are_checked() {
    let toolchain = |commit: &str, rustc: &str| {
        map(&[
            (0, text(rustc)),
            (1, text(commit)),
            (2, text("23.1.1")),
            (3, bytes(&[0; 32])),
        ])
    };
    let good_commit = "58cf8f9c35b355199727627d07ae34a4e2846d67";
    let cases: Vec<(&str, Vec<u8>, ManifestError)> = vec![
        (
            "schema 2",
            krn_with(|e| set(e, 0, uint(2))),
            ManifestError::UnsupportedSchema(2),
        ),
        (
            "schema as text",
            krn_with(|e| set(e, 0, text("1"))),
            ManifestError::WrongType(top(0)),
        ),
        (
            "build_id of 31 bytes",
            krn_with(|e| set(e, 1, bytes(&[0; 31]))),
            ManifestError::InvalidValue(top(1)),
        ),
        (
            "build_id as text",
            krn_with(|e| set(e, 1, text("x"))),
            ManifestError::WrongType(top(1)),
        ),
        (
            "uppercase commit",
            krn_with(|e| set(e, 2, toolchain(&good_commit.to_uppercase(), "1.99.1"))),
            ManifestError::InvalidValue(sub(2, 1)),
        ),
        (
            "short commit",
            krn_with(|e| set(e, 2, toolchain(&good_commit[..39], "1.99.1"))),
            ManifestError::InvalidValue(sub(2, 1)),
        ),
        (
            "non-ASCII rustc version",
            krn_with(|e| set(e, 2, toolchain(good_commit, "1.99.1\u{e9}"))),
            ManifestError::InvalidValue(sub(2, 0)),
        ),
        (
            "empty rustc version",
            krn_with(|e| set(e, 2, toolchain(good_commit, ""))),
            ManifestError::InvalidValue(sub(2, 0)),
        ),
        (
            "toolchain not a map",
            krn_with(|e| set(e, 2, uint(0))),
            ManifestError::WrongType(top(2)),
        ),
        (
            "toolchain missing flags_digest",
            krn_with(|e| {
                set(
                    e,
                    2,
                    map(&[(0, text("1")), (1, text(good_commit)), (2, text("2"))]),
                )
            }),
            ManifestError::MissingKey(sub(2, 3)),
        ),
        (
            "reserved feature bit",
            krn_with(|e| set(e, 3, uint(1 << 6))),
            ManifestError::ReservedFeatureBits(1 << 6),
        ),
        (
            "feature bitset as negative",
            krn_with(|e| set(e, 3, vec![0x20])),
            ManifestError::WrongType(top(3)),
        ),
        (
            "IBT on",
            krn_with(|e| set(e, 4, map(&[(0, cbor_true()), (1, cbor_true())]))),
            ManifestError::IbtNotSupported,
        ),
        (
            "shstk missing",
            krn_with(|e| set(e, 4, map(&[(1, cbor_false())]))),
            ManifestError::MissingKey(sub(4, 0)),
        ),
        (
            "shstk not a bool",
            krn_with(|e| set(e, 4, map(&[(0, uint(1)), (1, cbor_false())]))),
            ManifestError::WrongType(sub(4, 0)),
        ),
        (
            "entry as bytes",
            krn_with(|e| set(e, 5, bytes(&[1]))),
            ManifestError::WrongType(top(5)),
        ),
        (
            "boot_info_abi of one",
            krn_with(|e| set(e, 24, array(&[uint(1)]))),
            ManifestError::WrongType(top(24)),
        ),
        (
            "boot_info_abi out of u16 range",
            krn_with(|e| set(e, 24, array(&[uint(1), uint(70_000)]))),
            ManifestError::InvalidValue(top(24)),
        ),
        (
            "boot_info_abi negative",
            krn_with(|e| set(e, 24, array(&[uint(1), vec![0x20]]))),
            ManifestError::WrongType(top(24)),
        ),
        ("not a map", array(&[uint(0)]), ManifestError::NotAMap),
    ];
    for (name, doc, want) in cases {
        assert_eq!(decode_err(&doc), want, "{name}");
    }
    // Every defined feature bit is accepted.
    let doc = krn_with(|e| set(e, 3, uint(CpuFeatures::KNOWN)));
    assert_eq!(
        KrnManifest::decode(&doc).unwrap().cpu_features,
        CpuFeatures(0x3F)
    );
}

#[test]
fn krn_manifest_must_be_canonical_ocbor() {
    // A duplicate key is caught by the validator before the schema sees it.
    let mut e = krn_entries();
    e.insert(1, (0, uint(1)));
    let doc = map(&e);
    assert!(matches!(
        decode_err(&doc),
        ManifestError::Ocbor(OcborError {
            kind: K::KeysNotAscending,
            ..
        })
    ));
    // A non-shortest entry point.
    let doc = krn_with(|e| set(e, 5, vec![0x19, 0x00, 0x10]));
    assert!(matches!(
        decode_err(&doc),
        ManifestError::Ocbor(OcborError {
            kind: K::NonShortest,
            ..
        })
    ));
}

#[test]
fn every_truncation_and_bit_flip_of_a_manifest_is_handled() {
    let doc = krn_manifest_bytes();
    for cut in 0..doc.len() {
        assert!(KrnManifest::decode(&doc[..cut]).is_err(), "cut {cut}");
    }
    // Many flips land in free values (digest bytes, version characters, the entry point),
    // so many are accepted. The property that matters is ONBF-R6: an accepted manifest
    // must be the one canonical encoding of what it decodes to, so re-encoding the decoded
    // value must give back exactly the flipped bytes. No flip may panic.
    let (mut accepted, mut rejected) = (0, 0);
    for at in 0..doc.len() {
        for bit in 0..8 {
            let mut t = doc.clone();
            t[at] ^= 1 << bit;
            match KrnManifest::decode(&t) {
                Ok(m) => {
                    assert_eq!(
                        encode_krn(&m),
                        t,
                        "byte {at} bit {bit}: accepted but not canonical"
                    );
                    accepted += 1;
                }
                Err(_) => rejected += 1,
            }
        }
    }
    assert!(
        accepted > 0 && rejected > 0,
        "{accepted} accepted, {rejected} rejected"
    );
}

/// Canonical OCBOR-1 encoding of a decoded KRN manifest (test-only; `onbf-pack` will own
/// the real encoder).
pub(crate) fn encode_krn(m: &KrnManifest<'_>) -> Vec<u8> {
    let b = |v: bool| if v { cbor_true() } else { cbor_false() };
    map(&[
        (0, uint(1)),
        (1, bytes(&m.build_id)),
        (
            2,
            map(&[
                (0, text(m.toolchain.rustc)),
                (1, text(m.toolchain.rustc_commit)),
                (2, text(m.toolchain.llvm)),
                (3, bytes(&m.toolchain.flags_digest)),
            ]),
        ),
        (3, uint(m.cpu_features.0)),
        (4, map(&[(0, b(m.cet.shstk)), (1, b(m.cet.ibt))])),
        (5, uint(m.entry)),
        (
            24,
            array(&[
                uint(u64::from(m.boot_info_abi.0)),
                uint(u64::from(m.boot_info_abi.1)),
            ]),
        ),
    ])
}

#[test]
fn decode_then_encode_is_the_identity() {
    let doc = krn_manifest_bytes();
    assert_eq!(encode_krn(&KrnManifest::decode(&doc).unwrap()), doc);
}

/// The standard KRN manifest with a different entry point, for image-level tests.
pub(crate) fn krn_manifest_with_entry(entry: u64) -> Vec<u8> {
    krn_with(|e| set(e, 5, uint(entry)))
}
