//! Seeded multi-fault soak of OCBOR-1 and the KRN manifest decoder (ONBF §5, §13 item 3).
//!
//! The judge is an oracle written from §5 and errata R5: a recursive decoder into a value
//! tree, independent of the iterative validator. It must agree with [`ocbor::validate`] on
//! every input, including which rule fails and where, and an accepted document must
//! re-encode to exactly its own bytes (RFC 8949 deterministic encoding, ONBF-R6). The KRN
//! schema is judged the same way: from the tree, by key lookup, not by decoding in order.
//!
//! A failure is reproduced by its seed and case number; `ORIGIN_MUTATION_SCALE` scales the
//! case counts.

use crate::ocbor::{self, Cursor, Item, MAX_DOCUMENT, OcborErrorKind as K};
use crate::testing::{Reached, Rng, cases, mutate};
use crate::tests_ocbor::{encode_krn, krn_entries};
use crate::*;

// ------------------------------------------------------------------ the value tree

/// An OCBOR-1 value. Public to the crate for the signature-block soak's oracle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum V {
    U(u64),
    N(u64),
    B(Vec<u8>),
    T(String),
    A(Vec<V>),
    M(Vec<(u64, V)>),
    Bool(bool),
    Null,
}

fn head(out: &mut Vec<u8>, major: u8, arg: u64) {
    let m = major << 5;
    match arg {
        0..=23 => out.push(m | arg as u8),
        24..=0xFF => out.extend([m | 24, arg as u8]),
        0x100..=0xFFFF => {
            out.push(m | 25);
            out.extend((arg as u16).to_be_bytes());
        }
        0x1_0000..=0xFFFF_FFFF => {
            out.push(m | 26);
            out.extend((arg as u32).to_be_bytes());
        }
        _ => {
            out.push(m | 27);
            out.extend(arg.to_be_bytes());
        }
    }
}

/// The one canonical encoding of `v`, appended to `out`.
pub(crate) fn encode(v: &V, out: &mut Vec<u8>) {
    match v {
        V::U(n) => head(out, 0, *n),
        V::N(n) => head(out, 1, *n),
        V::B(b) => {
            head(out, 2, b.len() as u64);
            out.extend(b);
        }
        V::T(t) => {
            head(out, 3, t.len() as u64);
            out.extend(t.as_bytes());
        }
        V::A(items) => {
            head(out, 4, items.len() as u64);
            items.iter().for_each(|i| encode(i, out));
        }
        V::M(entries) => {
            head(out, 5, entries.len() as u64);
            for (k, value) in entries {
                head(out, 0, *k);
                encode(value, out);
            }
        }
        V::Bool(b) => out.push(if *b { 0xF5 } else { 0xF4 }),
        V::Null => out.push(0xF6),
    }
}

pub(crate) fn encoded(v: &V) -> Vec<u8> {
    let mut out = Vec::new();
    encode(v, &mut out);
    out
}

// ------------------------------------------------------------------ the oracle

/// One item's head, or (for a string) the whole item.
enum Shallow {
    Done(V),
    Array(u64),
    Map(u64),
}

/// A rule broken, and the offset of the item that broke it.
pub(crate) type Fail = (usize, K);

struct Oracle<'a> {
    doc: &'a [u8],
    pos: usize,
}

