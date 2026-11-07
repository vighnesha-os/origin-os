//! RELR and OCBOR-1 encodings, and `rustc -vV` parsing.

use std::collections::{BTreeMap, HashMap};

use onbf_format::ocbor::{self, Cursor, Item};

use crate::ocbor::Value;
use crate::relr::{self, RelrError};
use crate::{Toolchain, ToolchainError};

// ------------------------------------------------------------- RELR

#[track_caller]
fn round_trip(slots: &[u64]) -> Vec<u64> {
    let words = relr::encode(slots).unwrap();
    assert_eq!(
        relr::decode(&words).unwrap(),
        slots,
        "decode(encode(s)) == s"
    );
    let bytes = relr::words_to_bytes(&words);
    assert_eq!(bytes.len(), 8 * words.len());
    assert_eq!(relr::words_from_bytes(&bytes).unwrap(), words);
    words
}

#[test]
fn relr_exact_encodings() {
    const A: u64 = 0x10_0000;
    assert_eq!(round_trip(&[]), Vec::<u64>::new());
    assert_eq!(round_trip(&[A]), [A]);
    assert_eq!(round_trip(&[A, A + 8]), [A, 0b11]);
    // A slot 63 slots after the address is the last bit of the first bitmap.
    assert_eq!(round_trip(&[A, A + 8 * 63]), [A, 1 << 63 | 1]);
    // 64 slots after it falls outside the window: an isolated slot gets an address word.
    assert_eq!(round_trip(&[A, A + 8 * 64]), [A, A + 8 * 64]);
    // A run of 64 consecutive slots: address + one full bitmap.
    let run64: Vec<u64> = (0..64).map(|i| A + 8 * i).collect();
    assert_eq!(round_trip(&run64), [A, u64::MAX]);
    // 65 and 200 slots: runs longer than one bitmap continue into the next window.
    let run65: Vec<u64> = (0..65).map(|i| A + 8 * i).collect();
    assert_eq!(round_trip(&run65), [A, u64::MAX, 0b11]);
    let run200: Vec<u64> = (0..200).map(|i| A + 8 * i).collect();
    let w = round_trip(&run200);
    // 1 + 63 + 63 + 63 = 190 slots, then 10 more.
    assert_eq!(w, [A, u64::MAX, u64::MAX, u64::MAX, (1 << 11) - 1]);
    // A gap of one empty window inside a run starts a new address word rather than
    // emitting an empty bitmap.
    let gapped: Vec<u64> = (0..64)
        .map(|i| A + 8 * i)
        .chain((0..3).map(|i| A + 8 * (64 + 63 + 10 + i)))
        .collect();
    let first_after_gap = A + 8 * (64 + 63 + 10);
    assert_eq!(round_trip(&gapped), [A, u64::MAX, first_after_gap, 0b111]);
}

#[test]
fn relr_round_trips_many_shapes() {
    // A deterministic linear congruential generator: no dependency, same cases every run.
    let mut state = 0x0123_4567_89AB_CDEFu64;
    let mut next = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        state >> 33
    };
    for case in 0..2000 {
        let n = (next() % 300) as usize;
        let density = 1 + next() % 80; // mean gap in slots: dense runs to isolated slots
        let mut slot = 8 * (next() % 4096);
        let mut slots = Vec::with_capacity(n);
        for _ in 0..n {
            slots.push(slot);
            slot += 8 * (1 + next() % density);
        }
        let words = round_trip(&slots);
        // Re-encoding the decoded words gives the same words: one encoding per slot set.
        assert_eq!(
            relr::encode(&relr::decode(&words).unwrap()).unwrap(),
            words,
            "case {case}"
        );
    }
}

/// The fewest words any ascending RELR encoding of `slots` needs, by exhaustive search:
/// at each step either an address word for the next slot, or a bitmap (possibly empty)
/// for the current window, which must take every slot inside it.
fn optimal_words(slots: &[u64]) -> usize {
    fn go(
        slots: &[u64],
        i: usize,
        window: Option<u64>,
        memo: &mut HashMap<(usize, Option<u64>), usize>,
    ) -> usize {
        let Some(&next) = slots.get(i) else { return 0 };
        if let Some(&known) = memo.get(&(i, window)) {
            return known;
        }
        let mut best = 1 + go(slots, i + 1, Some(next + 8), memo);
        if let Some(base) = window
            && next >= base
            && next <= slots[slots.len() - 1]
        {
            let taken = slots[i..].iter().take_while(|&&s| s < base + 504).count();
            best = best.min(1 + go(slots, i + taken, Some(base + 504), memo));
        }
        memo.insert((i, window), best);
        best
    }
    go(slots, 0, None, &mut HashMap::new())
}

#[test]
fn relr_encoding_is_minimal_on_small_sets() {
    let mut state = 0xDEAD_BEEF_u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..3000 {
        let n = (next() % 14) as usize;
        let mut slots: Vec<u64> = (0..n).map(|_| 8 * (next() % 260)).collect();
        slots.sort_unstable();
        slots.dedup();
        let words = relr::encode(&slots).unwrap();
        assert_eq!(words.len(), optimal_words(&slots), "slots {slots:x?}");
    }
}

#[test]
fn relr_decode_rejects_malformed_words() {
    assert_eq!(
        relr::decode(&[0b101]),
        Err(RelrError::BitmapWithoutAddress { word: 0 })
    );
    assert_eq!(
        relr::decode(&[0x2000, 0x1000]),
        Err(RelrError::NotAscending { slot: 0x1000 })
    );
    // An address word naming a slot the previous bitmap already relocated.
    assert_eq!(
        relr::decode(&[0x1000, 0b111, 0x1008]),
        Err(RelrError::NotAscending { slot: 0x1008 })
    );
    // An address inside the previous window but above its last slot is ascending, so it
    // decodes (it is just not the canonical encoding).
    assert_eq!(
        relr::decode(&[0x1000, 0b11, 0x1010]).unwrap(),
        [0x1000, 0x1008, 0x1010]
    );
    assert_ne!(
        relr::encode(&[0x1000, 0x1008, 0x1010]).unwrap(),
        [0x1000, 0b11, 0x1010]
    );
    assert_eq!(
        relr::decode(&[0x1000, 0x1000]),
        Err(RelrError::NotAscending { slot: 0x1000 })
    );
    assert_eq!(
        relr::decode(&[u64::MAX - 7, 0b11]),
        Err(RelrError::Overflow { word: 0 })
    );
    // An empty bitmap is valid RELR (just not canonical) and relocates nothing.
    assert_eq!(
        relr::decode(&[0x1000, 1, 0b11]).unwrap(),
        [0x1000, 0x1000 + 8 + 504]
    );
    assert_eq!(relr::words_from_bytes(&[0; 7]), Err(RelrError::Length(7)));
}

#[test]
fn relr_encode_rejects_bad_slot_lists() {
    assert_eq!(
        relr::encode(&[0x1000, 0x1004]),
        Err(RelrError::Misaligned { slot: 0x1004 })
    );
    assert_eq!(
        relr::encode(&[0x1008, 0x1008]),
        Err(RelrError::NotAscending { slot: 0x1008 })
    );
    assert_eq!(
        relr::encode(&[0x1008, 0x1000]),
        Err(RelrError::NotAscending { slot: 0x1000 })
    );
}

// ------------------------------------------------------------- OCBOR-1

#[track_caller]
fn read_back(value: &Value, bytes: &[u8]) {
    ocbor::validate(bytes).unwrap();
    let mut c = Cursor::new(bytes);
    let item = c.next_item().unwrap();
    match (value, item) {
        (Value::Unsigned(v), Item::Unsigned(w)) => assert_eq!(*v, w),
        (Value::Bytes(v), Item::Bytes(w)) => assert_eq!(v.as_slice(), w),
        (Value::Text(v), Item::Text(w)) => assert_eq!(v, w),
        (Value::Bool(v), Item::Bool(w)) => assert_eq!(*v, w),
        (Value::Array(v), Item::Array(n)) => assert_eq!(v.len() as u64, n),
        (Value::Map(v), Item::Map(n)) => assert_eq!(v.len() as u64, n),
        (v, i) => panic!("{v:?} read back as {i:?}"),
    }
}