impl Oracle<'_> {
    /// Decodes one head (and a string's body). Rules are tried in the order the parser
    /// reports them, so the two can be compared exactly.
    fn shallow(&mut self) -> Result<Shallow, Fail> {
        let at = self.pos;
        let fail = |kind| Err((at, kind));
        let Some(&initial) = self.doc.get(at) else {
            return fail(K::Truncated);
        };
        self.pos = at + 1;
        let (major, info) = (initial >> 5, initial & 0x1F);
        if major == 7 {
            return match info {
                20 => Ok(Shallow::Done(V::Bool(false))),
                21 => Ok(Shallow::Done(V::Bool(true))),
                22 => Ok(Shallow::Done(V::Null)),
                25..=27 => fail(K::Float),
                28..=30 => fail(K::Reserved),
                31 => fail(K::Indefinite),
                _ => fail(K::Simple),
            };
        }
        let (arg, width) = match info {
            0..=23 => (u64::from(info), 0),
            24..=27 => {
                let width = 1usize << (info - 24);
                let Some(raw) = self.doc.get(self.pos..self.pos + width) else {
                    return fail(K::Truncated);
                };
                self.pos += width;
                (
                    raw.iter().fold(0u64, |a, &b| (a << 8) | u64::from(b)),
                    width,
                )
            }
            28..=30 => return fail(K::Reserved),
            _ => return fail(K::Indefinite),
        };
        let floor = match width {
            0 => 0,
            1 => 24,
            2 => 0x100,
            4 => 0x1_0000,
            _ => 0x1_0000_0000,
        };
        if arg < floor {
            return fail(K::NonShortest);
        }
        match major {
            0 => Ok(Shallow::Done(V::U(arg))),
            1 => Ok(Shallow::Done(V::N(arg))),
            2 | 3 => {
                if major == 3 && arg > 64 * 1024 {
                    return fail(K::TextTooLong);
                }
                let len = usize::try_from(arg).unwrap_or(usize::MAX);
                let Some(body) = self
                    .pos
                    .checked_add(len)
                    .and_then(|end| self.doc.get(self.pos..end))
                else {
                    return fail(K::Truncated);
                };
                self.pos += len;
                if major == 2 {
                    Ok(Shallow::Done(V::B(body.to_vec())))
                } else {
                    match std::str::from_utf8(body) {
                        Ok(t) => Ok(Shallow::Done(V::T(t.to_owned()))),
                        Err(_) => fail(K::InvalidUtf8),
                    }
                }
            }
            4 | 5 if arg > 65_536 => fail(K::TooManyItems),
            4 => Ok(Shallow::Array(arg)),
            5 => Ok(Shallow::Map(arg)),
            _ => fail(K::Tag),
        }
    }

    /// Decodes one complete value nested inside `depth` open containers.
    fn value(&mut self, depth: usize) -> Result<V, Fail> {
        let at = self.pos;
        match self.shallow()? {
            Shallow::Done(v) => Ok(v),
            Shallow::Array(n) => {
                if depth >= 16 {
                    return Err((at, K::Depth));
                }
                let mut items = Vec::new();
                for _ in 0..n {
                    items.push(self.value(depth + 1)?);
                }
                Ok(V::A(items))
            }
            Shallow::Map(n) => {
                if depth >= 16 {
                    return Err((at, K::Depth));
                }
                let mut entries: Vec<(u64, V)> = Vec::new();
                for _ in 0..n {
                    let key_at = self.pos;
                    let Shallow::Done(V::U(key)) = self.shallow()? else {
                        return Err((key_at, K::KeyNotUnsigned));
                    };
                    if entries.last().is_some_and(|&(prev, _)| key <= prev) {
                        return Err((key_at, K::KeysNotAscending));
                    }
                    let value = self.value(depth + 1)?;
                    entries.push((key, value));
                }
                Ok(V::M(entries))
            }
        }
    }
}

/// What §5 says about `doc`: its value tree, or the first rule it breaks.
pub(crate) fn oracle(doc: &[u8]) -> Result<V, Fail> {
    if doc.is_empty() {
        return Err((0, K::Empty));
    }
    if doc.len() > MAX_DOCUMENT {
        return Err((0, K::TooLarge));
    }
    let mut o = Oracle { doc, pos: 0 };
    let v = o.value(0)?;
    if o.pos != doc.len() {
        return Err((o.pos, K::TrailingData));
    }
    Ok(v)
}

fn kind_name(kind: K) -> &'static str {
    match kind {
        K::Empty => "Empty",
        K::TooLarge => "TooLarge",
        K::Truncated => "Truncated",
        K::NonShortest => "NonShortest",
        K::Indefinite => "Indefinite",
        K::Reserved => "Reserved",
        K::Tag => "Tag",
        K::Float => "Float",
        K::Simple => "Simple",
        K::Depth => "Depth",
        K::TooManyItems => "TooManyItems",
        K::TextTooLong => "TextTooLong",
        K::InvalidUtf8 => "InvalidUtf8",
        K::KeyNotUnsigned => "KeyNotUnsigned",
        K::KeysNotAscending => "KeysNotAscending",
        K::TrailingData => "TrailingData",
    }
}

const OCBOR_REFUSALS: [&str; 17] = [
    "Empty",
    "TooLarge",
    "Truncated",
    "NonShortest",
    "Indefinite",
    "Reserved",
    "Tag",
    "Float",
    "Simple",
    "Depth",
    "TooManyItems",
    "TextTooLong",
    "InvalidUtf8",
    "KeyNotUnsigned",
    "KeysNotAscending",
    "TrailingData",
    "accepted",
];

// ------------------------------------------------------------------ generators and faults

fn random_bytes(rng: &mut Rng, max: usize) -> Vec<u8> {
    let n = rng.below(max + 1);
    (0..n).map(|_| rng.byte()).collect()
}

fn random_text(rng: &mut Rng, max: usize) -> String {
    (0..rng.below(max + 1))
        .map(|_| {
            rng.pick(&[
                'a',
                'Z',
                '0',
                '.',
                ' ',
                '-',
                '\u{7f}',
                '\u{e9}',
                '\u{6c34}',
                '\u{10151}',
            ])
        })
        .collect()
}

/// A random well-formed value, at most `depth` containers deep.
pub(crate) fn random_value(rng: &mut Rng, depth: usize) -> V {
    let choice = if depth == 0 {
        rng.below(8)
    } else {
        rng.below(11)
    };
    match choice {
        0 | 1 => V::U(rng.edge_u64()),
        2 => V::N(rng.edge_u64()),
        3 => V::B(random_bytes(rng, 40)),
        4 => V::T(random_text(rng, 24)),
        5 => V::Bool(rng.chance(50)),
        6 => V::Null,
        7 => V::U(rng.below(30) as u64),
        8 => V::A(
            (0..rng.below(5))
                .map(|_| random_value(rng, depth - 1))
                .collect(),
        ),
        _ => {
            let mut key = rng.next() >> 40;
            let mut entries = Vec::new();
            for _ in 0..rng.below(5) {
                entries.push((key, random_value(rng, depth - 1)));
                key += 1 + rng.below(30) as u64;
            }
            V::M(entries)
        }
    }
}

/// A chain of `depth` nested containers, for the depth limit.
fn nested_chain(rng: &mut Rng, depth: usize) -> V {
    let mut v = V::U(rng.below(24) as u64);
    for _ in 0..depth {
        v = if rng.chance(50) {
            V::A(vec![v])
        } else {
            V::M(vec![(rng.below(24) as u64, v)])
        };
    }
    v
}

/// The offsets of the items in a well-formed document, from a plain walk.
fn item_offsets(doc: &[u8]) -> Vec<usize> {
    let mut offsets = Vec::new();
    let mut c = Cursor::new(doc);
    while c.position() < doc.len() {
        offsets.push(c.position());
        if c.next_item().is_err() {
            break;
        }
    }
    offsets
}

fn head_len(initial: u8) -> usize {
    match initial & 0x1F {
        info @ 24..=27 => 1 + (1 << (info - 24)),
        _ => 1,
    }
}

/// Rewrites one item's head: boundary arguments, widths that are not shortest, the reserved
/// and indefinite forms, or another major type. This is where length and count arithmetic
/// and the limits live, and a uniform byte flip rarely lands on them.
fn head_fault(doc: &mut Vec<u8>, rng: &mut Rng) {
    let offsets = item_offsets(doc);
    if offsets.is_empty() {
        return;
    }
    let at = rng.pick(&offsets);
    let old_len = head_len(doc[at]).min(doc.len() - at);
    let major = if rng.chance(70) {
        doc[at] >> 5
    } else {
        rng.below(8) as u8
    };
    let arg = match rng.below(9) {
        0 => rng.edge_u64(),
        1 => rng.pick(&[65_535, 65_536, 65_537]),
        2 => rng.pick(&[64 * 1024 - 1, 64 * 1024, 64 * 1024 + 1]),
        3 => rng.pick(&[23, 24, 255, 256, 65_535, 65_536, 0xFFFF_FFFF, 0x1_0000_0000]),
        4 => rng.pick(&[1 << 20, (1 << 20) + 1, u64::MAX, u64::MAX - 1]),
        _ => rng.below(32) as u64,
    };
    let mut new = Vec::new();
    match rng.below(12) {
        // A wider head than the value needs (not shortest).
        0..=2 => {
            let (info, width) = rng.pick(&[(24u8, 1usize), (25, 2), (26, 4), (27, 8)]);
            new.push((major << 5) | info);
            new.extend_from_slice(&arg.to_be_bytes()[8 - width..]);
        }
        // The reserved additional-information values, indefinite lengths, and major 7's
        // float, simple and break forms.
        3 | 4 => new.push((major << 5) | rng.pick(&[28, 29, 30, 31])),
        5 => new.extend([0xF7, 0xF8, 0xFC, 0xFF].iter().take(1 + rng.below(4)).rev()),
        6 => {
            new.push(0xF9);
            new.extend([0x7E, 0x00]);
        }
        7 => new.push(0xC0 | rng.below(24) as u8),
        _ => head(&mut new, major, arg),
    }
    doc.splice(at..at + old_len, new);
}