#[test]
fn ocbor_integers_use_the_shortest_head() {
    let cases: [(u64, &[u8]); 10] = [
        (0, &[0x00]),
        (23, &[0x17]),
        (24, &[0x18, 24]),
        (255, &[0x18, 0xFF]),
        (256, &[0x19, 0x01, 0x00]),
        (65_535, &[0x19, 0xFF, 0xFF]),
        (65_536, &[0x1A, 0x00, 0x01, 0x00, 0x00]),
        (u32::MAX as u64, &[0x1A, 0xFF, 0xFF, 0xFF, 0xFF]),
        (1 << 32, &[0x1B, 0, 0, 0, 1, 0, 0, 0, 0]),
        (
            u64::MAX,
            &[0x1B, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF],
        ),
    ];
    for (v, expected) in cases {
        let value = Value::Unsigned(v);
        let bytes = value.encode();
        assert_eq!(bytes, expected, "{v}");
        read_back(&value, &bytes);
    }
}

#[test]
fn ocbor_strings_and_simple_values() {
    for len in [0usize, 1, 23, 24, 255, 256, 70_000] {
        let value = Value::Bytes(vec![0xAB; len]);
        let bytes = value.encode();
        read_back(&value, &bytes);
        let text = Value::Text("x".repeat(len.min(65_536)));
        read_back(&text, &text.encode());
    }
    assert_eq!(Value::Text("1.99.1".into()).encode(), b"\x661.99.1");
    assert_eq!(Value::Bool(false).encode(), [0xF4]);
    assert_eq!(Value::Bool(true).encode(), [0xF5]);
    read_back(&Value::Bool(true), &[0xF5]);
}

#[test]
fn ocbor_maps_are_written_in_ascending_key_order() {
    let mut m = BTreeMap::new();
    for key in [1000u64, 24, 0, 23, 5] {
        m.insert(key, Value::Unsigned(key));
    }
    let value = Value::Map(m);
    let bytes = value.encode();
    assert_eq!(
        bytes,
        [
            0xA5, 0x00, 0x00, 0x05, 0x05, 0x17, 0x17, 0x18, 24, 0x18, 24, 0x19, 0x03, 0xE8, 0x19,
            0x03, 0xE8
        ]
    );
    read_back(&value, &bytes);
    // Nested containers.
    let nested = Value::Array(vec![
        Value::Map(BTreeMap::from([(1, Value::Array(vec![Value::Bool(true)]))])),
        Value::Array(Vec::new()),
    ]);
    let bytes = nested.encode();
    assert_eq!(bytes, [0x82, 0xA1, 0x01, 0x81, 0xF5, 0x80]);
    read_back(&nested, &bytes);
}

// ------------------------------------------------------------- toolchain

const RUSTC_VV: &str = "rustc 1.99.1 (58cf8f9c3 2026-10-13)
binary: rustc
commit-hash: 58cf8f9c35b355199727627d07ae34a4e2846d67
commit-date: 2026-10-13
host: aarch64-apple-darwin
release: 1.99.1
LLVM version: 23.1.1
";

#[test]
fn rustc_verbose_version_is_parsed() {
    let t = Toolchain::from_rustc_verbose_version(RUSTC_VV).unwrap();
    assert_eq!(t.rustc, "1.99.1");
    assert_eq!(t.rustc_commit, "58cf8f9c35b355199727627d07ae34a4e2846d67");
    assert_eq!(t.llvm, "23.1.1");
}

#[test]
fn rustc_verbose_version_problems_are_typed() {
    let without = RUSTC_VV.replace("LLVM version: 23.1.1\n", "");
    assert_eq!(
        Toolchain::from_rustc_verbose_version(&without),
        Err(ToolchainError::Missing("LLVM version:"))
    );
    // A toolchain built without git information.
    let unknown = RUSTC_VV.replace(
        "commit-hash: 58cf8f9c35b355199727627d07ae34a4e2846d67",
        "commit-hash: unknown",
    );
    assert_eq!(
        Toolchain::from_rustc_verbose_version(&unknown),
        Err(ToolchainError::Invalid {
            field: "rustc_commit",
            value: "unknown".into()
        })
    );
}