// ------------------------------------------------------------------ OCBOR soak

/// Items in document order, as a [`Cursor`] should return them.
fn flatten<'a>(v: &'a V, out: &mut Vec<Item<'a>>) {
    match v {
        V::U(n) => out.push(Item::Unsigned(*n)),
        V::N(n) => out.push(Item::Negative(*n)),
        V::B(b) => out.push(Item::Bytes(b)),
        V::T(t) => out.push(Item::Text(t)),
        V::A(items) => {
            out.push(Item::Array(items.len() as u64));
            items.iter().for_each(|i| flatten(i, out));
        }
        V::M(entries) => {
            out.push(Item::Map(entries.len() as u64));
            for (k, value) in entries {
                out.push(Item::Unsigned(*k));
                flatten(value, out);
            }
        }
        V::Bool(b) => out.push(Item::Bool(*b)),
        V::Null => out.push(Item::Null),
    }
}

/// `OCBOR_SEED`, case `n`, reproduces case `n` of the soak below.
const OCBOR_SEED: u64 = 0x4F43_424F_5253_4F41; // "OCBORSOA"

#[test]
fn mutated_documents_get_the_oracles_verdict() {
    let mut reached = Reached::default();
    let total = cases(24_000);
    for case in 0..total {
        let mut rng = Rng::for_case(OCBOR_SEED, case);
        let original = match rng.below(16) {
            0 => nested_chain(&mut rng, 14),
            1 => {
                let d = 15 + rng.below(3);
                nested_chain(&mut rng, d)
            }
            _ => random_value(&mut rng, 3),
        };
        let mut doc = encoded(&original);
        match rng.below(10) {
            // Leave a share untouched, so accepted documents and the limits stay covered.
            0 | 1 => {}
            2..=4 => mutate(&mut doc, &mut rng),
            5..=7 => {
                for _ in 0..1 + rng.below(4) {
                    head_fault(&mut doc, &mut rng);
                }
            }
            _ => {
                head_fault(&mut doc, &mut rng);
                mutate(&mut doc, &mut rng);
            }
        }
        if case % 4096 == 4095 {
            doc = vec![0; MAX_DOCUMENT + 1 + rng.below(2)];
        }

        let verdict = oracle(&doc);
        let got = ocbor::validate(&doc);
        assert_eq!(
            got.map_err(|e| (e.offset, e.kind)),
            verdict.as_ref().map(|_| ()).map_err(|&f| f),
            "case {case}: the validator and the oracle disagree"
        );
        match &verdict {
            Err((at, kind)) => {
                reached.hit(kind_name(*kind));
                // A refusal names an offset inside the document, or one past its end for
                // an item that is not there.
                assert!(*at <= doc.len(), "case {case}");
            }
            Ok(tree) => {
                reached.hit("accepted");
                // RFC 8949 §4.2: the accepted document is the one canonical encoding.
                assert!(
                    encoded(tree) == doc,
                    "case {case}: accepted but not canonical"
                );
                // A cursor over it reads exactly the tree, in order, to the last byte.
                let mut items = Vec::new();
                flatten(tree, &mut items);
                let mut c = Cursor::new(&doc);
                let mut last = 0;
                for want in items {
                    let item = c
                        .next_item()
                        .unwrap_or_else(|e| panic!("case {case}: {e:?}"));
                    assert_eq!(item, want, "case {case}");
                    assert!(c.position() > last, "case {case}");
                    last = c.position();
                }
                assert_eq!(c.position(), doc.len(), "case {case}");
                // Skipping the top-level item's contents also lands on the end.
                let mut c = Cursor::new(&doc);
                let top = c.next_item().unwrap();
                c.skip_contents(top).unwrap();
                assert_eq!(c.position(), doc.len(), "case {case}");
            }
        }

        // A cursor is documented for validated documents, but handed anything it must
        // still stop: every read advances or fails, and never passes the end.
        let mut c = Cursor::new(&doc);
        loop {
            let before = c.position();
            assert!(before <= doc.len(), "case {case}");
            let Ok(item) = c.next_item() else { break };
            assert!(c.position() > before, "case {case}");
            if rng.chance(50) && c.skip_contents(item).is_err() {
                break;
            }
            assert!(c.position() <= doc.len(), "case {case}");
        }
    }
    reached.require("OCBOR-1 soak", &OCBOR_REFUSALS);
    assert!(
        reached.count("accepted") >= total as u64 / 10,
        "only {} accepted",
        reached.count("accepted")
    );
}

// ------------------------------------------------------------------ KRN manifest soak

/// The manifest as the key registry (K1–K4, R4) reads it, by key lookup on the value tree.
#[derive(Debug, PartialEq, Eq)]
struct ModelKrn {
    build_id: Vec<u8>,
    rustc: String,
    commit: String,
    llvm: String,
    flags: Vec<u8>,
    cpu: u64,
    shstk: bool,
    entry: u64,
    abi: (u64, u64),
}

fn printable(t: &str) -> bool {
    !t.is_empty() && t.bytes().all(|b| (0x20..=0x7E).contains(&b))
}

fn lookup(entries: &[(u64, V)], key: u64) -> Option<&V> {
    entries.iter().find(|(k, _)| *k == key).map(|(_, v)| v)
}

fn allowed_keys(entries: &[(u64, V)], known: &[u64]) -> bool {
    entries.iter().all(|(k, _)| *k >= 1000 || known.contains(k))
}

/// `None` where the registry says the manifest is invalid.
fn model_krn(tree: &V) -> Option<ModelKrn> {
    let V::M(top) = tree else { return None };
    if !allowed_keys(top, &[0, 1, 2, 3, 4, 5, 24]) {
        return None;
    }
    if lookup(top, 0)? != &V::U(1) {
        return None;
    }
    let V::B(build_id) = lookup(top, 1)? else {
        return None;
    };
    let V::M(tc) = lookup(top, 2)? else {
        return None;
    };
    if !allowed_keys(tc, &[0, 1, 2, 3]) {
        return None;
    }
    let (V::T(rustc), V::T(commit), V::T(llvm), V::B(flags)) = (
        lookup(tc, 0)?,
        lookup(tc, 1)?,
        lookup(tc, 2)?,
        lookup(tc, 3)?,
    ) else {
        return None;
    };
    let hex = commit.len() == 40
        && commit
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if build_id.len() != 32 || flags.len() != 32 || !hex || !printable(rustc) || !printable(llvm) {
        return None;
    }
    let V::U(cpu) = lookup(top, 3)? else {
        return None;
    };
    if cpu & !0x3F != 0 {
        return None;
    }
    let V::M(cet) = lookup(top, 4)? else {
        return None;
    };
    if !allowed_keys(cet, &[0, 1]) {
        return None;
    }
    let (V::Bool(shstk), V::Bool(false)) = (lookup(cet, 0)?, lookup(cet, 1)?) else {
        return None;
    };
    let V::U(entry) = lookup(top, 5)? else {
        return None;
    };
    let V::A(abi) = lookup(top, 24)? else {
        return None;
    };
    let [V::U(major), V::U(minor)] = abi.as_slice() else {
        return None;
    };
    if *major > 0xFFFF || *minor > 0xFFFF {
        return None;
    }
    Some(ModelKrn {
        build_id: build_id.clone(),
        rustc: rustc.clone(),
        commit: commit.clone(),
        llvm: llvm.clone(),
        flags: flags.clone(),
        cpu: *cpu,
        shstk: *shstk,
        entry: *entry,
        abi: (*major, *minor),
    })
}

fn decoded_as_model(m: &KrnManifest<'_>) -> ModelKrn {
    ModelKrn {
        build_id: m.build_id.to_vec(),
        rustc: m.toolchain.rustc.to_owned(),
        commit: m.toolchain.rustc_commit.to_owned(),
        llvm: m.toolchain.llvm.to_owned(),
        flags: m.toolchain.flags_digest.to_vec(),
        cpu: m.cpu_features.0,
        shstk: m.cet.shstk,
        entry: m.entry,
        abi: (u64::from(m.boot_info_abi.0), u64::from(m.boot_info_abi.1)),
    }
}

fn manifest_error_name(e: ManifestError) -> &'static str {
    match e {
        ManifestError::Ocbor(_) => "Ocbor",
        ManifestError::NotAMap => "NotAMap",
        ManifestError::UnsupportedSchema(_) => "UnsupportedSchema",
        ManifestError::UnknownKey(_) => "UnknownKey",
        ManifestError::MissingKey(_) => "MissingKey",
        ManifestError::WrongType(_) => "WrongType",
        ManifestError::InvalidValue(_) => "InvalidValue",
        ManifestError::ReservedFeatureBits(_) => "ReservedFeatureBits",
        ManifestError::IbtNotSupported => "IbtNotSupported",
        ManifestError::NotKrn => "NotKrn",
        ManifestError::EntryNotExecutable(_) => "EntryNotExecutable",
    }
}

const KRN_REFUSALS: [&str; 10] = [
    "Ocbor",
    "NotAMap",
    "UnsupportedSchema",
    "UnknownKey",
    "MissingKey",
    "WrongType",
    "InvalidValue",
    "ReservedFeatureBits",
    "IbtNotSupported",
    "accepted",
];

const COMMIT: &str = "58cf8f9c35b355199727627d07ae34a4e2846d67";

/// One fault in the manifest's field list. Keys stay sorted and every length and count stays
/// right, so the schema checks are what refuses the bad ones.
fn manifest_fault(entries: &mut Vec<(u64, V)>, rng: &mut Rng) {
    fn set(entries: &mut Vec<(u64, V)>, key: u64, value: V) {
        match entries.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value,
            None => entries.push((key, value)),
        }
    }
    let wrong = random_value(rng, 1);
    match rng.below(16) {
        // Benign: free values change, extensions appear.
        0 => set(entries, 5, V::U(rng.edge_u64())),
        1 => set(entries, 3, V::U(rng.below(64) as u64)),
        2 => set(entries, 1, V::B(vec![rng.byte(); 32])),
        3 => set(entries, 1000 + rng.next() % 5000, wrong),
        4 => {
            let abi = V::A(vec![
                V::U(rng.below(1 << 16) as u64),
                V::U(rng.below(1 << 16) as u64),
            ]);
            set(entries, 24, abi);
        }
        // Wrong types and values.
        5 => {
            let key = rng.pick(&[0, 1, 2, 3, 4, 5, 24]);
            set(entries, key, wrong);
        }
        6 => set(entries, 0, V::U(rng.pick(&[0, 2, 3, u64::MAX]))),
        7 => set(entries, 1, V::B(vec![0; rng.pick(&[0, 31, 33, 64])])),
        8 => set(entries, 3, V::U(rng.edge_u64() | 1 << (6 + rng.below(58)))),
        9 => set(
            entries,
            24,
            match rng.below(3) {
                0 => V::A(vec![V::U(1)]),
                1 => V::A(vec![V::U(1), V::U(1), V::U(1)]),
                _ => V::A(vec![V::U(rng.edge_u64() | 1 << 16), V::U(0)]),
            },
        ),
        // Inside the nested maps.
        10 | 11 => {
            let Some((_, V::M(tc))) = entries.iter_mut().find(|(k, _)| *k == 2) else {
                return;
            };
            let value = match rng.below(9) {
                0 => V::T(String::new()),
                1 => V::T("1.99.1\u{e9}".to_owned()),
                2 => V::T(COMMIT.to_uppercase()),
                3 => V::T(COMMIT[..39].to_owned()),
                4 => V::B(vec![0; 31]),
                5 => V::T("\u{7f}".to_owned()),
                _ => V::U(1),
            };
            let key = rng.pick(&[0, 1, 2, 3, 4, 2000]);
            set(tc, key, value);
            tc.sort_by_key(|(k, _)| *k);
        }
        12 => {
            let Some((_, V::M(cet))) = entries.iter_mut().find(|(k, _)| *k == 4) else {
                return;
            };
            let key = rng.pick(&[0, 1, 2, 3000]);
            let value = match rng.below(4) {
                0 => V::Bool(true),
                1 => V::Bool(false),
                2 => V::U(0),
                _ => V::T("x".to_owned()),
            };
            set(cet, key, value);
            cet.sort_by_key(|(k, _)| *k);
        }
        // Keys appear and disappear.
        13 => set(entries, 6 + rng.below(994) as u64, V::U(0)),
        14 if !entries.is_empty() => {
            let i = rng.below(entries.len());
            entries.remove(i);
        }
        _ => {
            entries.clear();
        }
    }
    entries.sort_by_key(|(k, _)| *k);
}

fn entries_of(doc: &[u8]) -> Vec<(u64, V)> {
    match oracle(doc) {
        Ok(V::M(e)) => e,
        _ => unreachable!("the standard manifest is a map"),
    }
}

/// Drops extension keys (>= 1000), which `decode` skips, from all three maps.
fn without_extensions(entries: &[(u64, V)]) -> Vec<(u64, V)> {
    entries
        .iter()
        .filter(|(k, _)| *k < 1000)
        .map(|(k, v)| {
            let v = match v {
                V::M(sub) if matches!(*k, 2 | 4) => {
                    V::M(sub.iter().filter(|(k, _)| *k < 1000).cloned().collect())
                }
                other => other.clone(),
            };
            (*k, v)
        })
        .collect()
}

/// `KRN_SEED`, case `n`, reproduces case `n` of the soak below.
const KRN_SEED: u64 = 0x4B52_4E4D_414E_4946; // "KRNMANIF"

#[test]
fn mutated_krn_manifests_decode_only_what_the_registry_allows() {
    let standard = {
        let mut out = Vec::new();
        encode(
            &V::M({
                let doc = crate::tests_ocbor::map(&krn_entries());
                entries_of(&doc)
            }),
            &mut out,
        );
        out
    };
    let mut reached = Reached::default();
    let total = cases(10_000);
    for case in 0..total {
        let mut rng = Rng::for_case(KRN_SEED, case);
        let mut entries = entries_of(&standard);
        for _ in 0..rng.below(5) {
            manifest_fault(&mut entries, &mut rng);
        }
        let mut doc = encoded(&V::M(entries));
        if rng.chance(25) {
            mutate(&mut doc, &mut rng);
        } else if rng.chance(25) {
            head_fault(&mut doc, &mut rng);
        }

        let model = oracle(&doc).ok().as_ref().and_then(model_krn);
        match KrnManifest::decode(&doc) {
            Ok(m) => {
                reached.hit("accepted");
                let expect = model.unwrap_or_else(|| {
                    panic!("case {case}: decoded a manifest the registry refuses: {doc:02x?}")
                });
                assert_eq!(decoded_as_model(&m), expect, "case {case}");
                // Without its extensions, the decoded manifest re-encodes to the document.
                let tree = oracle(&doc).expect("accepted implies valid OCBOR-1");
                let V::M(top) = &tree else { unreachable!() };
                assert!(
                    encode_krn(&m) == encoded(&V::M(without_extensions(top))),
                    "case {case}: decoded fields do not re-encode to the document"
                );
            }
            Err(e) => {
                reached.hit(manifest_error_name(e));
                assert!(
                    model.is_none(),
                    "case {case}: refused ({e:?}) a manifest the registry allows: {doc:02x?}"
                );
            }
        }
    }
    reached.require("KRN manifest soak", &KRN_REFUSALS);
    assert!(
        reached.count("accepted") >= total as u64 / 20,
        "only {} accepted",
        reached.count("accepted")
    );
}
